//! Game settings: text speed, audio volume, display options.
//!
//! Serializable for persistence between sessions.

use serde::{Deserialize, Serialize};
use std::path::Path;

/// 快进模式：控制快进时的行为。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub enum SkipMode {
    /// 仅显示文本：以最快速度逐句跳过（不受语音等限制）。
    TextOnly,
    /// 包含语音：播放完语音后立刻进入下一句（语音播放期间等待）。
    WithVoice,
}

impl Default for SkipMode {
    fn default() -> Self {
        Self::TextOnly
    }
}

impl SkipMode {
    pub fn label(&self) -> &'static str {
        match self {
            Self::TextOnly => "仅显示文本",
            Self::WithVoice => "包含语音",
        }
    }

    pub fn all() -> &'static [Self] {
        &[Self::TextOnly, Self::WithVoice]
    }
}

/// 设置标签页分类。
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SettingsTab {
    /// 文本设置。
    Text,
    /// 音频设置。
    Audio,
    /// 画面与显示设置。
    Display,
    /// 快进设置。
    Skip,
}

impl SettingsTab {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Text => "文本",
            Self::Audio => "音频",
            Self::Display => "画面",
            Self::Skip => "快进",
        }
    }

    pub fn all() -> &'static [Self] {
        &[Self::Text, Self::Audio, Self::Display, Self::Skip]
    }
}

/// User-configurable game settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    /// Characters displayed per second (0 = instant, 999 = very fast).
    pub text_speed: f32,
    /// Background music volume (0.0 - 1.0).
    pub bgm_volume: f32,
    /// Sound effect volume (0.0 - 1.0).
    pub sfx_volume: f32,
    /// Voice volume (0.0 - 1.0).
    pub voice_volume: f32,
    /// 是否允许跳过未读文本。
    /// 关闭时，快进遇到未读文本会自动停下。
    #[serde(default)]
    pub skip_unread: bool,
    /// 快进模式。
    #[serde(default)]
    pub skip_mode: SkipMode,
    /// Fullscreen mode.
    pub fullscreen: bool,
    /// Window resolution.
    pub resolution: (u32, u32),
    /// Whether to auto-recover from crash-recovery autosaves on startup.
    /// When true and the game was not exited normally, the next launch will
    /// prompt to resume. When false, autosaves are ignored on startup.
    #[serde(default = "default_auto_recovery")]
    pub auto_recovery: bool,
}

fn default_auto_recovery() -> bool {
    true
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            text_speed: 30.0,
            bgm_volume: 0.8,
            sfx_volume: 1.0,
            voice_volume: 1.0,
            skip_unread: false,
            skip_mode: SkipMode::TextOnly,
            fullscreen: false,
            resolution: (1920, 1080),
            auto_recovery: true,
        }
    }
}

impl Settings {
    /// Load settings from a JSON file. Returns default if file doesn't exist.
    pub fn load(path: &Path) -> Self {
        match std::fs::read_to_string(path) {
            Ok(content) => serde_json::from_str(&content).unwrap_or_default(),
            Err(_) => Self::default(),
        }
    }

    /// Save settings to a JSON file.
    pub fn save(&self, path: &Path) -> Result<(), String> {
        let content = serde_json::to_string_pretty(self)
            .map_err(|e| format!("failed to serialize settings: {}", e))?;
        std::fs::write(path, content)
            .map_err(|e| format!("failed to write settings file: {}", e))?;
        Ok(())
    }

    /// Check if text display is instant.
    pub fn is_instant_text(&self) -> bool {
        self.text_speed <= 0.0 || self.text_speed >= 999.0
    }

    /// Return the standard resolution presets offered in the settings UI.
    pub fn resolution_presets() -> &'static [(u32, u32)] {
        &[
            (1920, 1080),
            (1600, 900),
            (1280, 720),
            (1024, 768),
        ]
    }

    /// The default file path for persistent settings (`saves/settings.json`).
    pub fn default_path() -> std::path::PathBuf {
        std::path::PathBuf::from("saves").join("settings.json")
    }
}
