//! akrs-editor 二进制入口。
//!
//! 薄封装，委托给 [`akrs_editor::run_editor`]。保留此文件使 `akrs-editor`
//! 从纯库 crate 变为 lib+bin crate，`cargo build -p akrs-editor` 即产出
//! 可执行文件（`akrs-editor` / `akrs-editor.exe`），不改动公开库 API。

// 临时改为 console 模式以便排查运行时 panic（原本是 windows = "windows" 隐藏控制台）。
// 排查完成后改回 `#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]`。
#![cfg_attr(target_os = "windows", windows_subsystem = "console")]

fn main() {
    // 初始化日志（env_logger 默认 warn+，设 RUST_LOG=debug 可看调试日志）。
    let _ = env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn"))
        .format_timestamp_secs()
        .try_init();

    // 安装 panic hook：把 panic 信息强制输出到 stderr 和消息框，避免控制台吞日志。
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let payload = info.payload();
        let msg = if let Some(s) = payload.downcast_ref::<&str>() {
            (*s).to_string()
        } else if let Some(s) = payload.downcast_ref::<String>() {
            s.clone()
        } else {
            "<panic payload 非 string>".to_string()
        };
        let location = info
            .location()
            .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
            .unwrap_or_else(|| "<未知位置>".to_string());
        let backtrace = std::backtrace::Backtrace::force_capture();
        let full = format!(
            "[editor] panic!\n位置: {location}\n消息: {msg}\n堆栈:\n{backtrace}\n"
        );
        eprintln!("{full}");
        #[cfg(target_os = "windows")]
        {
            use std::ffi::CString;
            let title = CString::new("akrs-editor panic").unwrap();
            let body = CString::new(full).unwrap();
            unsafe {
                extern "system" {
                    fn MessageBoxA(
                        hwnd: *mut std::ffi::c_void,
                        text: *const i8,
                        caption: *const i8,
                        u_type: u32,
        ) -> i32;
                }
                MessageBoxA(
                    std::ptr::null_mut(),
                    body.as_ptr(),
                    title.as_ptr(),
                    0x10, // MB_ICONERROR
                );
            }
        }
        default_hook(info);
    }));

    if let Err(e) = akrs_editor::run_editor() {
        eprintln!("[editor] 运行出错: {e:?}");
        std::process::exit(1);
    }
}
