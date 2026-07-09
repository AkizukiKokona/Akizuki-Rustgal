//! 音频后端（任务1 内置重做）：基于 [kira](https://crates.io/crates/kira) 0.12。
//!
//! 提供与 macroquad `audio` 模块同名的 API（`Sound` / `PlaySoundParams` /
//! `play_sound` / `stop_sound` / `set_sound_volume`），使 `renderer.rs` 仅需换
//! import 即可迁移。
//!
//! ## 实现
//!
//! - `Sound(pub usize)` 为 `Copy` 句柄，索引全局注册表中的 `StoredSound`
//!   （持有 `StaticSoundData` 与当前播放句柄）。这样 `Sound` 可像 macroquad 那样
//!   被自由复制/缓存。
//! - `AudioManager` 与所有句柄均为 `Send + Sync`（kira 自身把 cpal `Stream`
//!   隔离在独立线程），故安全地存于 `thread_local`。
//! - 若音频设备不可用（无音频硬件/驱动），`AudioManager` 创建失败时降级为
//!   `manager = None`：加载仍可（仅缓存数据），播放/停止/调音为空操作。
//! - 音量换算：macroquad 的 `volume: f32` 为线性振幅（0.0 静音、1.0 原始、>1 增益），
//!   kira 用分贝，故 `amplitude → 20·log10`。

use std::cell::RefCell;
use std::collections::HashMap;
use std::io::Cursor;
use std::time::Duration;

use kira::{
    AudioManager, AudioManagerSettings, Decibels, DefaultBackend, Tween,
    sound::static_sound::{StaticSoundData, StaticSoundHandle},
};

// ─── 公共类型（与 macroquad 同名） ──────────────────────────────────────────

/// 音频句柄，与 macroquad `Sound` 同名。`Copy`，可自由复制/缓存。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Sound(pub usize);

/// 播放参数，与 macroquad `PlaySoundParams` 同名同字段。
#[derive(Debug, Clone, Copy)]
pub struct PlaySoundParams {
    pub volume: f32,
    pub looped: bool,
}

// ─── 内部状态 ───────────────────────────────────────────────────────────────

/// 一个已加载声音：解码后的样本数据 + 当前播放句柄。
struct StoredSound {
    data: StaticSoundData,
    current: Option<StaticSoundHandle>,
}

struct AudioState {
    /// `None` 表示无可用音频设备（播放类操作降级为空操作）。
    manager: Option<AudioManager<DefaultBackend>>,
    sounds: HashMap<usize, StoredSound>,
    next_id: usize,
}

thread_local! {
    static AUDIO: RefCell<Option<AudioState>> = const { RefCell::new(None) };
}

/// 懒初始化音频后端。首次调用任意音频 API 时自动触发；也可由 `renderer::run` 显式调用。
pub fn init_audio() {
    AUDIO.with(|a| {
        if a.borrow().is_some() {
            return;
        }
        let manager = match AudioManager::<DefaultBackend>::new(AudioManagerSettings::default()) {
            Ok(m) => Some(m),
            Err(e) => {
                eprintln!("[audio] AudioManager 初始化失败（将禁用音频输出）：{}", e);
                None
            }
        };
        *a.borrow_mut() = Some(AudioState {
            manager,
            sounds: HashMap::new(),
            next_id: 1,
        });
    });
}

/// 线性振幅 → 分贝（macroquad 语义 → kira 语义）。
fn amp_to_db(a: f32) -> Decibels {
    if a <= 0.0 {
        Decibels::SILENCE
    } else {
        Decibels(20.0 * a.log10())
    }
}

fn instant_tween() -> Tween {
    Tween {
        duration: Duration::from_millis(10),
        ..Default::default()
    }
}

// ─── 公共 API（与 macroquad 同名） ──────────────────────────────────────────

/// 从字节同步加载音频（mp3/wav/ogg/flac 自动识别）。失败返回错误字符串。
/// 即使无音频设备也可调用（仅缓存解码数据）。
pub fn load_sound_from_bytes(bytes: &[u8]) -> Result<Sound, String> {
    init_audio();
    let data = StaticSoundData::from_cursor(Cursor::new(bytes.to_vec()))
        .map_err(|e| e.to_string())?;
    AUDIO.with(|a| {
        let mut g = a.borrow_mut();
        let st = g.as_mut().expect("audio backend not initialized");
        let id = st.next_id;
        st.next_id += 1;
        st.sounds.insert(id, StoredSound { data, current: None });
        Ok(Sound(id))
    })
}

/// 播放声音。`params.volume` 为线性振幅，`params.looped` 控制循环。
/// 无音频设备或未知句柄时为空操作。
pub fn play_sound(sound: Sound, params: PlaySoundParams) {
    init_audio();
    AUDIO.with(|a| {
        let mut g = a.borrow_mut();
        let Some(st) = g.as_mut() else { return };
        let Some(mgr) = st.manager.as_mut() else { return };
        let Some(stored) = st.sounds.get_mut(&sound.0) else { return };
        let mut data = stored.data.clone();
        data = data.volume(amp_to_db(params.volume));
        if params.looped {
            data = data.loop_region(..);
        }
        match mgr.play(data) {
            Ok(handle) => stored.current = Some(handle),
            Err(e) => eprintln!("[audio] 播放失败：{}", e),
        }
    });
}

/// 停止声音（淡出 80ms 后永久停止）。
pub fn stop_sound(sound: Sound) {
    init_audio();
    AUDIO.with(|a| {
        let mut g = a.borrow_mut();
        let Some(st) = g.as_mut() else { return };
        let Some(stored) = st.sounds.get_mut(&sound.0) else { return };
        if let Some(h) = stored.current.as_mut() {
            h.stop(Tween {
                duration: Duration::from_millis(80),
                ..Default::default()
            });
        }
        stored.current = None;
    });
}

/// 设置正在播放声音的音量（线性振幅）。
pub fn set_sound_volume(sound: Sound, volume: f32) {
    init_audio();
    AUDIO.with(|a| {
        let mut g = a.borrow_mut();
        let Some(st) = g.as_mut() else { return };
        let Some(stored) = st.sounds.get_mut(&sound.0) else { return };
        if let Some(h) = stored.current.as_mut() {
            h.set_volume(amp_to_db(volume), instant_tween());
        }
    });
}
