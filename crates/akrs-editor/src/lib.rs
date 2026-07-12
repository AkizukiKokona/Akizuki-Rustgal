//! Akizuki*Rustgal 剧本编辑器
//!
//! 基于 `iced 0.13` 构建的视觉小说剧本编辑器。提供三栏布局：
//!
//! - **左栏**：工作目录中的 `.akrs` 文件列表，支持新建 / 打开 / 保存。
//! - **中栏**：多行脚本编辑器（`iced::widget::text_editor`）。
//! - **右栏**：由 `akrs_runtime::Engine` 驱动的实时预览。
//! - **顶部工具栏**：新建 / 打开 / 保存 / 运行等操作。
//! - **底部状态栏**：编译诊断信息（错误 / 警告 / 提示）。
//!
//! 本期由 eframe/egui 迁移而来：保留全部数据结构与纯逻辑，仅重写 UI 层。
//! 多行编辑器使用 iced 的 `text_editor`（`Content` 持有文本），本期不做语法高亮。

#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::io::Read;
use std::time::{Duration, Instant};

use iced::advanced::text::highlighter::Format as HighlightFormat;
use iced::keyboard::{self, Key, Modifiers};
use iced::widget::{
    button, column, container, horizontal_space, image, opaque, pick_list, row, scrollable,
    slider, stack, text, text_editor, text_input, Column, Space,
};
use iced::{
    Alignment, Background, Border, Color, Element, Font, Length, Padding, Shadow,
    Subscription, Task, Theme,
};

// iced_aw 附加组件：`Card` 统一弹窗卡片，`Tabs`/`TabLabel` 用于右栏预览标签页。
use iced_aw::widget::{Card, TabLabel, Tabs};

use akrs_core::{
    compile, format_location, CompileError, DismissedWarnings, ErrSeverity, ProjectConfig,
    RecentProjects,
};
use akrs_runtime::{Engine, EnginePhase};

// ---------------------------------------------------------------------------
// 子模块：纯逻辑/数据/常量已拆分到独立文件，便于维护与测试。
// ---------------------------------------------------------------------------

/// 语法高亮调色板（不透明 RGB，源自规格文档的 RGBA 值）。
mod palette;
/// 编辑器常量与模板（字号、章节标题上限、示例剧本、build 号等）。
mod templates;
/// `=>`/`<=` 流程标记扫描与配对算法（纯逻辑，含单元测试）。
mod flow;
/// 文件系统与平台工具（sanitize/复制目录/打开文件管理器/检查 cargo）。
mod fs_util;
/// 剧本行解析与标签（立绘行/指令行解析、阶段标签、错误格式化）。
mod parse;
/// Ren'Py `.rpy` → `.akrs` 转换。
mod rpy;
/// 蓝图节点编辑器状态（纯数据 + 算法，不含 UI）。
mod state;

// 引入调色板与模板常量：颜色、字号、示例剧本、GitHub 链接、build 号等。
use palette::*;
use templates::*;

/// Rust 官网链接（cargo 引导弹窗的「打开 rust-lang.org」按钮使用）。
/// 仅编辑器使用，不属于通用模板，故保留在本文件。
const RUST_LANG_URL: &str = "https://rust-lang.org";

// ---------------------------------------------------------------------------
// 语法高亮器（iced::advanced::text::Highlighter trait 实现）
// ---------------------------------------------------------------------------

/// `.akrs` 脚本语法高亮设置（空结构体，复用全局调色板）。
#[derive(Debug, Clone, PartialEq)]
struct AkrsHighlightSettings;

/// `.akrs` 脚本语法高亮器：按行首 token 选择基础颜色，
/// 同时对 `"..."` 字符串字面量和 `//` 注释做分段着色。
/// 复用已有的 `line_base_color` 纯逻辑函数。
struct AkrsHighlighter {
    /// 当前高亮到的行号（text_editor 要求跟踪）。
    current_line: usize,
}

/// 高亮输出：一个颜色值（对应 `HighlightFormat` 的 `color` 字段）。
#[derive(Debug, Clone, Copy)]
struct AkrsHighlight(Color);

impl iced::advanced::text::Highlighter for AkrsHighlighter {
    type Settings = AkrsHighlightSettings;
    type Highlight = AkrsHighlight;
    type Iterator<'a> = std::vec::IntoIter<(std::ops::Range<usize>, Self::Highlight)>;

    fn new(_settings: &Self::Settings) -> Self {
        Self { current_line: 0 }
    }

    fn update(&mut self, _new_settings: &Self::Settings) {}

    fn change_line(&mut self, line: usize) {
        self.current_line = line;
    }

    fn highlight_line(&mut self, line: &str) -> Self::Iterator<'_> {
        let trimmed = line.trim_start();
        let base = line_base_color(trimmed);
        let leading_ws = line.len() - trimmed.len();

        // 收集高亮分片：(字节范围, 颜色)
        let mut spans: Vec<(std::ops::Range<usize>, AkrsHighlight)> = Vec::new();

        // 注释：`//` 后整行着色为注释色（优先于其他规则）
        if let Some(pos) = line.find("//") {
            // 注释前的部分用基础色
            if pos > 0 {
                spans.push((0..pos, AkrsHighlight(base)));
            }
            spans.push((pos..line.len(), AkrsHighlight(COLOR_COMMENT)));
            return spans.into_iter();
        }

        // 字符串字面量：扫描所有 "..." 区间，着色为字符串色
        let mut in_string = false;
        let mut string_start = 0usize;
        let bytes = line.as_bytes();
        let mut i = 0usize;
        let mut last_end = 0usize;
        while i < bytes.len() {
            if bytes[i] == b'"' {
                if !in_string {
                    // 字符串开始前的部分用基础色
                    if i > last_end {
                        spans.push((last_end..i, AkrsHighlight(base)));
                    }
                    string_start = i;
                    in_string = true;
                } else {
                    // 字符串结束（含闭合引号）
                    spans.push((string_start..i + 1, AkrsHighlight(COLOR_STRING)));
                    last_end = i + 1;
                    in_string = false;
                }
            }
            i += 1;
        }
        // 行末剩余部分
        if last_end < line.len() {
            let color = if in_string { COLOR_STRING } else { base };
            spans.push((last_end..line.len(), AkrsHighlight(color)));
        }

        // 如果没有任何分片（空行），返回空迭代器
        let _ = leading_ws;
        spans.into_iter()
    }

    fn current_line(&self) -> usize {
        self.current_line
    }
}

/// 将 `AkrsHighlight` 转换为 iced 渲染器需要的 `Format<Font>`。
fn akrs_highlight_to_format(h: &AkrsHighlight, _theme: &Theme) -> HighlightFormat<Font> {
    HighlightFormat {
        color: Some(h.0),
        font: None,
    }
}

// ---------------------------------------------------------------------------
// 立绘预览
// ---------------------------------------------------------------------------

/// 右栏预览的标签页。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PreviewTab {
    /// 剧本运行预览。
    Script,
    /// 立绘摆放预览（无需启动游戏进程）。
    Sprite,
    /// 背景预览。
    Background,
    /// 音乐预览。
    Music,
    /// 大纲视图：按缩进展示章节、分支选项与 `=>`/`<=` 配对结构。
    Outline,
}

impl std::fmt::Display for PreviewTab {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

impl PreviewTab {
    fn label(&self) -> &'static str {
        match self {
            Self::Script => "剧本预览",
            Self::Sprite => "立绘",
            Self::Background => "背景",
            Self::Music => "音乐",
            Self::Outline => "大纲",
        }
    }

    const ALL: [Self; 5] = [
        Self::Script,
        Self::Sprite,
        Self::Background,
        Self::Music,
        Self::Outline,
    ];
}

/// 立绘预览状态：允许作者在不启动游戏的情况下调整立绘位置与大小，
/// 并生成对应的 `.akrs` 语法。
///
/// 位置采用百分比坐标（0.0–1.0）以适配多分辨率，与运行时渲染逻辑一致。
struct SpritePreview {
    /// 角色名（用于生成 `+ 角色 ...` 语法，为空时使用立绘资源名）。
    character_name: String,
    /// 当前选中的立绘资源名（不含扩展名，对应 `assets/characters/{name}.png`）。
    selected: String,
    /// 水平位置百分比（0.0=最左，1.0=最右），默认 0.5（居中）。
    x_percent: f32,
    /// 垂直位置百分比（0.0=最上，1.0=底部站立），默认 1.0。
    y_percent: f32,
    /// 大小倍数，默认 1.0。
    scale: f32,
    /// 已加载的纹理缓存（按立绘名称索引，避免每帧重新解码）。
    textures: HashMap<String, iced::widget::image::Handle>,
    /// `assets/characters/` 中可用的立绘列表（不含扩展名）。
    available: Vec<String>,
    /// 已扫描的立绘目录（用于检测变更后重新扫描）。
    scanned_dir: Option<PathBuf>,
    /// 最近一次加载错误信息。
    load_error: Option<String>,
}

impl Default for SpritePreview {
    fn default() -> Self {
        Self {
            character_name: String::new(),
            selected: String::new(),
            x_percent: 0.5,
            y_percent: 1.0,
            scale: 1.0,
            textures: HashMap::new(),
            available: Vec::new(),
            scanned_dir: None,
            load_error: None,
        }
    }
}

// ---------------------------------------------------------------------------
// 背景预览
// ---------------------------------------------------------------------------

/// 背景预览状态：允许作者选择背景图片，预览效果，并生成对应的 `.akrs` 语法。
struct BackgroundPreview {
    /// 当前选中的背景资源名（不含扩展名，对应 `assets/bg/{name}.png`）。
    selected: String,
    /// 已加载的纹理缓存（按背景名称索引）。
    textures: HashMap<String, iced::widget::image::Handle>,
    /// `assets/bg/` 中可用的背景列表（不含扩展名）。
    available: Vec<String>,
    /// 已扫描的背景目录（用于检测变更后重新扫描）。
    scanned_dir: Option<PathBuf>,
    /// 最近一次加载错误信息。
    load_error: Option<String>,
    /// 过渡效果选择。
    transition: String,
}

impl Default for BackgroundPreview {
    fn default() -> Self {
        Self {
            selected: String::new(),
            textures: HashMap::new(),
            available: Vec::new(),
            scanned_dir: None,
            load_error: None,
            transition: "fade".to_string(),
        }
    }
}

// ---------------------------------------------------------------------------
// 音乐预览
// ---------------------------------------------------------------------------

/// 音乐预览状态：允许作者选择音乐文件，并生成对应的 `.akrs` 语法。
struct MusicPreview {
    /// 当前选中的音乐资源名（不含扩展名，对应 `assets/music/{name}.ogg/mp3`）。
    selected: String,
    /// `assets/music/` 中可用的音乐列表（不含扩展名）。
    available: Vec<String>,
    /// 已扫描的音乐目录（用于检测变更后重新扫描）。
    scanned_dir: Option<PathBuf>,
}

impl Default for MusicPreview {
    fn default() -> Self {
        Self {
            selected: String::new(),
            available: Vec::new(),
            scanned_dir: None,
        }
    }
}

// ---------------------------------------------------------------------------
// 编辑器应用状态
// ---------------------------------------------------------------------------

/// 编辑器的 iced 应用主体。
pub struct EditorApp {
    /// 中栏编辑器中显示的当前脚本文本（由 `text_editor::Content` 持有）。
    editor_content: text_editor::Content,
    /// 当前加载/保存的文件路径（未保存时为 `None`）。
    current_file: Option<PathBuf>,
    /// 扫描文件列表和保存/打开使用的工作目录。
    work_dir: PathBuf,
    /// 左栏中编辑的文件名输入。
    file_name_input: String,
    /// `work_dir` 中 `.akrs` 文件的缓存列表。
    file_list: Vec<String>,
    /// 运行中的预览引擎（由「运行」创建）。
    engine: Option<Engine>,
    /// 底部显示的可读状态信息。
    status: String,
    /// 格式化后的编译诊断信息（错误 / 警告 / 提示）。
    diagnostics: Vec<String>,
    /// 上一次 tick 的时刻，用于计算引擎的增量时间 dt。
    last_time: Option<Instant>,
    /// 是否显示首次启动欢迎面板。
    show_welcome: bool,
    /// 是否显示「关于」对话框。
    show_about: bool,
    /// 右栏预览的当前标签页。
    preview_tab: PreviewTab,
    /// 立绘预览状态。
    sprite_preview: SpritePreview,
    /// 背景预览状态。
    bg_preview: BackgroundPreview,
    /// 音乐预览状态。
    music_preview: MusicPreview,
    /// 是否显示放大预览弹窗。
    show_enlarged_preview: bool,
    /// 是否显示查找替换对话框。
    show_find_replace_dialog: bool,
    /// 查找替换对话框中的查找内容。
    find_replace_target: String,
    /// 查找替换对话框中的替换内容。
    find_replace_replacement: String,
    /// 查找替换对话框中待插入的语法（缓存）。
    find_replace_syntax: String,
    /// 文件选择对话框状态（None 表示未打开）。
    file_picker: Option<FilePickerState>,
    /// 当前项目配置（project.json）。
    project_config: ProjectConfig,
    /// 是否已经加载了项目配置。
    project_loaded: bool,
    /// 项目根目录（open_project 时记录，预览子进程的 CWD 用此而非 work_dir）。
    /// work_dir 会被 open_file_path 覆盖为「打开文件的父目录」，但预览子进程
    /// 必须以项目根为 CWD，否则 saves/*.json、project.json、assets/ 读取错误。
    project_dir: Option<PathBuf>,
    /// Ctrl 键当前是否按下（用于 Ctrl+点击剧本跳转预览）。
    ctrl_held: bool,
    /// 是否显示「请先保存再预览」提示弹窗。
    show_save_reminder: bool,
    /// 是否显示项目警告弹窗（project.json 的 warning 字段）。
    show_project_warning: bool,
    /// 最近打开的项目列表。
    recent_projects: RecentProjects,
    /// 是否显示项目设置对话框。
    show_project_settings: bool,
    /// 标题过长警告对话框状态。
    title_warning: Option<TitleWarningState>,
    /// 目录选择对话框状态（选择项目文件夹）。
    dir_picker: Option<DirPickerState>,
    /// 打包状态。
    build: BuildState,
    /// 游戏预览子进程（None 表示无预览运行）。
    game_process: Option<Child>,
    /// 是否显示 cargo 未安装引导。
    show_cargo_guide: bool,
    /// 对照翻译模式：开启后中央面板显示原文-译文对照视图。
    translation_mode: bool,
    /// 当前编辑的目标语言代码。
    translation_target_lang: String,
    /// 当前加载的翻译文件内容（内存中编辑，保存时写盘）。
    translation_file: Option<akrs_runtime::Translator>,
    /// 可翻译行列表（从剧本解析得到），每项为 (类型, 原文)。
    translatable_lines: Vec<TranslatableLine>,
    /// 是否显示快捷键帮助窗口。
    show_shortcuts: bool,
    /// 是否显示 rpy 导入窗口。
    show_rpy_import: bool,
    /// rpy 导入：选中的源文件路径。
    rpy_import_source: Option<PathBuf>,
    /// rpy 导入：目标保存路径。
    rpy_import_target: PathBuf,
    /// rpy 导入：转换警告信息。
    rpy_import_warnings: Vec<String>,
}

/// 可翻译行的类型。
#[derive(Debug, Clone, PartialEq)]
enum TranslatableKind {
    Section,
    Dialogue,
    Narration,
    Choice,
    ChoicePrompt,
    Character,
}

/// 一条可翻译条目。
#[derive(Debug, Clone)]
struct TranslatableLine {
    kind: TranslatableKind,
    original: String,
    line_number: usize,
}

/// 标题过长警告对话框状态。
#[derive(Clone)]
struct TitleWarningState {
    /// 待确认的新标题。
    new_title: String,
    /// 待确认的新副标题。
    new_subtitle: String,
}

/// 目录选择对话框状态。
struct DirPickerState {
    current_dir: PathBuf,
    entries: Vec<PickerEntry>,
    filter: String,
}

/// 文件选择对话框模式。
#[derive(Debug, Clone, Copy, PartialEq)]
#[allow(dead_code)]
enum FilePickerMode {
    Open,
}

/// 文件选择对话框状态。
#[allow(dead_code)]
struct FilePickerState {
    mode: FilePickerMode,
    current_dir: PathBuf,
    entries: Vec<PickerEntry>,
    selected: Option<String>,
    filter: String,
}

#[derive(Clone)]
struct PickerEntry {
    name: String,
    is_dir: bool,
}

// ---------------------------------------------------------------------------
// 打包与预览状态
// ---------------------------------------------------------------------------

/// 打包平台选项。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum BuildPlatform {
    Windows,
    Linux,
    MacOS,
}

impl BuildPlatform {
    fn label(&self) -> &'static str {
        match self {
            Self::Windows => "Windows (.exe)",
            Self::Linux => "Linux",
            Self::MacOS => "macOS",
        }
    }

    fn target(&self) -> &'static str {
        match self {
            Self::Windows => "x86_64-pc-windows-gnu",
            Self::Linux => "x86_64-unknown-linux-gnu",
            Self::MacOS => "x86_64-apple-darwin",
        }
    }

    fn all() -> [Self; 3] {
        [Self::Windows, Self::Linux, Self::MacOS]
    }
}

/// 打包状态。
struct BuildState {
    /// 是否显示打包对话框。
    show: bool,
    /// 各平台是否被勾选。
    selected: HashMap<BuildPlatform, bool>,
    /// 构建日志输出。
    log: String,
    /// 当前正在构建的平台（None 表示空闲）。
    building: Option<BuildPlatform>,
    /// 待构建的平台队列。
    queue: Vec<BuildPlatform>,
    /// 构建子进程。
    process: Option<Child>,
    /// 构建是否全部完成。
    done: bool,
    /// 导出目录。
    output_dir: PathBuf,
    /// 构建成功的平台列表。
    succeeded: Vec<BuildPlatform>,
    /// 构建失败的平台及其错误信息。
    failed: Vec<(BuildPlatform, String)>,
    /// 脚本快照目录（打包期间锁定脚本内容）。
    snapshot_dir: Option<PathBuf>,
    /// 日志是否跟随底部（用户上滚时暂停）。
    log_follow: bool,
}

impl Default for BuildState {
    fn default() -> Self {
        let mut selected = HashMap::new();
        // 默认勾选当前平台
        let current = if cfg!(target_os = "windows") {
            BuildPlatform::Windows
        } else if cfg!(target_os = "macos") {
            BuildPlatform::MacOS
        } else {
            BuildPlatform::Linux
        };
        selected.insert(current, true);
        Self {
            show: false,
            selected,
            log: String::new(),
            building: None,
            queue: Vec::new(),
            process: None,
            done: false,
            output_dir: PathBuf::from("build"),
            succeeded: Vec::new(),
            failed: Vec::new(),
            snapshot_dir: None,
            log_follow: true,
        }
    }
}

impl BuildState {
    /// 检查是否正在构建中。
    fn is_building(&self) -> bool {
        self.building.is_some() || !self.queue.is_empty()
    }

    /// 追加一行日志。
    fn log_line(&mut self, msg: impl AsRef<str>) {
        self.log.push_str(msg.as_ref());
        if !msg.as_ref().ends_with('\n') {
            self.log.push('\n');
        }
    }
}

impl Default for EditorApp {
    fn default() -> Self {
        let work_dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let recent_projects = RecentProjects::load();
        let mut app = Self {
            editor_content: text_editor::Content::new(),
            current_file: None,
            work_dir,
            file_name_input: "untitled.akrs".to_string(),
            file_list: Vec::new(),
            engine: None,
            status: "就绪".to_string(),
            diagnostics: Vec::new(),
            last_time: None,
            show_welcome: true,
            show_about: false,
            preview_tab: PreviewTab::Script,
            sprite_preview: SpritePreview::default(),
            bg_preview: BackgroundPreview::default(),
            music_preview: MusicPreview::default(),
            show_enlarged_preview: false,
            show_find_replace_dialog: false,
            find_replace_target: String::new(),
            find_replace_replacement: String::new(),
            find_replace_syntax: String::new(),
            file_picker: None,
            project_config: ProjectConfig::default(),
            project_loaded: false,
            project_dir: None,
            ctrl_held: false,
            show_save_reminder: false,
            show_project_warning: false,
            recent_projects,
            show_project_settings: false,
            title_warning: None,
            dir_picker: None,
            build: BuildState::default(),
            game_process: None,
            show_cargo_guide: false,
            translation_mode: false,
            translation_target_lang: "en-US".to_string(),
            translation_file: None,
            translatable_lines: Vec::new(),
            show_shortcuts: false,
            show_rpy_import: false,
            rpy_import_source: None,
            rpy_import_target: PathBuf::new(),
            rpy_import_warnings: Vec::new(),
        };
        app.refresh_file_list();
        app
    }
}

impl EditorApp {
    // -- 辅助方法：插入语法 -----------------------------------------------

    /// 智能插入语法：本期不接入系统剪贴板（iced 无直接读取剪贴板的同步 API），
    /// 统一将语法追加到脚本末尾。保留方法名与签名以便后续接入剪贴板。
    fn smart_insert_syntax(&mut self, syntax: &str) {
        let mut s = self.editor_content.text();
        s.push_str(&format!("{}\n", syntax));
        self.editor_content = text_editor::Content::with_text(&s);
        self.status = "语法已追加到脚本末尾".to_string();
    }

    // -- 辅助方法：查找并替换 -----------------------------------------------

    /// 查找脚本中匹配 target 的内容，替换为 replacement。
    /// 返回是否成功替换。
    fn find_and_replace(&mut self, target: &str, replacement: &str) -> bool {
        if target.is_empty() {
            return false;
        }
        let current = self.editor_content.text();
        if !current.contains(target) {
            return false;
        }
        let replaced = current.replace(target, replacement);
        self.editor_content = text_editor::Content::with_text(&replaced);
        true
    }

    /// 统计脚本中匹配 target 的次数（用于查找替换弹窗的命中数显示）。
    fn find_count(&self, target: &str) -> usize {
        if target.is_empty() {
            return 0;
        }
        self.editor_content.text().matches(target).count()
    }

    // -- 文件操作（全部可失败，不 panic）-----------------------------------

    /// 重新扫描 `work_dir` 中的 `.akrs` 文件。
    fn refresh_file_list(&mut self) {
        self.file_list.clear();
        if let Ok(entries) = std::fs::read_dir(&self.work_dir) {
            let mut files: Vec<String> = entries
                .filter_map(|e| e.ok())
                .filter_map(|e| {
                    let p = e.path();
                    if p.extension().is_some_and(|ext| ext == "akrs") {
                        p.file_name().map(|n| n.to_string_lossy().into_owned())
                    } else {
                        None
                    }
                })
                .collect();
            files.sort();
            self.file_list = files;
        }
    }

    /// 从最小模板新建（未保存的）脚本。
    fn new_file(&mut self) {
        self.editor_content = text_editor::Content::with_text(NEW_TEMPLATE);
        self.current_file = None;
        self.file_name_input = "untitled.akrs".to_string();
        self.engine = None;
        self.diagnostics.clear();
        self.show_welcome = false;
        self.status = "新文件（未保存）".to_string();
    }

    /// 打开文件选择对话框。
    fn open_file_picker(&mut self, mode: FilePickerMode) {
        let entries = Self::read_picker_entries(&self.work_dir, "");
        self.file_picker = Some(FilePickerState {
            mode,
            current_dir: self.work_dir.clone(),
            entries,
            selected: None,
            filter: String::new(),
        });
    }

    /// 读取目录中的文件选择项。
    fn read_picker_entries(dir: &Path, filter: &str) -> Vec<PickerEntry> {
        let mut entries: Vec<PickerEntry> = Vec::new();
        if let Ok(read_dir) = std::fs::read_dir(dir) {
            for entry in read_dir.flatten() {
                let path = entry.path();
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                if name.is_empty() {
                    continue;
                }
                let is_dir = path.is_dir();
                let filter_lower = filter.to_lowercase();
                let name_lower = name.to_lowercase();
                if !filter_lower.is_empty() && !name_lower.contains(&filter_lower) {
                    continue;
                }
                entries.push(PickerEntry { name, is_dir });
            }
        }
        // 目录在前，文件在后，各按名称排序
        entries.sort_by(|a, b| {
            if a.is_dir && !b.is_dir {
                std::cmp::Ordering::Less
            } else if !a.is_dir && b.is_dir {
                std::cmp::Ordering::Greater
            } else {
                a.name.to_lowercase().cmp(&b.name.to_lowercase())
            }
        });
        entries
    }

    /// 从完整路径打开文件。
    fn open_file_path(&mut self, path: &std::path::Path) {
        match std::fs::read_to_string(path) {
            Ok(content) => {
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "unknown.akrs".to_string());
                self.editor_content = text_editor::Content::with_text(&content);
                self.current_file = Some(path.to_path_buf());
                self.file_name_input = name.clone();
                // 如果文件在 work_dir 下，刷新文件列表并切换工作目录到文件所在目录
                if let Some(parent) = path.parent() {
                    self.work_dir = parent.to_path_buf();
                    self.refresh_file_list();
                }
                // 推断 project_dir：若尚未加载项目，从文件所在目录向上查找
                // 含 project.json 或 assets/ 的祖先目录作为项目根。
                // 这样打开单个剧本文件时资源预览也能正确读到 assets/。
                if self.project_dir.is_none() {
                    if let Some(root) = infer_project_root(path) {
                        self.project_dir = Some(root);
                    }
                }
                self.engine = None;
                self.diagnostics.clear();
                self.show_welcome = false;
                self.status = format!("已打开 {}", name);
            }
            Err(e) => {
                self.status = format!("打开失败：{}", e);
            }
        }
    }

    /// 从 `work_dir` 打开 `name` 到编辑器。
    fn open_file(&mut self, name: &str) {
        let path = self.work_dir.join(name);
        self.open_file_path(&path);
    }

    /// 打开 `file_name_input` 中当前输入的文件名。
    fn open_current_name(&mut self) {
        let name = sanitize_filename(&self.file_name_input);
        self.open_file(&name);
    }

    /// 加载内置示例剧本。
    fn load_sample(&mut self) {
        self.editor_content = text_editor::Content::with_text(SAMPLE_SCRIPT);
        self.current_file = None;
        self.file_name_input = "sample.akrs".to_string();
        self.engine = None;
        self.diagnostics.clear();
        self.show_welcome = false;
        self.status = "已加载示例剧本".to_string();
    }

    /// 将编辑器内容保存到 `work_dir/file_name_input`。
    fn save_file(&mut self) {
        let name = sanitize_filename(&self.file_name_input);
        let path = self.work_dir.join(&name);
        let content = self.editor_content.text();
        match std::fs::write(&path, &content) {
            Ok(()) => {
                self.current_file = Some(path);
                self.file_name_input = name.clone();
                self.status = format!("已保存 {}", name);
                // main_script 一致性检查：若保存的文件名与 project.json 的
                // main_script 不一致，状态栏追加提示（非阻断）。
                if self.project_loaded && name != self.project_config.main_script {
                    self.status.push_str(&format!(
                        " ｜ 提示：当前文件不是 project.json 的 main_script（{}），直接启动游戏将运行 {}；编辑器预览不受影响",
                        self.project_config.main_script, self.project_config.main_script
                    ));
                }
                self.refresh_file_list();
            }
            Err(e) => {
                self.status = format!("保存失败：{} - {}", name, e);
            }
        }
    }

    // -- 项目管理 ---------------------------------------------------------

    /// 打开项目文件夹。
    fn open_project(&mut self, project_dir: &Path) {
        // 加载项目配置
        self.project_config = ProjectConfig::load(project_dir);
        self.project_loaded = true;
        self.project_dir = Some(project_dir.to_path_buf());
        self.work_dir = project_dir.to_path_buf();
        self.refresh_file_list();

        // 项目警告弹窗：project.json 的 warning 字段非空且未被本地忽略时弹出。
        // canonical_key 由 DismissedWarnings 内部用规范化路径计算。
        self.show_project_warning = !self.project_config.warning.is_empty()
            && !DismissedWarnings::load().is_dismissed(project_dir);

        // 添加到最近项目列表
        let project_name = project_dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "未命名项目".to_string());
        self.recent_projects.add_project(project_dir, &project_name);

        // 尝试打开主剧本文件
        let main_script = self.project_config.main_script.clone();
        let script_path = project_dir.join(&main_script);
        if script_path.exists() {
            self.open_file_path(&script_path);
        } else {
            // 如果主剧本不存在，尝试找目录下第一个 .akrs 文件
            self.engine = None;
            self.show_welcome = false;
        }

        // 重立绘预览状态（切换项目后旧的立绘缓存和选中都无效）
        self.sprite_preview.scanned_dir = None;
        self.sprite_preview.selected = String::new();
        self.sprite_preview.character_name = String::new();
        self.sprite_preview.textures.clear();
        self.sprite_preview.available.clear();
        self.sprite_preview.load_error = None;

        // 重置背景预览状态
        self.bg_preview.scanned_dir = None;
        self.bg_preview.selected = String::new();
        self.bg_preview.textures.clear();
        self.bg_preview.available.clear();
        self.bg_preview.load_error = None;

        // 重置音乐预览状态
        self.music_preview.scanned_dir = None;
        self.music_preview.selected = String::new();
        self.music_preview.available.clear();

        self.status = format!("已打开项目：{}", project_name);
    }

    /// 保存项目配置（project.json）。
    fn save_project_config(&mut self) {
        if let Err(e) = self.project_config.save(&self.work_dir) {
            self.status = format!("保存项目配置失败：{}", e);
        } else {
            self.status = "项目配置已保存".to_string();
        }
    }

    /// 打开目录选择对话框（用于选择项目文件夹）。
    fn open_dir_picker(&mut self) {
        let entries = Self::read_picker_entries(&self.work_dir, "");
        self.dir_picker = Some(DirPickerState {
            current_dir: self.work_dir.clone(),
            entries,
            filter: String::new(),
        });
    }

    /// 确认标题过长警告，强制应用。
    fn confirm_title_warning(&mut self) {
        if let Some(warning) = self.title_warning.take() {
            self.project_config.title = warning.new_title;
            self.project_config.subtitle = warning.new_subtitle;
            self.save_project_config();
            self.status = "已应用标题更改（标题较长，可能影响显示效果）".to_string();
        }
    }

    /// 取消标题过长警告。
    fn cancel_title_warning(&mut self) {
        self.title_warning = None;
    }

    // -- 编译 + 预览 -------------------------------------------------------

    /// 编译当前编辑器内容并（成功时）启动预览引擎。诊断信息始终反映在状态栏。
    fn run_script(&mut self) {
        let src = self.editor_content.text();
        let (program, errors) = compile(&src);
        self.diagnostics = format_errors(&errors);

        // 章节标题长度检查：`# name title` 中的显示标题（无标题时取 name）
        // 超过 MAX_CHAPTER_TITLE_CHARS 字符时给出警告（非阻断，仍可运行）。
        // 顶部章节通知会自动缩小字号显示超长文本，但过长的标题观感不佳。
        if let Some(prog) = &program {
            for sec in &prog.sections {
                let display = sec.title.clone().unwrap_or_else(|| sec.name.clone());
                let len = display.chars().count();
                if len > MAX_CHAPTER_TITLE_CHARS {
                    let line = sec.span.start_linecol.line;
                    self.diagnostics.push(format!(
                        "[警告] 第 {} 行：章节标题过长（{} 字 > {}），顶部通知会自动缩字显示，建议精简",
                        line, len, MAX_CHAPTER_TITLE_CHARS
                    ));
                }
            }
        }

        match program {
            Some(_) => match Engine::start_running(&src) {
                Ok(mut engine) => {
                    // 预览中即时显示文字，便于阅读对话。
                    engine.settings_mut().text_speed = 999.0;
                    // 设置项目标题
                    engine.set_title(
                        self.project_config.title.clone(),
                        self.project_config.subtitle.clone(),
                    );
                    self.engine = Some(engine);
                    let n = self.diagnostics.len();
                    if n == 0 {
                        self.status = "运行中".to_string();
                    } else {
                        self.status = format!("运行中（{} 条诊断）", n);
                    }
                }
                Err(errs) => {
                    // 防御性处理：`compile` 返回了程序，所以这不应该发生，但仍需呈现。
                    self.diagnostics = format_errors(&errs);
                    self.engine = None;
                    self.status = "引擎启动失败".to_string();
                }
            },
            None => {
                self.engine = None;
                let n = self
                    .diagnostics
                    .iter()
                    .filter(|d| d.starts_with("[错误]"))
                    .count();
                self.status = format!("编译失败（{} 个错误）", n);
            }
        }
    }

    // -- 对照翻译 ---------------------------------------------------------

    /// 从剧本内容解析可翻译行，填充 translatable_lines。
    fn extract_translatable_lines(&mut self) {
        self.translatable_lines.clear();
        let mut characters_seen = std::collections::HashSet::new();
        let content = self.editor_content.text();
        for (line_idx, line) in content.lines().enumerate() {
            let line_num = line_idx + 1;
            let trimmed = line.trim();
            // 章节标题：# xxx
            if let Some(rest) = trimmed.strip_prefix('#') {
                let title = rest.trim().to_string();
                if !title.is_empty() {
                    self.translatable_lines.push(TranslatableLine {
                        kind: TranslatableKind::Section,
                        original: title,
                        line_number: line_num,
                    });
                }
            }
            // 角色对话：角色: "文本" 或 角色(pose): "文本"
            else if let Some(colon_pos) = trimmed.find(':') {
                let (speaker_part, rest) = trimmed.split_at(colon_pos);
                let after_colon = rest[1..].trim();
                // 提取角色名（去掉括号里的 pose）
                let speaker = if let Some(paren_pos) = speaker_part.find('(') {
                    speaker_part[..paren_pos].trim().to_string()
                } else {
                    speaker_part.trim().to_string()
                };
                // 后面应该是 "..."
                if after_colon.starts_with('"') && after_colon.ends_with('"') && after_colon.len() >= 2 {
                    let text = after_colon[1..after_colon.len() - 1].to_string();
                    // 记录角色名
                    if !speaker.is_empty() && characters_seen.insert(speaker.clone()) {
                        self.translatable_lines.push(TranslatableLine {
                            kind: TranslatableKind::Character,
                            original: speaker,
                            line_number: line_num,
                        });
                    }
                    self.translatable_lines.push(TranslatableLine {
                        kind: TranslatableKind::Dialogue,
                        original: text,
                        line_number: line_num,
                    });
                }
            }
            // 旁白：直接 "..."
            else if trimmed.starts_with('"') && trimmed.ends_with('"') && trimmed.len() >= 2 {
                let text = trimmed[1..trimmed.len() - 1].to_string();
                self.translatable_lines.push(TranslatableLine {
                    kind: TranslatableKind::Narration,
                    original: text,
                    line_number: line_num,
                });
            }
            // 选项提示：? "提示"
            else if trimmed.starts_with('?') {
                let rest = trimmed[1..].trim();
                if rest.starts_with('"') && rest.ends_with('"') && rest.len() >= 2 {
                    let prompt = rest[1..rest.len() - 1].to_string();
                    self.translatable_lines.push(TranslatableLine {
                        kind: TranslatableKind::ChoicePrompt,
                        original: prompt,
                        line_number: line_num,
                    });
                }
            }
            // 选项：| "文本" -> ...
            else if trimmed.starts_with('|') {
                let rest = trimmed[1..].trim();
                if rest.starts_with('"') {
                    // 找结束引号
                    if let Some(end_quote) = rest[1..].find('"') {
                        let text = rest[1..end_quote + 1].to_string();
                        self.translatable_lines.push(TranslatableLine {
                            kind: TranslatableKind::Choice,
                            original: text,
                            line_number: line_num,
                        });
                    }
                }
            }
        }
    }

    /// 翻译文件的路径（assets/scripts/languages/{lang}.json）。
    fn translation_file_path(&self, lang: &str) -> PathBuf {
        self.work_dir.join("assets").join("scripts").join("languages").join(format!("{}.json", lang))
    }

    /// 加载指定语言的翻译文件；不存在则创建空的。
    fn load_translation(&mut self, lang: &str) {
        let path = self.translation_file_path(lang);
        let translator = akrs_runtime::Translator::from_file(&path);
        self.translation_file = Some(translator);
        self.translation_target_lang = lang.to_string();
        self.status = format!("已加载翻译：{}", lang);
    }

    /// 保存当前翻译到文件。
    fn save_translation(&mut self) {
        let Some(ref translator) = self.translation_file else {
            self.status = "无翻译数据可保存".to_string();
            return;
        };
        let path = self.translation_file_path(&self.translation_target_lang);
        if let Some(parent) = path.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                self.status = format!("创建目录失败：{}", e);
                return;
            }
        }
        match translator.to_json() {
            Ok(json) => {
                match std::fs::write(&path, json) {
                    Ok(_) => {
                        self.status = format!("翻译已保存：{}", path.display());
                    }
                    Err(e) => {
                        self.status = format!("保存失败：{}", e);
                    }
                }
            }
            Err(e) => {
                self.status = format!("序列化失败：{}", e);
            }
        }
    }

    /// 切换对照翻译模式。
    fn toggle_translation_mode(&mut self) {
        self.translation_mode = !self.translation_mode;
        if self.translation_mode {
            self.extract_translatable_lines();
            // 加载或创建目标语言翻译
            let path = self.translation_file_path(&self.translation_target_lang);
            let translator = if path.exists() {
                akrs_runtime::Translator::from_file(&path)
            } else {
                let mut t = akrs_runtime::Translator::new();
                t.set_language(&self.translation_target_lang, "");
                t
            };
            self.translation_file = Some(translator);
            self.status = format!("对照翻译模式：{}", self.translation_target_lang);
        } else {
            self.status = "已退出对照翻译模式".to_string();
        }
    }

    // -- 打包与预览 -------------------------------------------------------

    /// 检测 cargo 是否可用。
    fn check_cargo() -> bool {
        Command::new("cargo")
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok()
    }

    /// 启动游戏预览（外部进程）。
    fn start_game_preview(&mut self) {
        if !Self::check_cargo() {
            self.show_cargo_guide = true;
            return;
        }

        // 未存档的新建文件无法预览：没有文件路径可传给游戏。
        if self.current_file.is_none() {
            self.show_save_reminder = true;
            return;
        }

        // 先保存当前文件，确保预览运行的是最新内容。
        self.save_file();

        // 把当前编辑的文件路径作为 --script 参数传给游戏，
        // 确保运行的就是用户当前编辑的文件（而非 project.json 的 main_script 或 demo 回退）。
        let script_path = match &self.current_file {
            Some(p) => p.clone(),
            None => return,
        };

        // 预览子进程的 CWD 必须是项目根目录，而非 work_dir。
        // open_file_path 会把 work_dir 覆盖为「打开文件的父目录」，
        // 若以 work_dir 为 CWD，游戏会从错误子目录读取 saves/*.json、assets/ 等。
        let preview_cwd = self.project_dir.clone().unwrap_or_else(|| self.work_dir.clone());

        match Command::new("cargo")
            .arg("run")
            .arg("--release")
            .arg("-p")
            .arg("akrs-game")
            .arg("--")
            .arg("--script")
            .arg(&script_path)
            .current_dir(&preview_cwd)
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .spawn()
        {
            Ok(child) => {
                self.game_process = Some(child);
                self.status = format!("游戏预览已启动（{}，CWD={}）", script_path.display(), preview_cwd.display());
            }
            Err(e) => {
                self.status = format!("启动预览失败: {}", e);
            }
        }
    }

    /// 轮询游戏预览子进程，若已退出则清理。
    fn poll_game_process(&mut self) {
        if let Some(mut child) = self.game_process.take() {
            match child.try_wait() {
                Ok(None) => {
                    // 仍在运行，放回
                    self.game_process = Some(child);
                }
                Ok(Some(_status)) => {
                    self.status = "游戏预览已结束".to_string();
                }
                Err(_) => {
                    self.status = "游戏预览进程异常".to_string();
                }
            }
        }
    }

    /// 开始打包流程。
    fn start_build(&mut self) {
        if !Self::check_cargo() {
            self.show_cargo_guide = true;
            return;
        }

        // 收集选中的平台
        let platforms: Vec<BuildPlatform> = BuildPlatform::all()
            .iter()
            .filter(|p| *self.build.selected.get(p).unwrap_or(&false))
            .copied()
            .collect();

        if platforms.is_empty() {
            self.build.log_line("请至少选择一个平台");
            return;
        }

        // 保存当前文件
        if self.current_file.is_some() {
            self.save_file();
        }

        // 创建脚本快照，确保打包期间内容一致
        let snapshot_dir = self.build.output_dir.join("snapshot");
        let _ = std::fs::remove_dir_all(&snapshot_dir);
        let scripts_src = self.work_dir.join("scripts");
        if scripts_src.exists() {
            let _ = std::fs::create_dir_all(&snapshot_dir);
            copy_dir_recursive(&scripts_src, &snapshot_dir);
            self.build.snapshot_dir = Some(snapshot_dir.clone());
            self.build.log_line(format!("脚本快照已创建: {}", snapshot_dir.display()));
        } else {
            self.build.snapshot_dir = None;
        }

        self.build.log.clear();
        self.build.done = false;
        self.build.succeeded.clear();
        self.build.failed.clear();
        self.build.log_follow = true;
        self.build.queue = platforms;
        self.build.log_line("开始打包流程...");
        self.status = "正在打包...".to_string();
    }

    /// 每次轮询调用，推进构建队列。
    fn poll_build(&mut self) {
        // 如果有正在构建的进程，检查其状态
        if let Some(mut child) = self.build.process.take() {
            match child.try_wait() {
                Ok(None) => {
                    // 仍在构建，放回
                    self.build.process = Some(child);
                    return;
                }
                Ok(Some(status)) => {
                    let platform = self.build.building.take().unwrap();
                    if status.success() {
                        self.build.log_line(format!("[成功] {} 构建成功", platform.label()));
                        self.build.succeeded.push(platform);

                        // 收集构建产物到 output_dir
                        let target_dir = self.work_dir.join(format!(
                            "target/{}/release",
                            platform.target()
                        ));
                        self.collect_build_artifacts(platform, &target_dir);
                    } else {
                        // 尝试读取 stderr 输出
                        let mut err_msg = String::new();
                        if let Some(stderr) = child.stderr.as_mut() {
                            let _ = stderr.read_to_string(&mut err_msg);
                        }
                        if !err_msg.is_empty() {
                            // 只保留最后几行关键信息
                            let lines: Vec<&str> = err_msg.lines().collect();
                            let tail = lines.len().saturating_sub(10);
                            for line in &lines[tail..] {
                                self.build.log.push_str(line);
                                self.build.log.push('\n');
                            }
                        }
                        let fail_msg = format!("exit code: {:?}", status.code());
                        self.build.log_line(format!("[失败] {} 构建失败 ({})", platform.label(), fail_msg));
                        self.build.failed.push((platform, fail_msg));
                    }
                }
                Err(e) => {
                    let platform = self.build.building.take().unwrap();
                    let fail_msg = format!("{}", e);
                    self.build
                        .log_line(format!("[失败] {} 构建异常: {}", platform.label(), fail_msg));
                    self.build.failed.push((platform, fail_msg));
                }
            }
        }

        // 如果空闲且有队列，启动下一个构建（失败不阻塞，继续下一个）
        if self.build.building.is_none() && !self.build.queue.is_empty() {
            let platform = self.build.queue.remove(0);
            self.build.building = Some(platform);
            self.build
                .log_line(format!("[构建] 正在构建 {} (target: {})...", platform.label(), platform.target()));

            // 确保安装了目标平台
            let _ = Command::new("cargo")
                .args(["rustup", "target", "add", platform.target()])
                .current_dir(&self.work_dir)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();

            match Command::new("cargo")
                .arg("build")
                .arg("--release")
                .arg("--target")
                .arg(platform.target())
                .current_dir(&self.work_dir)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
            {
                Ok(child) => {
                    self.build.process = Some(child);
                }
                Err(e) => {
                    let fail_msg = format!("无法启动构建进程: {}", e);
                    self.build.log_line(format!("[失败] {} {}", platform.label(), fail_msg));
                    self.build.failed.push((platform, fail_msg));
                    self.build.building = None;
                }
            }
        }

        // 队列空且无构建中 → 完成，输出汇总报告
        if self.build.building.is_none() && self.build.queue.is_empty() && !self.build.done {
            self.build.done = true;

            // 汇总报告
            let ok = self.build.succeeded.len();
            let fail = self.build.failed.len();
            // 先收集失败详情，避免借用冲突
            let fail_details: Vec<String> = self
                .build
                .failed
                .iter()
                .map(|(p, msg)| format!("  · {}: {}", p.label(), msg))
                .collect();
            self.build.log_line("\n========== 打包汇总 ==========");
            self.build.log_line(format!("成功: {} 个平台", ok));
            if fail > 0 {
                self.build.log_line(format!("失败: {} 个平台", fail));
                for detail in &fail_details {
                    self.build.log_line(detail);
                }
            }
            self.build.log_line("==============================");

            // 清理脚本快照
            if let Some(snap) = self.build.snapshot_dir.take() {
                let _ = std::fs::remove_dir_all(&snap);
                self.build.log_line("脚本快照已清理。");
            }

            self.status = format!("打包完成（成功 {}，失败 {}）", ok, fail);
        }
    }

    /// 收集构建产物到导出目录。
    fn collect_build_artifacts(&mut self, platform: BuildPlatform, target_dir: &Path) {
        let output_dir = self.build.output_dir.join(format!(
            "akrs-game-{}",
            match platform {
                BuildPlatform::Windows => "windows",
                BuildPlatform::Linux => "linux",
                BuildPlatform::MacOS => "macos",
            }
        ));

        let _ = std::fs::create_dir_all(&output_dir);

        // 从 Cargo.toml 动态读取二进制目标名
        let bin_names = read_binary_names(&self.work_dir);
        let is_windows = platform == BuildPlatform::Windows;

        for name in &bin_names {
            let exe_name = if is_windows {
                format!("{}.exe", name)
            } else {
                name.clone()
            };
            let exe_src = target_dir.join(&exe_name);
            let exe_dst = output_dir.join(&exe_name);
            if exe_src.exists() {
                let _ = std::fs::copy(&exe_src, &exe_dst);
                self.build
                    .log_line(format!("  产物 {} 已复制到: {}", exe_name, output_dir.display()));
            } else {
                self.build
                    .log_line(format!("  警告: 未找到产物 {}", exe_src.display()));
            }
        }

        // 从快照目录复制脚本（确保内容一致），无快照则从项目目录复制
        let scripts_src = self
            .build
            .snapshot_dir
            .clone()
            .unwrap_or_else(|| self.work_dir.join("scripts"));
        if scripts_src.exists() {
            let scripts_dst = output_dir.join("scripts");
            copy_dir_recursive(&scripts_src, &scripts_dst);
        }

        // 资源始终从项目目录复制（资源不在快照范围内）
        let assets_src = self.work_dir.join("assets");
        if assets_src.exists() {
            let assets_dst = output_dir.join("assets");
            copy_dir_recursive(&assets_src, &assets_dst);
        }
    }

    // -- 资源扫描 ---------------------------------------------------------

    /// 扫描 `assets/characters/` 下的 PNG 立绘（不含扩展名）。
    fn scan_sprites(&mut self) {
        // 优先用 project_dir（项目根），回退到 work_dir。
        // 打开单个文件时 work_dir 会被覆盖为文件所在目录（如 scripts/），
        // 此时 assets/ 在项目根而非 work_dir 下。
        let base = self.project_dir.as_ref().unwrap_or(&self.work_dir);
        let dir = base.join("assets").join("characters");
        if self.sprite_preview.scanned_dir.as_ref() == Some(&dir) {
            return;
        }
        self.sprite_preview.available.clear();
        self.sprite_preview.textures.clear();
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for e in entries.flatten() {
                let p = e.path();
                if p.extension().is_some_and(|x| x == "png") {
                    if let Some(stem) = p.file_stem().and_then(|s| s.to_str()) {
                        self.sprite_preview.available.push(stem.to_string());
                    }
                }
            }
            self.sprite_preview.available.sort();
        }
        self.sprite_preview.scanned_dir = Some(dir);
        if self.sprite_preview.selected.is_empty() && !self.sprite_preview.available.is_empty() {
            self.sprite_preview.selected = self.sprite_preview.available[0].clone();
        }
    }

    /// 扫描 `assets/bg/` 下的 PNG 背景（不含扩展名）。
    fn scan_bgs(&mut self) {
        let base = self.project_dir.as_ref().unwrap_or(&self.work_dir);
        let dir = base.join("assets").join("bg");
        if self.bg_preview.scanned_dir.as_ref() == Some(&dir) {
            return;
        }
        self.bg_preview.available.clear();
        self.bg_preview.textures.clear();
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for e in entries.flatten() {
                let p = e.path();
                if p.extension().is_some_and(|x| x == "png") {
                    if let Some(stem) = p.file_stem().and_then(|s| s.to_str()) {
                        self.bg_preview.available.push(stem.to_string());
                    }
                }
            }
            self.bg_preview.available.sort();
        }
        self.bg_preview.scanned_dir = Some(dir);
        if self.bg_preview.selected.is_empty() && !self.bg_preview.available.is_empty() {
            self.bg_preview.selected = self.bg_preview.available[0].clone();
        }
    }

    /// 扫描 `assets/music/` 下的音乐文件（不含扩展名）。
    fn scan_music(&mut self) {
        let base = self.project_dir.as_ref().unwrap_or(&self.work_dir);
        let dir = base.join("assets").join("music");
        if self.music_preview.scanned_dir.as_ref() == Some(&dir) {
            return;
        }
        self.music_preview.available.clear();
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for e in entries.flatten() {
                let p = e.path();
                let is_audio = p
                    .extension()
                    .and_then(|x| x.to_str())
                    .is_some_and(|x| matches!(x, "ogg" | "mp3" | "wav" | "flac"));
                if is_audio {
                    if let Some(stem) = p.file_stem().and_then(|s| s.to_str()) {
                        self.music_preview.available.push(stem.to_string());
                    }
                }
            }
            self.music_preview.available.sort();
        }
        self.music_preview.scanned_dir = Some(dir);
        if self.music_preview.selected.is_empty() && !self.music_preview.available.is_empty() {
            self.music_preview.selected = self.music_preview.available[0].clone();
        }
    }

    /// 加载立绘纹理到缓存（用于预览面板的 image 显示）。
    fn load_sprite_texture(&mut self, name: &str) {
        if name.is_empty() || self.sprite_preview.textures.contains_key(name) {
            return;
        }
        let base = self.project_dir.as_ref().unwrap_or(&self.work_dir);
        let path = base.join("assets").join("characters").join(format!("{}.png", name));
        match load_png_handle(&path) {
            Ok(handle) => {
                self.sprite_preview.textures.insert(name.to_string(), handle);
                self.sprite_preview.load_error = None;
            }
            Err(e) => {
                self.sprite_preview.load_error = Some(format!("{}: {}", name, e));
            }
        }
    }

    /// 加载背景纹理到缓存（用于预览面板的 image 显示）。
    fn load_bg_texture(&mut self, name: &str) {
        if name.is_empty() || self.bg_preview.textures.contains_key(name) {
            return;
        }
        let base = self.project_dir.as_ref().unwrap_or(&self.work_dir);
        let path = base.join("assets").join("bg").join(format!("{}.png", name));
        match load_png_handle(&path) {
            Ok(handle) => {
                self.bg_preview.textures.insert(name.to_string(), handle);
                self.bg_preview.load_error = None;
            }
            Err(e) => {
                self.bg_preview.load_error = Some(format!("{}: {}", name, e));
            }
        }
    }

    /// Ctrl+点击 / Ctrl+J：从光标所在行解析语句并跳转到对应预览标签页。
    ///
    /// 支持的行类型：
    /// - `+ 角色 [(pose)] [at x,y] [size s]` → 立绘预览，选中匹配立绘并应用位置/大小
    /// - `@bg 名字` → 背景预览，选中匹配背景
    /// - `@music 名字` / `@sound 名字` → 音乐预览，选中匹配音乐
    ///
    /// 若立绘/背景/音乐列表为空，先触发扫描（用 project_dir）。
    fn jump_to_preview_from_cursor(&mut self) {
        let (line, _col) = self.editor_content.cursor_position();
        let text = self.editor_content.text();
        let Some(line_str) = text.lines().nth(line) else {
            return;
        };
        let trimmed = line_str.trim();

        // `+` 立绘入场语句
        if let Some(rest) = trimmed.strip_prefix('+') {
            let rest = rest.trim();
            // 第一个 token 是角色/立绘名（可能后跟空格或 (pose)）
            let name: String = rest
                .chars()
                .take_while(|c| !c.is_whitespace() && *c != '(')
                .collect();
            if name.is_empty() {
                return;
            }
            // 确保立绘列表已扫描
            self.scan_sprites();
            let matched = self.sprite_preview.available.iter()
                .find(|s| *s == &name || s.starts_with(&format!("{}.", name)))
                .cloned();
            if let Some(m) = matched {
                self.sprite_preview.selected = m.clone();
                self.load_sprite_texture(&m);
                // 解析 at x,y 位置与 size s
                if let Some(at_idx) = trimmed.find("at ") {
                    let after_at = &trimmed[at_idx + 3..];
                    let nums: Vec<f32> = after_at
                        .split(|c: char| c == ',' || c.is_whitespace())
                        .filter_map(|s| s.parse().ok())
                        .collect();
                    if nums.len() >= 1 { self.sprite_preview.x_percent = nums[0].clamp(0.0, 1.0); }
                    if nums.len() >= 2 { self.sprite_preview.y_percent = nums[1].clamp(0.0, 1.0); }
                }
                if let Some(size_idx) = trimmed.find("size ") {
                    let after_size = &trimmed[size_idx + 5..];
                    if let Some(v) = after_size
                        .split_whitespace()
                        .next()
                        .and_then(|s| s.parse::<f32>().ok())
                    {
                        self.sprite_preview.scale = v.clamp(0.1, 5.0);
                    }
                }
                self.preview_tab = PreviewTab::Sprite;
                self.status = format!("已跳转到立绘预览：{}", name);
            } else {
                self.status = format!("未在立绘目录中找到「{}」，请先扫描立绘目录", name);
            }
            return;
        }

        // `@` 舞台命令语句（@bg / @music / @sound）
        if let Some(rest) = trimmed.strip_prefix('@') {
            let rest = rest.trim();
            let cmd: String = rest
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            let args: String = rest[cmd.len()..].trim().to_string();
            // 去掉可能的引号
            let res_name = args.trim_matches(|c: char| c == '"' || c == '\'' || c == '\u{201C}' || c == '\u{201D}').to_string();
            match cmd.as_str() {
                "bg" => {
                    if res_name.is_empty() {
                        return;
                    }
                    self.scan_bgs();
                    let matched = self.bg_preview.available.iter()
                        .find(|s| *s == &res_name || s.starts_with(&format!("{}.", res_name)))
                        .cloned();
                    if let Some(m) = matched {
                        self.bg_preview.selected = m.clone();
                        self.load_bg_texture(&m);
                        self.preview_tab = PreviewTab::Background;
                        self.status = format!("已跳转到背景预览：{}", res_name);
                    } else {
                        self.status = format!("未在背景目录中找到「{}」", res_name);
                    }
                }
                "music" | "sound" => {
                    if res_name.is_empty() {
                        return;
                    }
                    self.scan_music();
                    let matched = self.music_preview.available.iter()
                        .find(|s| *s == &res_name || s.starts_with(&format!("{}.", res_name)))
                        .cloned();
                    if let Some(m) = matched {
                        self.music_preview.selected = m;
                        self.preview_tab = PreviewTab::Music;
                        self.status = format!("已跳转到音乐预览：{}", res_name);
                    } else {
                        self.status = format!("未在音乐目录中找到「{}」", res_name);
                    }
                }
                _ => {
                    self.status = format!("不支持跳转的命令：@{}", cmd);
                }
            }
        }
    }

    /// 生成立绘摆放语法并插入脚本。
    fn generate_sprite_syntax(&mut self) {
        let sp = &self.sprite_preview;
        if sp.selected.is_empty() {
            self.status = "请先选择一个立绘".to_string();
            return;
        }
        let name = if sp.character_name.is_empty() {
            sp.selected.clone()
        } else {
            sp.character_name.clone()
        };
        let syntax = format!(
            "+ {} at {:.2},{:.2} size {:.2}",
            name, sp.x_percent, sp.y_percent, sp.scale
        );
        self.smart_insert_syntax(&syntax);
    }

    /// 生成背景切换语法并插入脚本。
    fn generate_bg_syntax(&mut self) {
        let bp = &self.bg_preview;
        if bp.selected.is_empty() {
            self.status = "请先选择一个背景".to_string();
            return;
        }
        let trans = if bp.transition.is_empty() {
            "fade".to_string()
        } else {
            bp.transition.clone()
        };
        let syntax = format!("@bg {} with {}", bp.selected, trans);
        self.smart_insert_syntax(&syntax);
    }

    /// 生成音乐播放语法并插入脚本。
    fn generate_music_syntax(&mut self) {
        let mp = &self.music_preview;
        if mp.selected.is_empty() {
            self.status = "请先选择一个音乐".to_string();
            return;
        }
        let syntax = format!("@music {}", mp.selected);
        self.smart_insert_syntax(&syntax);
    }

    // -- rpy 导入 ---------------------------------------------------------

    /// 将 Ren'Py `.rpy` 脚本转换为 `.akrs` 格式。
    /// 返回转换过程中产生的警告信息列表。
    ///
    /// 转换规则（参考原始 egui 实现）：
    /// - `label name` → `# name`（章节）
    /// - `scene bg_name` → `@bg bg_name with fade`（背景）
    /// - `show char pose` → `+ char pose`（角色上场）
    /// - `hide char` → `- char`（角色下场）
    /// - `menu:` → `?`（选择开始），`"text" expression:` → `| "text" -> ...`
    /// - `speaker "dialogue"` → `speaker: "dialogue"`
    /// - `jump label` → `-> label`（流程跳转）
    /// - `return` → `~~`（章节结束）
    /// - `#` 注释行保留
    fn convert_rpy_to_akrs(&mut self, source: &Path, target: &Path) -> Vec<String> {
        let mut warnings = Vec::new();
        let content = match std::fs::read_to_string(source) {
            Ok(c) => c,
            Err(e) => {
                warnings.push(format!("读取源文件失败：{}", e));
                return warnings;
            }
        };

        let mut output = String::new();
        let mut in_menu = false;
        let mut menu_idx = 0usize;

        for (line_no, raw) in content.lines().enumerate() {
            let trimmed = raw.trim();
            // 空行保留
            if trimmed.is_empty() {
                output.push('\n');
                continue;
            }
            // Ren'Py 注释 `#` 保留
            if trimmed.starts_with('#') {
                output.push_str(trimmed);
                output.push('\n');
                continue;
            }
            // label name → # name
            if let Some(rest) = trimmed.strip_prefix("label ") {
                let name = rest.split_whitespace().next().unwrap_or(rest).trim_end_matches(':');
                // 结束上一个章节
                if !output.is_empty() && !output.ends_with("~~\n") {
                    output.push_str("~~\n\n");
                }
                output.push_str(&format!("# {}\n\n", name));
                in_menu = false;
                continue;
            }
            // scene bg_name → @bg bg_name with fade
            if let Some(rest) = trimmed.strip_prefix("scene ") {
                let bg = rest.split_whitespace().next().unwrap_or(rest);
                output.push_str(&format!("@bg {} with fade\n", bg));
                continue;
            }
            // show char pose → + char pose
            if let Some(rest) = trimmed.strip_prefix("show ") {
                let parts: Vec<&str> = rest.split_whitespace().collect();
                let char_name = parts.first().copied().unwrap_or("");
                let pose = if parts.len() > 1 { parts[1] } else { "" };
                if pose.is_empty() {
                    output.push_str(&format!("+ {}\n", char_name));
                } else {
                    output.push_str(&format!("+ {} ({})\n", char_name, pose));
                }
                continue;
            }
            // hide char → - char
            if let Some(rest) = trimmed.strip_prefix("hide ") {
                let char_name = rest.split_whitespace().next().unwrap_or(rest);
                output.push_str(&format!("- {}\n", char_name));
                continue;
            }
            // menu: → ?
            if trimmed == "menu:" || trimmed.starts_with("menu ") {
                output.push_str("? \"请选择\"\n");
                in_menu = true;
                menu_idx = 0;
                continue;
            }
            // menu 选项："text" expression: → | "text" -> Branch_N
            if in_menu && trimmed.starts_with('"') {
                if let Some(end_quote) = trimmed[1..].find('"') {
                    let text = &trimmed[1..end_quote + 1];
                    menu_idx += 1;
                    let branch = format!("Branch_{}", menu_idx);
                    output.push_str(&format!("| \"{}\" -> {}\n", text, branch));
                    continue;
                }
            }
            // jump label → -> label
            if let Some(rest) = trimmed.strip_prefix("jump ") {
                let label = rest.split_whitespace().next().unwrap_or(rest);
                output.push_str(&format!("-> {}\n", label));
                continue;
            }
            // return → ~~
            if trimmed == "return" || trimmed == "return:" {
                output.push_str("~~\n\n");
                in_menu = false;
                continue;
            }
            // 对话行：speaker "dialogue" 或 "narration"
            // 形如 `e "Hello"` 或 `e happy "Hello"` → e: "Hello"
            if let Some(quote_pos) = trimmed.find('"') {
                if quote_pos > 0 {
                    let speaker_part = trimmed[..quote_pos].trim();
                    let dialogue = &trimmed[quote_pos..];
                    // 去掉 speaker 中的 pose（空格后的部分）
                    let speaker = speaker_part.split_whitespace().next().unwrap_or(speaker_part);
                    if !speaker.is_empty() && !speaker.contains('$') {
                        output.push_str(&format!("{}: {}\n", speaker, dialogue));
                        continue;
                    }
                }
                // 纯旁白 "text"
                output.push_str(&format!("{}\n", trimmed));
                continue;
            }
            // 无法识别的行：保留为注释并警告
            warnings.push(format!("第 {} 行：无法自动转换「{}」，已保留为注释", line_no + 1, trimmed));
            output.push_str(&format!("// {}（rpy 原行）\n", trimmed));
        }

        // 确保以 ~~ 结尾
        if !output.ends_with("~~\n") && !output.trim().is_empty() {
            output.push_str("~~\n");
        }

        match std::fs::write(target, &output) {
            Ok(()) => {
                self.status = format!("rpy 已转换：{}", target.display());
            }
            Err(e) => {
                warnings.push(format!("写入目标文件失败：{}", e));
                self.status = format!("rpy 转换写入失败：{}", e);
            }
        }
        warnings
    }

    // -- 标签辅助 ---------------------------------------------------------

    fn kind_label(kind: &TranslatableKind) -> &'static str {
        match kind {
            TranslatableKind::Section => "章节",
            TranslatableKind::Dialogue => "对话",
            TranslatableKind::Narration => "旁白",
            TranslatableKind::Choice => "选项",
            TranslatableKind::ChoicePrompt => "选项提示",
            TranslatableKind::Character => "角色",
        }
    }

    fn kind_color(kind: &TranslatableKind) -> Color {
        match kind {
            TranslatableKind::Section => COLOR_SECTION,
            TranslatableKind::Dialogue => COLOR_STRING,
            TranslatableKind::Narration => COLOR_STRING,
            TranslatableKind::Choice => COLOR_CHOICE,
            TranslatableKind::ChoicePrompt => COLOR_CHOICE,
            TranslatableKind::Character => COLOR_DIRECTION,
        }
    }
}

// ---------------------------------------------------------------------------
// iced 消息与 UI 层
// ---------------------------------------------------------------------------

/// UI 交互消息。
#[derive(Debug, Clone)]
enum Message {
    /// 多行编辑器动作（输入、删除、光标移动等）。
    Edit(text_editor::Action),
    /// 订阅轮询：推进子进程状态与引擎更新。
    PollTick,
    /// 键盘修饰键状态变化（用于追踪 Ctrl 是否按下，实现 Ctrl+点击跳转）。
    ModifiersChanged(bool),

    // 文件操作
    NewFile,
    LoadSample,
    SaveFile,
    OpenFilePicker,
    OpenDirPicker,
    /// 打开工作目录中指定文件名。
    OpenFile(String),
    /// 打开当前 `file_name_input` 指定的文件名。
    OpenCurrentName,
    /// 文件名输入框内容变更。
    FileNameInput(String),
    CloseWelcome,

    // 运行预览
    RunScript,
    /// 推进预览引擎到下一句。
    AdvancePreview,
    /// 在选项中选择 index。
    ChooseOption(usize),
    StartGamePreview,

    // 关于弹窗
    ShowAbout,
    CloseAbout,

    /// Ctrl+J：从光标所在行解析角色/立绘名，跳转到立绘预览。
    JumpToPreviewFromCursor,

    // 语法快速插入：ending 声明与 unlock 标记
    InsertEndingSyntax,
    InsertUnlockSyntax,

    // 文件选择对话框
    FilePickerFilter(String),
    FilePickerEnter(String),
    FilePickerSelect(String),
    FilePickerConfirm,
    FilePickerCancel,
    FilePickerParent,

    // 目录选择对话框
    DirPickerFilter(String),
    DirPickerEnter(String),
    DirPickerConfirm,
    DirPickerCancel,
    DirPickerParent,

    // 预览标签页
    PreviewTabChanged(PreviewTab),

    // 查找替换
    ToggleFindReplace,
    FindReplaceTarget(String),
    DoFindReplace,

    // 项目设置
    ShowProjectSettings,
    CloseProjectSettings,
    ProjectTitle(String),
    ProjectSubtitle(String),
    ProjectMainScript(String),
    ProjectAuthor(String),
    ProjectDescription(String),
    ProjectLanguage(String),
    SaveProjectConfig,
    OpenRecent(PathBuf),

    // 标题过长警告
    ConfirmTitleWarning,
    CancelTitleWarning,

    // 打包
    ToggleBuildPlatform(BuildPlatform),
    StartBuild,
    ToggleBuildDialog,
    CloseCargoGuide,
    /// 关闭「请先保存再预览」提示弹窗。
    CloseSaveReminder,

    // 项目警告弹窗（project.json 的 warning 字段）
    /// 确定：仅本次关闭项目警告弹窗。
    CloseProjectWarning,
    /// 不再显示：永久忽略本项目警告（存于编辑器数据目录）。
    DismissProjectWarning,

    // 资源预览
    ScanSprites,
    ScanBgs,
    ScanMusic,
    SpriteSelected(String),
    SpriteName(String),
    SpriteX(f32),
    SpriteY(f32),
    SpriteScale(f32),
    BgSelected(String),
    BgTransition(String),
    MusicSelected(String),
    GenerateSpriteSyntax,
    GenerateBgSyntax,
    GenerateMusicSyntax,

    // 翻译
    ToggleTranslationMode,
    TranslationTargetLang(String),
    LoadTranslation,
    ReextractTranslatable,
    SaveTranslation,
    /// 修改第 idx 条可翻译行的译文。
    TranslationInput(usize, String),

    // 在文件管理器中打开工作目录
    OpenWorkDirInFileManager,

    // 查找替换增强
    FindReplaceReplacement(String),
    FindNext,
    /// 触发「替换插入」：携带预生成的语法，打开查找替换弹窗以收集要被替换的查找内容。
    StartReplaceInsert(String),

    // 放大预览
    ShowEnlargedPreview,
    CloseEnlargedPreview,

    // rpy 导入
    ToggleRpyImport,
    RpyImportSourceInput(String),
    RpyImportTargetInput(String),
    DoRpyImport,

    // 打包：打开产物文件夹
    OpenBuildFolder,

    // cargo 引导：打开 rust-lang.org
    OpenRustLang,

    // 快捷键帮助
    ToggleShortcuts,
}

impl EditorApp {
    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Edit(action) => {
                // Ctrl+点击检测：Ctrl 按下时，点击（产生 Move 动作）跳转到预览。
                // text_editor::Action::Move 由鼠标点击触发（无拖拽时为单纯定位）。
                // 仅在 Move 且 ctrl_held 时触发跳转，不执行默认光标移动以避免副作用。
                if self.ctrl_held && matches!(action, text_editor::Action::Move(_)) {
                    self.jump_to_preview_from_cursor();
                } else {
                    self.editor_content.perform(action);
                }
            }
            Message::ModifiersChanged(ctrl) => {
                self.ctrl_held = ctrl;
            }
            Message::PollTick => {
                // 子进程轮询
                self.poll_game_process();
                self.poll_build();
                // 预览引擎动画推进：用 last_time 计算实际 dt
                let now = Instant::now();
                let dt = match self.last_time {
                    Some(prev) => now.duration_since(prev).as_secs_f32().min(0.5),
                    None => 0.1,
                };
                self.last_time = Some(now);
                if let Some(engine) = self.engine.as_mut() {
                    let _ = engine.update(dt);
                }
            }
            Message::NewFile => self.new_file(),
            Message::LoadSample => self.load_sample(),
            Message::SaveFile => self.save_file(),
            Message::OpenFilePicker => self.open_file_picker(FilePickerMode::Open),
            Message::OpenDirPicker => self.open_dir_picker(),
            Message::OpenFile(name) => self.open_file(&name),
            Message::OpenCurrentName => self.open_current_name(),
            Message::FileNameInput(s) => self.file_name_input = s,
            Message::CloseWelcome => self.show_welcome = false,
            Message::RunScript => self.run_script(),
            Message::AdvancePreview => {
                if let Some(engine) = self.engine.as_mut() {
                    let _ = engine.advance();
                }
            }
            Message::ChooseOption(i) => {
                if let Some(engine) = self.engine.as_mut() {
                    let _ = engine.choose(i);
                }
            }
            Message::StartGamePreview => self.start_game_preview(),
            Message::ShowAbout => self.show_about = true,
            Message::JumpToPreviewFromCursor => {
                self.jump_to_preview_from_cursor();
            }
            Message::InsertEndingSyntax => {
                // 插入 ending 声明骨架：ending "id" epilogue "path.akrs" [button "文本"]
                self.smart_insert_syntax(
                    "ending \"my_ending\" epilogue \"scripts/epilogue.akrs\" button \"尾声之后\"",
                );
            }
            Message::InsertUnlockSyntax => {
                // 插入 unlock 标记：unlock "id"
                self.smart_insert_syntax("unlock \"my_ending\"");
            }
            Message::CloseAbout => self.show_about = false,
            Message::FilePickerFilter(s) => {
                if let Some(picker) = self.file_picker.as_mut() {
                    picker.filter = s.clone();
                    picker.entries = Self::read_picker_entries(&picker.current_dir, &s);
                }
            }
            Message::FilePickerEnter(name) => {
                if let Some(picker) = self.file_picker.as_mut() {
                    let target = picker.current_dir.join(&name);
                    if target.is_dir() {
                        picker.current_dir = target;
                        picker.entries = Self::read_picker_entries(&picker.current_dir, &picker.filter);
                        picker.selected = None;
                    }
                }
            }
            Message::FilePickerSelect(name) => {
                if let Some(picker) = self.file_picker.as_mut() {
                    picker.selected = Some(name);
                }
            }
            Message::FilePickerConfirm => {
                if let Some(picker) = self.file_picker.take() {
                    if let Some(name) = picker.selected {
                        let path = picker.current_dir.join(&name);
                        if path.is_file() {
                            self.open_file_path(&path);
                        }
                    }
                }
            }
            Message::FilePickerCancel => {
                self.file_picker = None;
            }
            Message::FilePickerParent => {
                if let Some(picker) = self.file_picker.as_mut() {
                    if let Some(parent) = picker.current_dir.parent() {
                        picker.current_dir = parent.to_path_buf();
                        picker.entries = Self::read_picker_entries(&picker.current_dir, &picker.filter);
                        picker.selected = None;
                    }
                }
            }
            Message::DirPickerFilter(s) => {
                if let Some(picker) = self.dir_picker.as_mut() {
                    picker.filter = s.clone();
                    picker.entries = Self::read_picker_entries(&picker.current_dir, &s);
                }
            }
            Message::DirPickerEnter(name) => {
                if let Some(picker) = self.dir_picker.as_mut() {
                    let target = picker.current_dir.join(&name);
                    if target.is_dir() {
                        picker.current_dir = target;
                        picker.entries = Self::read_picker_entries(&picker.current_dir, &picker.filter);
                    }
                }
            }
            Message::DirPickerConfirm => {
                if let Some(picker) = self.dir_picker.take() {
                    self.open_project(&picker.current_dir);
                    self.show_welcome = false;
                }
            }
            Message::DirPickerCancel => {
                self.dir_picker = None;
            }
            Message::DirPickerParent => {
                if let Some(picker) = self.dir_picker.as_mut() {
                    if let Some(parent) = picker.current_dir.parent() {
                        picker.current_dir = parent.to_path_buf();
                        picker.entries = Self::read_picker_entries(&picker.current_dir, &picker.filter);
                    }
                }
            }
            Message::PreviewTabChanged(tab) => {
                self.preview_tab = tab;
                match tab {
                    PreviewTab::Sprite => self.scan_sprites(),
                    PreviewTab::Background => self.scan_bgs(),
                    PreviewTab::Music => self.scan_music(),
                    _ => {}
                }
            }
            Message::ToggleFindReplace => {
                self.show_find_replace_dialog = !self.show_find_replace_dialog;
                // 关闭弹窗时清空替换插入缓存，避免下次打开仍处于替换插入模式
                if !self.show_find_replace_dialog {
                    self.find_replace_syntax.clear();
                }
            }
            Message::FindReplaceTarget(s) => self.find_replace_target = s,
            Message::DoFindReplace => {
                let target = self.find_replace_target.clone();
                // 替换插入模式：find_replace_syntax 非空时，用缓存的语法替换脚本中
                // 匹配 target 的内容；否则走普通查找替换（用 find_replace_replacement）。
                let is_replace_insert = !self.find_replace_syntax.is_empty();
                let replacement = if is_replace_insert {
                    self.find_replace_syntax.clone()
                } else {
                    self.find_replace_replacement.clone()
                };
                if self.find_and_replace(&target, &replacement) {
                    let n = self.find_count(&target);
                    self.status = format!("已替换「{}」为「{}」（剩余 {} 处）", target, replacement, n);
                } else {
                    self.status = format!("未找到「{}」", target);
                }
                if is_replace_insert {
                    // 替换插入是一次性操作：完成后清空缓存并关闭弹窗
                    self.find_replace_syntax.clear();
                    self.show_find_replace_dialog = false;
                }
            }
            Message::StartReplaceInsert(syntax) => {
                // 缓存生成的语法，清空查找/替换输入，打开弹窗让用户输入要被替换的内容
                self.find_replace_syntax = syntax;
                self.find_replace_target = String::new();
                self.find_replace_replacement = String::new();
                self.show_find_replace_dialog = true;
            }
            Message::ShowProjectSettings => self.show_project_settings = true,
            Message::CloseProjectSettings => self.show_project_settings = false,
            Message::ProjectTitle(s) => self.project_config.title = s,
            Message::ProjectSubtitle(s) => self.project_config.subtitle = s,
            Message::ProjectMainScript(s) => self.project_config.main_script = s,
            Message::ProjectAuthor(s) => self.project_config.author = s,
            Message::ProjectDescription(s) => self.project_config.description = s,
            Message::ProjectLanguage(s) => self.project_config.language = s,
            Message::SaveProjectConfig => {
                // 保存前检查标题/副标题是否过长，过长则弹出警告对话框
                if self.project_config.is_title_too_long()
                    || self.project_config.is_subtitle_too_long()
                {
                    self.title_warning = Some(TitleWarningState {
                        new_title: self.project_config.title.clone(),
                        new_subtitle: self.project_config.subtitle.clone(),
                    });
                    self.status = "标题或副标题过长，请确认".to_string();
                } else {
                    self.save_project_config();
                }
            }
            Message::OpenRecent(path) => {
                self.open_project(&path);
                self.show_welcome = false;
            }
            Message::ConfirmTitleWarning => self.confirm_title_warning(),
            Message::CancelTitleWarning => self.cancel_title_warning(),
            Message::ToggleBuildPlatform(p) => {
                let entry = self.build.selected.entry(p).or_insert(false);
                *entry = !*entry;
            }
            Message::StartBuild => self.start_build(),
            Message::ToggleBuildDialog => self.build.show = !self.build.show,
            Message::CloseCargoGuide => self.show_cargo_guide = false,
            Message::CloseSaveReminder => self.show_save_reminder = false,
            Message::CloseProjectWarning => self.show_project_warning = false,
            Message::DismissProjectWarning => {
                // 永久忽略本项目警告：写入编辑器数据目录 dismissed_warnings.json。
                if let Some(dir) = self.project_dir.clone() {
                    DismissedWarnings::load().dismiss(&dir);
                }
                self.show_project_warning = false;
            }
            Message::ScanSprites => self.scan_sprites(),
            Message::ScanBgs => self.scan_bgs(),
            Message::ScanMusic => self.scan_music(),
            Message::SpriteSelected(s) => {
                self.sprite_preview.selected = s.clone();
                self.load_sprite_texture(&s);
            }
            Message::SpriteName(s) => self.sprite_preview.character_name = s,
            Message::SpriteX(v) => self.sprite_preview.x_percent = v,
            Message::SpriteY(v) => self.sprite_preview.y_percent = v,
            Message::SpriteScale(v) => self.sprite_preview.scale = v,
            Message::BgSelected(s) => {
                self.bg_preview.selected = s.clone();
                self.load_bg_texture(&s);
            }
            Message::BgTransition(s) => self.bg_preview.transition = s,
            Message::MusicSelected(s) => self.music_preview.selected = s,
            Message::GenerateSpriteSyntax => self.generate_sprite_syntax(),
            Message::GenerateBgSyntax => self.generate_bg_syntax(),
            Message::GenerateMusicSyntax => self.generate_music_syntax(),
            Message::ToggleTranslationMode => self.toggle_translation_mode(),
            Message::TranslationTargetLang(s) => self.translation_target_lang = s,
            Message::LoadTranslation => {
                let lang = self.translation_target_lang.clone();
                self.load_translation(&lang);
            }
            Message::ReextractTranslatable => {
                self.extract_translatable_lines();
                self.status = "已重新提取可翻译文本".to_string();
            }
            Message::SaveTranslation => self.save_translation(),
            Message::TranslationInput(idx, text) => {
                if let Some(t) = self.translation_file.as_mut() {
                    if let Some(line) = self.translatable_lines.get(idx) {
                        match line.kind {
                            TranslatableKind::Section => t.set_section(&line.original, &text),
                            TranslatableKind::Dialogue => t.set_dialogue(&line.original, &text),
                            TranslatableKind::Narration => t.set_narration(&line.original, &text),
                            TranslatableKind::Choice => t.set_choice(&line.original, &text),
                            TranslatableKind::ChoicePrompt => {
                                t.set_choice_prompt(&line.original, &text)
                            }
                            TranslatableKind::Character => t.set_character(&line.original, &text),
                        }
                    }
                }
            }
            Message::OpenWorkDirInFileManager => {
                open_path_in_file_manager(&self.work_dir);
            }
            Message::FindReplaceReplacement(s) => self.find_replace_replacement = s,
            Message::FindNext => {
                // 查找下一个：简单实现——统计命中数并在状态栏显示
                let target = self.find_replace_target.clone();
                let count = self.find_count(&target);
                if count > 0 {
                    self.status = format!("找到 {} 处「{}」", count, target);
                } else {
                    self.status = format!("未找到「{}」", target);
                }
            }
            Message::ShowEnlargedPreview => self.show_enlarged_preview = true,
            Message::CloseEnlargedPreview => self.show_enlarged_preview = false,
            Message::ToggleRpyImport => self.show_rpy_import = !self.show_rpy_import,
            Message::RpyImportSourceInput(s) => {
                self.rpy_import_source = if s.trim().is_empty() {
                    None
                } else {
                    Some(PathBuf::from(s))
                };
            }
            Message::RpyImportTargetInput(s) => {
                self.rpy_import_target = PathBuf::from(s);
            }
            Message::DoRpyImport => {
                if let Some(source) = self.rpy_import_source.clone() {
                    let target = if self.rpy_import_target.as_os_str().is_empty() {
                        let stem = source
                            .file_stem()
                            .and_then(|s| s.to_str())
                            .unwrap_or("converted");
                        self.work_dir.join(format!("{}.akrs", stem))
                    } else {
                        self.rpy_import_target.clone()
                    };
                    self.rpy_import_warnings =
                        self.convert_rpy_to_akrs(&source, &target);
                    if self.rpy_import_warnings.is_empty() {
                        self.rpy_import_warnings
                            .push("转换完成，无警告".to_string());
                    }
                } else {
                    self.rpy_import_warnings.clear();
                    self.rpy_import_warnings.push("请先指定源 .rpy 文件".to_string());
                }
            }
            Message::OpenBuildFolder => {
                let dir = self.build.output_dir.clone();
                open_path_in_file_manager(&dir);
            }
            Message::OpenRustLang => {
                open_url_in_browser(RUST_LANG_URL);
            }
            Message::ToggleShortcuts => self.show_shortcuts = !self.show_shortcuts,
        }
        Task::none()
    }

    fn subscription(&self) -> Subscription<Message> {
        // 合并三个订阅：100ms 轮询 + 键盘快捷键 + 修饰键状态追踪
        let tick = iced::time::every(Duration::from_millis(100)).map(|_| Message::PollTick);
        let keys = keyboard::on_key_press(handle_key_press);
        // 追踪 Ctrl 按下/释放状态，用于实现 Ctrl+点击剧本行跳转预览。
        // iced 的 text_editor 不暴露带修饰键的点击事件，故通过全局事件订阅
        // 维护 ctrl_held 状态，在 Edit(Move) 到达时检查该状态。
        let mods = iced::event::listen_with(track_modifiers);
        Subscription::batch([tick, keys, mods])
    }

    fn view(&self) -> Element<'_, Message> {
        let base: Element<'_, Message> = if self.show_welcome {
            self.view_welcome()
        } else {
            self.view_main()
        };

        // 弹窗层：用 stack + opaque 模拟 modal。
        let mut layers: Vec<Element<'_, Message>> = vec![base];

        if self.show_about {
            layers.push(self.view_about_modal());
        }
        if self.file_picker.is_some() {
            layers.push(self.view_file_picker_modal());
        }
        if self.dir_picker.is_some() {
            layers.push(self.view_dir_picker_modal());
        }
        if self.show_find_replace_dialog {
            layers.push(self.view_find_replace_modal());
        }
        if self.show_project_settings {
            layers.push(self.view_project_settings_modal());
        }
        if self.build.show {
            layers.push(self.view_build_modal());
        }
        if self.show_cargo_guide {
            layers.push(self.view_cargo_guide_modal());
        }
        if self.show_save_reminder {
            layers.push(self.view_save_reminder_modal());
        }
        if self.show_project_warning {
            layers.push(self.view_project_warning_modal());
        }
        if self.title_warning.is_some() {
            layers.push(self.view_title_warning_modal());
        }
        if self.show_rpy_import {
            layers.push(self.view_rpy_import_modal());
        }
        if self.show_enlarged_preview {
            layers.push(self.view_enlarged_preview_modal());
        }
        if self.show_shortcuts {
            layers.push(self.view_shortcuts_modal());
        }

        stack(layers)
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
    }

    // -- 欢迎面板 ---------------------------------------------------------

    fn view_welcome(&self) -> Element<'_, Message> {
        let title = text("欢迎使用 Akizuki*Rustgal 剧本编辑器")
            .size(26.0)
            .color(COLOR_SECTION);
        let subtitle = text("为视觉小说设计的轻量级剧本编写工具")
            .size(16.0)
            .color(Color::from_rgb(0.7, 0.75, 0.82));

        let btn = |label: &'static str, msg: Message| {
            button(text(label).size(15.0))
                .on_press(msg)
                .padding([10, 18])
                .style(move |_theme: &Theme, _status: button::Status| button::Style {
                    background: Some(Background::Color(Color::from_rgb(0.18, 0.21, 0.30))),
                    text_color: Color::WHITE,
                    border: Border::default().rounded(6.0),
                    ..Default::default()
                })
        };

        let actions = row![
            btn("打开项目", Message::OpenDirPicker),
            btn("新建剧本", Message::NewFile),
            btn("打开已有剧本", Message::OpenFilePicker),
            btn("打开示例剧本", Message::LoadSample),
            button(text("跳过").size(15.0))
                .on_press(Message::CloseWelcome)
                .padding([10, 18])
                .style(move |_theme: &Theme, _status: button::Status| button::Style {
                    background: Some(Background::Color(Color::from_rgb(0.12, 0.14, 0.18))),
                    text_color: Color::from_rgb(0.6, 0.65, 0.72),
                    border: Border::default().rounded(6.0),
                    ..Default::default()
                }),
        ]
        .spacing(12)
        .padding(20);

        // 最近项目列表
        let mut recent = Column::new().spacing(4);
        for project in &self.recent_projects.projects {
            recent = recent.push(
                row![
                    text(project.name.clone()).color(Color::from_rgb(0.6, 0.7, 0.9)),
                    horizontal_space(),
                    text(project.path.display().to_string())
                        .size(11.0)
                        .color(Color::from_rgb(0.45, 0.5, 0.6)),
                ]
                .push(
                    button(text("打开").size(12.0))
                        .on_press(Message::OpenRecent(project.path.clone()))
                        .padding([4, 10])
                ),
            );
        }

        let recent_block = if self.recent_projects.projects.is_empty() {
            Column::new()
        } else {
            column![
                text("最近项目").size(18.0).color(COLOR_FLOW),
                recent,
            ]
            .spacing(8)
        };

        let example = "# 章节标题\n\
            @bg 背景名 with fade\n\
            + 角色名 (pose1) at 0.5,1.0 size 1.0\n\
            角色名: \"对话内容\"\n\
            + 角色名 (pose2) swap  // 差分更换（无过渡）\n\
            $变量 = 1\n\
            ? \"选择提示\"\n\
            | \"选项1\"  -> 分支 A\n\
            | \"选项2\"  -> 分支 B\n\
            ?\n\
            ~~  // 章节结束";

        let syntax_card = container(
            column![
                text("语法示例").size(18.0).color(COLOR_FLOW),
                text(example)
                    .size(13.0)
                    .color(Color::from_rgb(0.78, 0.86, 1.0)),
            ]
            .spacing(8)
        )
        .padding(16)
        .style(|_t| container::Style {
            background: Some(Background::Color(Color::from_rgb(0.10, 0.12, 0.16))),
            border: Border::default().rounded(8.0).color(Color::from_rgb(0.2, 0.22, 0.28)),
            ..Default::default()
        });

        let tip = text(
            "提示：# 章节  @ 场景指令  + 角色上场  +...swap 差分  - 角色下场  $ 变量  ? 选择分支  ~~ 章节结束",
        )
        .size(12.0)
        .color(Color::from_rgb(0.45, 0.5, 0.6));

        let content = column![
            title,
            subtitle,
            Space::new(Length::Fill, 20),
            actions,
            Space::new(Length::Fill, 16),
            recent_block,
            Space::new(Length::Fill, 16),
            syntax_card,
            Space::new(Length::Fill, 8),
            tip,
        ]
        .spacing(6)
        .align_x(Alignment::Center)
        .padding(40);

        container(content)
            .width(Length::Fill)
            .height(Length::Fill)
            .center_x(Length::Fill)
            .style(|_t| container::Style {
                background: Some(Background::Color(Color::from_rgb(0.06, 0.07, 0.09))),
                ..Default::default()
            })
            .into()
    }

    // -- 主界面 -----------------------------------------------------------

    fn view_main(&self) -> Element<'_, Message> {
        let toolbar = self.view_toolbar();
        let panels = self.view_three_panels();
        let status_bar = self.view_status_bar();

        let main = column![toolbar, panels, status_bar]
            .width(Length::Fill)
            .height(Length::Fill);

        container(main)
            .width(Length::Fill)
            .height(Length::Fill)
            .style(|_t| container::Style {
                background: Some(Background::Color(Color::from_rgb(0.05, 0.06, 0.08))),
                ..Default::default()
            })
            .into()
    }

    fn view_toolbar(&self) -> Element<'_, Message> {
        let b = |label: &'static str, msg: Message| {
            button(text(label).size(13.0))
                .on_press(msg)
                .padding([6, 12])
                .style(|_t, _s| button::Style {
                    background: Some(Background::Color(Color::from_rgb(0.16, 0.19, 0.26))),
                    text_color: Color::WHITE,
                    border: Border::default().rounded(4.0),
                    ..Default::default()
                })
        };

        let left = row![
            b("新建", Message::NewFile),
            b("打开", Message::OpenFilePicker),
            b("保存", Message::SaveFile),
            b("运行", Message::RunScript),
            b("启动游戏", Message::StartGamePreview),
            b("查找替换", Message::ToggleFindReplace),
            b("项目设置", Message::ShowProjectSettings),
            b("打包", Message::ToggleBuildDialog),
            b("翻译", Message::ToggleTranslationMode),
            b("rpy导入", Message::ToggleRpyImport),
            b("插入ending", Message::InsertEndingSyntax),
            b("插入unlock", Message::InsertUnlockSyntax),
            b("快捷键", Message::ToggleShortcuts),
            b("关于", Message::ShowAbout),
        ]
        .spacing(6);

        let right = row![
            text(format!("工作目录：{}", self.work_dir.display()))
                .size(11.0)
                .color(Color::from_rgb(0.5, 0.55, 0.62)),
            b("打开目录", Message::OpenWorkDirInFileManager),
        ]
        .spacing(8)
        .align_y(Alignment::Center);

        let bar = row![left, horizontal_space(), right]
            .spacing(6)
            .padding([6, 10])
            .align_y(Alignment::Center);

        container(bar)
            .width(Length::Fill)
            .style(|_t| container::Style {
                background: Some(Background::Color(Color::from_rgb(0.10, 0.12, 0.16))),
                border: Border::default().color(Color::from_rgb(0.18, 0.20, 0.25)),
                ..Default::default()
            })
            .into()
    }

    fn view_three_panels(&self) -> Element<'_, Message> {
        let left = self.view_left_panel();
        let center = self.view_center_panel();
        let right = self.view_right_panel();

        let row = row![left, center, right]
            .spacing(1)
            .width(Length::Fill)
            .height(Length::Fill);

        container(row)
            .width(Length::Fill)
            .height(Length::Fill)
            .style(|_t| container::Style {
                background: Some(Background::Color(Color::from_rgb(0.12, 0.13, 0.16))),
                ..Default::default()
            })
            .into()
    }

    fn view_left_panel(&self) -> Element<'_, Message> {
        let header = text("文件列表").size(13.0).color(COLOR_FLOW);

        let name_input = text_input("文件名", &self.file_name_input)
            .on_input(Message::FileNameInput)
            .on_submit(Message::SaveFile)
            .padding(6);

        let actions = row![
            button(text("新建").size(12.0))
                .on_press(Message::NewFile)
                .padding([4, 8]),
            button(text("打开").size(12.0))
                .on_press(Message::OpenCurrentName)
                .padding([4, 8]),
            button(text("保存").size(12.0))
                .on_press(Message::SaveFile)
                .padding([4, 8]),
        ]
        .spacing(4);

        let mut list = Column::new().spacing(2);
        for name in &self.file_list {
            let is_current = self
                .current_file
                .as_ref()
                .and_then(|p| p.file_name())
                .map(|n| n.to_string_lossy().as_ref() == name.as_str())
                .unwrap_or(false);
            let label_color = if is_current {
                COLOR_FLOW
            } else {
                Color::from_rgb(0.82, 0.85, 0.92)
            };
            list = list.push(
                button(text(name.clone()).size(12.0).color(label_color))
                    .on_press(Message::OpenFile(name.clone()))
                    .padding([3, 6])
                    .width(Length::Fill)
                    .style(move |_t, _s| button::Style {
                        background: Some(Background::Color(if is_current {
                            Color::from_rgb(0.20, 0.16, 0.10)
                        } else {
                            Color::TRANSPARENT
                        })),
                        border: Border::default().rounded(3.0),
                        ..Default::default()
                    }),
            );
        }

        let body = column![
            header,
            name_input,
            actions,
            scrollable(list).width(Length::Fill).height(Length::Fill),
        ]
        .spacing(6)
        .padding(8);

        container(body)
            .width(220)
            .height(Length::Fill)
            .style(|_t| container::Style {
                background: Some(Background::Color(Color::from_rgb(0.08, 0.09, 0.12))),
                ..Default::default()
            })
            .into()
    }

    fn view_center_panel(&self) -> Element<'_, Message> {
        // 使用自定义 AkrsHighlighter 实现语法高亮：按行首 token 着色。
        let editor = text_editor(&self.editor_content)
            .on_action(Message::Edit)
            .font(Font::MONOSPACE)
            .size(FONT_SIZE)
            .padding(8)
            .highlight_with::<AkrsHighlighter>(
                AkrsHighlightSettings,
                akrs_highlight_to_format,
            );

        let content = if self.translation_mode {
            self.view_translation_panel()
                .or_else(|| Some(editor.into()))
                .unwrap()
        } else {
            editor.into()
        };

        container(scrollable(content).width(Length::Fill).height(Length::Fill))
            .width(Length::Fill)
            .height(Length::Fill)
            .style(|_t| container::Style {
                background: Some(Background::Color(Color::from_rgb(0.10, 0.11, 0.14))),
                ..Default::default()
            })
            .into()
    }

    fn view_translation_panel(&self) -> Option<Element<'_, Message>> {
        // 顶部工具栏：语言输入 + 加载/重新提取/保存按钮
        let toolbar = row![
            text("目标语言：").size(12.0).color(COLOR_FLOW),
            text_input("en-US", &self.translation_target_lang)
                .on_input(Message::TranslationTargetLang)
                .width(100)
                .padding(4),
            button(text("加载").size(11.0))
                .on_press(Message::LoadTranslation)
                .padding([4, 8]),
            button(text("重新提取").size(11.0))
                .on_press(Message::ReextractTranslatable)
                .padding([4, 8]),
            button(text("保存").size(11.0))
                .on_press(Message::SaveTranslation)
                .padding([4, 8]),
        ]
        .spacing(6)
        .align_y(Alignment::Center);

        let mut col = Column::new().spacing(4);
        col = col.push(toolbar);
        col = col.push(Space::new(Length::Fill, 4));

        for (idx, line) in self.translatable_lines.iter().enumerate() {
            let kind_color = Self::kind_color(&line.kind);
            let kind_label = Self::kind_label(&line.kind);
            let header = row![
                text(format!("[{}]", kind_label))
                    .font(Font::MONOSPACE)
                    .size(11.0)
                    .color(kind_color),
                text(format!("行 {}", line.line_number))
                    .font(Font::MONOSPACE)
                    .size(11.0)
                    .color(Color::from_rgb(0.5, 0.5, 0.5)),
                text(format!("#{}", idx + 1))
                    .font(Font::MONOSPACE)
                    .size(11.0)
                    .color(Color::from_rgb(0.45, 0.45, 0.45)),
            ]
            .spacing(8);

            // 获取当前译文（从 translation_file 中查询）
            let current_translation = if let Some(t) = &self.translation_file {
                match line.kind {
                    TranslatableKind::Section => t.t_section(&line.original).to_string(),
                    TranslatableKind::Dialogue => t.t_dialogue(&line.original).to_string(),
                    TranslatableKind::Narration => t.t_narration(&line.original).to_string(),
                    TranslatableKind::Choice => t.t_choice(&line.original).to_string(),
                    TranslatableKind::ChoicePrompt => t.t_choice_prompt(&line.original).to_string(),
                    TranslatableKind::Character => t.t_character(&line.original).to_string(),
                }
            } else {
                String::new()
            };
            // t_* 返回原文时表示无翻译——显示空输入框
            let display = if current_translation == line.original {
                String::new()
            } else {
                current_translation
            };

            // 左侧原文 + 右侧译文输入
            let line_row = row![
                text(line.original.clone())
                    .size(13.0)
                    .color(Color::from_rgb(0.78, 0.78, 0.78))
                    .width(Length::FillPortion(1)),
                text_input("译文", &display)
                    .on_input(move |s| Message::TranslationInput(idx, s))
                    .width(Length::FillPortion(1))
                    .padding(4),
            ]
            .spacing(8);

            col = col.push(header).push(line_row);
        }
        Some(col.into())
    }

    fn view_right_panel(&self) -> Element<'_, Message> {
        // 用 iced_aw::Tabs 替代手写的按钮式 tab bar：Tabs 内置标签栏 + 内容区，
        // 切换由 Message::PreviewTabChanged 驱动，活动标签由 set_active_tab 指定。
        // 遍历 PreviewTab::ALL 注册全部标签，避免逐个手写重复的 push 块。
        let mut tabs = Tabs::new(Message::PreviewTabChanged);
        for t in PreviewTab::ALL {
            tabs = tabs.push(
                t,
                TabLabel::Text(t.label().to_string()),
                scrollable(self.view_for_tab(t))
                    .width(Length::Fill)
                    .height(Length::Fill),
            );
        }
        let tabs = tabs
            .set_active_tab(&self.preview_tab)
            .width(Length::Fill)
            .height(Length::Fill);

        container(tabs)
            .width(320)
            .height(Length::Fill)
            .padding(8)
            .style(|_t| container::Style {
                background: Some(Background::Color(Color::from_rgb(0.08, 0.09, 0.12))),
                ..Default::default()
            })
            .into()
    }

    /// 按 PreviewTag 取对应预览主体。供 iced_aw::Tabs 注册各标签内容。
    fn view_for_tab(&self, tab: PreviewTab) -> Element<'_, Message> {
        match tab {
            PreviewTab::Script => self.view_script_preview(),
            PreviewTab::Sprite => self.view_sprite_preview(),
            PreviewTab::Background => self.view_bg_preview(),
            PreviewTab::Music => self.view_music_preview(),
            PreviewTab::Outline => self.view_outline(),
        }
    }

    fn view_script_preview(&self) -> Element<'_, Message> {
        let Some(engine) = self.engine.as_ref() else {
            return container(
                text("点击「运行」编译并启动预览引擎")
                    .color(Color::from_rgb(0.5, 0.55, 0.62))
                    .size(13.0),
            )
            .center(Length::Fill)
            .into();
        };

        let phase = phase_label(engine.phase());
        let scene = engine.scene();

        let mut col = Column::new().spacing(6);

        col = col.push(
            row![
                text("阶段：").size(12.0).color(COLOR_FLOW),
                text(phase).size(12.0).color(Color::WHITE),
            ]
            .spacing(4),
        );

        if let Some(bg) = &scene.background {
            col = col.push(
                row![
                    text("背景：").size(12.0).color(COLOR_COMMAND),
                    text(bg.name.clone()).size(12.0).color(Color::WHITE),
                ]
                .spacing(4),
            );
        }

        if let Some(music) = &scene.music {
            col = col.push(
                row![
                    text("音乐：").size(12.0).color(COLOR_COMMAND),
                    text(music.clone()).size(12.0).color(Color::WHITE),
                ]
                .spacing(4),
            );
        }

        if !scene.characters.is_empty() {
            let names: Vec<String> = scene
                .characters
                .iter()
                .map(|c| {
                    c.name.clone()
                        + c.pose.as_deref().map(|p| format!("({})", p)).as_deref().unwrap_or("")
                })
                .collect();
            col = col.push(
                row![
                    text("角色：").size(12.0).color(COLOR_DIRECTION),
                    text(names.join(", ")).size(12.0).color(Color::WHITE),
                ]
                .spacing(4),
            );
        }

        if let Some(dlg) = &scene.dialogue {
            let speaker = if dlg.speaker.is_empty() {
                "旁白".to_string()
            } else {
                dlg.speaker.clone()
            };
            col = col.push(
                container(
                    column![
                        text(speaker).size(13.0).color(COLOR_FLOW),
                        text(dlg.full_text.clone()).size(13.0).color(Color::WHITE),
                    ]
                    .spacing(2)
                )
                .padding(8)
                .style(|_t| container::Style {
                    background: Some(Background::Color(Color::from_rgba8(20, 24, 36, 0.85))),
                    border: Border::default().rounded(6.0),
                    ..Default::default()
                }),
            );
        }

        if let Some(choices) = &scene.choices {
            col = col.push(text(choices.prompt.clone().unwrap_or_else(|| "请选择".to_string()))
                .size(13.0)
                .color(COLOR_CHOICE));
            for (i, opt) in choices.options.iter().enumerate() {
                let color = if opt.available {
                    Color::WHITE
                } else {
                    Color::from_rgb(0.4, 0.4, 0.4)
                };
                col = col.push(
                    button(text(opt.text.clone()).size(12.0).color(color))
                        .on_press_maybe(if opt.available {
                            Some(Message::ChooseOption(i))
                        } else {
                            None
                        })
                        .padding([4, 8])
                        .width(Length::Fill)
                        .style(|_t, _s| button::Style {
                            background: Some(Background::Color(Color::from_rgb(0.14, 0.16, 0.22))),
                            border: Border::default().rounded(4.0),
                            ..Default::default()
                        }),
                );
            }
        }

        let advance = button(text("推进（空格）").size(12.0))
            .on_press(Message::AdvancePreview)
            .padding([6, 12]);

        col = col.push(Space::new(Length::Fill, 8));
        col = col.push(
            row![
                advance,
                horizontal_space(),
                button(text("放大").size(12.0))
                    .on_press(Message::ShowEnlargedPreview)
                    .padding([6, 12]),
            ]
            .spacing(4),
        );

        col.into()
    }

    fn view_sprite_preview(&self) -> Element<'_, Message> {
        let mut col = Column::new().spacing(6);
        col = col.push(
            button(text("扫描立绘目录").size(12.0))
                .on_press(Message::ScanSprites)
                .padding([4, 8]),
        );

        if self.sprite_preview.available.is_empty() {
            col = col.push(
                text("（未发现立绘，请扫描或放入 assets/characters/）")
                    .size(11.0)
                    .color(Color::from_rgb(0.5, 0.5, 0.55)),
            );
        } else {
            // 用 pick_list 选择立绘（列表可能很长，用下拉比按钮列表更紧凑）
            col = col.push(
                pick_list(
                    self.sprite_preview.available.clone(),
                    Some(self.sprite_preview.selected.clone()),
                    Message::SpriteSelected,
                )
                .padding(5)
                .placeholder("选择立绘"),
            );
        }

        // 加载错误提示
        if let Some(err) = &self.sprite_preview.load_error {
            col = col.push(
                text(err.clone())
                    .size(11.0)
                    .color(Color::from_rgb(0.8, 0.4, 0.4)),
            );
        }

        // 图片预览（从缓存或直接加载）
        // 用 scrollable 包裹，高度受限 280px，图片保持原始宽高比（width=Fill 自适应）。
        // 避免固定 height(200) 导致竖图被压扁或截断。
        if !self.sprite_preview.selected.is_empty() {
            if let Some(handle) = self.sprite_preview.textures.get(&self.sprite_preview.selected) {
                col = col.push(
                    container(
                        scrollable(
                            container(
                                image(handle).width(Length::Fill),
                            )
                            .width(Length::Fill)
                            .center_x(Length::Fill),
                        )
                        .height(280),
                    )
                    .style(|_t| container::Style {
                        background: Some(Background::Color(Color::from_rgb(0.05, 0.06, 0.08))),
                        border: Border::default().rounded(4.0),
                        ..Default::default()
                    }),
                );
            } else {
                col = col.push(
                    text("（图片预览将在选择后加载）")
                        .size(11.0)
                        .color(Color::from_rgb(0.5, 0.5, 0.55)),
                );
            }
        }

        col = col.push(
            text_input("角色名（可选）", &self.sprite_preview.character_name)
                .on_input(Message::SpriteName)
                .padding(5),
        );

        // 滑块：X 位置、Y 位置、缩放
        let sp = &self.sprite_preview;
        col = col.push(
            column![
                row![
                    text("X 位置").size(11.0).color(Color::from_rgb(0.6, 0.62, 0.68)),
                    horizontal_space(),
                    text(format!("{:.0}%", sp.x_percent * 100.0))
                        .size(11.0)
                        .color(COLOR_DIRECTION),
                ],
                slider(0.0f32..=1.0f32, sp.x_percent, Message::SpriteX),
            ]
            .spacing(2),
        );
        col = col.push(
            column![
                row![
                    text("Y 位置").size(11.0).color(Color::from_rgb(0.6, 0.62, 0.68)),
                    horizontal_space(),
                    text(format!("{:.0}%", sp.y_percent * 100.0))
                        .size(11.0)
                        .color(COLOR_DIRECTION),
                ],
                slider(0.0f32..=1.0f32, sp.y_percent, Message::SpriteY),
            ]
            .spacing(2),
        );
        col = col.push(
            column![
                row![
                    text("缩放").size(11.0).color(Color::from_rgb(0.6, 0.62, 0.68)),
                    horizontal_space(),
                    text(format!("{:.2}", sp.scale))
                        .size(11.0)
                        .color(COLOR_DIRECTION),
                ],
                slider(0.1f32..=3.0f32, sp.scale, Message::SpriteScale),
            ]
            .spacing(2),
        );

        let syntax = if self.sprite_preview.selected.is_empty() {
            "（未选择立绘）".to_string()
        } else {
            let sp = &self.sprite_preview;
            let name = if sp.character_name.is_empty() {
                sp.selected.clone()
            } else {
                sp.character_name.clone()
            };
            format!(
                "+ {} at {:.2},{:.2} size {:.2}",
                name, sp.x_percent, sp.y_percent, sp.scale
            )
        };
        col = col.push(
            text(syntax.clone())
                .font(Font::MONOSPACE)
                .size(12.0)
                .color(COLOR_DIRECTION),
        );

        // 「替换插入」按钮：未选择立绘时禁用，避免把占位文本作为语法缓存
        let mut replace_btn = button(text("替换插入").size(12.0)).padding([5, 10]);
        if !self.sprite_preview.selected.is_empty() {
            replace_btn = replace_btn.on_press(Message::StartReplaceInsert(syntax));
        }
        col = col.push(
            row![
                button(text("插入语法").size(12.0))
                    .on_press(Message::GenerateSpriteSyntax)
                    .padding([5, 10]),
                replace_btn,
            ]
            .spacing(6),
        );

        col.into()
    }

    fn view_bg_preview(&self) -> Element<'_, Message> {
        let mut col = Column::new().spacing(6);
        col = col.push(
            button(text("扫描背景目录").size(12.0))
                .on_press(Message::ScanBgs)
                .padding([4, 8]),
        );

        if self.bg_preview.available.is_empty() {
            col = col.push(
                text("（未发现背景，请扫描或放入 assets/bg/）")
                    .size(11.0)
                    .color(Color::from_rgb(0.5, 0.5, 0.55)),
            );
        } else {
            col = col.push(
                pick_list(
                    self.bg_preview.available.clone(),
                    Some(self.bg_preview.selected.clone()),
                    Message::BgSelected,
                )
                .padding(5)
                .placeholder("选择背景"),
            );
        }

        // 加载错误提示
        if let Some(err) = &self.bg_preview.load_error {
            col = col.push(
                text(err.clone())
                    .size(11.0)
                    .color(Color::from_rgb(0.8, 0.4, 0.4)),
            );
        }

        // 图片预览（scrollable 包裹，保持宽高比）
        if !self.bg_preview.selected.is_empty() {
            if let Some(handle) = self.bg_preview.textures.get(&self.bg_preview.selected) {
                col = col.push(
                    container(
                        scrollable(
                            container(
                                image(handle).width(Length::Fill),
                            )
                            .width(Length::Fill)
                            .center_x(Length::Fill),
                        )
                        .height(220),
                    )
                    .style(|_t| container::Style {
                        background: Some(Background::Color(Color::from_rgb(0.05, 0.06, 0.08))),
                        border: Border::default().rounded(4.0),
                        ..Default::default()
                    }),
                );
            } else {
                col = col.push(
                    text("（图片预览将在选择后加载）")
                        .size(11.0)
                        .color(Color::from_rgb(0.5, 0.5, 0.55)),
                );
            }
        }

        col = col.push(
            text_input("过渡效果", &self.bg_preview.transition)
                .on_input(Message::BgTransition)
                .padding(5),
        );

        let syntax = if self.bg_preview.selected.is_empty() {
            "（未选择背景）".to_string()
        } else {
            let trans = if self.bg_preview.transition.is_empty() {
                "fade".to_string()
            } else {
                self.bg_preview.transition.clone()
            };
            format!("@bg {} with {}", self.bg_preview.selected, trans)
        };
        col = col.push(
            text(syntax.clone())
                .font(Font::MONOSPACE)
                .size(12.0)
                .color(COLOR_COMMAND),
        );

        // 「替换插入」按钮：未选择背景时禁用，避免把占位文本作为语法缓存
        let mut replace_btn = button(text("替换插入").size(12.0)).padding([5, 10]);
        if !self.bg_preview.selected.is_empty() {
            replace_btn = replace_btn.on_press(Message::StartReplaceInsert(syntax));
        }
        col = col.push(
            row![
                button(text("插入语法").size(12.0))
                    .on_press(Message::GenerateBgSyntax)
                    .padding([5, 10]),
                replace_btn,
            ]
            .spacing(6),
        );

        col.into()
    }

    fn view_music_preview(&self) -> Element<'_, Message> {
        let mut col = Column::new().spacing(6);
        col = col.push(
            button(text("扫描音乐目录").size(12.0))
                .on_press(Message::ScanMusic)
                .padding([4, 8]),
        );

        if self.music_preview.available.is_empty() {
            col = col.push(
                text("（未发现音乐，请扫描或放入 assets/music/）")
                    .size(11.0)
                    .color(Color::from_rgb(0.5, 0.5, 0.55)),
            );
        } else {
            for name in &self.music_preview.available {
                let active = *name == self.music_preview.selected;
                col = col.push(
                    button(text(name.clone()).size(12.0))
                        .on_press(Message::MusicSelected(name.clone()))
                        .padding([3, 6])
                        .style(move |_t, _s| button::Style {
                            background: Some(Background::Color(if active {
                                Color::from_rgb(0.20, 0.22, 0.30)
                            } else {
                                Color::from_rgb(0.10, 0.11, 0.14)
                            })),
                            border: Border::default().rounded(3.0),
                            ..Default::default()
                        }),
                );
            }
        }

        let syntax = if self.music_preview.selected.is_empty() {
            "（未选择音乐）".to_string()
        } else {
            format!("@music {}", self.music_preview.selected)
        };
        col = col.push(
            text(syntax.clone())
                .font(Font::MONOSPACE)
                .size(12.0)
                .color(COLOR_COMMAND),
        );

        // 「替换插入」按钮：未选择音乐时禁用，避免把占位文本作为语法缓存
        let mut replace_btn = button(text("替换插入").size(12.0)).padding([5, 10]);
        if !self.music_preview.selected.is_empty() {
            replace_btn = replace_btn.on_press(Message::StartReplaceInsert(syntax));
        }
        col = col.push(
            row![
                button(text("插入语法").size(12.0))
                    .on_press(Message::GenerateMusicSyntax)
                    .padding([5, 10]),
                replace_btn,
            ]
            .spacing(6),
        );

        col.into()
    }

    fn view_outline(&self) -> Element<'_, Message> {
        let content = self.editor_content.text();
        if content.trim().is_empty() {
            return container(
                text("（暂无内容）")
                    .size(12.0)
                    .color(Color::from_rgb(0.45, 0.45, 0.5)),
            )
            .center(Length::Fill)
            .into();
        }

        let marks = scan_flow_marks(&content);
        let pairs = compute_flow_pairs(&marks);
        let lines: Vec<&str> = content.split('\n').collect();
        let indent_w: f32 = 16.0;

        // 光标配对高亮：光标所在行的 =>/<= 及其配对行高亮
        let cursor_line = self.editor_content.cursor_position().0;
        let active_mark = mark_at_cursor(&marks, cursor_line);
        let pair_mark = active_mark.and_then(|mi| pairs[mi]);
        let highlight_lines: Vec<usize> = match (active_mark, pair_mark) {
            (Some(a), Some(b)) => vec![marks[a].line, marks[b].line],
            (Some(a), None) => vec![marks[a].line],
            _ => vec![],
        };

        let mut mark_iter = 0usize;
        let mut depth: i32 = 0;
        let mut col = Column::new().spacing(2);

        for (line_no, raw) in lines.iter().enumerate() {
            while mark_iter < marks.len() && marks[mark_iter].line < line_no {
                mark_iter += 1;
            }
            let mark_here = if mark_iter < marks.len() && marks[mark_iter].line == line_no {
                Some(mark_iter)
            } else {
                None
            };

            let trimmed = raw.trim_start();
            if trimmed.is_empty() {
                continue;
            }

            let (indent_level, text_opt, color) = if let Some(mi) = mark_here {
                let m = &marks[mi];
                match m.kind {
                    FlowKind::Visit => {
                        let t = m.target.as_deref().unwrap_or("");
                        (depth, Some(format!("=> {}", t)), COLOR_FLOW)
                    }
                    FlowKind::Return => {
                        let lvl = if pairs[mi].is_some() { depth - 1 } else { depth };
                        (lvl, Some("<=".to_string()), COLOR_FLOW)
                    }
                }
            } else if let Some(rest) = trimmed.strip_prefix('#') {
                (depth, Some(format!("# {}", rest.trim())), COLOR_SECTION)
            } else if let Some(rest) = trimmed.strip_prefix('?') {
                let r = rest.trim();
                if r.is_empty() {
                    continue;
                }
                (depth, Some(format!("? {}", r)), COLOR_CHOICE)
            } else if let Some(rest) = trimmed.strip_prefix('|') {
                (depth, Some(format!("| {}", rest.trim())), COLOR_CHOICE)
            } else {
                continue;
            };

            if let Some(mi) = mark_here {
                match marks[mi].kind {
                    FlowKind::Visit => depth += 1,
                    FlowKind::Return => {
                        if pairs[mi].is_some() {
                            depth -= 1;
                        }
                    }
                }
            }

            if let Some(t) = text_opt {
                let indent_level = indent_level.max(0) as usize;
                let indent_pixels = (indent_level as f32) * indent_w;
                let is_highlighted = highlight_lines.contains(&line_no);
                let line_row = row![
                    Space::new(Length::Fixed(indent_pixels), 0),
                    text(t)
                        .font(Font::MONOSPACE)
                        .size(12.0)
                        .color(if is_highlighted { COLOR_PAIR_HIGHLIGHT } else { color }),
                ]
                .align_y(Alignment::Center);
                col = if is_highlighted {
                    col.push(
                        container(line_row)
                            .style(|_t| container::Style {
                                background: Some(Background::Color(Color::from_rgba(1.0, 0.86, 0.0, 0.15))),
                                border: Border::default().rounded(3.0).color(COLOR_PAIR_HIGHLIGHT),
                                ..Default::default()
                            })
                            .padding([2, 4]),
                    )
                } else {
                    col.push(line_row)
                };
            }
        }

        col.into()
    }

    fn view_status_bar(&self) -> Element<'_, Message> {
        let mut diag = self.diagnostics.iter().take(1).cloned();
        let first_diag = diag.next().unwrap_or_default();
        let n_err = self.diagnostics.iter().filter(|d| d.starts_with("[错误]")).count();
        let n_warn = self.diagnostics.iter().filter(|d| d.starts_with("[警告]")).count();

        let left = row![
            text(self.status.clone()).size(12.0).color(Color::from_rgb(0.8, 0.82, 0.88)),
            text(format!("  错误 {}  警告 {}", n_err, n_warn))
                .size(12.0)
                .color(Color::from_rgb(0.55, 0.58, 0.65)),
        ]
        .align_y(Alignment::Center);

        let right = text(if first_diag.is_empty() {
            String::new()
        } else {
            first_diag
        })
        .size(11.0)
        .color(Color::from_rgb(0.6, 0.62, 0.68));

        let bar = row![left, horizontal_space(), right]
            .spacing(6)
            .padding([4, 10])
            .align_y(Alignment::Center);

        container(bar)
            .width(Length::Fill)
            .style(|_t| container::Style {
                background: Some(Background::Color(Color::from_rgb(0.10, 0.12, 0.16))),
                border: Border::default().color(Color::from_rgb(0.18, 0.20, 0.25)),
                ..Default::default()
            })
            .into()
    }

    // -- 弹窗 -------------------------------------------------------------
    //
    // 弹窗机制：`iced_aw` 并未提供独立的 `modal` 组件（其历史上的 modal 已弃用，
    // 官方建议改用 iced 的 `stack` + `opaque` 组合——本编辑器即采用此模式）。
    // 这里用 `iced_aw::Card` 统一卡片外观（标题头 + 关闭按钮 + 主体），再用
    // `opaque` 全屏遮罩居中显示，模拟 modal 行为：opaque 拦截下层事件。

    fn modal_card<'a>(
        &'a self,
        title: &'a str,
        close_msg: Message,
        body: Element<'a, Message>,
    ) -> Element<'a, Message> {
        // Card 自带标题头与 on_close 关闭按钮。
        let card = Card::new(text(title).size(15.0).color(COLOR_FLOW), body)
            .padding(Padding::from(16))
            .max_width(560.0)
            .on_close(close_msg)
            .style(|_theme, _status| iced_aw::style::card::Style {
                background: Background::Color(Color::from_rgb(0.12, 0.14, 0.18)),
                border_radius: 8.0,
                border_width: 1.0,
                border_color: Color::from_rgb(0.25, 0.28, 0.35),
                head_background: Background::Color(Color::TRANSPARENT),
                head_text_color: COLOR_FLOW,
                body_background: Background::Color(Color::TRANSPARENT),
                body_text_color: Color::WHITE,
                foot_background: Background::Color(Color::TRANSPARENT),
                foot_text_color: Color::WHITE,
                close_color: Color::from_rgb(0.7, 0.72, 0.78),
            });

        // 阴影包裹容器：宽度 Fill + max_width 560，与 Card 一致，
        // 这样阴影容器与 Card 同宽，外层居中容器才能把整张卡居中。
        let card = container(card)
            .width(Length::Fill)
            .max_width(560.0)
            .style(|_t| container::Style {
                shadow: Shadow {
                    color: Color::from_rgba8(0, 0, 0, 0.5),
                    ..Default::default()
                },
                ..Default::default()
            });

        // 背景遮罩 + 居中。width/height Fill 让遮罩覆盖整个窗口，
        // center_x/center_y 把卡居中。opaque 阻止事件穿透到下层。
        opaque(
            container(card)
                .width(Length::Fill)
                .height(Length::Fill)
                .center_x(Length::Fill)
                .center_y(Length::Fill)
                .style(|_t| container::Style {
                    background: Some(Background::Color(Color::from_rgba8(0, 0, 0, 0.55))),
                    ..Default::default()
                }),
        )
        .into()
    }

    fn view_about_modal(&self) -> Element<'_, Message> {
        let body = column![
            text("Akizuki*Rustgal 剧本编辑器").size(18.0).color(COLOR_SECTION),
            text(format!("Build {} · 基于 iced 0.13 的视觉小说剧本编写工具", BUILD_NUMBER))
                .size(12.0)
                .color(Color::from_rgb(0.7, 0.72, 0.78)),
            Space::new(Length::Fill, 8),
            text(format!("GitHub: {}", GITHUB_URL)).size(12.0).color(COLOR_FLOW),
            Space::new(Length::Fill, 12),
            row![
                horizontal_space(),
                button(text("关闭").size(13.0))
                    .on_press(Message::CloseAbout)
                    .padding([6, 16]),
            ],
        ]
        .spacing(4);

        self.modal_card("关于", Message::CloseAbout, body.into())
    }

    fn view_file_picker_modal(&self) -> Element<'_, Message> {
        let picker = self.file_picker.as_ref().expect("file picker open");
        let header = row![
            text(format!("打开文件 — {}", picker.current_dir.display()))
                .size(12.0)
                .color(COLOR_FLOW),
            horizontal_space(),
            button(text("上级").size(12.0))
                .on_press(Message::FilePickerParent)
                .padding([4, 8]),
        ];

        let filter = text_input("过滤", &picker.filter)
            .on_input(Message::FilePickerFilter)
            .padding(5);

        let mut list = Column::new().spacing(2);
        for entry in &picker.entries {
            let icon = if entry.is_dir { "[目录] " } else { "[文件] " };
            let active = picker.selected.as_deref() == Some(entry.name.as_str());
            list = list.push(
                button(text(format!("{}{}", icon, entry.name)).size(12.0))
                    .on_press(if entry.is_dir {
                        Message::FilePickerEnter(entry.name.clone())
                    } else {
                        Message::FilePickerSelect(entry.name.clone())
                    })
                    .padding([3, 6])
                    .width(Length::Fill)
                    .style(move |_t, _s| button::Style {
                        background: Some(Background::Color(if active {
                            Color::from_rgb(0.20, 0.24, 0.32)
                        } else {
                            Color::TRANSPARENT
                        })),
                        border: Border::default().rounded(3.0),
                        ..Default::default()
                    }),
            );
        }

        let actions = row![
            button(text("取消").size(12.0))
                .on_press(Message::FilePickerCancel)
                .padding([5, 12]),
            horizontal_space(),
            button(text("打开").size(12.0))
                .on_press(Message::FilePickerConfirm)
                .padding([5, 12]),
        ];

        let body = column![header, filter, scrollable(list).height(300), actions]
            .spacing(6)
            .width(Length::Fill);

        self.modal_card("打开文件", Message::FilePickerCancel, body.into())
    }

    fn view_dir_picker_modal(&self) -> Element<'_, Message> {
        let picker = self.dir_picker.as_ref().expect("dir picker open");
        let header = row![
            text(format!("选择项目目录 — {}", picker.current_dir.display()))
                .size(12.0)
                .color(COLOR_FLOW),
            horizontal_space(),
            button(text("上级").size(12.0))
                .on_press(Message::DirPickerParent)
                .padding([4, 8]),
        ];

        let filter = text_input("过滤", &picker.filter)
            .on_input(Message::DirPickerFilter)
            .padding(5);

        let mut list = Column::new().spacing(2);
        for entry in &picker.entries {
            if !entry.is_dir {
                continue;
            }
            list = list.push(
                button(text(format!("[目录] {}", entry.name)).size(12.0))
                    .on_press(Message::DirPickerEnter(entry.name.clone()))
                    .padding([3, 6])
                    .width(Length::Fill)
                    .style(|_t, _s| button::Style {
                        background: Some(Background::Color(Color::TRANSPARENT)),
                        border: Border::default().rounded(3.0),
                        ..Default::default()
                    }),
            );
        }

        let actions = row![
            button(text("取消").size(12.0))
                .on_press(Message::DirPickerCancel)
                .padding([5, 12]),
            horizontal_space(),
            button(text("选择此目录").size(12.0))
                .on_press(Message::DirPickerConfirm)
                .padding([5, 12]),
        ];

        let body = column![header, filter, scrollable(list).height(300), actions]
            .spacing(6)
            .width(Length::Fill);

        self.modal_card("打开项目", Message::DirPickerCancel, body.into())
    }

    fn view_find_replace_modal(&self) -> Element<'_, Message> {
        // 命中数：实时统计当前查找串在编辑器中的出现次数
        let count = self.find_count(&self.find_replace_target);
        let count_text = if self.find_replace_target.is_empty() {
            String::new()
        } else {
            format!("命中 {} 处", count)
        };

        // 替换插入模式：find_replace_syntax 非空时，弹窗以「替换插入」为标题，
        // 提示用户输入要被替换的查找内容（替换值固定为缓存的语法）。
        let is_replace_insert = !self.find_replace_syntax.is_empty();
        let title_str = if is_replace_insert { "替换插入" } else { "查找替换" };
        let hint = if is_replace_insert {
            let target_disp = if self.find_replace_target.is_empty() {
                "查找内容".to_string()
            } else {
                self.find_replace_target.clone()
            };
            format!("将把脚本中匹配【{}】的部分替换为生成的语法", target_disp)
        } else {
            String::new()
        };

        let mut body = Column::new().spacing(6).width(Length::Fill);
        body = body.push(text(title_str).size(14.0).color(COLOR_FLOW));
        if is_replace_insert {
            body = body.push(
                text(hint)
                    .size(11.0)
                    .color(Color::from_rgb(0.9, 0.75, 0.3)),
            );
        }
        body = body.push(
            text("查找内容")
                .size(11.0)
                .color(Color::from_rgb(0.6, 0.62, 0.68)),
        );
        body = body.push(
            text_input("查找内容", &self.find_replace_target)
                .on_input(Message::FindReplaceTarget)
                .on_submit(Message::FindNext)
                .padding(6),
        );
        body = body.push(
            text("替换为")
                .size(11.0)
                .color(Color::from_rgb(0.6, 0.62, 0.68)),
        );
        body = body.push(
            text_input("替换内容", &self.find_replace_replacement)
                .on_input(Message::FindReplaceReplacement)
                .on_submit(Message::DoFindReplace)
                .padding(6),
        );
        body = body.push(text(count_text).size(11.0).color(COLOR_CHOICE));
        body = body.push(
            row![
                button(text("查找下一个").size(12.0))
                    .on_press(Message::FindNext)
                    .padding([5, 12]),
                button(text("全部替换").size(12.0))
                    .on_press(Message::DoFindReplace)
                    .padding([5, 12]),
                horizontal_space(),
                button(text("关闭").size(12.0))
                    .on_press(Message::ToggleFindReplace)
                    .padding([5, 12]),
            ],
        );

        self.modal_card(title_str, Message::ToggleFindReplace, body.into())
    }

    fn view_project_settings_modal(&self) -> Element<'_, Message> {
        let body = column![
            text("项目设置（project.json）").size(14.0).color(COLOR_FLOW),
            text("标题").size(11.0).color(Color::from_rgb(0.6, 0.62, 0.68)),
            text_input("标题", &self.project_config.title)
                .on_input(Message::ProjectTitle)
                .padding(5),
            text("副标题").size(11.0).color(Color::from_rgb(0.6, 0.62, 0.68)),
            text_input("副标题", &self.project_config.subtitle)
                .on_input(Message::ProjectSubtitle)
                .padding(5),
            text("主剧本文件").size(11.0).color(Color::from_rgb(0.6, 0.62, 0.68)),
            text_input("main.akrs", &self.project_config.main_script)
                .on_input(Message::ProjectMainScript)
                .padding(5),
            text("作者").size(11.0).color(Color::from_rgb(0.6, 0.62, 0.68)),
            text_input("作者", &self.project_config.author)
                .on_input(Message::ProjectAuthor)
                .padding(5),
            text("语言").size(11.0).color(Color::from_rgb(0.6, 0.62, 0.68)),
            text_input("zh-CN", &self.project_config.language)
                .on_input(Message::ProjectLanguage)
                .padding(5),
            text("描述").size(11.0).color(Color::from_rgb(0.6, 0.62, 0.68)),
            text_input("描述", &self.project_config.description)
                .on_input(Message::ProjectDescription)
                .padding(5),
            Space::new(Length::Fill, 6),
            row![
                button(text("保存").size(12.0))
                    .on_press(Message::SaveProjectConfig)
                    .padding([5, 12]),
                horizontal_space(),
                button(text("关闭").size(12.0))
                    .on_press(Message::CloseProjectSettings)
                    .padding([5, 12]),
            ],
        ]
        .spacing(4)
        .width(Length::Fill);

        self.modal_card("项目设置", Message::CloseProjectSettings, body.into())
    }

    fn view_build_modal(&self) -> Element<'_, Message> {
        let mut platforms = Column::new().spacing(4);
        for p in BuildPlatform::all() {
            let checked = *self.build.selected.get(&p).unwrap_or(&false);
            let mark = if checked { "[x] " } else { "[ ] " };
            platforms = platforms.push(
                button(text(format!("{}{}", mark, p.label())).size(12.0))
                    .on_press(Message::ToggleBuildPlatform(p))
                    .padding([4, 8])
                    .style(|_t, _s| button::Style {
                        background: Some(Background::Color(Color::from_rgb(0.12, 0.14, 0.18))),
                        border: Border::default().rounded(4.0),
                        ..Default::default()
                    }),
            );
        }

        let log = if self.build.log.is_empty() {
            "（暂无日志）".to_string()
        } else {
            self.build.log.clone()
        };

        // 构建结果摘要：成功/失败平台列表 + 产物目录
        let mut result_col = Column::new().spacing(2);
        if self.build.done {
            if !self.build.succeeded.is_empty() {
                let names: Vec<String> =
                    self.build.succeeded.iter().map(|p| p.label().to_string()).collect();
                result_col = result_col.push(
                    text(format!("[成功] 成功：{}", names.join(", ")))
                        .size(11.0)
                        .color(COLOR_CHOICE),
                );
            }
            if !self.build.failed.is_empty() {
                let names: Vec<String> =
                    self.build.failed.iter().map(|(p, _)| p.label().to_string()).collect();
                result_col = result_col.push(
                    text(format!("[失败] 失败：{}", names.join(", ")))
                        .size(11.0)
                        .color(COLOR_PAIR_HIGHLIGHT),
                );
            }
            result_col = result_col.push(
                text(format!("产物目录：{}", self.build.output_dir.display()))
                    .size(11.0)
                    .color(COLOR_FLOW),
            );
        }

        // 是否禁用「开始打包」：构建进行中
        let building = self.build.is_building();
        let start_btn = button(text("开始打包").size(12.0))
            .on_press_maybe(if building { None } else { Some(Message::StartBuild) })
            .padding([5, 12]);

        let body = column![
            text("打包（cargo build --release --target）").size(14.0).color(COLOR_FLOW),
            platforms,
            row![
                start_btn,
                button(text("打开产物目录").size(12.0))
                    .on_press(Message::OpenBuildFolder)
                    .padding([5, 12]),
                horizontal_space(),
                button(text("关闭").size(12.0))
                    .on_press(Message::ToggleBuildDialog)
                    .padding([5, 12]),
            ],
            result_col,
            scrollable(text(log).font(Font::MONOSPACE).size(11.0).color(Color::from_rgb(0.8, 0.85, 0.9)))
                .height(220),
        ]
        .spacing(6)
        .width(Length::Fill);

        self.modal_card("打包", Message::ToggleBuildDialog, body.into())
    }

    fn view_cargo_guide_modal(&self) -> Element<'_, Message> {
        let body = column![
            text("未检测到 cargo").size(15.0).color(COLOR_FLOW),
            text("请先安装 Rust 工具链（https://rustup.rs）后再使用运行/打包功能。")
                .size(12.0)
                .color(Color::from_rgb(0.75, 0.78, 0.84)),
            text(format!("也可访问官网了解：{}", RUST_LANG_URL))
                .size(11.0)
                .color(Color::from_rgb(0.6, 0.62, 0.68)),
            Space::new(Length::Fill, 8),
            row![
                button(text("打开 rust-lang.org").size(12.0))
                    .on_press(Message::OpenRustLang)
                    .padding([5, 12]),
                horizontal_space(),
                button(text("知道了").size(12.0))
                    .on_press(Message::CloseCargoGuide)
                    .padding([5, 12]),
            ],
        ]
        .spacing(4)
        .width(Length::Fill);

        self.modal_card("提示", Message::CloseCargoGuide, body.into())
    }

    fn view_save_reminder_modal(&self) -> Element<'_, Message> {
        let body = column![
            text("当前文件尚未保存到磁盘，无法预览。").size(14.0).color(COLOR_FLOW),
            text("请先按 Ctrl+S 保存文件（或用左栏文件名输入框命名后保存），再点击预览。")
                .size(12.0)
                .color(Color::from_rgb(0.75, 0.78, 0.84)),
            Space::new(Length::Fill, 8),
            row![
                horizontal_space(),
                button(text("知道了").size(12.0))
                    .on_press(Message::CloseSaveReminder)
                    .padding([5, 12]),
            ],
        ]
        .spacing(4)
        .width(Length::Fill);

        self.modal_card("无法预览", Message::CloseSaveReminder, body.into())
    }

    fn view_project_warning_modal(&self) -> Element<'_, Message> {
        // 内容包入 scrollable 防止过长警告文字被截断。
        let warning_text = scrollable(
            text(self.project_config.warning.clone())
                .size(13.0)
                .color(Color::from_rgb(0.85, 0.82, 0.7)),
        )
        .width(Length::Fill)
        .height(240);

        let body = column![
            text("作者留下了以下提示：").size(12.0).color(COLOR_FLOW),
            warning_text,
            Space::new(Length::Fill, 8),
            row![
                button(text("不再显示").size(12.0))
                    .on_press(Message::DismissProjectWarning)
                    .padding([5, 12]),
                horizontal_space(),
                button(text("确定").size(12.0))
                    .on_press(Message::CloseProjectWarning)
                    .padding([5, 12]),
            ],
        ]
        .spacing(6)
        .width(Length::Fill);

        self.modal_card("项目警告", Message::CloseProjectWarning, body.into())
    }

    fn view_title_warning_modal(&self) -> Element<'_, Message> {
        let warning = self.title_warning.as_ref().expect("title warning open");
        let body = column![
            text("标题过长警告").size(15.0).color(COLOR_PAIR_HIGHLIGHT),
            text(format!("新标题「{}」或副标题过长，可能影响标题页显示效果。", warning.new_title))
                .size(12.0)
                .color(Color::from_rgb(0.8, 0.82, 0.88)),
            Space::new(Length::Fill, 8),
            row![
                button(text("仍然应用").size(12.0))
                    .on_press(Message::ConfirmTitleWarning)
                    .padding([5, 12]),
                horizontal_space(),
                button(text("取消").size(12.0))
                    .on_press(Message::CancelTitleWarning)
                    .padding([5, 12]),
            ],
        ]
        .spacing(4)
        .width(Length::Fill);

        self.modal_card("警告", Message::CancelTitleWarning, body.into())
    }

    // -- rpy 导入弹窗 -----------------------------------------------------

    fn view_rpy_import_modal(&self) -> Element<'_, Message> {
        let source_str = self
            .rpy_import_source
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_default();
        let target_str = self.rpy_import_target.display().to_string();

        // 警告/结果列表
        let mut warn_col = Column::new().spacing(2);
        for w in &self.rpy_import_warnings {
            let color = if w.starts_with("转换完成") {
                COLOR_CHOICE
            } else if w.contains("警告") || w.contains("请先") {
                COLOR_PAIR_HIGHLIGHT
            } else {
                Color::from_rgb(0.8, 0.82, 0.88)
            };
            warn_col = warn_col.push(text(w.clone()).size(11.0).color(color));
        }

        let body = column![
            text("将 Ren'Py .rpy 脚本转换为 .akrs 格式").size(14.0).color(COLOR_FLOW),
            text("源 .rpy 文件").size(11.0).color(Color::from_rgb(0.6, 0.62, 0.68)),
            text_input("例如 script.rpy", &source_str)
                .on_input(Message::RpyImportSourceInput)
                .padding(5),
            text("目标 .akrs 文件（留空则自动同名）")
                .size(11.0)
                .color(Color::from_rgb(0.6, 0.62, 0.68)),
            text_input("例如 script.akrs", &target_str)
                .on_input(Message::RpyImportTargetInput)
                .padding(5),
            warn_col,
            Space::new(Length::Fill, 6),
            row![
                button(text("开始转换").size(12.0))
                    .on_press(Message::DoRpyImport)
                    .padding([5, 12]),
                horizontal_space(),
                button(text("关闭").size(12.0))
                    .on_press(Message::ToggleRpyImport)
                    .padding([5, 12]),
            ],
        ]
        .spacing(6)
        .width(Length::Fill);

        self.modal_card("导入 rpy", Message::ToggleRpyImport, body.into())
    }

    // -- 放大预览弹窗 -----------------------------------------------------

    fn view_enlarged_preview_modal(&self) -> Element<'_, Message> {
        // 复用剧本预览内容，放进更大尺寸的 modal 中
        let preview = self.view_script_preview();
        let body = container(preview)
            .max_width(720)
            .padding(8)
            .style(|_t| container::Style {
                background: Some(Background::Color(Color::from_rgb(0.10, 0.12, 0.16))),
                border: Border::default().rounded(6.0),
                ..Default::default()
            });

        let body = column![
            body,
            row![
                horizontal_space(),
                button(text("关闭").size(12.0))
                    .on_press(Message::CloseEnlargedPreview)
                    .padding([5, 12]),
            ],
        ]
        .spacing(8)
        .width(Length::Fill);

        self.modal_card("剧本预览（放大）", Message::CloseEnlargedPreview, body.into())
    }

    // -- 快捷键帮助弹窗 ---------------------------------------------------

    fn view_shortcuts_modal(&self) -> Element<'_, Message> {
        let shortcuts = [
            ("Ctrl + S", "保存当前剧本"),
            ("Ctrl + N", "新建剧本"),
            ("Ctrl + O", "打开已有剧本"),
            ("Ctrl + F", "打开/关闭查找替换"),
            ("Ctrl + R", "运行剧本预览"),
            ("Ctrl + H", "打开/关闭快捷键帮助"),
            ("F5", "运行当前脚本"),
        ];

        let mut list = Column::new().spacing(3);
        for (key, desc) in shortcuts {
            list = list.push(
                row![
                    text(key).size(12.0).color(COLOR_FLOW).width(Length::Fixed(110.0)),
                    text(desc).size(12.0).color(Color::from_rgb(0.8, 0.82, 0.88)),
                ]
                .spacing(8),
            );
        }

        let body = column![
            text("快捷键").size(14.0).color(COLOR_FLOW),
            list,
            Space::new(Length::Fill, 6),
            row![
                horizontal_space(),
                button(text("关闭").size(12.0))
                    .on_press(Message::ToggleShortcuts)
                    .padding([5, 12]),
            ],
        ]
        .spacing(6)
        .width(Length::Fill);

        self.modal_card("快捷键", Message::ToggleShortcuts, body.into())
    }
}

// ---------------------------------------------------------------------------
// 入口
// ---------------------------------------------------------------------------

/// 启动 GUI 编辑器应用。
pub fn run_editor() -> Result<(), Box<dyn std::error::Error>> {
    // 初始化统一日志：默认 warn+，设 RUST_LOG=debug 可看调试日志。
    let _ = env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).try_init();

    let icon = load_icon();

    let mut window = iced::window::Settings::default();
    window.size = iced::Size::new(1280.0, 820.0);
    window.resizable = true;
    window.icon = icon;

    let app = iced::application(
        "Akizuki*Rustgal 剧本编辑器",
        EditorApp::update,
        EditorApp::view,
    );

    // 注册 CJK 字体：用 cosmic-text 字形回退渲染中文。
    // 关键：不要用 Font::with_name(具体名字) 设默认字体——因为加载到的字体
    // 可能是 SourceHanSansSC（family=思源黑体），也可能是 Windows 系统回退
    // msyh.ttc（family=Microsoft YaHei）或 macOS 的 PingFang（family=PingFang SC）。
    // family name 不匹配时 cosmic-text 找不到对应字体，中文全显示为方框。
    // 正确做法：仅 .font(bytes) 注册字体字节，默认字体保持 Font::DEFAULT
    // （Family::SansSerif）。cosmic-text 会把注册的字体作为 SansSerif 的字形
    // 回退，无论其实际 family name 是什么都能命中中文字形。
    let app = match install_cjk_fonts() {
        Some(bytes) => app.font(bytes),
        None => app,
    };

    let app = app.window(window);

    // 使用 EditorApp::subscription：合并 100ms 轮询 + 键盘快捷键
    let app = app.subscription(EditorApp::subscription);

    app.run()
        .map_err(|e| Box::new(e) as Box<dyn std::error::Error>)?;
    Ok(())
}

/// 键盘快捷键处理：将按键事件映射为 Message。
/// - Ctrl+S 保存 | Ctrl+N 新建 | Ctrl+O 打开 | Ctrl+F 查找替换
/// - F5 运行 | Ctrl+R 启动游戏预览 | Ctrl+H 快捷键帮助
fn handle_key_press(key: Key, modifiers: Modifiers) -> Option<Message> {
    use iced::keyboard::key::Named;
    if modifiers.control() {
        match key.as_ref() {
            Key::Character(c) => match c {
                "s" | "S" => Some(Message::SaveFile),
                "n" | "N" => Some(Message::NewFile),
                "o" | "O" => Some(Message::OpenFilePicker),
                "f" | "F" => Some(Message::ToggleFindReplace),
                "r" | "R" => Some(Message::StartGamePreview),
                "h" | "H" => Some(Message::ToggleShortcuts),
                "j" | "J" => Some(Message::JumpToPreviewFromCursor),
                _ => None,
            },
            _ => None,
        }
    } else if modifiers.is_empty() {
        if let Key::Named(Named::F5) = key.as_ref() {
            Some(Message::RunScript)
        } else {
            None
        }
    } else {
        None
    }
}

/// 全局事件过滤器：追踪 Ctrl 键按下/释放状态。
/// 用于实现 Ctrl+点击剧本行跳转预览（iced text_editor 不暴露带修饰键的点击事件）。
fn track_modifiers(
    event: iced::Event,
    _status: iced::event::Status,
    _window: iced::window::Id,
) -> Option<Message> {
    match event {
        iced::Event::Keyboard(keyboard::Event::KeyPressed { modifiers, .. })
        | iced::Event::Keyboard(keyboard::Event::KeyReleased { modifiers, .. }) => {
            Some(Message::ModifiersChanged(modifiers.control()))
        }
        _ => None,
    }
}

/// 加载外部中文字体字节（优先加载运行时字体文件，其次系统字体）。
///
/// 返回 `Some(bytes)` 表示找到可用 CJK 字体，由 `run_editor` 通过
/// `iced::application(...).font(bytes)` 注册，交由 cosmic-text 做字形回退。
fn install_cjk_fonts() -> Option<Vec<u8>> {
    let rel_path = "assets/fonts/SourceHanSansSC-Regular-2.otf";

    // 1. 运行时外部字体文件：从 CWD 和 exe 目录各自向上遍历目录树，
    //    找到含 assets/fonts/ 的祖先目录。这样无论从项目根、target/debug、
    //    还是其他位置运行都能找到字体文件。
    let mut roots: Vec<PathBuf> = Vec::new();
    if let Ok(cwd) = std::env::current_dir() {
        roots.push(cwd);
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(exe_dir) = exe.parent() {
            roots.push(exe_dir.to_path_buf());
        }
    }
    for root in &roots {
        let mut dir: Option<&std::path::Path> = Some(root.as_path());
        while let Some(d) = dir {
            let candidate = d.join(rel_path);
            if let Ok(bytes) = std::fs::read(&candidate) {
                eprintln!("[editor] 中文字体已加载: {}", candidate.display());
                return Some(bytes);
            }
            dir = d.parent();
        }
    }

    // 2. 系统字体回退（按平台分别列出候选路径）
    let sys_candidates = system_cjk_font_path();
    for path in &sys_candidates {
        if let Ok(bytes) = std::fs::read(path) {
            eprintln!("[editor] 使用系统中文字体: {}", path);
            return Some(bytes);
        }
    }

    eprintln!("[editor] 警告：未找到中文字体，中文可能无法正确显示");
    None
}

/// 返回当前平台的系统 CJK 字体候选路径列表。
fn system_cjk_font_path() -> Vec<String> {
    let mut candidates: Vec<String> = Vec::new();
    #[cfg(target_os = "windows")]
    {
        let win_dir = std::env::var("WINDIR").unwrap_or_else(|_| "C:\\Windows".to_string());
        let fonts_dir = std::path::Path::new(&win_dir).join("Fonts");
        for name in &["msyh.ttc", "msyh.ttf", "msyhbd.ttc", "simsun.ttc", "simhei.ttf"] {
            candidates.push(fonts_dir.join(name).to_string_lossy().into_owned());
        }
        if let Some(home) = std::env::var_os("USERPROFILE") {
            let user_fonts = std::path::Path::new(&home)
                .join("AppData/Local/Microsoft/Windows/Fonts/msyh.ttc");
            candidates.push(user_fonts.to_string_lossy().into_owned());
        }
    }
    #[cfg(target_os = "macos")]
    {
        for path in &[
            "/System/Library/Fonts/PingFang.ttc",
            "/Library/Fonts/PingFang.ttc",
            "/System/Library/Fonts/STHeiti Light.ttc",
        ] {
            candidates.push((*path).to_string());
        }
    }
    #[cfg(target_os = "linux")]
    {
        for path in &[
            "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
            "/usr/share/fonts/truetype/noto/NotoSansCJK-Regular.ttc",
            "/usr/share/fonts/truetype/wqy/wqy-microhei.ttc",
            "/usr/share/fonts/wenquanyi/wqy-microhei/wqy-microhei.ttc",
        ] {
            candidates.push((*path).to_string());
        }
    }
    candidates
}

/// 从嵌入的 RGBA 数据加载窗口图标。
fn load_icon() -> Option<iced::window::Icon> {
    let data = include_bytes!("../../../assets/icon_kokona_64.bin");
    const W: u32 = 64;
    const H: u32 = 64;
    if data.len() == (W * H * 4) as usize {
        iced::window::icon::from_rgba(data.to_vec(), W, H).ok()
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// 辅助函数：文件名清理、诊断格式化、标签
// ---------------------------------------------------------------------------

/// 从 PNG 文件加载 iced image Handle（RGBA 格式）。
/// 注意：`image` 在本模块中被 `iced::widget::image` 遮蔽，
/// 故用 `::image::` 前缀引用外部 image crate。
fn load_png_handle(path: &Path) -> Result<iced::widget::image::Handle, String> {
    let data = std::fs::read(path).map_err(|e| format!("{}", e))?;
    let img = ::image::load_from_memory(&data)
        .map_err(|e| format!("{}", e))?
        .to_rgba8();
    let (w, h) = img.dimensions();
    Ok(iced::widget::image::Handle::from_rgba(w, h, img.into_raw()))
}

/// 推断项目根目录：从给定文件路径向上逐级查找，直到找到含 `project.json`
/// 或 `assets/` 子目录的祖先目录。找不到则返回 None。
/// 用于打开单个剧本文件时自动定位资源根。
fn infer_project_root(file_path: &Path) -> Option<PathBuf> {
    let mut dir = file_path.parent()?;
    loop {
        if dir.join("project.json").is_file() || dir.join("assets").is_dir() {
            return Some(dir.to_path_buf());
        }
        dir = match dir.parent() {
            Some(p) => p,
            None => return None,
        };
    }
}

/// 在系统文件管理器中打开路径。
fn open_path_in_file_manager(path: &Path) {
    #[cfg(target_os = "windows")]
    {
        let _ = Command::new("explorer").arg(path).spawn();
    }
    #[cfg(target_os = "macos")]
    {
        let _ = Command::new("open").arg(path).spawn();
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let _ = Command::new("xdg-open").arg(path).spawn();
    }
}

/// 在系统默认浏览器中打开 URL。
fn open_url_in_browser(url: &str) {
    #[cfg(target_os = "windows")]
    {
        let _ = Command::new("cmd").args(["/C", "start", url]).spawn();
    }
    #[cfg(target_os = "macos")]
    {
        let _ = Command::new("open").arg(url).spawn();
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let _ = Command::new("xdg-open").arg(url).spawn();
    }
}

/// 从 Cargo.toml 读取所有 `[[bin]]` 定义的二进制目标名。
/// 查找 workspace 根目录和各成员 crate 的 Cargo.toml。
/// 如无法读取，使用项目目录名作为后备。
fn read_binary_names(work_dir: &Path) -> Vec<String> {
    let mut names = Vec::new();

    // 搜索 workspace 根和 crates/ 子目录下的 Cargo.toml
    let candidates = [
        work_dir.join("Cargo.toml"),
        work_dir.join("crates").join("akrs-game").join("Cargo.toml"),
    ];

    for cargo_toml_path in &candidates {
        if !cargo_toml_path.exists() {
            continue;
        }
        let content = match std::fs::read_to_string(cargo_toml_path) {
            Ok(c) => c,
            Err(_) => continue,
        };

        let mut in_bin_section = false;
        for line in content.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with("[[") {
                in_bin_section = trimmed == "[[bin]]";
                continue;
            }
            if trimmed.starts_with('[') {
                in_bin_section = false;
                continue;
            }
            if in_bin_section {
                if let Some(name) = trimmed.strip_prefix("name") {
                    let name = name.trim_start();
                    if let Some(name) = name.strip_prefix('=') {
                        let name = name.trim().trim_matches(|c| c == '"' || c == '\'');
                        if !name.is_empty() && !names.contains(&name.to_string()) {
                            names.push(name.to_string());
                        }
                    }
                }
            }
        }
    }

    // 后备：使用项目目录名
    if names.is_empty() {
        if let Some(dir_name) = work_dir.file_name().and_then(|n| n.to_str()) {
            names.push(dir_name.to_string());
        } else {
            names.push("akrs-game".to_string());
        }
    }

    names
}

/// 递归复制目录。
fn copy_dir_recursive(src: &Path, dst: &Path) {
    if !src.is_dir() {
        return;
    }
    let _ = std::fs::create_dir_all(dst);
    if let Ok(entries) = std::fs::read_dir(src) {
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name();
            let dest = dst.join(name);
            if path.is_dir() {
                copy_dir_recursive(&path, &dest);
            } else {
                let _ = std::fs::copy(&path, &dest);
            }
        }
    }
}

/// 规范化用户输入的文件名：去除路径分隔符并确保有 `.akrs` 扩展名。
fn sanitize_filename(input: &str) -> String {
    let mut name: String = input
        .trim()
        .chars()
        .filter(|c| !matches!(c, '/' | '\\'))
        .collect();
    if name.is_empty() {
        name = "untitled.akrs".to_string();
    }
    if !name.ends_with(".akrs") {
        name.push_str(".akrs");
    }
    name
}

/// 将编译诊断格式化为带严重性标签的单行字符串。
fn format_errors(errors: &[CompileError]) -> Vec<String> {
    errors
        .iter()
        .map(|e| {
            let sev = match e.severity {
                ErrSeverity::Error => "错误",
                ErrSeverity::Warning => "警告",
                ErrSeverity::Note => "提示",
            };
            let loc = format_location(&e.span);
            match &e.hint {
                Some(h) => format!("[{}] {} - 位于 {}（提示：{}）", sev, e.message, loc, h),
                None => format!("[{}] {} - 位于 {}", sev, e.message, loc),
            }
        })
        .collect()
}

/// 引擎阶段的可读名称。
fn phase_label(phase: EnginePhase) -> &'static str {
    match phase {
        EnginePhase::Title => "标题",
        EnginePhase::Running => "运行中",
        EnginePhase::Transitioning => "过渡中",
        EnginePhase::Waiting => "等待",
        EnginePhase::ChoicePending => "等待选择",
        EnginePhase::StoryEnded => "故事结束",
    }
}

/// 根据行首非空白 token 选择基础颜色（语法高亮的纯逻辑部分）。
fn line_base_color(trimmed: &str) -> Color {
    // 以 `-` 开头的双字符标记 `->` 必须先于单字符 `-` 方向标记检查。
    if trimmed.starts_with('#') {
        COLOR_SECTION
    } else if trimmed.starts_with("//") {
        COLOR_COMMENT
    } else if trimmed.starts_with("->")
        || trimmed.starts_with("=>")
        || trimmed.starts_with("<=")
        || trimmed.starts_with("~~")
    {
        COLOR_FLOW
    } else if trimmed.starts_with('@') {
        COLOR_COMMAND
    } else if trimmed.starts_with('+') {
        COLOR_DIRECTION
    } else if trimmed.starts_with('-') {
        COLOR_DIRECTION
    } else if trimmed.starts_with('$') {
        COLOR_VARIABLE
    } else if trimmed.starts_with('?') || trimmed.starts_with('|') {
        COLOR_CHOICE
    } else if trimmed.starts_with("ending ") || trimmed.starts_with("unlock ") {
        // 隐藏结局声明（ending "id" epilogue "path" [button "text"]）
        // 与解锁标记（unlock "id"）作为流程级关键字着色。
        COLOR_FLOW
    } else {
        COLOR_DEFAULT
    }
}

// ---------------------------------------------------------------------------
// `=>` / `<=` 流程标记配对辅助
// ---------------------------------------------------------------------------
//
// 纯文本层扫描与配对，用于编辑器的配对高亮、悬停提示与大纲视图。
// 不修改任何脚本语法或编译逻辑，仅影响视觉呈现。
//
// 配对规则（经典括号栈匹配，按文本顺序扫描，不考虑嵌套语义或作用域边界）：
// - `=>`（访问子章节）入栈，`<=`（从子章节返回）弹出栈顶 `=>` 并互相配对。
// - 栈空时遇到的 `<=` 无配对（孤立返回）；栈中剩余的 `=>` 无配对（未闭合访问）。

/// 流程标记种类。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FlowKind {
    /// `=> target`：访问子章节。
    Visit,
    /// `<=`：从子章节返回。
    Return,
}

/// 文本中扫描到的一个 `=>`/`<=` 流程标记。
#[derive(Debug, Clone, PartialEq, Eq)]
struct FlowMark {
    kind: FlowKind,
    /// 标记在全文中的起始字符索引（`=>`/`<=` 首字符）。
    char_start: usize,
    /// 标记在全文中的结束字符索引（exclusive，`=>`/`<=` 末尾）。
    char_end: usize,
    /// 标记所在行号（从 0 开始）。
    line: usize,
    /// `=>` 的目标章节名（`=> Sub` 中的 `Sub`）；`<=` 为 None。
    target: Option<String>,
}

/// 扫描全文，按文本顺序收集所有行首的 `=>`/`<=` 流程标记。
///
/// 判定与 lexer 一致：仅识别行首（去除前导空白后）的 `=>`/`<=`。表达式中的
/// `<=`（如 `if a <= b`）不会出现在行首语句位置，故按行首判定即可消歧。
/// 字符索引按 `char` 计数，非字节偏移。
fn scan_flow_marks(text: &str) -> Vec<FlowMark> {
    let mut marks = Vec::new();
    let mut line = 0usize;
    let mut line_start_char = 0usize; // 当前行首在全文中的字符索引
    for raw_line in text.split('\n') {
        let trimmed = raw_line.trim_start();
        let leading_ws = raw_line.chars().take_while(|c| c.is_whitespace()).count();
        let token_char_start = line_start_char + leading_ws;
        if let Some(rest) = trimmed.strip_prefix("=>") {
            let target = rest.trim();
            marks.push(FlowMark {
                kind: FlowKind::Visit,
                char_start: token_char_start,
                char_end: token_char_start + "=>".chars().count(),
                line,
                target: if target.is_empty() { None } else { Some(target.to_string()) },
            });
        } else if trimmed.strip_prefix("<=").is_some() {
            marks.push(FlowMark {
                kind: FlowKind::Return,
                char_start: token_char_start,
                char_end: token_char_start + "<=".chars().count(),
                line,
                target: None,
            });
        }
        line += 1;
        line_start_char += raw_line.chars().count() + 1; // +1 为换行符
    }
    marks
}

/// 计算每个 FlowMark 的配对索引。
///
/// 返回 `pairs[i]` = 第 i 个 mark 的配对 mark 索引，无配对为 `None`。
fn compute_flow_pairs(marks: &[FlowMark]) -> Vec<Option<usize>> {
    let mut pairs = vec![None; marks.len()];
    let mut stack: Vec<usize> = Vec::new(); // 待配对的 Visit 索引
    for (i, m) in marks.iter().enumerate() {
        match m.kind {
            FlowKind::Visit => stack.push(i),
            FlowKind::Return => {
                if let Some(j) = stack.pop() {
                    pairs[j] = Some(i);
                    pairs[i] = Some(j);
                }
            }
        }
    }
    pairs
}

/// 查找光标所在的 FlowMark 索引。
/// `cursor_line` 为光标所在行号（从 0 开始），由 `text_editor::Content::cursor_position().0` 提供。
fn mark_at_cursor(marks: &[FlowMark], cursor_line: usize) -> Option<usize> {
    marks.iter().position(|m| m.line == cursor_line)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_appends_extension() {
        assert_eq!(sanitize_filename("demo"), "demo.akrs");
        assert_eq!(sanitize_filename("demo.akrs"), "demo.akrs");
        assert_eq!(sanitize_filename("  a/b "), "ab.akrs");
        assert_eq!(sanitize_filename(""), "untitled.akrs");
    }

    #[test]
    fn flow_marks_scan_visit_and_return() {
        let src = "# Main\nAki: \"Hi\"\n=> Sub\nAki: \"in sub\"\n<=\nAki: \"back\"\n";
        let marks = scan_flow_marks(src);
        assert_eq!(marks.len(), 2);
        assert_eq!(marks[0].kind, FlowKind::Visit);
        assert_eq!(marks[0].target.as_deref(), Some("Sub"));
        assert_eq!(marks[0].line, 2);
        assert_eq!(marks[1].kind, FlowKind::Return);
        assert_eq!(marks[1].target, None);
        assert_eq!(marks[1].line, 4);
        // char 索引：行 0 "# Main"(6) +\n=7，行1 "Aki: \"Hi\""(9)+\n=10 ->17，
        // 行2 起始 char=17，"=> Sub" 的 "=>" 在 17..19
        assert_eq!(marks[0].char_start, 17);
        assert_eq!(marks[0].char_end, 19);
    }

    #[test]
    fn flow_marks_ignore_expression_leq() {
        // `if a <= b then` 中的 <= 不在行首，不应被识别
        let src = "$x = 1\nif a <= b then\n  Aki: \"ok\"\nend\n";
        let marks = scan_flow_marks(src);
        assert!(marks.is_empty(), "表达式中的 <= 不应被识别为返回标记");
    }

    #[test]
    fn flow_marks_leading_whitespace() {
        let src = "  => Sub\n  <=\n";
        let marks = scan_flow_marks(src);
        assert_eq!(marks.len(), 2);
        assert_eq!(marks[0].char_start, 2); // 2 个空格后
        assert_eq!(marks[0].char_end, 4);
    }

    #[test]
    fn flow_pairs_basic() {
        let src = "=> A\n<=\n=> B\n<=\n";
        let marks = scan_flow_marks(src);
        let pairs = compute_flow_pairs(&marks);
        assert_eq!(pairs, vec![Some(1), Some(0), Some(3), Some(2)]);
    }

    #[test]
    fn flow_pairs_nested() {
        // 嵌套：外层 => 配最后一个 <=，内层 => 配第一个 <=
        let src = "=> Outer\n=> Inner\n<=\n<=\n";
        let marks = scan_flow_marks(src);
        let pairs = compute_flow_pairs(&marks);
        // mark0(=>Outer) - mark3(<=)
        // mark1(=>Inner) - mark2(<=)
        assert_eq!(pairs[0], Some(3));
        assert_eq!(pairs[1], Some(2));
        assert_eq!(pairs[2], Some(1));
        assert_eq!(pairs[3], Some(0));
    }

    #[test]
    fn flow_pairs_unbalanced() {
        // 孤立 <=（无 => 可配）+ 未闭合 =>
        let src = "<=\n=> A\n<=\n=> B\n";
        let marks = scan_flow_marks(src);
        let pairs = compute_flow_pairs(&marks);
        // mark0(<=) 无配对
        assert_eq!(pairs[0], None);
        // mark1(=>A) - mark2(<=)
        assert_eq!(pairs[1], Some(2));
        assert_eq!(pairs[2], Some(1));
        // mark3(=>B) 未闭合
        assert_eq!(pairs[3], None);
    }

    #[test]
    fn flow_mark_at_cursor() {
        let src = "=> Sub\n<=\n";
        let marks = scan_flow_marks(src);
        // 光标在第 0 行（=> Sub）
        assert_eq!(mark_at_cursor(&marks, 0), Some(0));
        // 光标在第 1 行（<=）
        assert_eq!(mark_at_cursor(&marks, 1), Some(1));
        // 光标在第 2 行（空行，无标记）
        assert_eq!(mark_at_cursor(&marks, 2), None);
    }

    #[test]
    fn line_base_color_classification() {
        assert_eq!(line_base_color("# Title"), COLOR_SECTION);
        assert_eq!(line_base_color("=> Sub"), COLOR_FLOW);
        assert_eq!(line_base_color("<="), COLOR_FLOW);
        assert_eq!(line_base_color("@bg school"), COLOR_COMMAND);
        assert_eq!(line_base_color("+ Aki"), COLOR_DIRECTION);
        assert_eq!(line_base_color("- Aki"), COLOR_DIRECTION);
        assert_eq!(line_base_color("$x = 1"), COLOR_VARIABLE);
        assert_eq!(line_base_color("? \"q\""), COLOR_CHOICE);
        assert_eq!(line_base_color("| \"a\""), COLOR_CHOICE);
        assert_eq!(line_base_color("// comment"), COLOR_COMMENT);
        assert_eq!(line_base_color("ending \"id\""), COLOR_FLOW);
        assert_eq!(line_base_color("plain text"), COLOR_DEFAULT);
    }

    #[test]
    fn run_script_compiles_sample() {
        let mut app = EditorApp::default();
        app.editor_content = text_editor::Content::with_text(SAMPLE_SCRIPT);
        app.show_welcome = false;
        app.run_script();
        assert!(app.engine.is_some(), "sample script should compile");
        assert!(
            app.diagnostics.iter().all(|d| !d.starts_with("[错误]")),
            "no errors expected for the sample"
        );
    }

    #[test]
    fn welcome_shown_by_default() {
        let app = EditorApp::default();
        assert!(app.show_welcome, "welcome panel should show on first launch");
        assert!(
            app.editor_content.text().trim().is_empty(),
            "editor should be empty on first launch"
        );
    }

    #[test]
    fn load_sample_hides_welcome() {
        let mut app = EditorApp::default();
        app.load_sample();
        assert!(!app.show_welcome, "welcome panel should hide after loading sample");
        assert!(
            !app.editor_content.text().is_empty(),
            "editor should have content after loading sample"
        );
    }

    #[test]
    fn new_file_hides_welcome() {
        let mut app = EditorApp::default();
        app.new_file();
        assert!(!app.show_welcome, "welcome panel should hide after new file");
    }

    #[test]
    fn find_and_replace_works() {
        let mut app = EditorApp::default();
        app.editor_content = text_editor::Content::with_text("hello world hello");
        assert!(app.find_and_replace("hello", "hi"));
        assert_eq!(app.editor_content.text(), "hi world hi\n");
        assert!(!app.find_and_replace("nonexistent", "x"));
        assert!(!app.find_and_replace("", "x"));
    }
}
