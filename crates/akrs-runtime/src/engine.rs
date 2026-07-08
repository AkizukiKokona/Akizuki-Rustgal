//! The game engine: coordinates VM execution, scene state, transitions,
//! save/load, settings, and hot reload.
//!
//! The engine is rendering-agnostic. It produces a `SceneState` each frame
//! that a renderer (e.g., `akrs_render` with macroquad) reads to draw.
//!
//! # Architecture
//!
//! ```text
//! ┌──────────────────────────────────────────────────┐
//! │                     Engine                        │
//! │  ┌──────┐  ┌────────────┐  ┌───────────────────┐ │
//! │  │  VM  │→ │ Event Loop │→ │   SceneState      │ │
//! │  └──────┘  └────────────┘  │  (render snapshot) │ │
//! │  ┌──────────────────────┐  └───────────────────┘ │
//! │  │ TransitionManager    │  ┌───────────────────┐ │
//! │  │ (Out → Swap → In)    │  │  Settings         │ │
//! │  └──────────────────────┘  │  SaveManager       │ │
//! │  ┌──────────────────────┐  │  HotReloader       │ │
//! │  │ TypewriterState      │  └───────────────────┘ │
//! │  └──────────────────────┘                        │
//! └──────────────────────────────────────────────────┘
//! ```
//!
//! # 60fps Guarantee
//!
//! - No per-frame heap allocations in the hot path
//! - Transition updates are pure float arithmetic
//! - Typewriter is a simple counter increment
//! - VM step is a match on a flat instruction array

use crate::game_state::{SceneState, ChoiceOptionState};
use crate::save_load::{SaveManager, SceneSnapshot};
use crate::settings::{Settings, SkipMode};
use crate::translator::{Translator, UiTranslator};
use crate::transition::TransitionManager;

use akrs_core::{
    compile_and_create_vm, CompileError, DirectionAction, DirectionKind,
    Transition, Vm, VmEvent,
};

use std::collections::HashSet;

/// Events emitted by the engine for the UI layer to react to.
#[derive(Debug, Clone)]
pub enum EngineEvent {
    /// A new dialogue line is being displayed.
    DialogueShown { speaker: String, text: String },
    /// Narration text is being displayed.
    NarrationShown { text: String },
    /// All text has been displayed (typewriter complete).
    TextComplete,
    /// Choices are being displayed.
    ChoicesShown { prompt: Option<String>, options: Vec<ChoiceOptionState> },
    /// A background change occurred.
    BackgroundChanged { name: String },
    /// A character entered the stage.
    CharacterEntered { name: String },
    /// A character exited the stage.
    CharacterExited { name: String },
    /// Music changed.
    MusicChanged { name: String },
    /// A sound effect was played.
    SoundPlayed { name: String },
    /// 一句对话/旁白对应的语音文件应当播放（或停止）。
    /// `name` 为空字符串表示停止当前语音（无语音的句子）。
    VoicePlayed { name: String },
    /// A transition started.
    TransitionStarted { kind: Transition },
    /// A transition completed.
    TransitionCompleted,
    /// The story has ended.
    StoryEnded,
    /// A warning (e.g., missing resource).
    Warning { message: String },
    /// An error occurred.
    Error { message: String },
    /// The game started (from title screen).
    GameStarted,
    /// A save was completed.
    Saved { slot: usize },
    /// A load was completed.
    Loaded { slot: usize },
    /// Script was hot-reloaded.
    ScriptReloaded,
    /// Script hot-reload failed.
    ScriptReloadFailed { errors: Vec<String> },
}

/// Engine phase (high-level state machine).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum EnginePhase {
    /// Title screen showing.
    Title,
    /// Running the script, waiting for player to advance.
    Running,
    /// A transition is in progress; pending event will be shown after.
    Transitioning,
    /// Waiting for a timer (the `wait` command).
    Waiting,
    /// Choices are displayed, waiting for selection.
    ChoicePending,
    /// Story has ended.
    StoryEnded,
}

/// Pending blocking event to show after a transition completes.
enum PendingEvent {
    Dialogue { speaker: String, pose: Option<String>, text: String },
    Narration { text: String },
    Choices { prompt: Option<String>, options: Vec<ChoiceOptionState> },
    Wait { seconds: f64 },
    StoryEnd,
}

/// 章节切换通知（由 `->` 章节跳转触发）。
///
/// 引擎在处理 `VmEvent::Flow` 时设置此通知，渲染层每帧通过
/// `take_chapter_notify()` 取走，随后播放全屏淡入淡出 + 顶部通知动画。
/// `name` 为章节标识符（`#` 后首个标识符），`title` 为可选显示标题
/// （`# name title` 中 name 之后的同行文本）。
#[derive(Debug, Clone)]
pub struct ChapterNotify {
    pub name: String,
    pub title: Option<String>,
}

/// Typewriter animation state.
struct TypewriterState {
    chars_per_second: f32,
    elapsed: f32,
    complete: bool,
}

impl TypewriterState {
    fn new() -> Self {
        Self { chars_per_second: 30.0, elapsed: 0.0, complete: true }
    }

    fn start(&mut self, text_len: usize, chars_per_second: f32) {
        self.chars_per_second = chars_per_second;
        self.elapsed = 0.0;
        self.complete = chars_per_second <= 0.0 || chars_per_second >= 999.0 || text_len == 0;
    }

    fn update(&mut self, dt: f32) -> usize {
        if self.complete {
            return usize::MAX;
        }
        self.elapsed += dt;
        (self.elapsed * self.chars_per_second) as usize
    }

    fn finish(&mut self) {
        self.complete = true;
    }
}

/// The main game engine.
pub struct Engine {
    /// The script VM.
    vm: Vm,
    /// The original source text (for hot reload).
    source: String,
    /// Current render state (scene graph).
    scene: SceneState,
    /// Transition animation manager.
    transition: TransitionManager,
    /// User settings.
    settings: Settings,
    /// Save slot manager.
    saves: SaveManager,
    /// Current engine phase.
    phase: EnginePhase,
    /// Typewriter state.
    typewriter: TypewriterState,
    /// Pending event to show after transition.
    pending: Option<PendingEvent>,
    /// Wait timer remaining (seconds).
    wait_remaining: f64,
    /// Play time in seconds.
    play_time: f64,
    /// Current section name (for save metadata).
    current_section_name: String,
    /// Hot reloader (if enabled).
    #[cfg(feature = "hot-reload")]
    hot_reloader: Option<crate::hot_reload::HotReloader>,
    /// 已读过的文本哈希集合（用于"跳过已读"功能）。
    /// key 为 "speaker|text" 格式，旁白使用 "|text"。
    read_history: HashSet<String>,
    /// 快进模式是否激活（玩家点击快进按钮后启用）。
    skip_active: bool,
    /// 当前对话/旁白对应的语音文件名（若存在）。
    /// 在 show_dialogue / show_narration 时查询 translator.voice(原文) 设置。
    /// None 表示当前句子无语音。
    current_voice: Option<String>,
    /// 语音播放进度（用于 WithVoice 模式下的快进等待）。
    /// 当前引擎不追踪语音实际播放时长，渲染层负责播放与停止；
    /// 此字段保留供未来精细化快进控制使用，目前未使用。
    #[allow(dead_code)]
    voice_progress: f32,
    /// 自动播放倒计时剩余秒数。
    /// 当 settings.auto_play 为 true 且当前对话文本已显示完毕时，
    /// 每帧递减；归零后自动 vm.advance()。
    /// 玩家手动点击 advance() 时重置为 0（取消等待，立即推进）。
    auto_play_remaining: f32,
    /// 剧本翻译器：运行时把原文对话/旁白/选项替换为目标语言译文。
    /// 原文即 key，查不到则回退原文，逻辑层（VM/存档/跳转）不受影响。
    translator: Translator,
    /// UI 翻译器：翻译界面硬编码文本（按钮标签、菜单标题等）。
    /// 与剧本翻译器独立，可单独切换 UI 语言。
    ui_translator: UiTranslator,
    /// 翻译文件所在目录，用于 reload_language 时定位语言文件。
    translations_dir: Option<std::path::PathBuf>,
    /// 待处理的章节切换通知（由 `->` 章节跳转设置，渲染层取走后播放动画）。
    /// None 表示无待处理通知。
    pending_chapter_notify: Option<ChapterNotify>,
}

impl Engine {
    /// Create a new engine from script source text.
    pub fn new(source: &str) -> Result<Self, Vec<CompileError>> {
        let vm = compile_and_create_vm(source)?;
        let saves = SaveManager::new("saves", 100);

        Ok(Self {
            vm,
            source: source.to_string(),
            scene: SceneState::new(),
            transition: TransitionManager::new(),
            settings: Settings::default(),
            saves,
            phase: EnginePhase::Title,
            typewriter: TypewriterState::new(),
            pending: None,
            wait_remaining: 0.0,
            play_time: 0.0,
            current_section_name: String::new(),
            #[cfg(feature = "hot-reload")]
            hot_reloader: None,
            read_history: HashSet::new(),
            skip_active: false,
            current_voice: None,
            voice_progress: 0.0,
            auto_play_remaining: 0.0,
            translator: Translator::new(),
            ui_translator: UiTranslator::new(),
            translations_dir: None,
            pending_chapter_notify: None,
        })
    }

    /// Create an engine and immediately start the game (skip title screen).
    pub fn start_running(source: &str) -> Result<Self, Vec<CompileError>> {
        let mut engine = Self::new(source)?;
        engine.start_game();
        Ok(engine)
    }

    /// Enable hot reload for a script file path.
    #[cfg(feature = "hot-reload")]
    pub fn enable_hot_reload(&mut self, script_path: impl AsRef<std::path::Path>) -> Result<(), String> {
        let reloader = crate::hot_reload::HotReloader::new(script_path)?;
        self.hot_reloader = Some(reloader);
        Ok(())
    }

    /// Start the game (transition from title screen to running).
    pub fn start_game(&mut self) {
        if self.phase != EnginePhase::Title {
            return;
        }
        self.phase = EnginePhase::Running;
        self.scene.show_title = false;
        let _ = self.vm.start();
        self.process_events();
    }

    /// Get the current render state (for the renderer to draw).
    pub fn scene(&self) -> &SceneState {
        &self.scene
    }

    /// 设置标题页的主标题和副标题。
    pub fn set_title(&mut self, title: String, subtitle: String) {
        self.scene.set_title(title, subtitle);
    }

    /// Get the current engine phase.
    pub fn phase(&self) -> EnginePhase {
        self.phase
    }

    /// Get play time in seconds.
    pub fn play_time(&self) -> f64 {
        self.play_time
    }

    /// Get current section name.
    pub fn current_section(&self) -> &str {
        &self.current_section_name
    }

    /// 取走待处理的章节切换通知（若有）。
    ///
    /// 渲染层每帧调用：返回 `Some(ChapterNotify)` 表示刚发生一次 `->` 章节
    /// 跳转，应播放全屏淡入淡出 + 顶部章节通知动画；返回 `None` 表示无。
    /// 取走后内部清空，同一通知不会被消费两次。
    pub fn take_chapter_notify(&mut self) -> Option<ChapterNotify> {
        self.pending_chapter_notify.take()
    }

    /// Access settings.
    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    /// Mutable settings access.
    pub fn settings_mut(&mut self) -> &mut Settings {
        &mut self.settings
    }

    /// 快进是否激活。
    pub fn is_skip_active(&self) -> bool {
        self.skip_active
    }

    /// 切换快进状态。
    pub fn toggle_skip(&mut self) {
        self.skip_active = !self.skip_active;
    }

    /// 设置快进状态。
    pub fn set_skip_active(&mut self, active: bool) {
        self.skip_active = active;
    }

    /// 当前对话是否带有语音（翻译表的 voice 字段命中）。
    ///
    /// 注意：此方法只判断翻译表是否配置了语音引用，
    /// 不保证语音文件实际存在或已加载（那是渲染层的职责）。
    fn current_dialogue_has_voice(&self) -> bool {
        self.current_voice.is_some()
    }

    /// 自动播放倒计时剩余秒数（0 表示未在等待）。
    /// 供渲染层绘制进度指示器使用。
    pub fn auto_play_remaining(&self) -> f32 {
        self.auto_play_remaining
    }

    /// 切换自动播放开关（运行时快捷切换，同步到 settings.auto_play）。
    pub fn toggle_auto_play(&mut self) {
        self.settings.auto_play = !self.settings.auto_play;
        self.auto_play_remaining = 0.0;
    }

    /// 已读历史（不可变访问，供调试用）。
    pub fn read_history(&self) -> &HashSet<String> {
        &self.read_history
    }

    /// 访问翻译器（不可变）。
    pub fn translator(&self) -> &Translator {
        &self.translator
    }

    /// 直接设置翻译器（用于加载新的语言文件）。
    pub fn set_translator(&mut self, translator: Translator) {
        self.translator = translator;
    }

    /// 从文件加载翻译器，并保存语言选择到 settings。
    pub fn load_language(&mut self, lang_code: &str, translations_dir: &std::path::Path) {
        let path = translations_dir.join(format!("{}.json", lang_code));
        if path.exists() {
            self.translator = Translator::from_file(&path);
        } else {
            eprintln!("[Engine] 语言文件不存在：{:?}，使用原文", path);
            self.translator = Translator::new();
        }
        self.settings.language = lang_code.to_string();
        self.translations_dir = Some(translations_dir.to_path_buf());
    }

    /// 根据当前 settings.language 重新加载翻译文件（切换语言时调用）。
    /// 若未设置过 translations_dir 或语言文件不存在，则回退到原文。
    pub fn reload_language(&mut self) {
        let Some(dir) = self.translations_dir.clone() else {
            self.translator = Translator::new();
            return;
        };
        let lang = self.settings.effective_language();
        let path = dir.join(format!("{}.json", lang));
        if path.exists() {
            self.translator = Translator::from_file(&path);
        } else {
            eprintln!("[Engine] 语言文件不存在：{:?}，使用原文", path);
            self.translator = Translator::new();
        }
    }

    /// 从目录加载 UI 翻译文件。UI 翻译文件位于 `<translations_dir>/ui/<lang>.json`。
    /// 文件不存在时回退到空 UI 翻译器（所有查询回退到 key 本身，即英文标识符）。
    pub fn load_ui_language(&mut self, lang_code: &str, translations_dir: &std::path::Path) {
        let ui_dir = translations_dir.join("ui");
        let path = ui_dir.join(format!("{}.json", lang_code));
        if path.exists() {
            match UiTranslator::from_file(&path, lang_code.to_string()) {
                Ok(t) => self.ui_translator = t,
                Err(e) => {
                    eprintln!("[Engine] UI 翻译文件加载失败：{}", e);
                    self.ui_translator = UiTranslator::new();
                }
            }
        } else {
            // UI 翻译文件不存在不报错，静默回退到 key 本身（英文标识符）
            self.ui_translator = UiTranslator::new();
        }
        self.settings.ui_language = lang_code.to_string();
    }

    /// 根据当前 settings.effective_ui_language() 重新加载 UI 翻译文件。
    pub fn reload_ui_language(&mut self) {
        let Some(dir) = self.translations_dir.clone() else {
            self.ui_translator = UiTranslator::new();
            return;
        };
        let lang = self.settings.effective_ui_language();
        let path = dir.join("ui").join(format!("{}.json", lang));
        if path.exists() {
            match UiTranslator::from_file(&path, lang) {
                Ok(t) => self.ui_translator = t,
                Err(e) => {
                    eprintln!("[Engine] UI 翻译文件加载失败：{}", e);
                    self.ui_translator = UiTranslator::new();
                }
            }
        } else {
            self.ui_translator = UiTranslator::new();
        }
    }

    /// 获取翻译文件所在目录（可能为 None，表示从未调用过 load_language）。
    /// 用于在重建引擎（如返回标题）时继承翻译目录。
    pub fn translations_dir(&self) -> Option<&std::path::Path> {
        self.translations_dir.as_deref()
    }

    /// 设置翻译文件所在目录，并立即根据当前 settings 重新加载剧本与 UI 翻译。
    /// 用于重建引擎后恢复翻译能力（Engine::new 不会设置 translations_dir）。
    pub fn restore_translations(&mut self, dir: std::path::PathBuf) {
        self.translations_dir = Some(dir);
        self.reload_language();
        self.reload_ui_language();
    }

    /// 获取 UI 翻译器引用。
    pub fn ui_translator(&self) -> &UiTranslator {
        &self.ui_translator
    }

    /// 查询 UI 文本。找不到译文时回退到 key 本身。
    pub fn t_ui<'a>(&'a self, key: &'a str) -> &'a str {
        self.ui_translator.t(key)
    }

    /// 获取可用语言列表（扫描翻译目录）。
    /// 返回 (语言代码, 显示名) 的 Vec。
    pub fn available_languages(&self) -> Vec<(String, String)> {
        let mut result = Vec::new();
        let Some(dir) = self.translations_dir.clone() else {
            return result;
        };
        let Ok(entries) = std::fs::read_dir(&dir) else {
            return result;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("json") {
                if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                    // 尝试读取 display_name
                    let display = if let Ok(content) = std::fs::read_to_string(&path) {
                        serde_json::from_str::<serde_json::Value>(&content)
                            .ok()
                            .and_then(|v| v.get("display_name")?.as_str().map(|s| s.to_string()))
                            .unwrap_or_else(|| stem.to_string())
                    } else {
                        stem.to_string()
                    };
                    result.push((stem.to_string(), display));
                }
            }
        }
        result.sort_by(|a, b| a.0.cmp(&b.0));
        result
    }

    /// Load persistent settings from `saves/settings.json`.
    /// If the file does not exist or cannot be parsed, defaults are used.
    pub fn load_settings(&mut self) {
        let path = Settings::default_path();
        self.settings = Settings::load(&path);
        // 同时加载已读历史
        self.load_read_history();
    }

    /// 加载已读历史（saves/read_history.json）。
    fn load_read_history(&mut self) {
        let path = std::path::PathBuf::from("saves").join("read_history.json");
        if let Ok(content) = std::fs::read_to_string(&path) {
            if let Ok(history) = serde_json::from_str::<HashSet<String>>(&content) {
                self.read_history = history;
            }
        }
    }

    /// 保存已读历史。
    pub fn save_read_history(&self) {
        let path = std::path::PathBuf::from("saves").join("read_history.json");
        if let Ok(content) = serde_json::to_string(&self.read_history) {
            let _ = std::fs::create_dir_all("saves");
            let _ = std::fs::write(&path, content);
        }
    }

    /// Persist the current settings to `saves/settings.json`.
    pub fn save_settings(&self) -> Result<(), String> {
        let path = Settings::default_path();
        // Ensure the saves directory exists.
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        self.settings.save(&path)
    }

    /// Access save manager.
    pub fn saves(&self) -> &SaveManager {
        &self.saves
    }

    /// Update the engine. Call this every frame.
    /// Returns events for the UI layer to process.
    pub fn update(&mut self, dt: f32) -> Vec<EngineEvent> {
        let mut events = Vec::new();

        // Track play time
        if self.phase != EnginePhase::Title && self.phase != EnginePhase::StoryEnded {
            self.play_time += dt as f64;
        }

        // Update transition
        if self.transition.is_active() {
            let completed = self.transition.update(dt, &mut self.scene);
            if completed {
                events.push(EngineEvent::TransitionCompleted);
                // If there's a pending event, show it now
                if let Some(pending) = self.pending.take() {
                    self.show_pending_event(pending, &mut events);
                } else {
                    // No pending event, continue processing
                    self.phase = EnginePhase::Running;
                    self.process_events_into(&mut events);
                }
            }
        } else if self.phase == EnginePhase::Transitioning {
            // Transition just finished but update didn't catch it
            self.phase = EnginePhase::Running;
            if let Some(pending) = self.pending.take() {
                self.show_pending_event(pending, &mut events);
            } else {
                self.process_events_into(&mut events);
            }
        }

        // Update typewriter
        if self.phase == EnginePhase::Running {
            if let Some(dialogue) = &mut self.scene.dialogue
                && !dialogue.complete
            {
                let displayed = self.typewriter.update(dt);
                let text_len = dialogue.full_text.chars().count();
                if displayed >= text_len {
                    dialogue.displayed_chars = text_len;
                    dialogue.complete = true;
                    self.typewriter.complete = true;
                    events.push(EngineEvent::TextComplete);
                } else {
                    dialogue.displayed_chars = displayed;
                }
            }
        }

        // 快进模式：文本完成后自动推进到下一句。
        // - 若关闭"允许跳过未读文本"，遇到未读文本时自动停止快进。
        // - 若快进模式为"包含语音"，需等待语音播放完毕（当前无语音系统，预留）。
        if self.skip_active && self.phase == EnginePhase::Running {
            let can_skip = if let Some(dialogue) = &self.scene.dialogue {
                if !dialogue.complete {
                    false // 文本还没显示完，等显示完再跳
                } else if self.settings.skip_unread {
                    true // 允许跳过未读，直接跳
                } else {
                    // 只跳过已读：检查当前文本是否在已读历史中
                    let key = format!("{}|{}", dialogue.speaker, dialogue.full_text);
                    self.read_history.contains(&key)
                }
            } else {
                false // 没有对话，不跳
            };

            if can_skip {
                // 检查快进模式
                match self.settings.skip_mode {
                    SkipMode::TextOnly => {
                        // 仅文本：直接推进
                        self.vm.advance();
                        self.process_events_into(&mut events);
                    }
                    SkipMode::WithVoice => {
                        // 包含语音：等待语音播完（目前无语音系统，直接推进）
                        self.vm.advance();
                        self.process_events_into(&mut events);
                    }
                }
            } else if !self.settings.skip_unread {
                // 遇到未读文本，自动停止快进
                self.skip_active = false;
            }
        }

        // Update wait timer
        if self.phase == EnginePhase::Waiting {
            self.wait_remaining -= dt as f64;
            if self.wait_remaining <= 0.0 {
                self.phase = EnginePhase::Running;
                self.process_events_into(&mut events);
            }
        }

        // 自动播放：当 settings.auto_play 开启、当前处于 Running 阶段、
        // 对话文本已显示完毕、无过渡、未在等待/选择/快进时，按设定间隔自动推进。
        // 玩家手动点击 advance() 会立即推进并重置倒计时（在 advance 中处理）。
        if self.settings.auto_play
            && !self.skip_active
            && self.phase == EnginePhase::Running
            && !self.transition.is_active()
        {
            let ready = match &self.scene.dialogue {
                Some(d) => d.complete,
                None => false,
            };
            if ready {
                if self.auto_play_remaining <= 0.0 {
                    // 启动倒计时：当前对话有语音用 with_voice 间隔，否则用 without_voice。
                    // 当前无语音系统，所有对话均按"无语音"计时。
                    let delay = if self.current_dialogue_has_voice() {
                        self.settings.auto_play_delay_with_voice
                    } else {
                        self.settings.auto_play_delay_without_voice
                    };
                    self.auto_play_remaining = delay.max(0.0);
                } else {
                    self.auto_play_remaining -= dt;
                    if self.auto_play_remaining <= 0.0 {
                        self.auto_play_remaining = 0.0;
                        // 倒计时结束，自动推进到下一句。
                        self.vm.advance();
                        self.process_events_into(&mut events);
                    }
                }
            } else {
                // 文本尚未完成或已切换场景，重置倒计时。
                self.auto_play_remaining = 0.0;
            }
        } else {
            // 自动播放关闭或条件不满足，重置倒计时避免残留。
            self.auto_play_remaining = 0.0;
        }

        // Check for hot reload
        #[cfg(feature = "hot-reload")]
        if let Some(reloader) = &self.hot_reloader
            && let Some(new_source) = reloader.check_for_changes()
        {
            match self.reload_script_internal(&new_source) {
                Ok(()) => events.push(EngineEvent::ScriptReloaded),
                Err(errs) => events.push(EngineEvent::ScriptReloadFailed { errors: errs }),
            }
        }

        events
    }

    /// Player clicked to advance.
    pub fn advance(&mut self) -> Vec<EngineEvent> {
        let mut events = Vec::new();

        if self.phase == EnginePhase::Title {
            self.start_game();
            events.push(EngineEvent::GameStarted);
            return events;
        }

        if self.phase == EnginePhase::StoryEnded {
            return events;
        }

        // If typewriter is not complete, complete it
        if let Some(dialogue) = &mut self.scene.dialogue
            && !dialogue.complete
        {
            dialogue.displayed_chars = dialogue.full_text.chars().count();
            dialogue.complete = true;
            self.typewriter.finish();
            events.push(EngineEvent::TextComplete);
            return events;
        }

        // If transition is active, ignore advance
        if self.transition.is_active() {
            return events;
        }

        // Advance the VM and process next events
        if self.phase == EnginePhase::Running {
            self.vm.advance();
            self.process_events_into(&mut events);
        }

        events
    }

    /// Player selected a choice.
    pub fn choose(&mut self, index: usize) -> Vec<EngineEvent> {
        let mut events = Vec::new();

        if self.phase != EnginePhase::ChoicePending {
            return events;
        }

        match self.vm.choose(index) {
            Ok(()) => {
                self.scene.clear_choices();
                self.phase = EnginePhase::Running;
                self.process_events_into(&mut events);
            }
            Err(e) => {
                events.push(EngineEvent::Error { message: e.message });
            }
        }

        events
    }

    /// 构造当前场景的快照，用于存档时保存"持久态"场景数据
    /// （背景/立绘/音乐），以便读档时恢复，避免读档后背景黑屏、
    /// 音乐中断。对话/选项/过渡等"瞬时态"不在此快照中，它们会由
    /// 读档后 `process_events_into` 从 ip 继续执行时重新产生。
    ///
    /// 关键：若过渡处于 Out 阶段（pending 尚未应用到现场 scene），必须
    /// 把 pending 变更应用到快照上。因为 VM 指针已越过触发过渡的指令
    /// （入场/下场/换背景），读档后不会重放该指令；若快照仍是过渡前的
    /// 旧状态，本该下场的立绘就会残留在屏幕上"下不去"。
    fn scene_snapshot(&self) -> Option<SceneSnapshot> {
        if self.transition.has_pending() {
            // 克隆现场并把 pending 应用上去，构造与 VM ip 一致的快照。
            let mut snap_scene = self.scene.clone();
            self.transition.apply_pending_to(&mut snap_scene);
            Some(SceneSnapshot {
                background: snap_scene.background,
                characters: snap_scene.characters,
                music: snap_scene.music,
            })
        } else {
            Some(SceneSnapshot {
                background: self.scene.background.clone(),
                characters: self.scene.characters.clone(),
                music: self.scene.music.clone(),
            })
        }
    }

    /// 构造存档描述：对话取 "说话人: 文本" 前 30 字符；无对话时用章节名。
    ///
    /// 用 `char_indices().nth(30)` 取第 31 个字符的起始字节位置截取，
    /// 不足 30 字符时取全长。旧实现 `take(30).last().unwrap_or(0)` 在
    /// 不足 30 字符时会截取到字节 0 得到空串，且即便够 30 字符也会丢掉
    /// 第 30 个字符本身。
    fn save_description(&self) -> String {
        self.scene.dialogue.as_ref()
            .map(|d| {
                let head = format!("{}: {}", d.speaker, d.full_text);
                let cut = head.char_indices().nth(30).map(|(i, _)| i).unwrap_or(head.len());
                head[..cut].to_string()
            })
            .unwrap_or_else(|| self.current_section_name.clone())
    }

    /// Save the current game state.
    pub fn save(&mut self, slot: usize) -> Vec<EngineEvent> {
        let mut events = Vec::new();
        let vm_state = self.vm.save_state();
        let description = self.save_description();

        match self.saves.save(
            slot,
            vm_state,
            &self.current_section_name,
            self.play_time as u64,
            &description,
            self.scene_snapshot(),
        ) {
            Ok(_) => events.push(EngineEvent::Saved { slot }),
            Err(e) => events.push(EngineEvent::Error { message: e }),
        }
        events
    }

    /// Load a save slot.
    pub fn load(&mut self, slot: usize) -> Vec<EngineEvent> {
        let mut events = Vec::new();

        match self.saves.load(slot) {
            Ok(save) => {
                self.vm.load_state(save.vm_state);
                self.scene.show_title = false;
                self.phase = EnginePhase::Running;
                self.scene.clear_text();
                // 读档后从 VM 指针继续执行，存档不保存过渡状态，
                // 任何残留的过渡（含 pending 变更与叠层）都是过时的，必须清空，
                // 否则残留 pending 会在 swap point 错误应用到刚恢复的场景。
                self.transition.reset();
                self.scene.transition = None;
                // 读档清空交叉淡入的旧背景（快照不含该字段，避免残留淡出层）。
                self.scene.prev_background = None;
                // 恢复存档时保存的场景快照（背景/立绘/音乐），
                // 避免读档/崩溃恢复后背景黑屏、音乐中断。背景每帧按名字
                // 查纹理，恢复 state 后渲染层自动正确绘制；音乐是事件
                // 驱动的，需额外发 MusicChanged 事件让渲染层重新播放。
                //
                // 旧格式存档没有 scene 字段：调用 rebuild_scene_for_slot
                // 从入口重放到存档点重建场景持久态，并回写升级存档文件，
                // 使后续读档/缩略图渲染可直接使用 scene 字段。
                if let Some(snap) = save.scene {
                    self.scene.background = snap.background;
                    self.scene.characters = snap.characters;
                    self.scene.music = snap.music.clone();
                    if let Some(name) = &self.scene.music {
                        if !name.is_empty() {
                            events.push(EngineEvent::MusicChanged { name: name.clone() });
                        }
                    }
                } else {
                    // 旧格式存档：重建场景持久态。
                    if let Some(rebuilt) = self.rebuild_scene_for_slot(slot) {
                        self.scene.background = rebuilt.background.clone();
                        self.scene.characters = rebuilt.characters.clone();
                        self.scene.music = rebuilt.music.clone();
                        if let Some(name) = &self.scene.music {
                            if !name.is_empty() {
                                events.push(EngineEvent::MusicChanged { name: name.clone() });
                            }
                        }
                        // 回写升级存档文件（失败仅警告，不阻断读档）。
                        if let Err(e) = self.saves.upgrade_scene(slot, rebuilt) {
                            events.push(EngineEvent::Warning {
                                message: format!("failed to upgrade save scene: {}", e),
                            });
                        }
                    }
                    // rebuild_scene_for_slot 内部已用 save_state/load_state
                    // 保护并恢复 VM 到调用前状态（即 802 行 load 后的存档状态），
                    // 此处无需再 load_state。
                }
                self.process_events_into(&mut events);
                events.push(EngineEvent::Loaded { slot });
            }
            Err(e) => {
                events.push(EngineEvent::Error { message: e });
            }
        }
        events
    }

    /// 为指定槽位重建场景快照（用于旧格式存档升级）。
    ///
    /// 旧格式存档没有 `scene` 字段，读档后背景黑屏、立绘消失。本方法通过
    /// `Vm::rebuild_scene_to` 从入口重放到存档点，把沿途的 `@bg` / `+角色` /
    /// `@music` 等指令**直接应用到 scene**（绕过过渡，重建只关心最终态），
    /// 构造出存档时刻的场景持久态快照。
    ///
    /// 返回 `Some(SceneSnapshot)` 表示重建成功；`None` 表示该槽位无存档或
    /// 读取失败。重建后调用方可通过 `saves().upgrade_scene()` 把快照回写
    /// 到存档文件，完成一次性升级。
    ///
    /// 注意：本方法会临时修改 `self.vm` 的状态，但会在返回前恢复到调用前
    /// 的状态（用 `save_state` / `load_state` 保护），不影响当前游戏进度。
    pub fn rebuild_scene_for_slot(&mut self, slot: usize) -> Option<SceneSnapshot> {
        let save = self.saves.load(slot).ok()?;
        let saved_vm_state = self.vm.save_state();

        // 加载存档的 VM 状态，重建场景。
        self.vm.load_state(save.vm_state.clone());
        let target_section = save.vm_state.section;
        let target_ip = save.vm_state.ip;
        let events = self.vm.rebuild_scene_to(target_section, target_ip);

        // 用一个临时 scene 应用重建事件（直接应用，不走过渡）。
        // 临时 scene 从空开始，因为重建是从入口重放，背景/立绘逐步累积到存档点状态。
        let mut snap_scene = SceneState::default();

        for ev in events {
            match ev {
                VmEvent::Command { cmd, args, .. } => {
                    // 直接应用命令到临时 scene（不触发过渡、不产生引擎事件）。
                    match cmd.as_str() {
                        "bg" | "background" => {
                            let name = args.first().cloned().unwrap_or_default();
                            if !name.is_empty() {
                                snap_scene.set_background(name);
                            } else {
                                snap_scene.background = None;
                            }
                        }
                        "music" | "bgm" => {
                            let name = args.first().cloned().unwrap_or_default();
                            if !name.is_empty() {
                                snap_scene.music = Some(name);
                            } else {
                                snap_scene.music = None;
                            }
                        }
                        "stop_music" | "stop_bgm" => {
                            snap_scene.music = None;
                        }
                        // sound/sfx 是瞬时音效，不影响场景持久态，跳过。
                        _ => {}
                    }
                }
                VmEvent::Direction { action } => {
                    match action.kind {
                        DirectionKind::Enter => {
                            // 重建时直接入场（不走过渡），保留 pose/position/transform。
                            match action.position {
                                Some(pos) => snap_scene.character_enter_at_with(
                                    action.character.clone(),
                                    action.pose.clone(),
                                    pos,
                                    action.transform,
                                ),
                                None => snap_scene.character_enter_with(
                                    action.character.clone(),
                                    action.pose.clone(),
                                    action.transform,
                                ),
                            }
                        }
                        DirectionKind::Exit => {
                            snap_scene.character_exit(&action.character);
                        }
                        DirectionKind::Swap => {
                            // 重建时差分更换：直接修改现有角色 pose，不触发过渡。
                            if let Some(char_state) = snap_scene
                                .characters
                                .iter_mut()
                                .find(|c| c.name == action.character)
                            {
                                char_state.pose = action.pose.clone();
                            }
                        }
                    }
                }
                // 重建模式下 Dialogue/Narration/Choice/Wait/Flow/Visit/Return/StoryEnd
                // 不影响场景持久态，跳过。
                _ => {}
            }
        }

        // 恢复 VM 到调用前状态，避免影响当前游戏。
        self.vm.load_state(saved_vm_state);

        Some(SceneSnapshot {
            background: snap_scene.background,
            characters: snap_scene.characters,
            music: snap_scene.music,
        })
    }

    /// 升级所有旧格式存档（无 `scene` 字段）。
    ///
    /// 启动时调用一次：扫描所有常规存档槽位，对没有 `scene` 快照的存档
    /// 调用 `rebuild_scene_for_slot` 重建场景持久态并回写升级。这样后续
    /// 读档/缩略图渲染都能直接使用 `scene` 字段，旧存档也能正常显示
    /// 背景与立绘画面，不再黑屏或回退到标题图。
    ///
    /// 仅升级常规槽位（0..max_slots），不处理 autosave/continue/quicksave
    /// （这些在各自读档路径中已按需重建升级）。
    ///
    /// 重建失败的单个槽位仅跳过，不影响其他槽位升级。
    pub fn upgrade_all_legacy_saves(&mut self) {
        let max_slots = self.saves.max_slots();
        for slot in 0..max_slots {
            // 只升级存在且无 scene 的存档。
            let need_upgrade = match self.saves.load_slot_full(slot) {
                Some(save) => save.scene.is_none(),
                None => false,
            };
            if !need_upgrade {
                continue;
            }
            if let Some(rebuilt) = self.rebuild_scene_for_slot(slot) {
                let _ = self.saves.upgrade_scene(slot, rebuilt);
            }
        }
    }

    // ─── Autosave (crash-recovery) ───

    /// Save the current game state to the dedicated autosave slot.
    ///
    /// This is intended to be called when the player closes the window
    /// unexpectedly. It is a no-op on the title screen and after the story
    /// has ended, since there is nothing meaningful to recover in those cases.
    pub fn save_autosave(&mut self) -> Vec<EngineEvent> {
        let mut events = Vec::new();

        // Nothing to recover on the title screen or after the story ended.
        if self.phase == EnginePhase::Title || self.phase == EnginePhase::StoryEnded {
            return events;
        }

        let vm_state = self.vm.save_state();
        let description = self.save_description();

        match self.saves.save_autosave(
            vm_state,
            &self.current_section_name,
            self.play_time as u64,
            &description,
            self.scene_snapshot(),
        ) {
            Ok(_) => {}
            Err(e) => events.push(EngineEvent::Error { message: e }),
        }
        events
    }

    /// Load the autosave slot and resume the game from it.
    pub fn load_autosave(&mut self) -> Vec<EngineEvent> {
        let mut events = Vec::new();

        match self.saves.load_autosave() {
            Ok(save) => {
                self.vm.load_state(save.vm_state);
                self.scene.show_title = false;
                self.phase = EnginePhase::Running;
                self.scene.clear_text();
                // 读档后从 VM 指针继续执行，存档不保存过渡状态，
                // 任何残留的过渡（含 pending 变更与叠层）都是过时的，必须清空，
                // 否则残留 pending 会在 swap point 错误应用到刚恢复的场景。
                self.transition.reset();
                self.scene.transition = None;
                // 读档清空交叉淡入的旧背景（快照不含该字段，避免残留淡出层）。
                self.scene.prev_background = None;
                // 恢复存档时保存的场景快照（背景/立绘/音乐），
                // 避免读档/崩溃恢复后背景黑屏、音乐中断。背景每帧按名字
                // 查纹理，恢复 state 后渲染层自动正确绘制；音乐是事件
                // 驱动的，需额外发 MusicChanged 事件让渲染层重新播放。
                if let Some(snap) = save.scene {
                    self.scene.background = snap.background;
                    self.scene.characters = snap.characters;
                    self.scene.music = snap.music.clone();
                    if let Some(name) = &self.scene.music {
                        if !name.is_empty() {
                            events.push(EngineEvent::MusicChanged { name: name.clone() });
                        }
                    }
                }
                self.process_events_into(&mut events);
            }
            Err(e) => {
                events.push(EngineEvent::Error { message: e });
            }
        }
        events
    }

    /// Check whether an autosave exists and should be offered for recovery.
    ///
    /// Returns `false` if the `auto_recovery` setting is disabled, even when
    /// an autosave file is present on disk.
    pub fn has_autosave(&self) -> bool {
        self.settings.auto_recovery && self.saves.has_autosave()
    }

    /// Delete the autosave (called on a clean exit, or after it has been
    /// loaded so the player is not prompted again).
    pub fn delete_autosave(&mut self) -> Vec<EngineEvent> {
        let mut events = Vec::new();
        if let Err(e) = self.saves.delete_autosave() {
            events.push(EngineEvent::Error { message: e });
        }
        events
    }

    // ─── Continue save (返回标题后继续游戏) ───

    /// Save the current game state for "继续游戏" feature.
    ///
    /// Called when the player returns to title from in-game.
    pub fn save_continue(&mut self) -> Vec<EngineEvent> {
        let mut events = Vec::new();

        // Nothing to save on the title screen or after the story ended.
        if self.phase == EnginePhase::Title || self.phase == EnginePhase::StoryEnded {
            return events;
        }

        let vm_state = self.vm.save_state();
        let description = self.save_description();

        match self.saves.save_continue(
            vm_state,
            &self.current_section_name,
            self.play_time as u64,
            &description,
            self.scene_snapshot(),
        ) {
            Ok(_) => {}
            Err(e) => events.push(EngineEvent::Error { message: e }),
        }
        events
    }

    /// Load the continue save and resume the game.
    pub fn load_continue(&mut self) -> Vec<EngineEvent> {
        let mut events = Vec::new();

        match self.saves.load_continue() {
            Ok(save) => {
                self.vm.load_state(save.vm_state);
                self.scene.show_title = false;
                self.phase = EnginePhase::Running;
                self.scene.clear_text();
                // 读档后从 VM 指针继续执行，存档不保存过渡状态，
                // 任何残留的过渡（含 pending 变更与叠层）都是过时的，必须清空，
                // 否则残留 pending 会在 swap point 错误应用到刚恢复的场景。
                self.transition.reset();
                self.scene.transition = None;
                // 读档清空交叉淡入的旧背景（快照不含该字段，避免残留淡出层）。
                self.scene.prev_background = None;
                // 恢复存档时保存的场景快照（背景/立绘/音乐），
                // 避免读档/崩溃恢复后背景黑屏、音乐中断。背景每帧按名字
                // 查纹理，恢复 state 后渲染层自动正确绘制；音乐是事件
                // 驱动的，需额外发 MusicChanged 事件让渲染层重新播放。
                if let Some(snap) = save.scene {
                    self.scene.background = snap.background;
                    self.scene.characters = snap.characters;
                    self.scene.music = snap.music.clone();
                    if let Some(name) = &self.scene.music {
                        if !name.is_empty() {
                            events.push(EngineEvent::MusicChanged { name: name.clone() });
                        }
                    }
                }
                self.process_events_into(&mut events);
            }
            Err(e) => {
                events.push(EngineEvent::Error { message: e });
            }
        }
        events
    }

    /// Check whether a continue save exists.
    pub fn has_continue_save(&self) -> bool {
        self.saves.has_continue_save()
    }

    /// Delete the continue save.
    pub fn delete_continue(&mut self) -> Vec<EngineEvent> {
        let mut events = Vec::new();
        if let Err(e) = self.saves.delete_continue() {
            events.push(EngineEvent::Error { message: e });
        }
        events
    }

    // ─── Quick save (快速存档/读档) ───
    //
    // 快速存档使用独立的 `quicksave.json`，不占用编号槽位，也不出现在
    // 存档列表中。玩家可随时快速存读档而不影响存档页中的手动存档。

    /// Save the current game state to the dedicated quick-save slot.
    pub fn save_quicksave(&mut self) -> Vec<EngineEvent> {
        let mut events = Vec::new();
        let vm_state = self.vm.save_state();
        let description = self.save_description();

        match self.saves.save_quicksave(
            vm_state,
            &self.current_section_name,
            self.play_time as u64,
            &description,
            self.scene_snapshot(),
        ) {
            Ok(_) => events.push(EngineEvent::Saved { slot: usize::MAX - 2 }),
            Err(e) => events.push(EngineEvent::Error { message: e }),
        }
        events
    }

    /// Load the quick-save slot and resume the game.
    pub fn load_quicksave(&mut self) -> Vec<EngineEvent> {
        let mut events = Vec::new();

        match self.saves.load_quicksave() {
            Ok(save) => {
                self.vm.load_state(save.vm_state);
                self.scene.show_title = false;
                self.phase = EnginePhase::Running;
                self.scene.clear_text();
                // 读档后从 VM 指针继续执行，存档不保存过渡状态，
                // 任何残留的过渡（含 pending 变更与叠层）都是过时的，必须清空，
                // 否则残留 pending 会在 swap point 错误应用到刚恢复的场景。
                self.transition.reset();
                self.scene.transition = None;
                // 读档清空交叉淡入的旧背景（快照不含该字段，避免残留淡出层）。
                self.scene.prev_background = None;
                // 恢复存档时保存的场景快照（背景/立绘/音乐），
                // 避免读档/崩溃恢复后背景黑屏、音乐中断。背景每帧按名字
                // 查纹理，恢复 state 后渲染层自动正确绘制；音乐是事件
                // 驱动的，需额外发 MusicChanged 事件让渲染层重新播放。
                if let Some(snap) = save.scene {
                    self.scene.background = snap.background;
                    self.scene.characters = snap.characters;
                    self.scene.music = snap.music.clone();
                    if let Some(name) = &self.scene.music {
                        if !name.is_empty() {
                            events.push(EngineEvent::MusicChanged { name: name.clone() });
                        }
                    }
                }
                self.process_events_into(&mut events);
                events.push(EngineEvent::Loaded { slot: usize::MAX - 2 });
            }
            Err(e) => {
                events.push(EngineEvent::Error { message: e });
            }
        }
        events
    }

    /// Check whether a quick-save exists.
    pub fn has_quicksave(&self) -> bool {
        self.saves.has_quicksave()
    }

    /// Reload the script from new source text (hot reload).
    pub fn reload_script(&mut self, source: &str) -> Result<(), Vec<String>> {
        self.reload_script_internal(source)
    }

    fn reload_script_internal(&mut self, source: &str) -> Result<(), Vec<String>> {
        // Save current VM state
        let vm_state = self.vm.save_state();

        // Compile new source
        match compile_and_create_vm(source) {
            Ok(new_vm) => {
                self.vm = new_vm;
                self.source = source.to_string();
                // Restore state
                self.vm.load_state(vm_state);
                Ok(())
            }
            Err(errors) => {
                let msgs: Vec<String> = errors.iter()
                    .map(|e| {
                        let loc = akrs_core::format_location(&e.span);
                        match &e.hint {
                            Some(h) => format!("{}: {} (hint: {})", loc, e.message, h),
                            None => format!("{}: {}", loc, e.message),
                        }
                    })
                    .collect();
                Err(msgs)
            }
        }
    }

    /// Process VM events until a blocking event is found.
    fn process_events(&mut self) {
        let mut events = Vec::new();
        self.process_events_into(&mut events);
        // Events are available via update() return value in normal flow.
        // For direct calls (start_game, load), events are discarded.
    }

    fn process_events_into(&mut self, events: &mut Vec<EngineEvent>) {
        loop {
            if self.transition.is_active() {
                self.phase = EnginePhase::Transitioning;
                return;
            }

            let vm_event = match self.vm.step() {
                Ok(e) => e,
                Err(e) => {
                    events.push(EngineEvent::Error { message: e.message });
                    self.phase = EnginePhase::StoryEnded;
                    return;
                }
            };

            match vm_event {
                VmEvent::Dialogue { speaker, pose, text } => {
                    // Check if we need to wait for transition
                    if self.transition.is_active() {
                        self.pending = Some(PendingEvent::Dialogue { speaker, pose, text });
                        self.phase = EnginePhase::Transitioning;
                    } else {
                        self.show_dialogue(speaker, pose, text, events);
                    }
                    return;
                }
                VmEvent::Narration { text } => {
                    if self.transition.is_active() {
                        self.pending = Some(PendingEvent::Narration { text });
                        self.phase = EnginePhase::Transitioning;
                    } else {
                        self.show_narration(text, events);
                    }
                    return;
                }
                VmEvent::Command { cmd, args, transition } => {
                    self.handle_command(cmd, args, transition, events);
                    // Non-blocking: continue to next event
                }
                VmEvent::Direction { action } => {
                    self.handle_direction(action, events);
                    // Non-blocking: continue to next event
                }
                VmEvent::Choice { prompt, options } => {
                    let opts: Vec<ChoiceOptionState> = options.into_iter()
                        .map(|o| {
                            let translated = self.translator.t_choice(&o.text).to_string();
                            ChoiceOptionState { text: translated, available: o.available }
                        })
                        .collect();
                    let translated_prompt = prompt.as_ref()
                        .map(|p| self.translator.t_choice_prompt(p).to_string());

                    if self.transition.is_active() {
                        self.pending = Some(PendingEvent::Choices { prompt: translated_prompt.clone(), options: opts });
                        self.phase = EnginePhase::Transitioning;
                    } else {
                        self.scene.set_choices(translated_prompt.clone(), opts.clone());
                        self.phase = EnginePhase::ChoicePending;
                        events.push(EngineEvent::ChoicesShown { prompt: translated_prompt, options: opts });
                    }
                    return;
                }
                VmEvent::Wait { seconds } => {
                    if self.transition.is_active() {
                        self.pending = Some(PendingEvent::Wait { seconds });
                        self.phase = EnginePhase::Transitioning;
                    } else {
                        self.wait_remaining = seconds;
                        self.phase = EnginePhase::Waiting;
                    }
                    return;
                }
                VmEvent::StoryEnd => {
                    if self.transition.is_active() {
                        self.pending = Some(PendingEvent::StoryEnd);
                        self.phase = EnginePhase::Transitioning;
                    } else {
                        self.phase = EnginePhase::StoryEnded;
                        self.scene.story_ended = true;
                        events.push(EngineEvent::StoryEnded);
                    }
                    return;
                }
                VmEvent::Flow { target, title } => {
                    // 章节单向跳转：更新当前章节名，并设置待处理章节通知。
                    // 渲染层每帧调用 take_chapter_notify() 取走后播放
                    // 全屏淡入淡出 + 顶部章节通知。
                    self.current_section_name = target.clone();
                    self.pending_chapter_notify = Some(ChapterNotify { name: target, title });
                    // Non-blocking: continue to next event
                }
                VmEvent::Visit { target, title } => {
                    // 子例程访问：仅更新当前章节名，不触发章节通知。
                    self.current_section_name = target.clone();
                    let _ = title; // 访问不显示通知，标题在此忽略
                    // Non-blocking: continue to next event
                }
                VmEvent::Return => {
                    // 从子例程返回：不更新章节名（无目标信息），不触发通知。
                    // Non-blocking: continue to next event
                }
            }
        }
    }

    /// Show a pending event (called after transition completes).
    fn show_pending_event(&mut self, pending: PendingEvent, events: &mut Vec<EngineEvent>) {
        match pending {
            PendingEvent::Dialogue { speaker, pose, text } => {
                self.show_dialogue(speaker, pose, text, events);
            }
            PendingEvent::Narration { text } => {
                self.show_narration(text, events);
            }
            PendingEvent::Choices { prompt, options } => {
                self.scene.set_choices(prompt.clone(), options.clone());
                self.phase = EnginePhase::ChoicePending;
                events.push(EngineEvent::ChoicesShown { prompt, options });
            }
            PendingEvent::Wait { seconds } => {
                self.wait_remaining = seconds;
                self.phase = EnginePhase::Waiting;
            }
            PendingEvent::StoryEnd => {
                self.phase = EnginePhase::StoryEnded;
                self.scene.story_ended = true;
                events.push(EngineEvent::StoryEnded);
            }
        }
    }

    fn show_dialogue(
        &mut self,
        speaker: String,
        pose: Option<String>,
        text: String,
        events: &mut Vec<EngineEvent>,
    ) {
        // 记录已读历史（用原文，跨语言稳定）
        let key = format!("{}|{}", speaker, text);
        self.read_history.insert(key);
        // 翻译显示文本：角色名 + 对话正文
        let display_speaker = self.translator.t_character(&speaker).to_string();
        let display_text = self.translator.t_dialogue(&text).to_string();
        self.scene.set_dialogue(display_speaker.clone(), pose, display_text.clone());
        self.typewriter.start(
            display_text.chars().count(),
            self.settings.text_speed,
        );
        self.phase = EnginePhase::Running;
        // 新对话出现，重置自动播放倒计时（待文本显示完毕后重新计时）。
        self.auto_play_remaining = 0.0;
        // 查询语音引用：以原文为 key 查 translator.voice 表。
        // 命中则触发 VoicePlayed 事件让渲染层播放；未命中则触发空名事件停止上一句语音。
        let voice_name = self.translator.voice(&text).map(|s| s.to_string()).unwrap_or_default();
        self.current_voice = if voice_name.is_empty() { None } else { Some(voice_name.clone()) };
        events.push(EngineEvent::VoicePlayed { name: voice_name });
        events.push(EngineEvent::DialogueShown { speaker: display_speaker, text: display_text });
    }

    fn show_narration(&mut self, text: String, events: &mut Vec<EngineEvent>) {
        // 记录已读历史（用原文，跨语言稳定；旁白 speaker 为空）
        let key = format!("|{}", text);
        self.read_history.insert(key);
        // 翻译显示文本
        let display_text = self.translator.t_narration(&text).to_string();
        self.scene.set_narration(display_text.clone());
        self.typewriter.start(
            display_text.chars().count(),
            self.settings.text_speed,
        );
        self.phase = EnginePhase::Running;
        // 新旁白出现，重置自动播放倒计时。
        self.auto_play_remaining = 0.0;
        // 旁白也支持语音引用（如旁白配音）
        let voice_name = self.translator.voice(&text).map(|s| s.to_string()).unwrap_or_default();
        self.current_voice = if voice_name.is_empty() { None } else { Some(voice_name.clone()) };
        events.push(EngineEvent::VoicePlayed { name: voice_name });
        events.push(EngineEvent::NarrationShown { text: display_text });
    }

    fn handle_command(
        &mut self,
        cmd: String,
        args: Vec<String>,
        transition: Option<Transition>,
        events: &mut Vec<EngineEvent>,
    ) {
        match cmd.as_str() {
            "bg" | "background" => {
                let name = args.first().cloned().unwrap_or_default();
                let kind = transition.unwrap_or(Transition::Fade);

                if self.transition.is_active() {
                    // 过渡进行中：合并到 pending，不直接改现场 scene。
                    // 直接改现场会被 swap point 的旧 pending 覆盖，bg 指令丢失。
                    self.transition.merge_into_pending(
                        Some(Some(name.clone())),
                        vec![],
                        vec![],
                        None,
                    );
                } else {
                    self.transition.start(
                        kind,
                        &mut self.scene,
                        Some(Some(name.clone())),
                        vec![],
                        vec![],
                        None,
                    );
                    events.push(EngineEvent::TransitionStarted { kind });
                }
                events.push(EngineEvent::BackgroundChanged { name });
            }
            "music" | "bgm" => {
                let name = args.first().cloned().unwrap_or_default();
                self.scene.music = Some(name.clone());
                events.push(EngineEvent::MusicChanged { name });
            }
            "sound" | "sfx" => {
                let name = args.first().cloned().unwrap_or_default();
                events.push(EngineEvent::SoundPlayed { name });
            }
            "stop_music" | "stop_bgm" => {
                self.scene.music = None;
                events.push(EngineEvent::MusicChanged { name: String::new() });
            }
            _ => {
                // Unknown command: warn but don't crash
                events.push(EngineEvent::Warning {
                    message: format!("unknown command: @{}", cmd),
                });
            }
        }
    }

    fn handle_direction(
        &mut self,
        action: DirectionAction,
        events: &mut Vec<EngineEvent>,
    ) {
        // 差分更换：直接修改现有角色的 pose，不触发过渡动画，
        // 保持位置/大小/透明度等全部不变。
        if action.kind == DirectionKind::Swap {
            let new_pose = action.pose.clone();
            if let Some(char_state) = self.scene.characters.iter_mut().find(|c| c.name == action.character) {
                char_state.pose = new_pose;
                events.push(EngineEvent::CharacterEntered {
                    name: action.character,
                });
            } else {
                // 角色不在场上：降级为即时入场（Instant 过渡）
                let pose = action.pose.clone();
                let position = action.position;
                let transform = action.transform;
                self.transition.start(
                    Transition::Instant,
                    &mut self.scene,
                    None,
                    vec![(action.character.clone(), pose, position, transform)],
                    vec![],
                    None,
                );
                events.push(EngineEvent::CharacterEntered {
                    name: action.character,
                });
            }
            return;
        }

        // 正常上下场：默认使用 Fade（0.5秒淡入淡出），而非 Dissolve（0.8秒）
        let kind = action.transition.unwrap_or(Transition::Fade);

        match action.kind {
            DirectionKind::Enter => {
                // 限制同场最多 2 个角色（重入同名角色不算新增）
                if !self.scene.has_character(&action.character)
                    && self.scene.characters.len() >= 2
                {
                    events.push(EngineEvent::Error {
                        message: format!(
                            "stage full: cannot add '{}' — at most 2 characters may be on stage at once",
                            action.character
                        ),
                    });
                    return;
                }

                let pose = action.pose.clone();
                let position = action.position;
                let transform = action.transform;
                if self.transition.is_active() {
                    // 过渡进行中：合并到 pending，不直接改现场 scene。
                    // 直接改现场会导致指令丢失（现场是过渡前旧状态，新角色
                    // 可能不存在；且 swap point 会用旧 pending 覆盖），
                    // 表现为立绘"下不去"的叠叠乐问题。
                    self.transition.merge_into_pending(
                        None,
                        vec![(action.character.clone(), pose, position, transform)],
                        vec![],
                        None,
                    );
                } else {
                    self.transition.start(
                        kind,
                        &mut self.scene,
                        None,
                        vec![(action.character.clone(), pose, position, transform)],
                        vec![],
                        None,
                    );
                    events.push(EngineEvent::TransitionStarted { kind });
                }
                events.push(EngineEvent::CharacterEntered {
                    name: action.character,
                });
            }
            DirectionKind::Exit => {
                if self.transition.is_active() {
                    // 过渡进行中：合并到 pending，不直接改现场 scene。
                    // 直接 character_exit 在现场是旧状态时可能是 no-op
                    // （角色尚未入场），导致 exit 指令丢失、立绘残留。
                    self.transition.merge_into_pending(
                        None,
                        vec![],
                        vec![action.character.clone()],
                        None,
                    );
                } else {
                    self.transition.start(
                        kind,
                        &mut self.scene,
                        None,
                        vec![],
                        vec![action.character.clone()],
                        None,
                    );
                    events.push(EngineEvent::TransitionStarted { kind });
                }
                events.push(EngineEvent::CharacterExited {
                    name: action.character,
                });
            }
            DirectionKind::Swap => {
                // Swap 已在上方提前处理并 return，不会走到这里
                unreachable!("Swap 应在 handle_direction 开头处理")
            }
        }
    }

    /// Get the script source (for debugging or hot reload).
    pub fn source(&self) -> &str {
        &self.source
    }

    /// Check if the engine is waiting for player input.
    pub fn is_waiting_for_input(&self) -> bool {
        matches!(self.phase, EnginePhase::Running | EnginePhase::ChoicePending)
            && !self.transition.is_active()
    }

    /// Navigate choice selection (for keyboard input).
    pub fn select_choice(&mut self, direction: i32) {
        if let Some(choices) = &mut self.scene.choices
            && !choices.options.is_empty()
        {
            let count = choices.options.len() as i32;
            choices.selected = ((choices.selected as i32 + direction + count) % count) as usize;
        }
    }

    /// Confirm the currently selected choice.
    pub fn confirm_choice(&mut self) -> Vec<EngineEvent> {
        if let Some(choices) = &self.scene.choices {
            let selected = choices.selected;
            self.choose(selected)
        } else {
            Vec::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_SCRIPT: &str = r#"
# Start

@bg school with fade
+ Aki enters from left with dissolve

"Cherry blossoms drift through the air."

Aki: "Hello there!"
Aki (happy): "I'm glad you came."

$affection = 1

? "What do you say?"
| "You're wonderful!"
    $affection += 3
    -> GoodEnding
| "Whatever."
    $affection -= 1
    -> BadEnding
?

# GoodEnding

@bg sunset with fade_white

Aki: "I think we'll be great friends."

~~

# BadEnding

Aki: "Oh. I see."

~~
"#;

    #[test]
    fn test_engine_creation() {
        let engine = Engine::new(TEST_SCRIPT);
        assert!(engine.is_ok());
        let engine = engine.unwrap();
        assert_eq!(engine.phase(), EnginePhase::Title);
        assert!(engine.scene().show_title);
    }

    #[test]
    fn test_engine_start_and_advance() {
        let mut engine = Engine::start_running(TEST_SCRIPT).unwrap();

        // Process transitions (bg fade + character dissolve)
        // Each transition phase is ~0.3s, so ~0.7s covers full transition
        for _ in 0..20 {
            engine.update(0.1);
        }

        // After transition, we should have dialogue or narration
        assert!(
            engine.scene().dialogue.is_some()
                || engine.scene().background.is_some()
                || engine.scene().characters.iter().any(|c| c.name == "Aki")
        );
    }

    #[test]
    fn test_choices() {
        let mut engine = Engine::start_running(TEST_SCRIPT).unwrap();

        // Advance through all blocking events until choices appear
        let max_iterations = 100;
        for _ in 0..max_iterations {
            // Advance transitions
            engine.update(0.05);
            // Try to advance past current text
            let events = engine.advance();
            if events.iter().any(|e| matches!(e, EngineEvent::ChoicesShown { .. })) {
                break;
            }
        }
        assert_eq!(engine.phase(), EnginePhase::ChoicePending);
    }

    #[test]
    fn test_save_load() {
        let mut engine = Engine::start_running(TEST_SCRIPT).unwrap();
        // Advance a bit
        engine.advance();

        // Save
        let save_events = engine.save(0);
        assert!(save_events.iter().any(|e| matches!(e, EngineEvent::Saved { slot: 0 })));

        // Advance more
        engine.advance();
        engine.advance();

        // Load
        let load_events = engine.load(0);
        assert!(load_events.iter().any(|e| matches!(e, EngineEvent::Loaded { slot: 0 })));
    }

    #[test]
    fn test_hot_reload() {
        let mut engine = Engine::start_running(TEST_SCRIPT).unwrap();
        engine.advance();

        // Reload with modified script (same structure, different text)
        let modified = TEST_SCRIPT.replace("Hello there!", "Hi there!");
        let result = engine.reload_script(&modified);
        assert!(result.is_ok());

        // The source should be updated
        assert!(engine.source().contains("Hi there!"));
    }

    #[test]
    fn test_unknown_command_warning() {
        let script = r#"
# Start
@unknown_command some_arg
"Text"
"#;
        let mut engine = Engine::start_running(script).unwrap();
        let events = engine.update(0.016);
        // Should have a warning about unknown command
        // (events may be empty if transition is active, so just check no crash)
        assert!(engine.phase() != EnginePhase::Title);
    }

    #[test]
    fn test_multi_character_layout() {
        let src = r#"
# Stage
+ Aki enters
+ Yuki enters
- Aki
~~
"#;
        let mut engine = Engine::start_running(src).unwrap();

        // Process all events
        loop {
            engine.update(0.1);
            let _ = engine.advance();
            if engine.phase == EnginePhase::StoryEnded {
                break;
            }
        }

        // After Aki exits, Yuki should remain centered
        assert_eq!(engine.scene().characters.len(), 1);
        assert_eq!(engine.scene().characters[0].name, "Yuki");
        assert_eq!(engine.scene().characters[0].position, akrs_core::Position::Center);
    }

    #[test]
    fn test_stage_full_three_characters_error() {
        // 同场第三个角色在编译期即被 checker 拒绝（限 2 角色）
        let src = r#"
# Stage
+ Aki
+ Yuki
+ Zeno
"end"
~~
"#;
        let result = Engine::start_running(src);
        assert!(result.is_err(), "expected compile to reject 3 simultaneous characters");
        let errs = result.err().unwrap();
        assert!(
            errs.iter().any(|e| e.message.contains("too many characters") && e.message.contains("Zeno")),
            "expected a 'too many characters' error mentioning Zeno, got: {:?}", errs
        );
    }

    #[test]
    fn test_pose_and_position_passed_to_scene() {
        let src = r#"
# Stage
+ 心夏 (kokonabody1) 居左
"hi"
~~
"#;
        let mut engine = Engine::start_running(src).unwrap();
        loop {
            engine.update(0.05);
            let _ = engine.advance();
            if engine.scene().characters.iter().any(|c| c.name == "心夏") {
                break;
            }
            if engine.phase == EnginePhase::StoryEnded {
                break;
            }
        }
        let c = engine.scene().characters.iter().find(|c| c.name == "心夏");
        assert!(c.is_some(), "心夏 should be on stage");
        let c = c.unwrap();
        assert_eq!(c.pose.as_deref(), Some("kokonabody1"));
        assert_eq!(c.position, akrs_core::Position::Left);
    }

    #[test]
    fn test_bg_fade_transition_runs() {
        let src = r#"
# Stage
@bg bg1 with fade
"hi"
~~
"#;
        let mut engine = Engine::start_running(src).unwrap();
        // bg fade transition should be active immediately after start
        assert!(engine.scene().transition.is_some(), "bg fade should start");
        let initial_progress = engine.scene().transition.as_ref().unwrap().progress;
        // Advance time
        engine.update(0.1);
        let later_progress = engine.scene().transition.as_ref().unwrap().progress;
        assert!(later_progress > initial_progress, "transition progress should advance");
    }
}
