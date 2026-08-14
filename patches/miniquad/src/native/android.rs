//! Akizuki fork: NativeActivity-based Android backend.
//!
//! Upstream miniquad 0.3.16 requires a custom Java `MainActivity` that has to
//! be compiled into a dex and embedded into the APK, which `cargo-apk` 0.10
//! cannot produce (it has no Java/dex tooling). Instead we implement the
//! classic `android_native_app_glue` pattern in pure Rust:
//!
//! * `ANativeActivity_onCreate` is called by the system, we install lifecycle
//!   callbacks, keep a global ref to the activity and then call `quad_main()`
//!   (exported by the application crate).
//! * An input thread owns an `ALooper` with the `AInputQueue` attached and
//!   translates `AMotionEvent`/`AKeyEvent` into `Message::Touch/Key*`.
//! * A render thread waits for the first `Message::SurfaceChanged` (delivered
//!   from `onNativeWindowCreated`) and then drives EGL + the event handler.
//!
//! No Java code is required in the APK.

use crate::{
    event::{EventHandler, KeyCode, TouchPhase},
    native::egl::{self, LibEgl},
    native::NativeDisplay,
    GraphicsContext,
};

use std::{
    ffi::{CStr, CString},
    sync::{
        atomic::{AtomicBool, AtomicPtr, Ordering},
        mpsc, Mutex,
    },
    thread,
};

pub use crate::gl::{self, *};

mod keycodes;

pub use ndk_sys;

pub mod ndk_utils;

#[no_mangle]
pub unsafe extern "C" fn JNI_OnLoad(
    vm: *mut ndk_sys::JavaVM,
    _: std::ffi::c_void,
) -> ndk_sys::jint {
    VM = vm as *mut _ as _;

    ndk_sys::JNI_VERSION_1_6 as _
}

extern "C" {
    fn quad_main();
}

/// Looper identifier for the input queue, matches android_native_app_glue.
const LOOPER_ID_INPUT: i32 = 3;

/// Short recap on how miniquad on Android works
/// There is a MainActivity, a normal Java activity
/// It creates a View and pass a reference to a view to rust.
/// Rust spawn a thread that render things into this view as often as
/// possible.
/// Also MainActivty collects user input events and calls native rust functions.
///
/// This long explanation was to illustrate how we ended up with evets callback
/// and drawing in the different threads.
/// Message enum is used to send data from the callbacks to the drawing thread.
#[derive(Debug)]
enum Message {
    SurfaceChanged {
        window: *mut ndk_sys::ANativeWindow,
        width: i32,
        height: i32,
    },
    SurfaceDestroyed,
    Touch {
        phase: TouchPhase,
        touch_id: u64,
        x: f32,
        y: f32,
    },
    KeyDown {
        keycode: KeyCode,
    },
    KeyUp {
        keycode: KeyCode,
    },
    Pause,
    Resume,
    Destroy,
}
unsafe impl Send for Message {}

// Messages are produced on the UI thread (lifecycle callbacks) and on the
// input thread (touch/key events), consumed by the render thread.
static MESSAGES_TX: Mutex<Option<mpsc::Sender<Message>>> = Mutex::new(None);

fn send_message(message: Message) {
    if let Ok(guard) = MESSAGES_TX.lock() {
        if let Some(tx) = guard.as_ref() {
            let _ = tx.send(message);
        }
    }
}

static mut ACTIVITY: ndk_sys::jobject = std::ptr::null_mut();
static mut VM: *mut ndk_sys::JavaVM = std::ptr::null_mut();
static mut ACTIVITY_PTR: *mut ndk_sys::ANativeActivity = std::ptr::null_mut();
static mut ASSET_MANAGER: *mut ndk_sys::AAssetManager = std::ptr::null_mut();
static INTERNAL_STORAGE_PATH: Mutex<Option<String>> = Mutex::new(None);

// Mirrors the last fullscreen request so the resume callback can re-apply
// the window flags after the system reset them.
static FULLSCREEN_REQUESTED: AtomicBool = AtomicBool::new(false);

// Written by the UI thread callbacks, read by the input thread.
static INPUT_QUEUE: AtomicPtr<ndk_sys::AInputQueue> = AtomicPtr::new(std::ptr::null_mut());
static INPUT_QUEUE_ATTACHED: AtomicPtr<ndk_sys::AInputQueue> = AtomicPtr::new(std::ptr::null_mut());
static EXIT_INPUT_THREAD: AtomicBool = AtomicBool::new(false);

struct AndroidDisplay {
    screen_width: f32,
    screen_height: f32,
    fullscreen: bool,
}

impl NativeDisplay for AndroidDisplay {
    fn screen_size(&self) -> (f32, f32) {
        (self.screen_width as _, self.screen_height as _)
    }
    fn dpi_scale(&self) -> f32 {
        1.
    }
    fn high_dpi(&self) -> bool {
        true
    }
    fn order_quit(&mut self) {}
    fn request_quit(&mut self) {}
    fn cancel_quit(&mut self) {}
    fn set_cursor_grab(&mut self, _grab: bool) {}
    fn show_mouse(&mut self, _shown: bool) {}
    fn set_mouse_cursor(&mut self, _cursor: crate::CursorIcon) {}
    fn set_window_size(&mut self, _new_width: u32, _new_height: u32) {}
    fn set_fullscreen(&mut self, fullscreen: bool) {
        unsafe {
            set_full_screen(fullscreen);
        }
        self.fullscreen = fullscreen;
    }
    fn clipboard_get(&mut self) -> Option<String> {
        None
    }
    fn clipboard_set(&mut self, _data: &str) {}
    fn show_keyboard(&mut self, _show: bool) {
        // NativeActivity has no Java view to attach an IME to; hardware
        // keyboards still work through the input queue.
    }
    fn as_any(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

pub unsafe fn console_debug(msg: *const ::std::os::raw::c_char) {
    ndk_sys::__android_log_write(
        ndk_sys::android_LogPriority_ANDROID_LOG_DEBUG as _,
        b"SAPP\0".as_ptr() as _,
        msg,
    );
}

pub unsafe fn console_info(msg: *const ::std::os::raw::c_char) {
    ndk_sys::__android_log_write(
        ndk_sys::android_LogPriority_ANDROID_LOG_INFO as _,
        b"SAPP\0".as_ptr() as _,
        msg,
    );
}

pub unsafe fn console_warn(msg: *const ::std::os::raw::c_char) {
    ndk_sys::__android_log_write(
        ndk_sys::android_LogPriority_ANDROID_LOG_WARN as _,
        b"SAPP\0".as_ptr() as _,
        msg,
    );
}

pub unsafe fn console_error(msg: *const ::std::os::raw::c_char) {
    ndk_sys::__android_log_write(
        ndk_sys::android_LogPriority_ANDROID_LOG_ERROR as _,
        b"SAPP\0".as_ptr() as _,
        msg,
    );
}

/// Get the JNI Env by calling ndk's AttachCurrentThread
///
/// Safety note: This function is not exactly correct now, it should be fixed!
///
/// AttachCurrentThread should be called at least once for any given thread that
/// wants to use the JNI and DetachCurrentThread should be called only once, when
/// the thread stack is empty and the thread is about to stop
///
/// calling AttachCurrentThread from the same thread multiple time is very cheap
pub unsafe fn attach_jni_env() -> *mut ndk_sys::JNIEnv {
    let mut env: *mut ndk_sys::JNIEnv = std::ptr::null_mut();
    let attach_current_thread = (**VM).AttachCurrentThread.unwrap();

    let res = attach_current_thread(VM, &mut env, std::ptr::null_mut());
    assert!(res == 0);

    env
}

// ============ NativeActivity glue ============

unsafe fn set_full_screen(fullscreen: bool) {
    let activity = ACTIVITY_PTR;
    if activity.is_null() {
        return;
    }
    FULLSCREEN_REQUESTED.store(fullscreen, Ordering::SeqCst);
    if fullscreen {
        ndk_sys::ANativeActivity_setWindowFlags(
            activity,
            ndk_sys::AWINDOW_FLAG_FULLSCREEN | ndk_sys::AWINDOW_FLAG_LAYOUT_NO_LIMITS,
            0,
        );
    } else {
        ndk_sys::ANativeActivity_setWindowFlags(
            activity,
            0,
            ndk_sys::AWINDOW_FLAG_FULLSCREEN | ndk_sys::AWINDOW_FLAG_LAYOUT_NO_LIMITS,
        );
    }
}

/// Entry point called by `android.app.NativeActivity` (see the manifest).
/// Must return quickly; the game runs on dedicated threads spawned from
/// `run()` which is reached through `quad_main()`.
#[no_mangle]
pub unsafe extern "C" fn ANativeActivity_onCreate(
    activity: *mut ndk_sys::ANativeActivity,
    _saved_state: *mut std::ffi::c_void,
    _saved_state_size: usize,
) {
    if activity.is_null() {
        return;
    }

    VM = (*activity).vm;
    ACTIVITY_PTR = activity;

    let env = (*activity).env;
    if !env.is_null() && !(*activity).clazz.is_null() {
        ACTIVITY = crate::new_global_ref!(env, (*activity).clazz);
    }

    if !(*activity).internalDataPath.is_null() {
        let path = CStr::from_ptr((*activity).internalDataPath)
            .to_string_lossy()
            .into_owned();
        if let Ok(mut guard) = INTERNAL_STORAGE_PATH.lock() {
            *guard = Some(path);
        }
    }

    ASSET_MANAGER = (*activity).assetManager;

    ndk_sys::ANativeActivity_setWindowFormat(
        activity,
        ndk_sys::ANativeWindow_LegacyFormat_WINDOW_FORMAT_RGBA_8888 as i32,
    );

    let mut callbacks: ndk_sys::ANativeActivityCallbacks = std::mem::zeroed();
    callbacks.onStart = Some(on_start);
    callbacks.onResume = Some(on_resume);
    callbacks.onPause = Some(on_pause);
    callbacks.onStop = Some(on_stop);
    callbacks.onDestroy = Some(on_destroy);
    callbacks.onWindowFocusChanged = Some(on_window_focus_changed);
    callbacks.onNativeWindowCreated = Some(on_native_window_created);
    callbacks.onNativeWindowResized = Some(on_native_window_resized);
    callbacks.onNativeWindowDestroyed = Some(on_native_window_destroyed);
    callbacks.onInputQueueCreated = Some(on_input_queue_created);
    callbacks.onInputQueueDestroyed = Some(on_input_queue_destroyed);
    callbacks.onConfigurationChanged = Some(on_configuration_changed);
    callbacks.onLowMemory = Some(on_low_memory);

    // The system keeps the activity alive for the whole app lifetime, so
    // leaking the callbacks box is intentional.
    let callbacks = Box::into_raw(Box::new(callbacks));
    (*activity).callbacks = callbacks;
    (*activity).instance = callbacks as *mut _;

    quad_main();
}

unsafe extern "C" fn on_start(_: *mut ndk_sys::ANativeActivity) {}

unsafe extern "C" fn on_resume(_: *mut ndk_sys::ANativeActivity) {
    if FULLSCREEN_REQUESTED.load(Ordering::SeqCst) {
        set_full_screen(true);
    }
    send_message(Message::Resume);
}

unsafe extern "C" fn on_pause(_: *mut ndk_sys::ANativeActivity) {
    send_message(Message::Pause);
}

unsafe extern "C" fn on_stop(_: *mut ndk_sys::ANativeActivity) {}

unsafe extern "C" fn on_destroy(_: *mut ndk_sys::ANativeActivity) {
    EXIT_INPUT_THREAD.store(true, Ordering::SeqCst);
    send_message(Message::Destroy);
}

unsafe extern "C" fn on_window_focus_changed(
    _: *mut ndk_sys::ANativeActivity,
    _has_focus: std::os::raw::c_int,
) {
}

unsafe extern "C" fn on_native_window_created(
    _: *mut ndk_sys::ANativeActivity,
    window: *mut ndk_sys::ANativeWindow,
) {
    if window.is_null() {
        return;
    }
    // Take our own reference: it is released by the render thread when the
    // surface is destroyed (Message::SurfaceDestroyed).
    ndk_sys::ANativeWindow_acquire(window);
    let width = ndk_sys::ANativeWindow_getWidth(window);
    let height = ndk_sys::ANativeWindow_getHeight(window);
    send_message(Message::SurfaceChanged {
        window,
        width,
        height,
    });
}

unsafe extern "C" fn on_native_window_resized(
    _: *mut ndk_sys::ANativeActivity,
    window: *mut ndk_sys::ANativeWindow,
) {
    if window.is_null() {
        return;
    }
    let width = ndk_sys::ANativeWindow_getWidth(window);
    let height = ndk_sys::ANativeWindow_getHeight(window);
    send_message(Message::SurfaceChanged {
        window,
        width,
        height,
    });
}

unsafe extern "C" fn on_native_window_destroyed(
    _: *mut ndk_sys::ANativeActivity,
    _window: *mut ndk_sys::ANativeWindow,
) {
    send_message(Message::SurfaceDestroyed);
}

unsafe extern "C" fn on_input_queue_created(
    _: *mut ndk_sys::ANativeActivity,
    queue: *mut ndk_sys::AInputQueue,
) {
    INPUT_QUEUE.store(queue, Ordering::SeqCst);
}

unsafe extern "C" fn on_input_queue_destroyed(
    _: *mut ndk_sys::ANativeActivity,
    _queue: *mut ndk_sys::AInputQueue,
) {
    INPUT_QUEUE.store(std::ptr::null_mut(), Ordering::SeqCst);
}

unsafe extern "C" fn on_configuration_changed(_: *mut ndk_sys::ANativeActivity) {}

unsafe extern "C" fn on_low_memory(_: *mut ndk_sys::ANativeActivity) {}

// ============ Input thread ============

fn input_thread_loop() {
    unsafe {
        let looper = ndk_sys::ALooper_prepare(0);
        if looper.is_null() {
            console_error(b"ALooper_prepare failed\0".as_ptr() as _);
            return;
        }

        while !EXIT_INPUT_THREAD.load(Ordering::SeqCst) {
            let queue = INPUT_QUEUE.load(Ordering::SeqCst);
            let attached = INPUT_QUEUE_ATTACHED.load(Ordering::SeqCst);

            if attached != queue {
                if !attached.is_null() {
                    ndk_sys::AInputQueue_detachLooper(attached);
                }
                INPUT_QUEUE_ATTACHED.store(queue, Ordering::SeqCst);
                if !queue.is_null() {
                    ndk_sys::AInputQueue_attachLooper(
                        queue,
                        looper,
                        LOOPER_ID_INPUT,
                        None,
                        std::ptr::null_mut(),
                    );
                }
            }

            if !queue.is_null() && ndk_sys::AInputQueue_hasEvents(queue) != 0 {
                drain_input(queue);
            } else {
                thread::sleep(std::time::Duration::from_millis(5));
            }
        }
    }
}

unsafe fn drain_input(queue: *mut ndk_sys::AInputQueue) {
    loop {
        let mut event: *mut ndk_sys::AInputEvent = std::ptr::null_mut();
        let res = ndk_sys::AInputQueue_getEvent(queue, &mut event);
        if res < 0 || event.is_null() {
            break;
        }
        let _handled = process_input_event(event);
        ndk_sys::AInputQueue_finishEvent(queue, event, 1);
    }
}

unsafe fn process_input_event(event: *mut ndk_sys::AInputEvent) -> bool {
    let etype = ndk_sys::AInputEvent_getType(event);

    if etype == ndk_sys::AINPUT_EVENT_TYPE_MOTION as i32 {
        let action = ndk_sys::AMotionEvent_getAction(event) as u32;
        let masked = action & ndk_sys::AMOTION_EVENT_ACTION_MASK;
        let pointer_count = ndk_sys::AMotionEvent_getPointerCount(event);

        match masked {
            ndk_sys::AMOTION_EVENT_ACTION_DOWN
            | ndk_sys::AMOTION_EVENT_ACTION_MOVE
            | ndk_sys::AMOTION_EVENT_ACTION_UP
            | ndk_sys::AMOTION_EVENT_ACTION_CANCEL => {
                let phase = if masked == ndk_sys::AMOTION_EVENT_ACTION_DOWN {
                    TouchPhase::Started
                } else if masked == ndk_sys::AMOTION_EVENT_ACTION_UP {
                    TouchPhase::Ended
                } else if masked == ndk_sys::AMOTION_EVENT_ACTION_CANCEL {
                    TouchPhase::Cancelled
                } else {
                    TouchPhase::Moved
                };
                for i in 0..pointer_count {
                    let touch_id = ndk_sys::AMotionEvent_getPointerId(event, i) as u64;
                    let x = ndk_sys::AMotionEvent_getX(event, i);
                    let y = ndk_sys::AMotionEvent_getY(event, i);
                    send_message(Message::Touch {
                        phase,
                        touch_id,
                        x,
                        y,
                    });
                }
            }
            ndk_sys::AMOTION_EVENT_ACTION_POINTER_DOWN
            | ndk_sys::AMOTION_EVENT_ACTION_POINTER_UP => {
                let index = ((action & ndk_sys::AMOTION_EVENT_ACTION_POINTER_INDEX_MASK)
                    >> ndk_sys::AMOTION_EVENT_ACTION_POINTER_INDEX_SHIFT) as u64;
                if index < pointer_count {
                    let touch_id = ndk_sys::AMotionEvent_getPointerId(event, index) as u64;
                    let x = ndk_sys::AMotionEvent_getX(event, index);
                    let y = ndk_sys::AMotionEvent_getY(event, index);
                    let phase = if masked == ndk_sys::AMOTION_EVENT_ACTION_POINTER_DOWN {
                        TouchPhase::Started
                    } else {
                        TouchPhase::Ended
                    };
                    send_message(Message::Touch {
                        phase,
                        touch_id,
                        x,
                        y,
                    });
                }
            }
            _ => {}
        }
        return true;
    }

    if etype == ndk_sys::AINPUT_EVENT_TYPE_KEY as i32 {
        let action = ndk_sys::AKeyEvent_getAction(event) as u32;
        let keycode = keycodes::translate_keycode(ndk_sys::AKeyEvent_getKeyCode(event) as u32);
        match action {
            ndk_sys::AKEY_EVENT_ACTION_DOWN => {
                send_message(Message::KeyDown { keycode });
            }
            ndk_sys::AKEY_EVENT_ACTION_UP => {
                send_message(Message::KeyUp { keycode });
            }
            _ => {}
        }
        return true;
    }

    false
}

// ============ Render thread ============

struct MainThreadState {
    libegl: LibEgl,
    context: GraphicsContext,
    egl_display: egl::EGLDisplay,
    egl_config: egl::EGLConfig,
    egl_context: egl::EGLContext,
    surface: egl::EGLSurface,
    display: AndroidDisplay,
    window: *mut ndk_sys::ANativeWindow,
    event_handler: Box<dyn EventHandler>,
    quit: bool,
}

impl MainThreadState {
    unsafe fn destroy_surface(&mut self) {
        (self.libegl.eglMakeCurrent.unwrap())(
            self.egl_display,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        );
        (self.libegl.eglDestroySurface.unwrap())(self.egl_display, self.surface);
        self.surface = std::ptr::null_mut();
    }

    unsafe fn update_surface(&mut self, window: *mut ndk_sys::ANativeWindow) {
        self.window = window;
        if self.surface.is_null() == false {
            self.destroy_surface();
        }

        self.surface = (self.libegl.eglCreateWindowSurface.unwrap())(
            self.egl_display,
            self.egl_config,
            window as _,
            std::ptr::null_mut(),
        );

        assert!(!self.surface.is_null());

        let res = (self.libegl.eglMakeCurrent.unwrap())(
            self.egl_display,
            self.surface,
            self.surface,
            self.egl_context,
        );

        assert!(res != 0);
    }

    fn process_message(&mut self, msg: Message) {
        match msg {
            Message::SurfaceDestroyed => unsafe {
                self.destroy_surface();
                // Release the reference acquired in on_native_window_created.
                if !self.window.is_null() {
                    ndk_sys::ANativeWindow_release(self.window);
                    self.window = std::ptr::null_mut();
                }
            },
            Message::SurfaceChanged {
                window,
                width,
                height,
            } => {
                unsafe {
                    self.update_surface(window);
                }

                self.display.screen_width = width as _;
                self.display.screen_height = height as _;
                self.event_handler.resize_event(
                    self.context.with_display(&mut self.display),
                    width as _,
                    height as _,
                );
            }
            Message::Touch {
                phase,
                touch_id,
                x,
                y,
            } => {
                self.event_handler.touch_event(
                    self.context.with_display(&mut self.display),
                    phase,
                    touch_id,
                    x,
                    y,
                );
            }
            Message::KeyDown { keycode } => {
                self.event_handler.key_down_event(
                    self.context.with_display(&mut self.display),
                    keycode,
                    Default::default(),
                    false,
                );
            }
            Message::KeyUp { keycode } => {
                self.event_handler.key_up_event(
                    self.context.with_display(&mut self.display),
                    keycode,
                    Default::default(),
                );
            }
            Message::Pause => self
                .event_handler
                .window_minimized_event(self.context.with_display(&mut self.display)),
            Message::Resume => {
                if self.display.fullscreen {
                    unsafe {
                        set_full_screen(true);
                    }
                }

                self.event_handler
                    .window_restored_event(self.context.with_display(&mut self.display))
            }
            Message::Destroy => {
                self.quit = true;
            }
        }
    }

    fn frame(&mut self) {
        self.event_handler
            .update(self.context.with_display(&mut self.display));

        if self.surface.is_null() == false {
            self.event_handler
                .draw(self.context.with_display(&mut self.display));

            unsafe {
                (self.libegl.eglSwapBuffers.unwrap())(self.egl_display, self.surface);
            }
        }
    }
}

pub unsafe fn run<F>(conf: crate::conf::Conf, f: F)
where
    F: 'static + FnOnce(&mut crate::Context) -> Box<dyn EventHandler>,
{
    {
        use std::panic;

        panic::set_hook(Box::new(|info| {
            let msg = CString::new(format!("{:?}", info)).unwrap_or_else(|_| {
                CString::new(format!("MALFORMED ERROR MESSAGE {:?}", info.location())).unwrap()
            });
            console_error(msg.as_ptr());
        }));
    }

    if conf.fullscreen {
        set_full_screen(true);
    }

    // yeah, just adding Send to outer F will do it, but it will brake the API
    // in other backends
    struct SendHack<F>(F);
    unsafe impl<F> Send for SendHack<F> {}

    let f = SendHack(f);

    let (tx, rx) = mpsc::channel();

    if let Ok(mut guard) = MESSAGES_TX.lock() {
        *guard = Some(tx);
    }

    thread::spawn(move || {
        input_thread_loop();
    });

    thread::spawn(move || {
        let mut libegl = LibEgl::try_load().expect("Cant load LibEGL");

        // skip all the messages until android will be able to actually open a window
        //
        // sometimes before launching an app android will show a permission dialog
        // it is important to create GL context only after a first SurfaceChanged
        let (window, screen_width, screen_height) = 'a: loop {
            match rx.try_recv() {
                Ok(Message::SurfaceChanged {
                    window,
                    width,
                    height,
                }) => {
                    break 'a (window, width as f32, height as f32);
                }
                _ => {}
            }
        };

        let (egl_context, egl_config, egl_display) = crate::native::egl::create_egl_context(
            &mut libegl,
            std::ptr::null_mut(), /* EGL_DEFAULT_DISPLAY */
            conf.platform.framebuffer_alpha,
        )
        .expect("Cant create EGL context");

        assert!(!egl_display.is_null());
        assert!(!egl_config.is_null());

        crate::native::gl::load_gl_funcs(|proc| {
            let name = std::ffi::CString::new(proc).unwrap();
            libegl.eglGetProcAddress.expect("non-null function pointer")(name.as_ptr() as _)
        });

        let surface = (libegl.eglCreateWindowSurface.unwrap())(
            egl_display,
            egl_config,
            window as _,
            std::ptr::null_mut(),
        );

        if (libegl.eglMakeCurrent.unwrap())(egl_display, surface, surface, egl_context) == 0 {
            panic!();
        }

        let mut context = GraphicsContext::new(gl::is_gl2());

        let mut display = AndroidDisplay {
            screen_width,
            screen_height,
            fullscreen: conf.fullscreen,
        };
        let event_handler = f.0(context.with_display(&mut display));
        let mut s = MainThreadState {
            libegl,
            egl_display,
            egl_config,
            egl_context,
            surface,
            context,
            display,
            window,
            event_handler,
            quit: false,
        };

        while !s.quit {
            // process all the messages from the main thread
            while let Ok(msg) = rx.try_recv() {
                s.process_message(msg);
            }

            s.frame();

            thread::yield_now();
        }

        (s.libegl.eglMakeCurrent.unwrap())(
            s.egl_display,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        );
        (s.libegl.eglDestroySurface.unwrap())(s.egl_display, s.surface);
        (s.libegl.eglDestroyContext.unwrap())(s.egl_display, s.egl_context);
        (s.libegl.eglTerminate.unwrap())(s.egl_display);
    });
}

// ============ Assets / storage helpers ============

#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct android_asset {
    pub content: *mut ::std::os::raw::c_char,
    pub content_length: ::std::os::raw::c_int,
}

pub(crate) unsafe fn load_asset(filepath: *const ::std::os::raw::c_char, out: *mut android_asset) {
    let asset_manager = ASSET_MANAGER;
    if asset_manager.is_null() {
        return;
    }
    let asset = ndk_sys::AAssetManager_open(asset_manager, filepath, ndk_sys::AASSET_MODE_BUFFER as _);
    if asset.is_null() {
        return;
    }
    let length = ndk_sys::AAsset_getLength64(asset);
    let buffer = libc::malloc(length as _);
    if ndk_sys::AAsset_read(asset, buffer, length as _) > 0 {
        ndk_sys::AAsset_close(asset);

        (*out).content_length = length as _;
        (*out).content = buffer as _;
    }
}

/// Synchronously load a file from the APK assets (relative path).
///
/// Unlike the upstream implementation this frees the temporary buffer after
/// copying the data into an owned `Vec`.
pub fn load_asset_bytes(path: &str) -> Option<Vec<u8>> {
    let c_path = CString::new(path).ok()?;
    let mut asset: android_asset = unsafe { std::mem::zeroed() };

    unsafe { load_asset(c_path.as_ptr(), &mut asset as _) };

    if asset.content.is_null() {
        return None;
    }

    let data = unsafe {
        std::slice::from_raw_parts(asset.content as *const u8, asset.content_length as usize)
            .to_vec()
    };
    unsafe {
        libc::free(asset.content as *mut _);
    }
    Some(data)
}

/// Absolute path to the app's internal storage directory (writeable).
pub fn get_internal_storage_path() -> String {
    INTERNAL_STORAGE_PATH
        .lock()
        .ok()
        .and_then(|g| g.clone())
        .unwrap_or_else(|| String::from("/data/data/rustgal"))
}
