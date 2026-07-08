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
    /// 配色自定义。
    Color,
    /// 开发者模式：调试开关与高危操作。
    Developer,
    /// 帮助：键位功能对照。
    Help,
    /// 关于：引擎信息。
    About,
}

impl SettingsTab {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Text => "文本",
            Self::Audio => "音频",
            Self::Display => "画面",
            Self::Skip => "快进",
            Self::Color => "配色",
            Self::Developer => "开发者",
            Self::Help => "帮助",
            Self::About => "关于",
        }
    }

    pub fn all() -> &'static [Self] {
        &[
            Self::Text,
            Self::Audio,
            Self::Display,
            Self::Skip,
            Self::Color,
            Self::Developer,
            Self::Help,
            Self::About,
        ]
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
    /// 是否启用自动播放：对话文本显示完毕后按设定间隔自动跳转下一句。
    /// 关闭时由玩家手动点击推进。
    #[serde(default)]
    pub auto_play: bool,
    /// 自动播放时，当前对话有语音的间隔（秒），默认 1.0。
    #[serde(default = "default_auto_play_delay_with_voice")]
    pub auto_play_delay_with_voice: f32,
    /// 自动播放时，当前对话无语音的间隔（秒），默认 2.0。
    #[serde(default = "default_auto_play_delay_without_voice")]
    pub auto_play_delay_without_voice: f32,
    /// 当前选择的剧本语言代码（如 "zh-CN"、"en-US"、"ja-JP"）。
    /// 仅影响剧本翻译（对话/旁白/选项/角色名/语音引用）。
    /// 空字符串表示使用系统默认语言。
    #[serde(default)]
    pub language: String,
    /// UI 界面语言代码（影响菜单按钮、标签等硬编码界面文本）。
    /// 空字符串表示回退到 `language`，再回退到系统默认语言。
    /// 这样默认 UI 语言跟随剧本语言，玩家也可单独指定。
    #[serde(default)]
    pub ui_language: String,
    /// 是否显示终端调试输出（默认关闭）。
    ///
    /// 三端默认静默启动：
    /// - Windows：通过 `windows_subsystem = "windows"` 默认不弹控制台；
    ///   开启此项后会调用 `AllocConsole()` 重新分配控制台以显示 println!/eprintln!。
    /// - Linux/macOS：默认 GUI 启动无终端；从终端启动时输出可见。
    ///   开启此项不影响行为，仅作为开发者标志位。
    #[serde(default)]
    pub debug_terminal: bool,
    /// 已读文字颜色（RGBA 0–255）。玩家看过的对话/旁白用此色显示。
    /// 默认浅紫色 [200, 170, 230, 255]。
    #[serde(default = "default_read_text_color")]
    pub read_text_color: [u8; 4],
    /// 未读文字颜色（RGBA 0–255）。玩家尚未看过的对话/旁白用此色显示。
    /// 默认白色 [255, 255, 255, 255]。
    #[serde(default = "default_unread_text_color")]
    pub unread_text_color: [u8; 4],
    /// 主题色1（面板背景）玩家覆盖。`None` = 使用项目主题（`ProjectConfig.theme.primary`）。
    #[serde(default)]
    pub theme_primary: Option<[u8; 4]>,
    /// 主题色2（按钮背景）玩家覆盖。`None` = 使用项目主题。
    #[serde(default)]
    pub theme_secondary: Option<[u8; 4]>,
    /// 对话框色（文本框渐变）玩家覆盖。`None` = 使用项目主题。
    #[serde(default)]
    pub theme_dialogue: Option<[u8; 4]>,
}

fn default_read_text_color() -> [u8; 4] {
    [200, 170, 230, 255]
}

fn default_unread_text_color() -> [u8; 4] {
    [255, 255, 255, 255]
}

fn default_auto_recovery() -> bool {
    true
}

fn default_auto_play_delay_with_voice() -> f32 {
    1.0
}

fn default_auto_play_delay_without_voice() -> f32 {
    2.0
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
            auto_play: false,
            auto_play_delay_with_voice: 1.0,
            auto_play_delay_without_voice: 2.0,
            language: String::new(),
            ui_language: String::new(),
            debug_terminal: false,
            read_text_color: default_read_text_color(),
            unread_text_color: default_unread_text_color(),
            theme_primary: None,
            theme_secondary: None,
            theme_dialogue: None,
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

    /// 获取有效剧本语言代码。
    /// 若 settings.language 为空，则检测系统语言并返回；
    /// 不在支持列表中时回退到 en-US。
    pub fn effective_language(&self) -> String {
        if !self.language.is_empty() {
            self.language.clone()
        } else {
            crate::translator::detect_system_language()
        }
    }

    /// 获取有效 UI 语言代码。
    /// 优先级：ui_language → language → 系统语言。
    /// 这样默认 UI 语言跟随剧本语言，玩家也可单独指定 ui_language。
    pub fn effective_ui_language(&self) -> String {
        if !self.ui_language.is_empty() {
            self.ui_language.clone()
        } else if !self.language.is_empty() {
            self.language.clone()
        } else {
            crate::translator::detect_system_language()
        }
    }
}
