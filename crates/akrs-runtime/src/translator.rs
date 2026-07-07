//! 剧本翻译器：运行时把原文剧本文本替换为目标语言译文。
//!
//! # 设计原则
//!
//! - **原文即 key**：翻译文件以原文文本为键，译文为值，原剧本完全不需要修改。
//! - **注入点在 Engine 层**：VM 和编译器零改动，切语言无需重编译、不影响存档。
//! - **显示层翻译，逻辑层不变**：角色名、章节名等在逻辑层（跳转、舞台管理、存档元数据）
//!   始终用原文，仅在显示给玩家前查表替换。
//! - **回退原文**：找不到译文时直接返回原文，游戏不会因缺少翻译而报错。
//! - **零热路径分配**：查询是 `HashMap::get` 均摊 O(1)，不分配内存。
//!
//! # 翻译文件格式（JSON）
//!
//! ```json
//! {
//!   "language": "ja-JP",
//!   "display_name": "日本語",
//!   "sections": { "序章": "プロローグ" },
//!   "dialogue": { "你好！": "こんにちは！" },
//!   "narration": { "风吹过。": "風が吹いた。" },
//!   "choices": { "选项A": "選択肢A" },
//!   "choice_prompts": { "你选哪个？": "どちらを選ぶ？" },
//!   "characters": { "心夏": "心夏" },
//!   "voice": { "你好！": "voice/hello_ja.wav" }
//! }
//! ```
//!
//! 文件路径：`assets/scripts/languages/<lang>.json`

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

/// 翻译文件的 JSON 结构。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct TranslationFile {
    /// 语言代码，如 "zh-CN"、"en-US"、"ja-JP"。
    #[serde(default)]
    language: String,
    /// 语言显示名（如 "简体中文"、"English"、"日本語"）。
    #[serde(default)]
    display_name: String,
    /// 章节标题翻译：key = 原文章节名，value = 译文。
    #[serde(default)]
    sections: HashMap<String, String>,
    /// 对话文本翻译：key = 原文对话文本，value = 译文。
    #[serde(default)]
    dialogue: HashMap<String, String>,
    /// 旁白文本翻译：key = 原文旁白文本，value = 译文。
    #[serde(default)]
    narration: HashMap<String, String>,
    /// 选项文本翻译：key = 原文选项文本，value = 译文。
    #[serde(default)]
    choices: HashMap<String, String>,
    /// 选项提示语翻译：key = 原文提示语，value = 译文。
    #[serde(default)]
    choice_prompts: HashMap<String, String>,
    /// 角色名翻译：key = 原文角色名，value = 译文。
    /// 逻辑层（跳转、舞台管理、存档）始终用原文，仅显示时替换。
    #[serde(default)]
    characters: HashMap<String, String>,
    /// 语音文件引用：key = 原文对话/旁白文本，value = 语音文件名（相对 assets/voice/）。
    /// 与文本翻译不同，找不到 key 时返回 None（表示该句无语音），不回退原文。
    /// 语音引用按语言区分，可实现「日语配音 + 中文字幕」等组合。
    #[serde(default)]
    voice: HashMap<String, String>,
}

/// 剧本翻译器：运行时查表替换原文为译文。
#[derive(Debug, Clone)]
pub struct Translator {
    /// 当前语言代码。
    language: String,
    /// 语言显示名。
    display_name: String,
    /// 章节标题翻译表。
    sections: HashMap<String, String>,
    /// 对话文本翻译表。
    dialogue: HashMap<String, String>,
    /// 旁白文本翻译表。
    narration: HashMap<String, String>,
    /// 选项文本翻译表。
    choices: HashMap<String, String>,
    /// 选项提示语翻译表。
    choice_prompts: HashMap<String, String>,
    /// 角色名翻译表。
    characters: HashMap<String, String>,
    /// 语音文件引用表。
    voice: HashMap<String, String>,
}

impl Default for Translator {
    fn default() -> Self {
        Self {
            language: "zh-CN".to_string(),
            display_name: "简体中文".to_string(),
            sections: HashMap::new(),
            dialogue: HashMap::new(),
            narration: HashMap::new(),
            choices: HashMap::new(),
            choice_prompts: HashMap::new(),
            characters: HashMap::new(),
            voice: HashMap::new(),
        }
    }
}

impl Translator {
    /// 创建空翻译器（相当于原文模式，所有查询直接返回原文）。
    pub fn new() -> Self {
        Self::default()
    }

    /// 从 JSON 文件加载翻译表。
    ///
    /// 若文件不存在或解析失败，返回空翻译器（回退原文）并打印警告。
    pub fn from_file(path: &Path) -> Self {
        match std::fs::read_to_string(path) {
            Ok(content) => Self::from_json_str(&content).unwrap_or_else(|e| {
                eprintln!("[Translator] 解析翻译文件 {:?} 失败：{}", path, e);
                Self::default()
            }),
            Err(e) => {
                eprintln!("[Translator] 读取翻译文件 {:?} 失败：{}", path, e);
                Self::default()
            }
        }
    }

    /// 从 JSON 字符串加载翻译表。
    pub fn from_json_str(content: &str) -> Result<Self, String> {
        let file: TranslationFile = serde_json::from_str(content)
            .map_err(|e| format!("JSON 解析失败：{}", e))?;
        let lang = if file.language.is_empty() {
            "unknown".to_string()
        } else {
            file.language
        };
        let disp = if file.display_name.is_empty() {
            lang.clone()
        } else {
            file.display_name
        };
        Ok(Self {
            language: lang,
            display_name: disp,
            sections: file.sections,
            dialogue: file.dialogue,
            narration: file.narration,
            choices: file.choices,
            choice_prompts: file.choice_prompts,
            characters: file.characters,
            voice: file.voice,
        })
    }

    /// 当前语言代码。
    pub fn language(&self) -> &str {
        &self.language
    }

    /// 语言显示名。
    pub fn display_name(&self) -> &str {
        &self.display_name
    }

    /// 翻译章节标题。找不到或译文为空则返回原文。
    pub fn t_section<'a>(&'a self, original: &'a str) -> &'a str {
        self.sections.get(original).filter(|s| !s.is_empty()).map(|s| s.as_str()).unwrap_or(original)
    }

    /// 翻译对话文本。找不到或译文为空则返回原文。
    pub fn t_dialogue<'a>(&'a self, original: &'a str) -> &'a str {
        self.dialogue.get(original).filter(|s| !s.is_empty()).map(|s| s.as_str()).unwrap_or(original)
    }

    /// 翻译旁白文本。找不到或译文为空则返回原文。
    pub fn t_narration<'a>(&'a self, original: &'a str) -> &'a str {
        self.narration.get(original).filter(|s| !s.is_empty()).map(|s| s.as_str()).unwrap_or(original)
    }

    /// 翻译选项文本。找不到或译文为空则返回原文。
    pub fn t_choice<'a>(&'a self, original: &'a str) -> &'a str {
        self.choices.get(original).filter(|s| !s.is_empty()).map(|s| s.as_str()).unwrap_or(original)
    }

    /// 翻译选项提示语。找不到或译文为空则返回原文。
    pub fn t_choice_prompt<'a>(&'a self, original: &'a str) -> &'a str {
        self.choice_prompts.get(original).filter(|s| !s.is_empty()).map(|s| s.as_str()).unwrap_or(original)
    }

    /// 翻译角色名（显示用）。找不到或译文为空则返回原文。
    ///
    /// 注意：逻辑层（角色上场/下场/存档/跳转）必须用原文，
    /// 仅在绘制到屏幕前调用此方法替换显示名。
    pub fn t_character<'a>(&'a self, original: &'a str) -> &'a str {
        self.characters.get(original).filter(|s| !s.is_empty()).map(|s| s.as_str()).unwrap_or(original)
    }

    /// 查询原文对应的语音文件名。
    /// 与文本翻译不同，找不到 key 或值为空时返回 None（表示该句无语音），
    /// 而不是回退原文 —— 没有语音就是没有语音。
    ///
    /// 语音引用按语言区分，因此可实现「日语配音 + 中文字幕」等组合：
    /// 在 ja-JP.json 里填 `voice`，但 `language` 设置读 zh-CN.json。
    pub fn voice(&self, original: &str) -> Option<&str> {
        self.voice.get(original).filter(|s| !s.is_empty()).map(|s| s.as_str())
    }

    /// 是否为原文模式（无任何翻译条目）。
    pub fn is_original(&self) -> bool {
        self.dialogue.is_empty()
            && self.narration.is_empty()
            && self.choices.is_empty()
            && self.choice_prompts.is_empty()
    }

    /// 设置章节标题翻译（空字符串表示未翻译，仍保留 key 便于骨架生成）。
    pub fn set_section(&mut self, original: &str, translated: &str) {
        self.sections.insert(original.to_string(), translated.to_string());
    }

    /// 设置对话翻译。
    pub fn set_dialogue(&mut self, original: &str, translated: &str) {
        self.dialogue.insert(original.to_string(), translated.to_string());
    }

    /// 设置旁白翻译。
    pub fn set_narration(&mut self, original: &str, translated: &str) {
        self.narration.insert(original.to_string(), translated.to_string());
    }

    /// 设置选项翻译。
    pub fn set_choice(&mut self, original: &str, translated: &str) {
        self.choices.insert(original.to_string(), translated.to_string());
    }

    /// 设置选项提示语翻译。
    pub fn set_choice_prompt(&mut self, original: &str, translated: &str) {
        self.choice_prompts.insert(original.to_string(), translated.to_string());
    }

    /// 设置角色名翻译。
    pub fn set_character(&mut self, original: &str, translated: &str) {
        self.characters.insert(original.to_string(), translated.to_string());
    }

    /// 设置语音文件引用。
    pub fn set_voice(&mut self, original: &str, voice_file: &str) {
        self.voice.insert(original.to_string(), voice_file.to_string());
    }

    /// 序列化为 JSON 字符串。
    pub fn to_json(&self) -> Result<String, String> {
        let file = TranslationFile {
            language: self.language.clone(),
            display_name: self.display_name.clone(),
            sections: self.sections.clone(),
            dialogue: self.dialogue.clone(),
            narration: self.narration.clone(),
            choices: self.choices.clone(),
            choice_prompts: self.choice_prompts.clone(),
            characters: self.characters.clone(),
            voice: self.voice.clone(),
        };
        serde_json::to_string_pretty(&file)
            .map_err(|e| format!("序列化失败：{}", e))
    }

    /// 设置语言代码和显示名。
    pub fn set_language(&mut self, lang: &str, display_name: &str) {
        self.language = lang.to_string();
        self.display_name = if display_name.is_empty() {
            lang.to_string()
        } else {
            display_name.to_string()
        };
    }
}

/// 检测系统语言，返回最接近的支持语言代码。
///
/// 支持列表：zh-CN、zh-TW、en-US、ja-JP。
/// 不在支持列表中时回退到 en-US。
///
/// # 平台实现
///
/// - **Windows**：调用 `GetUserDefaultLocaleName` 读取用户区域设置
///   （返回 BCP 47 格式如 `zh-CN`、`ja-JP`、`en-US`）。这是修复
///   「Windows 上自动检测永远返回 en-US」的关键——Unix 的 `LANG`
///   等环境变量在 Windows 上根本不存在。
/// - **Linux/macOS/其他**：按 `LANGUAGE`→`LC_ALL`→`LC_MESSAGES`→`LANG`
///   顺序读取环境变量（POSIX 惯例）。
/// - 全部失败时回退到 en-US。
pub fn detect_system_language() -> String {
    // Windows 优先：用 Win32 API 读取用户区域设置。
    #[cfg(target_os = "windows")]
    if let Some(locale) = windows_user_locale() {
        if let Some(code) = normalize_locale(&locale) {
            return code;
        }
    }

    // POSIX 路径：按优先级尝试多个环境变量。
    let candidates = ["LANGUAGE", "LC_ALL", "LC_MESSAGES", "LANG"];
    for var in &candidates {
        if let Ok(val) = std::env::var(var) {
            // 取首个语言（LANGUAGE 可能是 "zh_CN:en_US:en" 形式）
            let first = val.split(':').next().unwrap_or(&val);
            let lang = first.split('.').next().unwrap_or(first);
            if let Some(code) = normalize_locale(lang) {
                return code;
            }
        }
    }
    // 均未识别，回退到英语
    "en-US".to_string()
}

/// 把任意 locale 字符串（POSIX 风格 `zh_CN.UTF-8` 或 BCP 47 风格
/// `zh-Hans-CN`）归一化为引擎支持的语言代码。
///
/// 支持的输出：`zh-CN`、`zh-TW`、`en-US`、`ja-JP`。
/// 无法识别时返回 `None`（由调用方决定回退策略）。
fn normalize_locale(raw: &str) -> Option<String> {
    let s = raw.trim().to_lowercase();
    if s.is_empty() {
        return None;
    }
    // 中文：区分简体/繁体。BCP 47 可能写作 zh-Hans-CN / zh-Hant-TW，
    // POSIX 可能写作 zh_CN / zh_TW / zh_HK。繁体判定关键字：tw、hk、hant。
    if s.starts_with("zh") {
        let is_traditional = s.contains("hant") || s.contains("tw") || s.contains("hk");
        return Some(if is_traditional { "zh-TW" } else { "zh-CN" }.to_string());
    }
    if s.starts_with("ja") {
        return Some("ja-JP".to_string());
    }
    if s.starts_with("en") {
        return Some("en-US".to_string());
    }
    None
}

// ─── Windows 实现 ───────────────────────────────────────────────────────────

#[cfg(target_os = "windows")]
fn windows_user_locale() -> Option<String> {
    use windows_sys::Win32::Globalization::GetUserDefaultLocaleName;
    // LOCALE_NAME_MAX_LENGTH = 85（含结尾 \0）。
    let mut buf = [0u16; 85];
    // 返回值为写入的字符数（含 \0）；0 表示失败。
    let len = unsafe { GetUserDefaultLocaleName(buf.as_mut_ptr(), buf.len() as i32) };
    if len <= 0 {
        return None;
    }
    // 去掉结尾 \0。
    let end = (len as usize).saturating_sub(1);
    let s = String::from_utf16_lossy(&buf[..end]);
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

/// UI 界面文本翻译器。
///
/// 与剧本翻译器（`Translator`）不同，UI 翻译器只翻译界面硬编码文本
/// （按钮标签、菜单标题等），不影响剧本文本。两者使用独立的翻译文件，
/// 互不干扰，从而实现「UI 语言」与「剧本语言」分离。
///
/// # 文件格式
///
/// `assets/scripts/languages/ui/<lang>.json` 是扁平的 `{ "key": "译文" }` 结构：
///
/// ```json
/// {
///   "title.start": "开始游戏",
///   "title.settings": "设置",
///   "settings.label": "设置",
///   "button.back": "返回"
/// }
/// ```
///
/// key 使用稳定标识符（不随 UI 改版而变化），找不到时回退到 key 本身。
pub struct UiTranslator {
    /// 语言代码。
    language: String,
    /// 翻译表：key → 译文。
    entries: HashMap<String, String>,
}

impl UiTranslator {
    /// 创建空的 UI 翻译器（无任何翻译，所有查询回退到 key）。
    pub fn new() -> Self {
        Self {
            language: String::new(),
            entries: HashMap::new(),
        }
    }

    /// 从 JSON 字符串加载 UI 翻译表。
    /// JSON 格式为 `{ "key": "译文", ... }`。
    pub fn from_json_str(json: &str, language: String) -> Result<Self, String> {
        let entries: HashMap<String, String> = serde_json::from_str(json)
            .map_err(|e| format!("UI 翻译文件解析失败：{}", e))?;
        Ok(Self { language, entries })
    }

    /// 从文件加载 UI 翻译表。
    pub fn from_file(path: &Path, language: String) -> Result<Self, String> {
        let content = std::fs::read_to_string(path)
            .map_err(|e| format!("读取 UI 翻译文件失败 {}: {}", path.display(), e))?;
        Self::from_json_str(&content, language)
    }

    /// 当前语言代码。
    pub fn language(&self) -> &str {
        &self.language
    }

    /// 查询 key 对应的译文。查找顺序：
    /// 1. 当前语言的翻译表（命中且非空）；
    /// 2. 内置英文默认值（见 [`builtin_english`]）；
    /// 3. key 本身。
    ///
    /// 内置英文兜底确保即便目标语言文件缺失或部分 key 未翻译，
    /// 界面也只会显示英文而非形如 `title.start` 的 raw key。
    pub fn t<'a>(&'a self, key: &'a str) -> &'a str {
        if let Some(s) = self.entries.get(key).filter(|s| !s.is_empty()) {
            return s.as_str();
        }
        if let Some(s) = builtin_english(key) {
            return s;
        }
        key
    }

    /// 是否为空翻译器（无任何条目）。
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

impl Default for UiTranslator {
    fn default() -> Self {
        Self::new()
    }
}

/// UI 翻译的内置英文兜底表。
///
/// 当目标语言文件缺失或某个 key 未翻译时，[`UiTranslator::t`] 会回退到
/// 这里返回的英文文案，而非把 `title.start` 这样的 raw key 直接显示给玩家。
///
/// 新增 UI 文本时必须同步在此处补上英文默认值，并在各语言文件里补译。
/// key 与 `assets/scripts/languages/ui/*.json` 的键保持一致。
fn builtin_english(key: &str) -> Option<&'static str> {
    Some(match key {
        // 标题界面
        "title.start" => "Start Game",
        "title.continue" => "Continue",
        "title.load" => "Load Game",
        "title.settings" => "Settings",
        "title.exit" => "Quit",
        // HUD
        "hud.skip" => "Skip",
        "hud.auto" => "Auto",
        "hud.quick_save" => "Q.Save",
        "hud.quick_load" => "Q.Load",
        "hud.save" => "Save",
        "hud.load" => "Load",
        "hud.title" => "Title",
        "hud.settings" => "Settings",
        "hud.hide" => "Hide",
        // 设置面板
        "settings.title" => "Settings",
        "settings.tab.text" => "Text",
        "settings.tab.audio" => "Audio",
        "settings.tab.display" => "Display",
        "settings.tab.skip" => "Skip",
        "settings.tab.help" => "Help",
        "settings.tab.about" => "About",
        "settings.apply" => "Apply",
        "settings.cancel" => "Cancel",
        "settings.text_speed" => "Text Speed",
        "settings.auto_play" => "Auto Play",
        "settings.auto_play_delay_with_voice" => "Delay (Voiced)",
        "settings.auto_play_delay_without_voice" => "Delay (Unvoiced)",
        "settings.bgm_volume" => "BGM Volume",
        "settings.sfx_volume" => "SFX Volume",
        "settings.voice_volume" => "Voice Volume",
        "settings.auto_recovery" => "Auto Recovery",
        "settings.fullscreen" => "Fullscreen",
        "settings.resolution" => "Resolution",
        "settings.ui_language" => "UI Language",
        "settings.script_language" => "Script Language",
        "settings.skip_unread" => "Skip Unread Text",
        "settings.skip_mode" => "Skip Mode",
        "settings.debug_terminal" => "Debug Terminal",
        // 快进模式
        "skip_mode.text_only" => "Text Only",
        "skip_mode.with_voice" => "With Voice",
        // 存档/读档
        "save.load_title" => "Load Game",
        "save.save_title" => "Save Game",
        "save.slot" => "Slot",
        "save.empty" => "Empty",
        "save.back" => "Back",
        "save.continue" => "Continue",
        "save.cancel" => "Cancel",
        // 备注编辑
        "note.edit_title" => "Edit Note",
        "note.edit_title_slot" => "Edit Note (Slot {slot})",
        "note.hint" => "Enter to confirm · Esc to cancel · Backspace to delete",
        // 确认对话框
        "confirm.return_title" => "Return to Title",
        "confirm.unapplied" => "Unapplied Settings",
        "confirm.default_title" => "Confirm",
        "confirm.return_message" => "Return to the title screen?\nYour progress will be auto-saved as \"Continue\".",
        "confirm.unapplied_message" => "Settings changes have not been applied.\nDiscard changes and exit?",
        "confirm.default_message" => "Are you sure you want to proceed?",
        "confirm.ok" => "OK",
        "confirm.cancel" => "Cancel",
        // 自动恢复
        "autosave.title" => "Abnormal Exit Detected",
        "autosave.line1" => "An abnormal exit was detected during the last session.",
        "autosave.line2" => "Continue from where you left off?",
        "autosave.prompt" => "Abnormal exit detected. Continue the game?",
        "autosave.continue" => "Continue",
        "autosave.restart" => "Restart",
        // 语言选项
        "language.original" => "Original",
        "language.follow_script" => "Follow Script",
        // 帮助选项卡
        "help.title" => "Keyboard Shortcuts",
        "help.section.game" => "In-Game Controls",
        "help.section.note" => "Note Editing",
        "help.key" => "Key",
        "help.function" => "Function",
        "help.advance" => "Advance dialogue",
        "help.escape" => "Open settings / Cancel dialog / Close note",
        "help.backspace" => "Delete note character",
        "help.enter_note" => "Confirm note save",
        "help.click" => "Click to advance / select choice / interact",
        "help.hint" => "Use HUD buttons at the bottom for skip, auto, quick save, quick load, etc.",
        // 关于选项卡
        "about.title" => "About",
        "about.name" => "Akizuki*Rustgal",
        "about.subtitle" => "A simple and easy-to-use visual novel engine",
        "about.version_label" => "Version",
        "about.build_label" => "Build",
        "about.line4" => "心夏麻麻可爱喵",
        "about.line5" => "最喜欢心夏麻麻了喵",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_JSON: &str = r#"
{
  "language": "ja-JP",
  "display_name": "日本語",
  "sections": {
    "序章": "プロローグ"
  },
  "dialogue": {
    "你好！": "こんにちは！"
  },
  "narration": {
    "风吹过。": "風が吹いた。"
  },
  "choices": {
    "选项A": "選択肢A"
  },
  "choice_prompts": {
    "你选哪个？": "どちらを選ぶ？"
  },
  "characters": {
    "心夏": "心夏"
  },
  "voice": {
    "你好！": "voice/hello_ja.wav"
  }
}
"#;

    #[test]
    fn test_load_from_json() {
        let t = Translator::from_json_str(TEST_JSON).unwrap();
        assert_eq!(t.language(), "ja-JP");
        assert_eq!(t.display_name(), "日本語");
        assert_eq!(t.t_dialogue("你好！"), "こんにちは！");
        assert_eq!(t.t_narration("风吹过。"), "風が吹いた。");
        assert_eq!(t.t_choice("选项A"), "選択肢A");
        assert_eq!(t.t_choice_prompt("你选哪个？"), "どちらを選ぶ？");
        assert_eq!(t.t_section("序章"), "プロローグ");
        assert_eq!(t.t_character("心夏"), "心夏");
    }

    #[test]
    fn test_voice_lookup() {
        let t = Translator::from_json_str(TEST_JSON).unwrap();
        // 命中：返回语音文件名
        assert_eq!(t.voice("你好！"), Some("voice/hello_ja.wav"));
        // 未命中：返回 None（不回退原文）
        assert_eq!(t.voice("没有语音的句子"), None);
        assert_eq!(t.voice("风吹过。"), None);
    }

    #[test]
    fn test_voice_empty_string_is_none() {
        let json = r#"
{
  "language": "ja-JP",
  "voice": {
    "你好！": ""
  }
}
"#;
        let t = Translator::from_json_str(json).unwrap();
        // 空字符串视为未配置语音
        assert_eq!(t.voice("你好！"), None);
    }

    #[test]
    fn test_voice_omitted_in_old_json() {
        // 旧版翻译文件没有 voice 字段，应能正常加载，voice 查询恒返回 None
        let json = r#"
{
  "language": "zh-CN",
  "dialogue": { "你好": "你好" }
}
"#;
        let t = Translator::from_json_str(json).unwrap();
        assert_eq!(t.voice("你好"), None);
    }

    #[test]
    fn test_voice_roundtrip() {
        let mut t = Translator::default();
        t.set_voice("原文对话", "voice/line_001.wav");
        let json = t.to_json().unwrap();
        let t2 = Translator::from_json_str(&json).unwrap();
        assert_eq!(t2.voice("原文对话"), Some("voice/line_001.wav"));
    }

    #[test]
    fn test_fallback_to_original() {
        let t = Translator::from_json_str(TEST_JSON).unwrap();
        assert_eq!(t.t_dialogue("不存在的文本"), "不存在的文本");
        assert_eq!(t.t_narration("不存在的旁白"), "不存在的旁白");
        assert_eq!(t.t_choice("不存在的选项"), "不存在的选项");
    }

    #[test]
    fn test_default_is_original() {
        let t = Translator::default();
        assert_eq!(t.language(), "zh-CN");
        assert_eq!(t.t_dialogue("原文"), "原文");
        assert!(t.is_original());
    }

    #[test]
    fn test_invalid_json_falls_back() {
        let t = Translator::from_json_str("not valid json");
        assert!(t.is_err());
    }

    #[test]
    fn test_normalize_locale_posix() {
        // POSIX 风格（含编码后缀）
        assert_eq!(normalize_locale("zh_CN.UTF-8"), Some("zh-CN".to_string()));
        assert_eq!(normalize_locale("zh_TW.BIG5"), Some("zh-TW".to_string()));
        assert_eq!(normalize_locale("zh_HK"), Some("zh-TW".to_string()));
        assert_eq!(normalize_locale("ja_JP.UTF-8"), Some("ja-JP".to_string()));
        assert_eq!(normalize_locale("en_US"), Some("en-US".to_string()));
        assert_eq!(normalize_locale("en_GB.UTF-8"), Some("en-US".to_string()));
        assert_eq!(normalize_locale("zh"), Some("zh-CN".to_string()));
    }

    #[test]
    fn test_normalize_locale_bcp47() {
        // BCP 47 风格（Windows GetUserDefaultLocaleName 返回此格式）
        assert_eq!(normalize_locale("zh-CN"), Some("zh-CN".to_string()));
        assert_eq!(normalize_locale("zh-TW"), Some("zh-TW".to_string()));
        assert_eq!(normalize_locale("zh-Hans-CN"), Some("zh-CN".to_string()));
        assert_eq!(normalize_locale("zh-Hant-TW"), Some("zh-TW".to_string()));
        assert_eq!(normalize_locale("ja-JP"), Some("ja-JP".to_string()));
        assert_eq!(normalize_locale("en-US"), Some("en-US".to_string()));
    }

    #[test]
    fn test_normalize_locale_unsupported() {
        // 不支持的语言返回 None（由调用方回退到 en-US）
        assert_eq!(normalize_locale("ko_KR"), None);
        assert_eq!(normalize_locale("fr_FR.UTF-8"), None);
        assert_eq!(normalize_locale(""), None);
        assert_eq!(normalize_locale("   "), None);
    }

    #[test]
    fn test_ui_translator_falls_back_to_builtin_english() {
        // 空翻译器（语言文件缺失）应回退到内置英文，而非 raw key。
        let t = UiTranslator::new();
        assert_eq!(t.t("title.start"), "Start Game");
        assert_eq!(t.t("title.exit"), "Quit");
        assert_eq!(t.t("save.empty"), "Empty");
        assert_eq!(t.t("note.edit_title"), "Edit Note");
    }

    #[test]
    fn test_ui_translator_prefers_loaded_entries() {
        // 已加载的译文优先于内置英文。
        let t = UiTranslator::from_json_str(
            r#"{ "title.start": "开始游戏" }"#,
            "zh-CN".to_string(),
        )
        .unwrap();
        assert_eq!(t.t("title.start"), "开始游戏");
        // 未覆盖的 key 仍回退到内置英文。
        assert_eq!(t.t("title.exit"), "Quit");
    }

    #[test]
    fn test_ui_translator_unknown_key_returns_key() {
        // 完全未知的 key（不在内置表里）才回退到 key 本身。
        let t = UiTranslator::new();
        assert_eq!(t.t("some.unknown.key"), "some.unknown.key");
    }

    #[test]
    fn test_builtin_english_covers_all_known_keys() {
        // 内置英文表必须覆盖 zh-CN.json 中的全部 key（含 note.* / help.* / about.*）。
        for key in [
            "title.start", "title.continue", "title.load", "title.settings", "title.exit",
            "hud.skip", "hud.auto", "hud.quick_save", "hud.quick_load",
            "hud.save", "hud.load", "hud.title", "hud.settings", "hud.hide",
            "settings.title", "settings.tab.text", "settings.tab.audio",
            "settings.tab.display", "settings.tab.skip",
            "settings.tab.help", "settings.tab.about",
            "settings.apply", "settings.cancel",
            "settings.text_speed", "settings.auto_play",
            "settings.auto_play_delay_with_voice", "settings.auto_play_delay_without_voice",
            "settings.bgm_volume", "settings.sfx_volume", "settings.voice_volume",
            "settings.auto_recovery", "settings.fullscreen", "settings.resolution",
            "settings.ui_language", "settings.script_language",
            "settings.skip_unread", "settings.skip_mode", "settings.debug_terminal",
            "skip_mode.text_only", "skip_mode.with_voice",
            "save.load_title", "save.save_title", "save.slot", "save.empty",
            "save.back", "save.continue", "save.cancel",
            "note.edit_title", "note.edit_title_slot", "note.hint",
            "confirm.return_title", "confirm.unapplied", "confirm.default_title",
            "confirm.return_message", "confirm.unapplied_message", "confirm.default_message",
            "confirm.ok", "confirm.cancel",
            "autosave.title", "autosave.line1", "autosave.line2", "autosave.prompt",
            "autosave.continue", "autosave.restart",
            "language.original", "language.follow_script",
            "help.title", "help.section.game", "help.section.note",
            "help.key", "help.function", "help.advance", "help.escape",
            "help.backspace", "help.enter_note", "help.click", "help.hint",
            "about.title", "about.name", "about.subtitle",
            "about.version_label", "about.build_label",
            "about.line4", "about.line5",
        ] {
            assert!(builtin_english(key).is_some(), "内置英文表缺失 key: {}", key);
        }
    }
}
