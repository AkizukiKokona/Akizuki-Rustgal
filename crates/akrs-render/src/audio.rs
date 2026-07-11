//! 音频后端（基于 [kira](https://crates.io/crates/kira) 0.12）。
//!
//! 在初版仅用 `StaticSoundData` 整段解码的基础上，本版本启用了 kira 的更多能力：
//!
//! - **流式播放长 BGM**：BGM 通过 [`StreamingSoundData`] 从文件按需解码，长曲不全量
//!   驻留内存；SE / 语音仍用 [`StaticSoundData`] 整段驻留，便于快速触发。
//! - **BGM / SE / Voice 三轨路由**：在 [`AudioManager`] 下建三条 sub-track，`play_sound`
//!   按声音类型路由到对应轨，[`set_track_volume`] / [`set_sound_volume`] 作用于对应轨
//!   音量，使 `settings` 的 `bgm_volume` / `sfx_volume` / `voice_volume` 三个字段能
//!   真正独立生效。
//! - **BGM 交叉淡入**：新 BGM 播放时自动淡入（`fade_in_tween`），旧 BGM 通过
//!   [`stop_bgm_with_crossfade`] 淡出，组合实现交叉淡入。SE / 语音停止仍保留 80ms 快速
//!   淡出（[`stop_sound`]）。
//!
//! ## 向后兼容
//!
//! 保留与 macroquad `audio` 同名的 API（[`Sound`] / [`PlaySoundParams`] /
//! [`play_sound`] / [`stop_sound`] / [`set_sound_volume`] / [`load_sound_from_bytes`]），
//! [`Sound`](`Sound`) 仍为 `Copy` 句柄。新增 [`SoundKind`] / [`load_streaming_sound`] /
//! [`load_sound_kind_from_bytes`] / [`set_track_volume`] / [`stop_bgm_with_crossfade`]。
//!
//! ## 音量语义
//!
//! - **track 音量**：由 [`set_track_volume`] 设定，通常绑定到 `settings` 的三个音量字段，
//!   决定该类型声音的绝对响度。
//! - **per-sound 音量**：[`PlaySoundParams::volume`] 是相对于 track 音量的线性振幅增益
//!   （1.0 = 不额外放大）。调用方应传 1.0，把响度交给 track 音量控制，避免双重叠加。

use std::cell::RefCell;
use std::collections::HashMap;
use std::io::Cursor;
use std::path::PathBuf;
use std::time::Duration;

use kira::{
    AudioManager, AudioManagerSettings, Decibels, DefaultBackend, Tween,
    sound::static_sound::{StaticSoundData, StaticSoundHandle},
    sound::streaming::{StreamingSoundData, StreamingSoundHandle},
    sound::FromFileError,
    track::{TrackBuilder, TrackHandle},
};

// ─── 公共类型 ───────────────────────────────────────────────────────────────

/// 声音类型，决定路由到哪条 track 以及受哪个音量设置控制。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SoundKind {
    /// 背景音乐：流式播放，循环，支持交叉淡入。
    Bgm,
    /// 音效：整段驻留，一次性播放，80ms 快速停止。
    Se,
    /// 语音：整段驻留，一次性播放，80ms 快速停止。
    Voice,
}

/// 音频句柄，与 macroquad `Sound` 同名。`Copy`，可自由复制/缓存。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Sound(pub usize);

/// 播放参数，与 macroquad `PlaySoundParams` 同名同字段。
///
/// `volume` 为相对于所在 track 音量的线性振幅增益（1.0 = 不额外放大）。
#[derive(Debug, Clone, Copy)]
pub struct PlaySoundParams {
    pub volume: f32,
    pub looped: bool,
}

/// BGM 交叉淡入淡出的默认时长。
pub const BGM_CROSSFADE_DURATION: Duration = Duration::from_millis(1000);

/// 音频加载错误。
///
/// 用 `thiserror` 派生 `Error`/`Display`，替代原先散落各处的 `Result<_, String>`。
#[derive(Debug, thiserror::Error)]
pub enum AudioError {
    /// 从字节解码音频数据失败（mp3/wav/ogg/flac 解码或格式识别错误）。
    #[error("failed to decode audio data: {0}")]
    Decode(String),
    /// 打开流式音频文件失败（文件不存在、权限不足或解码探测失败）。
    #[error("failed to open streaming audio {path}: {message}")]
    Streaming {
        /// 流式音频文件路径。
        path: PathBuf,
        /// 底层错误描述。
        message: String,
    },
}

// ─── 内部状态 ───────────────────────────────────────────────────────────────

/// 三条独立音轨的句柄。
struct TrackRoutes {
    bgm: TrackHandle,
    se: TrackHandle,
    voice: TrackHandle,
}

/// 已加载声音的解码数据来源。
enum SoundDataKind {
    /// 整段驻留内存（SE/语音）。可廉价 clone，便于快速重复触发。
    Static(StaticSoundData),
    /// 文件路径，播放时流式解码（BGM）。不全量驻留内存。
    Streaming(PathBuf),
}

/// 当前播放句柄（静态或流式）。
enum CurrentHandle {
    Static(StaticSoundHandle),
    Streaming(StreamingSoundHandle<FromFileError>),
}

struct StoredSound {
    kind: SoundKind,
    data: SoundDataKind,
    current: Option<CurrentHandle>,
}

struct AudioState {
    /// `None` 表示无可用音频设备（播放类操作降级为空操作）。
    /// 必须保活以维持 cpal 输出流；初始化后不再直接读取。
    #[allow(dead_code)]
    manager: Option<AudioManager<DefaultBackend>>,
    /// 三条 sub-track；`None` 表示设备不可用或建轨失败。
    tracks: Option<TrackRoutes>,
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
        let mut manager = match AudioManager::<DefaultBackend>::new(AudioManagerSettings::default())
        {
            Ok(m) => Some(m),
            Err(e) => {
                log::warn!("[audio] AudioManager 初始化失败（将禁用音频输出）：{}", e);
                None
            }
        };
        // 建立 BGM / SE / Voice 三条 sub-track，路由与独立音量控制的基础。
        let tracks = manager.as_mut().and_then(|m| {
            let bgm = m.add_sub_track(TrackBuilder::new()).ok()?;
            let se = m.add_sub_track(TrackBuilder::new()).ok()?;
            let voice = m.add_sub_track(TrackBuilder::new()).ok()?;
            Some(TrackRoutes { bgm, se, voice })
        });
        if tracks.is_none() && manager.is_some() {
            log::warn!("[audio] 子轨创建失败（将禁用音频输出）");
        }
        *a.borrow_mut() = Some(AudioState {
            manager,
            tracks,
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

/// BGM 播放时的淡入 tween（用于交叉淡入的新曲淡入）。
fn bgm_fade_in_tween() -> Option<Tween> {
    Some(Tween {
        duration: BGM_CROSSFADE_DURATION,
        ..Default::default()
    })
}

fn track_of<'a>(tracks: &'a mut TrackRoutes, kind: SoundKind) -> &'a mut TrackHandle {
    match kind {
        SoundKind::Bgm => &mut tracks.bgm,
        SoundKind::Se => &mut tracks.se,
        SoundKind::Voice => &mut tracks.voice,
    }
}

/// 内部：在已借用的 `AudioState` 上设置某条 track 的音量（避免 thread_local 重入）。
fn set_track_volume_inner(st: &mut AudioState, kind: SoundKind, volume: f32) {
    let Some(tracks) = st.tracks.as_mut() else {
        return;
    };
    track_of(tracks, kind).set_volume(amp_to_db(volume), instant_tween());
}

// ─── 公共 API ───────────────────────────────────────────────────────────────

/// 从字节同步加载音频（mp3/wav/ogg/flac 自动识别），默认归类为 [`SoundKind::Se`]。
/// 失败返回 [`AudioError`]。即使无音频设备也可调用（仅缓存解码数据）。
///
/// 保留以兼容 macroquad 同名 API；如需指定类型（如语音），用 [`load_sound_kind_from_bytes`]。
pub fn load_sound_from_bytes(bytes: &[u8]) -> Result<Sound, AudioError> {
    load_sound_kind_from_bytes(bytes, SoundKind::Se)
}

/// 从字节同步加载音频并指定声音类型（决定路由到的 track）。
/// 用于 SE / 语音等整段驻留的短/中长度声音。BGM 请用 [`load_streaming_sound`]。
pub fn load_sound_kind_from_bytes(bytes: &[u8], kind: SoundKind) -> Result<Sound, AudioError> {
    init_audio();
    let data = StaticSoundData::from_cursor(Cursor::new(bytes.to_vec()))
        .map_err(|e| AudioError::Decode(e.to_string()))?;
    AUDIO.with(|a| {
        let mut g = a.borrow_mut();
        let st = g.as_mut().expect("audio backend not initialized");
        let id = st.next_id;
        st.next_id += 1;
        st.sounds.insert(
            id,
            StoredSound {
                kind,
                data: SoundDataKind::Static(data),
                current: None,
            },
        );
        Ok(Sound(id))
    })
}

/// 从文件路径加载流式音频（BGM 专用）。不全量驻留内存，播放时按需解码。
///
/// 加载时会预打开并探测一次文件头以确保可解码（探测结果随即丢弃，关闭文件）；
/// 每次 [`play_sound`] 时重新 `from_file` 流式解码。
pub fn load_streaming_sound(
    path: impl AsRef<std::path::Path>,
    kind: SoundKind,
) -> Result<Sound, AudioError> {
    init_audio();
    let path = path.as_ref().to_path_buf();
    // 预探测：确保文件存在且可被 symphonia 解析，错误尽早暴露（与静态加载语义一致）。
    StreamingSoundData::from_file(&path).map_err(|e| AudioError::Streaming {
        path: path.clone(),
        message: e.to_string(),
    })?;
    AUDIO.with(|a| {
        let mut g = a.borrow_mut();
        let st = g.as_mut().expect("audio backend not initialized");
        let id = st.next_id;
        st.next_id += 1;
        st.sounds.insert(
            id,
            StoredSound {
                kind,
                data: SoundDataKind::Streaming(path),
                current: None,
            },
        );
        Ok(Sound(id))
    })
}

/// 播放声音。按声音加载时注册的 [`SoundKind`] 路由到对应 track。
///
/// - `params.volume` 为相对于 track 音量的线性振幅增益（建议传 1.0）。
/// - `params.looped` 控制循环。
/// - BGM 自动以 [`BGM_CROSSFADE_DURATION`] 淡入，便于与旧 BGM 淡出组合成交叉淡入。
///
/// 无音频设备或未知句柄时为空操作。
pub fn play_sound(sound: Sound, params: PlaySoundParams) {
    init_audio();
    AUDIO.with(|a| {
        let mut g = a.borrow_mut();
        let Some(st) = g.as_mut() else {
            return;
        };
        let Some(tracks) = st.tracks.as_mut() else {
            return;
        };
        let Some(stored) = st.sounds.get_mut(&sound.0) else {
            return;
        };
        let kind = stored.kind;
        let track = track_of(tracks, kind);
        let vol_db = amp_to_db(params.volume);
        match &stored.data {
            SoundDataKind::Static(data) => {
                let mut d = data.clone().volume(vol_db);
                if params.looped {
                    d = d.loop_region(..);
                }
                if kind == SoundKind::Bgm {
                    d = d.fade_in_tween(bgm_fade_in_tween());
                }
                match track.play(d) {
                    Ok(h) => stored.current = Some(CurrentHandle::Static(h)),
                    Err(e) => log::warn!("[audio] 播放失败：{}", e),
                }
            }
            SoundDataKind::Streaming(path) => {
                let path = path.clone();
                let mut d = match StreamingSoundData::from_file(&path) {
                    Ok(d) => d,
                    Err(e) => {
                        log::warn!("[audio] 流式打开失败 {}: {}", path.display(), e);
                        return;
                    }
                };
                d = d.volume(vol_db);
                if params.looped {
                    d = d.loop_region(..);
                }
                if kind == SoundKind::Bgm {
                    d = d.fade_in_tween(bgm_fade_in_tween());
                }
                match track.play(d) {
                    Ok(h) => stored.current = Some(CurrentHandle::Streaming(h)),
                    Err(e) => log::warn!("[audio] 播放失败：{}", e),
                }
            }
        }
    });
}

/// 停止声音（淡出 80ms 后永久停止）。用于 SE / 语音的快速停止。
pub fn stop_sound(sound: Sound) {
    init_audio();
    AUDIO.with(|a| {
        let mut g = a.borrow_mut();
        let Some(st) = g.as_mut() else {
            return;
        };
        let Some(stored) = st.sounds.get_mut(&sound.0) else {
            return;
        };
        if let Some(h) = stored.current.as_mut() {
            match h {
                CurrentHandle::Static(s) => s.stop(Tween {
                    duration: Duration::from_millis(80),
                    ..Default::default()
                }),
                CurrentHandle::Streaming(s) => s.stop(Tween {
                    duration: Duration::from_millis(80),
                    ..Default::default()
                }),
            }
        }
        stored.current = None;
    });
}

/// 停止 BGM 并在 `duration` 内淡出至静音。与 [`play_sound`] 对新 BGM 的自动淡入
/// 组合，实现交叉淡入（旧曲淡出 + 新曲淡入同时进行）。
pub fn stop_bgm_with_crossfade(sound: Sound, duration: Duration) {
    init_audio();
    AUDIO.with(|a| {
        let mut g = a.borrow_mut();
        let Some(st) = g.as_mut() else {
            return;
        };
        let Some(stored) = st.sounds.get_mut(&sound.0) else {
            return;
        };
        if let Some(h) = stored.current.as_mut() {
            match h {
                CurrentHandle::Static(s) => s.stop(Tween {
                    duration,
                    ..Default::default()
                }),
                CurrentHandle::Streaming(s) => s.stop(Tween {
                    duration,
                    ..Default::default()
                }),
            }
        }
        stored.current = None;
    });
}

/// 设置某条 track 的音量（线性振幅）。影响该类型所有正在播放及后续播放的声音。
/// 用于把 `settings` 的 `bgm_volume` / `sfx_volume` / `voice_volume` 接到对应轨。
pub fn set_track_volume(kind: SoundKind, volume: f32) {
    init_audio();
    AUDIO.with(|a| {
        let mut g = a.borrow_mut();
        let Some(st) = g.as_mut() else {
            return;
        };
        set_track_volume_inner(st, kind, volume);
    });
}

/// 设置声音所在 track 的音量（线性振幅）。
///
/// 语义已从初版的“设置单个声音音量”升级为“设置该声音所属类型的整条 track 音量”，
/// 这样 `settings` 的三个音量字段能通过同一个 API 独立作用于 BGM / SE / Voice。
/// 旧调用方签名保持不变。
pub fn set_sound_volume(sound: Sound, volume: f32) {
    init_audio();
    AUDIO.with(|a| {
        let mut g = a.borrow_mut();
        let Some(st) = g.as_mut() else {
            return;
        };
        let Some(kind) = st.sounds.get(&sound.0).map(|s| s.kind) else {
            return;
        };
        set_track_volume_inner(st, kind, volume);
    });
}
