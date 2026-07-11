//! Asset loading and caching.
//!
//! Resources are looked up under `assets/` with subdirectories:
//! - `assets/bg/` — background images
//! - `assets/characters/` — character sprites
//! - `assets/music/` — background music
//! - `assets/sound/` — sound effects
//! - `assets/voice/` — 语音文件
//! - `assets/title/` — title screen resources
//!
//! Missing resources produce a warning and a placeholder is used instead.

use crate::audio::{self, Sound};
use crate::wgpu_backend::{self, FilterMode, Texture2D};
use std::collections::HashMap;
use std::path::PathBuf;

/// Asset category (maps to a subdirectory under `assets/`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AssetKind {
    Bg,
    Character,
    Music,
    Sound,
    Voice,
    #[allow(dead_code)]
    Title,
}

impl AssetKind {
    fn subdir(self) -> &'static str {
        match self {
            AssetKind::Bg => "bg",
            AssetKind::Character => "characters",
            AssetKind::Music => "music",
            AssetKind::Sound => "sound",
            AssetKind::Voice => "voice",
            AssetKind::Title => "title",
        }
    }
}

/// Manages loaded textures and sounds with lazy loading and caching.
pub struct AssetManager {
    cache: HashMap<String, Option<Texture2D>>,
    /// 音频缓存：key = `{AssetKind}/{name}`，value = None 表示加载失败或文件不存在。
    sound_cache: HashMap<String, Option<Sound>>,
    base_dir: PathBuf,
}

impl AssetManager {
    /// Create a new asset manager with `assets/` as the base directory.
    pub fn new() -> Self {
        let base_dir = PathBuf::from("assets");
        Self {
            cache: HashMap::new(),
            sound_cache: HashMap::new(),
            base_dir,
        }
    }

    /// Resolve a resource name to a full path.
    /// If the name already contains a path separator, use it as-is relative to assets/.
    /// Otherwise, look in the category subdirectory.
    fn resolve_path(&self, kind: AssetKind, name: &str) -> PathBuf {
        if name.contains('/') || name.contains('\\') {
            self.base_dir.join(name)
        } else {
            self.base_dir.join(kind.subdir()).join(name)
        }
    }

    /// Load a texture by resource name. Returns None if the file doesn't exist.
    /// Logs a warning to stderr on missing resources.
    /// Automatically tries appending .png if the file isn't found as-is.
    pub fn get_texture(&mut self, kind: AssetKind, name: &str) -> Option<Texture2D> {
        let key = format!("{:?}/{}", kind, name);
        if let Some(cached) = self.cache.get(&key) {
            return cached.clone();
        }

        let path = self.resolve_path(kind, name);
        let path_str = path.to_string_lossy().to_string();

        let final_path = if path.exists() {
            path_str
        } else {
            let with_png = format!("{}.png", path_str);
            if std::path::Path::new(&with_png).exists() {
                with_png
            } else {
                eprintln!("[Warning] Missing resource: {} (expected at {} or {}.png)", name, path_str, path_str);
                self.cache.insert(key, None);
                return None;
            }
        };

        match image::open(&final_path) {
            Ok(img) => {
                let rgba = img.to_rgba8();
                let (w, h) = rgba.dimensions();
                let texture = wgpu_backend::create_texture(w, h, &rgba);
                texture.set_filter(FilterMode::Linear);
                self.cache.insert(key, Some(texture.clone()));
                Some(texture)
            }
            Err(e) => {
                eprintln!("[Warning] Failed to load texture '{}': {}", final_path, e);
                self.cache.insert(key, None);
                None
            }
        }
    }

    /// Check if a music file exists. Logs a warning if missing.
    pub fn check_music(&mut self, name: &str) -> bool {
        self.check_audio_exists(AssetKind::Music, name)
    }

    /// Check if a sound file exists. Logs a warning if missing.
    #[allow(dead_code)]
    pub fn check_sound(&mut self, name: &str) -> bool {
        self.check_audio_exists(AssetKind::Sound, name)
    }

    /// 通用：检查音频文件是否存在，缺失时打印警告。
    fn check_audio_exists(&self, kind: AssetKind, name: &str) -> bool {
        let path = self.resolve_path(kind, name);
        if !path.exists() {
            eprintln!(
                "[Warning] Missing {kind:?}: {name} (expected at {path})",
                path = path.to_string_lossy()
            );
            false
        } else {
            true
        }
    }

    /// 尝试解析音频文件路径，自动追加常见扩展名（.mp3/.wav/.ogg/.flac）。
    /// 返回第一个存在的文件路径；若都不存在则返回 None。
    fn resolve_audio_path(&self, kind: AssetKind, name: &str) -> Option<PathBuf> {
        let path = self.resolve_path(kind, name);
        if path.exists() {
            return Some(path);
        }
        for ext in &["mp3", "wav", "ogg", "flac", "m4a"] {
            let with_ext = path.with_extension(ext);
            if with_ext.exists() {
                return Some(with_ext);
            }
        }
        None
    }

    /// 同步加载音频并缓存（懒加载）。
    /// 文件不存在或加载失败时返回 None 并打印警告，缓存 None 避免重复尝试。
    pub fn get_sound(&mut self, kind: AssetKind, name: &str) -> Option<Sound> {
        let key = format!("{:?}/{}", kind, name);
        if let Some(cached) = self.sound_cache.get(&key) {
            return *cached;
        }

        let path = match self.resolve_audio_path(kind, name) {
            Some(p) => p,
            None => {
                eprintln!(
                    "[Warning] Missing {kind:?}: {name} (looked in assets/{subdir}/{name}[.mp3/.wav/.ogg/.flac])",
                    subdir = kind.subdir()
                );
                self.sound_cache.insert(key, None);
                return None;
            }
        };

        let path_str = path.to_string_lossy().to_string();
        // BGM 流式播放（不全量驻留内存）；SE/语音整段驻留以便快速触发。
        // 各自带 SoundKind，使 play_sound 能路由到对应 track、settings 的三个音量字段
        // 能独立生效。
        let result: Result<Sound, String> = match kind {
            AssetKind::Music => audio::load_streaming_sound(&path, audio::SoundKind::Bgm),
            AssetKind::Voice => std::fs::read(&path)
                .map_err(|e| e.to_string())
                .and_then(|b| audio::load_sound_kind_from_bytes(&b, audio::SoundKind::Voice)),
            _ => {
                // AssetKind::Sound 走 SE；Bg/Character/Title 不会经 get_sound，
                // 兜底按 SE 处理。
                std::fs::read(&path)
                    .map_err(|e| e.to_string())
                    .and_then(|b| audio::load_sound_kind_from_bytes(&b, audio::SoundKind::Se))
            }
        };
        match result {
            Ok(sound) => {
                self.sound_cache.insert(key, Some(sound));
                Some(sound)
            }
            Err(e) => {
                eprintln!("[Warning] Failed to load sound '{}': {}", path_str, e);
                self.sound_cache.insert(key, None);
                None
            }
        }
    }
}

impl Default for AssetManager {
    fn default() -> Self {
        Self::new()
    }
}
