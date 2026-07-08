//! Akizuki*Rustgal 剧本编辑器
//!
//! 基于 `egui 0.21` + `eframe 0.21` 构建的视觉小说剧本编辑器。提供三栏布局：
//!
//! - **左栏**：工作目录中的 `.akrs` 文件列表，支持新建 / 打开 / 保存。
//! - **中栏**：多行脚本编辑器，带基础语法高亮。
//! - **右栏**：由 `akrs_runtime::Engine` 驱动的实时预览。
//! - **顶部工具栏**：新建 / 打开 / 保存 / 运行等操作。
//! - **底部状态栏**：编译诊断信息（错误 / 警告 / 提示）。
//!
//! 所有文件操作使用 `Result` 风格的错误处理，不会 panic；失败信息显示在底部状态栏。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::io::Read;

use eframe::egui;
use egui::{ColorImage, TextureHandle};

use akrs_core::{compile, format_location, CompileError, ErrSeverity, Position, ProjectConfig, RecentProjects};
use akrs_runtime::{Engine, EnginePhase};

// ---------------------------------------------------------------------------
// 语法高亮调色板（不透明 RGB，源自规格文档的 RGBA 值）
// ---------------------------------------------------------------------------

/// `#` 章节标题 -> (0.9, 0.8, 1.0)
const COLOR_SECTION: egui::Color32 = egui::Color32::from_rgb(229, 204, 255);
/// `->` `=>` `<=` `~~` 流程控制 -> (1.0, 0.6, 0.3)
const COLOR_FLOW: egui::Color32 = egui::Color32::from_rgb(255, 153, 76);
/// `@` 指令 -> (0.3, 0.8, 0.3)
const COLOR_COMMAND: egui::Color32 = egui::Color32::from_rgb(76, 204, 76);
/// `+` `-` 角色方向 -> (0.3, 0.7, 1.0)
const COLOR_DIRECTION: egui::Color32 = egui::Color32::from_rgb(76, 178, 255);
/// `$` 变量操作 -> (1.0, 0.8, 0.3)
const COLOR_VARIABLE: egui::Color32 = egui::Color32::from_rgb(255, 204, 76);
/// `?` `|` 选择分支 -> (0.8, 0.3, 0.8)
const COLOR_CHOICE: egui::Color32 = egui::Color32::from_rgb(204, 76, 204);
/// `--` 注释 -> (0.4, 0.4, 0.4)
const COLOR_COMMENT: egui::Color32 = egui::Color32::from_rgb(102, 102, 102);
/// `"..."` 字符串 -> (0.9, 0.9, 0.4)
const COLOR_STRING: egui::Color32 = egui::Color32::from_rgb(229, 229, 102);
/// 默认文字 -> 白色
const COLOR_DEFAULT: egui::Color32 = egui::Color32::from_rgb(255, 255, 255);

const FONT_SIZE: f32 = 14.0;

/// 「新建」操作使用的小型有效模板。
const NEW_TEMPLATE: &str = "# Start\n\n~~\n";

/// 首次启动或点击「打开示例剧本」时加载的丰富示例。
const SAMPLE_SCRIPT: &str = r#"# Start

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

/// GitHub 仓库链接
const GITHUB_URL: &str = "https://github.com/AkizukiKokona/Akizuki-Rustgal";

// ---------------------------------------------------------------------------
// 立绘预览
// ---------------------------------------------------------------------------

/// 右栏预览的标签页。
#[derive(Clone, Copy, PartialEq)]
enum PreviewTab {
    /// 剧本运行预览。
    Script,
    /// 立绘摆放预览（无需启动游戏进程）。
    Sprite,
    /// 背景预览。
    Background,
    /// 音乐预览。
    Music,
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
    textures: HashMap<String, TextureHandle>,
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
    textures: HashMap<String, TextureHandle>,
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

/// 编辑器的 egui 应用主体。
pub struct EditorApp {
    /// 中栏编辑器中显示的当前脚本文本。
    editor_content: String,
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
    /// 上一帧的时间（秒），用于计算引擎的增量时间。
    last_time: f64,
    /// 是否已应用暗色主题。
    theme_applied: bool,
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
    /// 查找替换对话框中待插入的语法（缓存）。
    find_replace_syntax: String,
    /// 文件选择对话框状态（None 表示未打开）。
    file_picker: Option<FilePickerState>,
    /// 当前项目配置（project.json）。
    project_config: ProjectConfig,
    /// 是否已经加载了项目配置。
    project_loaded: bool,
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
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
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
            editor_content: String::new(),
            current_file: None,
            work_dir,
            file_name_input: "untitled.akrs".to_string(),
            file_list: Vec::new(),
            engine: None,
            status: "就绪".to_string(),
            diagnostics: Vec::new(),
            last_time: 0.0,
            theme_applied: false,
            show_welcome: true,
            show_about: false,
            preview_tab: PreviewTab::Script,
            sprite_preview: SpritePreview::default(),
            bg_preview: BackgroundPreview::default(),
            music_preview: MusicPreview::default(),
            show_enlarged_preview: false,
            show_find_replace_dialog: false,
            find_replace_target: String::new(),
            find_replace_syntax: String::new(),
            file_picker: None,
            project_config: ProjectConfig::default(),
            project_loaded: false,
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

    /// 智能插入语法：如果剪贴板内容存在于脚本中，替换第一个匹配；
    /// 否则追加到脚本末尾。
    fn smart_insert_syntax(&mut self, syntax: &str, ctx: &egui::Context) {
        // 获取剪贴板内容
        let clipboard_text = ctx.input(|i| i.raw.events.iter().filter_map(|e| {
            if let egui::Event::Paste(s) = e { Some(s.clone()) } else { None }
        }).next());

        // 如果剪贴板有内容且在脚本中能找到，则替换
        if let Some(clip) = clipboard_text {
            if !clip.is_empty() && self.editor_content.contains(&clip) {
                // 替换第一个匹配
                self.editor_content = self.editor_content.replace(&clip, syntax);
                self.status = "已替换剪贴板内容为生成的语法".to_string();
                return;
            }
        }

        // 否则追加到末尾
        self.editor_content.push_str(&format!("{}\n", syntax));
        self.status = "语法已追加到脚本末尾".to_string();
    }

    /// 替换剪贴板内容为生成的语法。
    fn replace_clipboard_with_syntax(&mut self, syntax: &str, ctx: &egui::Context) {
        // 尝试从系统剪贴板获取文本
        let clipboard_text = ctx.input(|i| {
            // 从最近的 Paste 事件获取
            i.raw.events.iter().filter_map(|e| {
                if let egui::Event::Paste(s) = e { Some(s.clone()) } else { None }
            }).next()
        });

        // 如果没有从事件获取，尝试使用 output 的剪贴板（用户之前复制过的）
        let clip = clipboard_text.or_else(|| {
            // 尝试读取系统剪贴板（需要特殊处理）
            None // egui 不提供直接读取剪贴板的方法
        });

        // 如果剪贴板内容存在于脚本中，则替换
        if let Some(clip_text) = clip {
            if !clip_text.is_empty() && self.editor_content.contains(&clip_text) {
                self.editor_content = self.editor_content.replace(&clip_text, syntax);
                self.status = "已替换剪贴板内容为生成的语法".to_string();
            } else {
                self.editor_content.push_str(&format!("{}\n", syntax));
                self.status = "剪贴板内容未在脚本中找到，语法已追加到末尾".to_string();
            }
        } else {
            self.editor_content.push_str(&format!("{}\n", syntax));
            self.status = "剪贴板为空，语法已追加到末尾".to_string();
        }
    }

    // -- 辅助方法：查找并替换 -----------------------------------------------

    /// 查找脚本中匹配 target 的内容，替换为 replacement。
    /// 返回是否成功替换。
    fn find_and_replace(&mut self, target: &str, replacement: &str) -> bool {
        if target.is_empty() || !self.editor_content.contains(target) {
            return false;
        }
        self.editor_content = self.editor_content.replace(target, replacement);
        true
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
        self.editor_content = NEW_TEMPLATE.to_string();
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
                self.editor_content = content;
                self.current_file = Some(path.to_path_buf());
                self.file_name_input = name.clone();
                // 如果文件在 work_dir 下，刷新文件列表并切换工作目录到文件所在目录
                if let Some(parent) = path.parent() {
                    self.work_dir = parent.to_path_buf();
                    self.refresh_file_list();
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
        self.editor_content = SAMPLE_SCRIPT.to_string();
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
        match std::fs::write(&path, &self.editor_content) {
            Ok(()) => {
                self.current_file = Some(path);
                self.file_name_input = name.clone();
                self.status = format!("已保存 {}", name);
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
        self.work_dir = project_dir.to_path_buf();
        self.refresh_file_list();

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

    /// 尝试设置标题，如果标题过长则显示警告对话框。
    fn try_set_title(&mut self, new_title: String) {
        let temp_config = ProjectConfig {
            title: new_title.clone(),
            subtitle: self.project_config.subtitle.clone(),
            ..self.project_config.clone()
        };
        if temp_config.is_title_too_long() {
            self.title_warning = Some(TitleWarningState {
                new_title,
                new_subtitle: self.project_config.subtitle.clone(),
            });
        } else {
            self.project_config.title = new_title;
            self.save_project_config();
        }
    }

    /// 尝试设置副标题，如果副标题过长则显示警告对话框。
    fn try_set_subtitle(&mut self, new_subtitle: String) {
        let temp_config = ProjectConfig {
            title: self.project_config.title.clone(),
            subtitle: new_subtitle.clone(),
            ..self.project_config.clone()
        };
        if temp_config.is_subtitle_too_long() {
            self.title_warning = Some(TitleWarningState {
                new_title: self.project_config.title.clone(),
                new_subtitle,
            });
        } else {
            self.project_config.subtitle = new_subtitle;
            self.save_project_config();
        }
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
        let (program, errors) = compile(&self.editor_content);
        self.diagnostics = format_errors(&errors);

        match program {
            Some(_) => match Engine::start_running(&self.editor_content) {
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
        for (line_idx, line) in self.editor_content.lines().enumerate() {
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

        // 先保存当前文件
        if self.current_file.is_some() {
            self.save_file();
        }

        match Command::new("cargo")
            .arg("run")
            .arg("--release")
            .arg("-p")
            .arg("akrs-game")
            .current_dir(&self.work_dir)
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .spawn()
        {
            Ok(child) => {
                self.game_process = Some(child);
                self.status = "游戏预览已启动（独立窗口）".to_string();
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

    /// 每帧调用，推进构建队列。
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
                        self.build.log_line(format!("✓ {} 构建成功", platform.label()));
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
                        self.build.log_line(format!("✗ {} 构建失败 ({})", platform.label(), fail_msg));
                        self.build.failed.push((platform, fail_msg));
                    }
                }
                Err(e) => {
                    let platform = self.build.building.take().unwrap();
                    let fail_msg = format!("{}", e);
                    self.build
                        .log_line(format!("✗ {} 构建异常: {}", platform.label(), fail_msg));
                    self.build.failed.push((platform, fail_msg));
                }
            }
        }

        // 如果空闲且有队列，启动下一个构建（失败不阻塞，继续下一个）
        if self.build.building.is_none() && !self.build.queue.is_empty() {
            let platform = self.build.queue.remove(0);
            self.build.building = Some(platform);
            self.build
                .log_line(format!("→ 正在构建 {} (target: {})...", platform.label(), platform.target()));

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
                    self.build.log_line(format!("✗ {} {}", platform.label(), fail_msg));
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

    // -- 面板渲染 ----------------------------------------------------------

    /// 渲染首次启动欢迎面板。
    fn show_welcome_panel(&mut self, ui: &mut egui::Ui) {
        ui.vertical_centered(|ui| {
            ui.add_space(50.0);
            ui.heading(
                egui::RichText::new("欢迎使用 Akizuki*Rustgal 剧本编辑器").size(26.0),
            );
            ui.add_space(8.0);
            ui.label(
                egui::RichText::new("为视觉小说设计的轻量级剧本编写工具")
                    .size(16.0)
                    .color(egui::Color32::from_rgb(180, 190, 210)),
            );
            ui.add_space(28.0);

            ui.horizontal(|ui| {
                ui.add_space(80.0);
                let btn = egui::Button::new(egui::RichText::new("打开项目").size(15.0))
                    .min_size(egui::Vec2::new(130.0, 38.0));
                if ui.add(btn).clicked() {
                    self.open_dir_picker();
                }
                let btn = egui::Button::new(egui::RichText::new("新建剧本").size(15.0))
                    .min_size(egui::Vec2::new(130.0, 38.0));
                if ui.add(btn).clicked() {
                    self.new_file();
                }
                let btn = egui::Button::new(egui::RichText::new("打开已有剧本").size(15.0))
                    .min_size(egui::Vec2::new(130.0, 38.0));
                if ui.add(btn).clicked() {
                    self.open_file_picker(FilePickerMode::Open);
                }
                let btn = egui::Button::new(egui::RichText::new("打开示例剧本").size(15.0))
                    .min_size(egui::Vec2::new(130.0, 38.0));
                if ui.add(btn).clicked() {
                    self.load_sample();
                }
            });

            ui.add_space(24.0);

            // 最近项目列表
            if !self.recent_projects.projects.is_empty() {
                ui.label(egui::RichText::new("最近项目").size(18.0));
                ui.add_space(8.0);

                egui::Frame::group(ui.style())
                    .inner_margin(12.0)
                    .show(ui, |ui| {
                        ui.set_max_width(560.0);
                        let projects = self.recent_projects.projects.clone();
                        for project in &projects {
                            ui.horizontal(|ui| {
                                if ui.link(&project.name).clicked() {
                                    self.open_project(&project.path);
                                }
                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                    ui.label(
                                        egui::RichText::new(project.path.display().to_string())
                                            .small()
                                            .color(egui::Color32::from_rgb(140, 150, 170)),
                                    );
                                });
                            });
                        }
                    });
            }

            ui.add_space(24.0);

            // 语法示例
            ui.label(egui::RichText::new("语法示例").size(18.0));
            ui.add_space(8.0);

            egui::Frame::group(ui.style())
                .inner_margin(16.0)
                .show(ui, |ui| {
                    ui.set_max_width(560.0);
                    let example = "# 章节标题\n\
                        @bg 背景名 with fade\n\
                        + 角色名 enters from left\n\
                        角色名: \"对话内容\"\n\
                        $变量 = 1\n\
                        ? \"选择提示\"\n\
                        | \"选项1\"  -> 分支A\n\
                        | \"选项2\"  -> 分支B\n\
                        ?\n\
                        ~~  -- 章节结束";
                    ui.label(
                        egui::RichText::new(example)
                            .monospace()
                            .size(14.0)
                            .color(egui::Color32::from_rgb(200, 220, 255)),
                    );
                });

            ui.add_space(12.0);
            ui.label(
                egui::RichText::new(
                    "提示：# 定义章节  @ 场景指令  + 角色上场  $ 变量操作  ? 选择分支  ~~ 章节结束",
                )
                .size(12.0)
                .color(egui::Color32::from_rgb(140, 150, 170)),
            );
        });
    }

    fn show_editor(&mut self, ui: &mut egui::Ui) {
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let mut layouter = |ui: &egui::Ui, string: &str, wrap_width: f32| {
                    let mut job = highlight_code(string);
                    job.wrap.max_width = wrap_width;
                    ui.fonts(|f| f.layout_job(job))
                };
                ui.add(
                    egui::TextEdit::multiline(&mut self.editor_content)
                        .code_editor()
                        .desired_width(f32::MAX)
                        .layouter(&mut layouter),
                );
            });
    }

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

    fn kind_color(kind: &TranslatableKind) -> egui::Color32 {
        match kind {
            TranslatableKind::Section => COLOR_SECTION,
            TranslatableKind::Dialogue => COLOR_STRING,
            TranslatableKind::Narration => COLOR_STRING,
            TranslatableKind::Choice => COLOR_CHOICE,
            TranslatableKind::ChoicePrompt => COLOR_CHOICE,
            TranslatableKind::Character => COLOR_DIRECTION,
        }
    }

    fn show_translation_view(&mut self, ui: &mut egui::Ui) {
        // 顶部工具栏：目标语言选择 + 刷新 + 保存
        ui.horizontal(|ui| {
            ui.label("目标语言：");
            let mut lang_input = self.translation_target_lang.clone();
            if ui.text_edit_singleline(&mut lang_input).changed() {
                self.translation_target_lang = lang_input;
            }
            if ui.button("加载").clicked() {
                self.load_translation(&self.translation_target_lang.clone());
            }
            if ui.button("重新提取").clicked() {
                self.extract_translatable_lines();
                self.status = "已重新提取可翻译文本".to_string();
            }
            if ui.button("保存翻译").clicked() {
                self.save_translation();
            }
            ui.separator();
            ui.label(format!("共 {} 行可翻译", self.translatable_lines.len()));
        });
        ui.separator();

        // 对照翻译列表
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let lines: Vec<TranslatableLine> = self.translatable_lines.clone();
                for (idx, line) in lines.iter().enumerate() {
                    let kind_color = Self::kind_color(&line.kind);
                    let kind_label = Self::kind_label(&line.kind);

                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new(format!("[{}]", kind_label))
                                .color(kind_color)
                                .monospace()
                                .size(11.0),
                        );
                        ui.label(
                            egui::RichText::new(format!("行 {}", line.line_number))
                                .color(egui::Color32::from_rgb(150, 150, 150))
                                .monospace()
                                .size(11.0),
                        );
                        ui.label(
                            egui::RichText::new(format!("#{}", idx + 1))
                                .color(egui::Color32::from_rgb(120, 120, 120))
                                .monospace()
                                .size(11.0),
                        );
                    });

                    // 原文（只读，灰色背景，带格式前缀）
                    ui.horizontal(|ui| {
                        ui.add_space(8.0);
                        ui.vertical(|ui| {
                            ui.label(
                                egui::RichText::new("原文")
                                    .color(egui::Color32::from_rgb(180, 180, 180))
                                    .size(11.0),
                            );
                            // 根据类型添加格式前缀显示
                            let formatted_original = match line.kind {
                                TranslatableKind::Section => format!("# {}", line.original),
                                TranslatableKind::Dialogue => {
                                    // 对话格式需要说话人，这里简化只显示引号内容
                                    format!("\"{}\"", line.original)
                                }
                                TranslatableKind::Narration => format!("\"{}\"", line.original),
                                TranslatableKind::Choice => format!("| \"{}\"", line.original),
                                TranslatableKind::ChoicePrompt => format!("? \"{}\"", line.original),
                                TranslatableKind::Character => line.original.clone(),
                            };
                            let bg = egui::Frame::none()
                                .fill(egui::Color32::from_rgba_unmultiplied(40, 40, 50, 180))
                                .rounding(4.0)
                                .inner_margin(egui::Vec2::new(8.0, 4.0));
                            bg.show(ui, |ui| {
                                ui.add(
                                    egui::TextEdit::multiline(&mut formatted_original.as_str())
                                        .desired_width(f32::MAX)
                                        .desired_rows(1)
                                        .text_color(egui::Color32::from_rgb(220, 220, 220))
                                        .interactive(false),
                                );
                            });
                        });
                    });

                    // 译文输入框（默认带同样的格式前缀）
                    if let Some(translator) = self.translation_file.as_mut() {
                        let current_translation = match line.kind {
                            TranslatableKind::Section => translator.t_section(&line.original).to_string(),
                            TranslatableKind::Dialogue => translator.t_dialogue(&line.original).to_string(),
                            TranslatableKind::Narration => translator.t_narration(&line.original).to_string(),
                            TranslatableKind::Choice => translator.t_choice(&line.original).to_string(),
                            TranslatableKind::ChoicePrompt => translator.t_choice_prompt(&line.original).to_string(),
                            TranslatableKind::Character => translator.t_character(&line.original).to_string(),
                        };
                        // 根据类型添加格式前缀（如果译文尚未填写，使用原格式）
                        let mut translation_buf = if current_translation == line.original {
                            // 未翻译时，使用格式前缀作为默认值
                            match line.kind {
                                TranslatableKind::Section => format!("# {}", line.original),
                                TranslatableKind::Dialogue => format!("\"{}\"", line.original),
                                TranslatableKind::Narration => format!("\"{}\"", line.original),
                                TranslatableKind::Choice => format!("| \"{}\"", line.original),
                                TranslatableKind::ChoicePrompt => format!("? \"{}\"", line.original),
                                TranslatableKind::Character => line.original.clone(),
                            }
                        } else {
                            // 已翻译时，也需要带格式前缀
                            match line.kind {
                                TranslatableKind::Section => format!("# {}", current_translation),
                                TranslatableKind::Dialogue => format!("\"{}\"", current_translation),
                                TranslatableKind::Narration => format!("\"{}\"", current_translation),
                                TranslatableKind::Choice => format!("| \"{}\"", current_translation),
                                TranslatableKind::ChoicePrompt => format!("? \"{}\"", current_translation),
                                TranslatableKind::Character => current_translation.clone(),
                            }
                        };

                        ui.horizontal(|ui| {
                            ui.add_space(8.0);
                            ui.vertical(|ui| {
                                ui.label(
                                    egui::RichText::new("译文")
                                        .color(egui::Color32::from_rgb(150, 200, 255))
                                        .size(11.0),
                                );
                                let bg = egui::Frame::none()
                                    .fill(egui::Color32::from_rgba_unmultiplied(30, 40, 60, 200))
                                    .rounding(4.0)
                                    .inner_margin(egui::Vec2::new(8.0, 4.0));
                                let response = bg.show(ui, |ui| {
                                    ui.add(
                                        egui::TextEdit::multiline(&mut translation_buf)
                                            .desired_width(f32::MAX)
                                            .desired_rows(1)
                                            .text_color(egui::Color32::WHITE)
                                            .hint_text("输入翻译..."),
                                    )
                                });
                                if response.inner.changed() {
                                    // 从译文输入中提取纯文本（去掉格式前缀）
                                    let pure_translation = match line.kind {
                                        TranslatableKind::Section => {
                                            translation_buf.strip_prefix('#').map(|s| s.trim().to_string()).unwrap_or(translation_buf.clone())
                                        }
                                        TranslatableKind::Dialogue | TranslatableKind::Narration => {
                                            // 去掉引号
                                            translation_buf.trim_matches('"').to_string()
                                        }
                                        TranslatableKind::Choice => {
                                            translation_buf.strip_prefix('|').map(|s| s.trim().trim_matches('"').to_string()).unwrap_or(translation_buf.clone())
                                        }
                                        TranslatableKind::ChoicePrompt => {
                                            translation_buf.strip_prefix('?').map(|s| s.trim().trim_matches('"').to_string()).unwrap_or(translation_buf.clone())
                                        }
                                        TranslatableKind::Character => translation_buf.clone(),
                                    };
                                    match line.kind {
                                        TranslatableKind::Section => translator.set_section(&line.original, &pure_translation),
                                        TranslatableKind::Dialogue => translator.set_dialogue(&line.original, &pure_translation),
                                        TranslatableKind::Narration => translator.set_narration(&line.original, &pure_translation),
                                        TranslatableKind::Choice => translator.set_choice(&line.original, &pure_translation),
                                        TranslatableKind::ChoicePrompt => translator.set_choice_prompt(&line.original, &pure_translation),
                                        TranslatableKind::Character => translator.set_character(&line.original, &pure_translation),
                                    }
                                }
                            });
                        });
                    }

                    ui.add_space(4.0);
                    ui.separator();
                    ui.add_space(4.0);
                }
            });
    }

    fn show_preview(&mut self, ui: &mut egui::Ui) {
        // 快照场景，以便下方的选择按钮可以修改引擎而不持有 `self.engine` 的不可变借用。
        let (phase, scene) = match self.engine.as_ref() {
            Some(engine) => (engine.phase(), engine.scene().clone()),
            None => {
                ui.label("点击「运行」预览剧本。");
                return;
            }
        };

        ui.label(format!("状态：{}", phase_label(phase)));
        ui.add_space(4.0);

        ui.strong("背景");
        match &scene.background {
            Some(bg) => ui.label(format!("名称 = {}", bg.name)),
            None => ui.label("（无）"),
        };
        ui.add_space(4.0);

        ui.strong("角色");
        if scene.characters.is_empty() {
            ui.label("（舞台上无角色）");
        } else {
            for c in &scene.characters {
                ui.label(format!("- {} [{}]", c.name, position_label(&c.position)));
            }
        }
        ui.add_space(4.0);

        ui.strong("对话");
        match &scene.dialogue {
            Some(d) => {
                if d.speaker.is_empty() {
                    ui.label(egui::RichText::new("（旁白）").italics());
                } else {
                    ui.strong(d.speaker.as_str());
                }
                let shown: String = d.full_text.chars().take(d.displayed_chars).collect();
                ui.label(shown);
                ui.label(format!(
                    "（{}/{} 字，完成={}）",
                    d.displayed_chars,
                    d.full_text.chars().count(),
                    d.complete
                ));
            }
            None => {
                ui.label("（无）");
            }
        };
        ui.add_space(4.0);

        ui.strong("选项");
        match &scene.choices {
            Some(ch) => {
                if let Some(p) = &ch.prompt {
                    ui.label(format!("提示：{}", p));
                }
                if ch.options.is_empty() {
                    ui.label("（无选项）");
                }
                for (i, opt) in ch.options.iter().enumerate() {
                    let label = format!(
                        "{}. {}{}",
                        i + 1,
                        opt.text,
                        if opt.available { "" } else { "（禁用）" }
                    );
                    if ui.button(label).clicked() {
                        if let Some(engine) = self.engine.as_mut() {
                            let _ = engine.choose(i);
                        }
                    }
                }
            }
            None => {
                ui.label("（无）");
            }
        };

        if scene.story_ended {
            ui.add_space(4.0);
            ui.colored_label(
                egui::Color32::from_rgb(255, 150, 150),
                "故事已结束。",
            );
        }
    }

    /// 渲染立绘预览面板：允许作者在不启动游戏的情况下调整立绘位置与大小，
    /// 实时查看效果，并生成对应的 `.akrs` 语法。
    ///
    /// 位置与大小语义与运行时渲染（`akrs_render`）完全一致：
    /// - 立绘自然高度为预览区高度的 80%，再乘以 `scale`。
    /// - `x_percent` 控制立绘水平中心点的百分比位置。
    /// - `y_percent = 1.0` 时立绘底部贴齐预览区底部（留小边距）；
    ///   `y_percent < 1.0` 时立绘中心点对齐到预览区该百分比位置。
    fn show_sprite_preview(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();

        // 扫描 assets/characters/ 目录（检测变更后重新扫描）。
        let chars_dir = self.work_dir.join("assets").join("characters");
        if self.sprite_preview.scanned_dir.as_ref() != Some(&chars_dir) {
            self.sprite_preview.available.clear();
            if let Ok(entries) = std::fs::read_dir(&chars_dir) {
                let mut names: Vec<String> = entries
                    .filter_map(|e| e.ok())
                    .filter_map(|e| {
                        let p = e.path();
                        if p.extension().is_some_and(|ext| ext.eq_ignore_ascii_case("png")) {
                            p.file_stem().map(|n| n.to_string_lossy().into_owned())
                        } else {
                            None
                        }
                    })
                    .collect();
                names.sort();
                self.sprite_preview.available = names;
            }
            self.sprite_preview.scanned_dir = Some(chars_dir);
            // 默认选中第一个可用立绘。
            if self.sprite_preview.selected.is_empty() {
                if let Some(first) = self.sprite_preview.available.first() {
                    self.sprite_preview.selected = first.clone();
                    self.sprite_preview.character_name = first.clone();
                }
            }
        }

        // -- 立绘选择 --
        ui.label("选择立绘：");
        let selected_empty = self.sprite_preview.selected.is_empty();
        egui::ComboBox::from_id_source("sprite_preview_select")
            .selected_text(if selected_empty {
                "（无可用立绘）"
            } else {
                self.sprite_preview.selected.as_str()
            })
            .show_ui(ui, |ui| {
                for name in &self.sprite_preview.available {
                    ui.selectable_value(
                        &mut self.sprite_preview.selected,
                        name.clone(),
                        name,
                    );
                }
            });

        ui.add_space(4.0);
        ui.label("角色名（用于生成语法，留空则使用立绘名）：");
        ui.text_edit_singleline(&mut self.sprite_preview.character_name);

        ui.add_space(8.0);

        // -- 位置与大小滑块 --
        ui.horizontal(|ui| {
            ui.label("X 位置:");
            ui.add(
                egui::Slider::new(&mut self.sprite_preview.x_percent, 0.0..=1.0)
                    .step_by(0.01)
                    .fixed_decimals(2),
            );
        });
        ui.horizontal(|ui| {
            ui.label("Y 位置:");
            ui.add(
                egui::Slider::new(&mut self.sprite_preview.y_percent, 0.0..=1.0)
                    .step_by(0.01)
                    .fixed_decimals(2),
            );
        });
        ui.horizontal(|ui| {
            ui.label("大小:  ");
            ui.add(
                egui::Slider::new(&mut self.sprite_preview.scale, 0.1..=3.0)
                    .step_by(0.05)
                    .fixed_decimals(2),
            );
        });

        ui.add_space(2.0);
        ui.label(
            egui::RichText::new("默认：X=0.5（居中）  Y=1.0（底部站立）  大小=1.0")
                .small()
                .color(egui::Color32::from_rgb(140, 150, 170)),
        );

        ui.add_space(8.0);

        // -- 预览区域（16:9，模拟游戏屏幕）--
        let avail_w = ui.available_width();
        let preview_w = avail_w;
        let preview_h = (preview_w * 9.0 / 16.0).max(180.0);

        let (rect, _response) = ui.allocate_exact_size(
            egui::Vec2::new(preview_w, preview_h),
            egui::Sense::hover(),
        );

        let painter = ui.painter();
        // 预览背景（模拟游戏屏幕）。
        painter.rect_filled(rect, 0.0, egui::Color32::from_rgb(28, 28, 38));
        painter.rect_stroke(
            rect,
            0.0,
            egui::Stroke::new(1.0, egui::Color32::from_rgb(80, 80, 100)),
        );

        // 加载并渲染立绘。
        if !self.sprite_preview.selected.is_empty() {
            let selected = self.sprite_preview.selected.clone();
            let need_load = !self.sprite_preview.textures.contains_key(&selected);
            if need_load {
                let path = self
                    .work_dir
                    .join("assets")
                    .join("characters")
                    .join(format!("{}.png", selected));
                match image::open(&path) {
                    Ok(img) => {
                        let rgba = img.to_rgba8();
                        let (w, h) = (rgba.width() as usize, rgba.height() as usize);
                        let color_image =
                            ColorImage::from_rgba_unmultiplied([w, h], rgba.as_raw());
                        let handle = ctx.load_texture(&selected, color_image, Default::default());
                        self.sprite_preview.textures.insert(selected.clone(), handle);
                        self.sprite_preview.load_error = None;
                    }
                    Err(e) => {
                        self.sprite_preview.load_error = Some(format!("加载失败：{}", e));
                    }
                }
            }

            if let Some(handle) = self.sprite_preview.textures.get(&selected) {
                let tex_w = handle.size()[0] as f32;
                let tex_h = handle.size()[1] as f32;
                // 与运行时一致：立绘自然高度 = 预览区高度 × 80%。
                let scale_factor = (preview_h * 0.8) / tex_h;
                let draw_w = tex_w * scale_factor * self.sprite_preview.scale;
                let draw_h = tex_h * scale_factor * self.sprite_preview.scale;
                let x_frac = self.sprite_preview.x_percent;
                let y_frac = self.sprite_preview.y_percent;
                // x：立绘中心点对齐到预览区 x_frac。
                let x = rect.left() + preview_w * x_frac - draw_w / 2.0;
                // y：1.0 时底部贴齐（留 50/1080 比例边距，与游戏一致）；
                //    否则立绘中心点对齐到预览区 y_frac。
                let bottom_margin = preview_h * (50.0 / 1080.0);
                let y = if (y_frac - 1.0).abs() < 0.001 {
                    rect.bottom() - draw_h - bottom_margin
                } else {
                    rect.top() + preview_h * y_frac - draw_h / 2.0
                };
                let dest_rect = egui::Rect::from_min_size(
                    egui::pos2(x, y),
                    egui::Vec2::new(draw_w, draw_h),
                );
                painter.image(
                    handle.id(),
                    dest_rect,
                    egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                    egui::Color32::WHITE,
                );
            } else if let Some(err) = &self.sprite_preview.load_error {
                painter.text(
                    rect.center(),
                    egui::Align2::CENTER_CENTER,
                    err,
                    egui::FontId::proportional(12.0),
                    egui::Color32::from_rgb(255, 150, 150),
                );
            }
        } else {
            painter.text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                "请在 assets/characters/ 放置 PNG 立绘",
                egui::FontId::proportional(12.0),
                egui::Color32::from_rgb(140, 150, 170),
            );
        }

        ui.add_space(8.0);

        // -- 生成的语法 --
        ui.separator();
        ui.strong("生成的语法：");
        let name = if self.sprite_preview.character_name.trim().is_empty() {
            self.sprite_preview.selected.clone()
        } else {
            self.sprite_preview.character_name.clone()
        };
        let syntax = format!(
            "+ {} at {:.2},{:.2} size {:.2}",
            name, self.sprite_preview.x_percent, self.sprite_preview.y_percent, self.sprite_preview.scale
        );
        ui.label(
            egui::RichText::new(&syntax)
                .monospace()
                .color(egui::Color32::from_rgb(200, 220, 255)),
        );
        ui.horizontal(|ui| {
            if ui.button("复制到剪贴板").clicked() {
                ctx.output_mut(|o| o.copied_text = syntax.clone());
                self.status = "语法已复制到剪贴板".to_string();
            }
            if ui.button("追加到脚本").clicked() {
                self.editor_content.push_str(&format!("{}\n", syntax));
                self.status = "语法已追加到脚本末尾".to_string();
            }
            if ui.button("隐藏此立绘").clicked() {
                let hide_syntax = format!("- {}\n", name);
                self.editor_content.push_str(&hide_syntax);
                self.status = "隐藏立绘语法已追加到脚本末尾".to_string();
            }
            if ui.button("放大预览").clicked() {
                self.show_enlarged_preview = true;
            }
            if ui.button("替换插入").clicked() {
                self.find_replace_syntax = syntax.clone();
                self.show_find_replace_dialog = true;
            }
        });
    }

    /// 渲染背景预览面板：允许作者选择背景图片，预览效果，并生成对应的 `.akrs` 语法。
    fn show_bg_preview(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();

        // 扫描 assets/bg/ 目录（检测变更后重新扫描）
        let bg_dir = self.work_dir.join("assets").join("bg");
        if self.bg_preview.scanned_dir.as_ref() != Some(&bg_dir) {
            self.bg_preview.available.clear();
            if let Ok(entries) = std::fs::read_dir(&bg_dir) {
                let mut names: Vec<String> = entries
                    .filter_map(|e| e.ok())
                    .filter_map(|e| {
                        let p = e.path();
                        if p.extension().is_some_and(|ext| ext.eq_ignore_ascii_case("png") || ext.eq_ignore_ascii_case("jpg") || ext.eq_ignore_ascii_case("jpeg")) {
                            p.file_stem().map(|n| n.to_string_lossy().into_owned())
                        } else {
                            None
                        }
                    })
                    .collect();
                names.sort();
                self.bg_preview.available = names;
            }
            self.bg_preview.scanned_dir = Some(bg_dir);
            // 默认选中第一个可用背景
            if self.bg_preview.selected.is_empty() {
                if let Some(first) = self.bg_preview.available.first() {
                    self.bg_preview.selected = first.clone();
                }
            }
        }

        // -- 背景选择 --
        ui.label("选择背景：");
        let selected_empty = self.bg_preview.selected.is_empty();
        egui::ComboBox::from_id_source("bg_preview_select")
            .selected_text(if selected_empty {
                "（无可用背景）"
            } else {
                self.bg_preview.selected.as_str()
            })
            .show_ui(ui, |ui| {
                for name in &self.bg_preview.available {
                    ui.selectable_value(
                        &mut self.bg_preview.selected,
                        name.clone(),
                        name,
                    );
                }
            });

        ui.add_space(4.0);

        // -- 过渡效果选择 --
        ui.label("过渡效果：");
        let transitions = ["fade", "fade_black", "fade_white", "dissolve", "slide_left", "slide_right", "slide_up", "slide_down", "wipe_left", "wipe_right", "blur", "instant"];
        egui::ComboBox::from_id_source("bg_transition_select")
            .selected_text(self.bg_preview.transition.as_str())
            .show_ui(ui, |ui| {
                for t in transitions {
                    ui.selectable_value(&mut self.bg_preview.transition, t.to_string(), t);
                }
            });

        ui.add_space(8.0);

        // -- 预览区域（16:9，模拟游戏屏幕）--
        let avail_w = ui.available_width();
        let preview_w = avail_w;
        let preview_h = (preview_w * 9.0 / 16.0).max(180.0);

        let (rect, _response) = ui.allocate_exact_size(
            egui::Vec2::new(preview_w, preview_h),
            egui::Sense::hover(),
        );

        let painter = ui.painter();
        // 预览背景（模拟游戏屏幕）
        painter.rect_filled(rect, 0.0, egui::Color32::from_rgb(28, 28, 38));
        painter.rect_stroke(
            rect,
            0.0,
            egui::Stroke::new(1.0, egui::Color32::from_rgb(80, 80, 100)),
        );

        // 加载并渲染背景图片
        if !self.bg_preview.selected.is_empty() {
            let selected = self.bg_preview.selected.clone();
            let need_load = !self.bg_preview.textures.contains_key(&selected);
            if need_load {
                let bg_dir = self.work_dir.join("assets").join("bg");
                // 尝试 png, jpg, jpeg 扩展名
                let extensions = ["png", "jpg", "jpeg"];
                for ext in extensions {
                    let path = bg_dir.join(format!("{}.{}", selected, ext));
                    if path.exists() {
                        match image::open(&path) {
                            Ok(img) => {
                                let rgba = img.to_rgba8();
                                let (w, h) = (rgba.width() as usize, rgba.height() as usize);
                                let color_image = ColorImage::from_rgba_unmultiplied([w, h], rgba.as_raw());
                                let handle = ctx.load_texture(&selected, color_image, Default::default());
                                self.bg_preview.textures.insert(selected.clone(), handle);
                                self.bg_preview.load_error = None;
                                break;
                            }
                            Err(e) => {
                                self.bg_preview.load_error = Some(format!("加载失败：{}", e));
                            }
                        }
                    }
                }
                if !self.bg_preview.textures.contains_key(&selected) && self.bg_preview.load_error.is_none() {
                    self.bg_preview.load_error = Some("未找到背景文件".to_string());
                }
            }

            if let Some(handle) = self.bg_preview.textures.get(&selected) {
                let tex_w = handle.size()[0] as f32;
                let tex_h = handle.size()[1] as f32;
                // 按预览区比例缩放，保持宽高比
                let scale = (preview_w / tex_w).min(preview_h / tex_h);
                let draw_w = tex_w * scale;
                let draw_h = tex_h * scale;
                let x = rect.left() + (preview_w - draw_w) / 2.0;
                let y = rect.top() + (preview_h - draw_h) / 2.0;
                let dest_rect = egui::Rect::from_min_size(
                    egui::pos2(x, y),
                    egui::Vec2::new(draw_w, draw_h),
                );
                painter.image(
                    handle.id(),
                    dest_rect,
                    egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                    egui::Color32::WHITE,
                );
            } else if let Some(err) = &self.bg_preview.load_error {
                painter.text(
                    rect.center(),
                    egui::Align2::CENTER_CENTER,
                    err,
                    egui::FontId::proportional(12.0),
                    egui::Color32::from_rgb(255, 150, 150),
                );
            }
        } else {
            painter.text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                "请在 assets/bg/ 放置 PNG/JPG 背景",
                egui::FontId::proportional(12.0),
                egui::Color32::from_rgb(140, 150, 170),
            );
        }

        ui.add_space(8.0);

        // -- 生成的语法 --
        ui.separator();
        ui.strong("生成的语法：");
        let syntax = if self.bg_preview.selected.is_empty() {
            "（未选择背景）".to_string()
        } else {
            format!(
                "@bg {} with {}",
                self.bg_preview.selected, self.bg_preview.transition
            )
        };
        ui.label(
            egui::RichText::new(&syntax)
                .monospace()
                .color(egui::Color32::from_rgb(200, 220, 255)),
        );
        ui.horizontal(|ui| {
            if ui.button("复制到剪贴板").clicked() {
                ctx.output_mut(|o| o.copied_text = syntax.clone());
                self.status = "语法已复制到剪贴板".to_string();
            }
            if ui.button("追加到脚本").clicked() {
                self.editor_content.push_str(&format!("{}\n", syntax));
                self.status = "语法已追加到脚本末尾".to_string();
            }
            if ui.button("放大预览").clicked() {
                self.show_enlarged_preview = true;
            }
            if ui.button("替换插入").clicked() {
                self.find_replace_syntax = syntax.clone();
                self.show_find_replace_dialog = true;
            }
        });
    }

    /// 渲染音乐预览面板：允许作者选择音乐文件，并生成对应的 `.akrs` 语法。
    fn show_music_preview(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();

        // 扫描 assets/music/ 目录（检测变更后重新扫描）
        let music_dir = self.work_dir.join("assets").join("music");
        if self.music_preview.scanned_dir.as_ref() != Some(&music_dir) {
            self.music_preview.available.clear();
            if let Ok(entries) = std::fs::read_dir(&music_dir) {
                let mut names: Vec<String> = entries
                    .filter_map(|e| e.ok())
                    .filter_map(|e| {
                        let p = e.path();
                        if p.extension().is_some_and(|ext| ext.eq_ignore_ascii_case("ogg") || ext.eq_ignore_ascii_case("mp3") || ext.eq_ignore_ascii_case("wav")) {
                            p.file_stem().map(|n| n.to_string_lossy().into_owned())
                        } else {
                            None
                        }
                    })
                    .collect();
                names.sort();
                self.music_preview.available = names;
            }
            self.music_preview.scanned_dir = Some(music_dir);
            // 默认选中第一个可用音乐
            if self.music_preview.selected.is_empty() {
                if let Some(first) = self.music_preview.available.first() {
                    self.music_preview.selected = first.clone();
                }
            }
        }

        // -- 音乐选择 --
        ui.label("选择音乐：");
        let selected_empty = self.music_preview.selected.is_empty();
        egui::ComboBox::from_id_source("music_preview_select")
            .selected_text(if selected_empty {
                "（无可用音乐）"
            } else {
                self.music_preview.selected.as_str()
            })
            .show_ui(ui, |ui| {
                for name in &self.music_preview.available {
                    ui.selectable_value(
                        &mut self.music_preview.selected,
                        name.clone(),
                        name,
                    );
                }
            });

        ui.add_space(8.0);

        // -- 生成的语法 --
        ui.separator();
        ui.strong("生成的语法：");
        let play_syntax = if self.music_preview.selected.is_empty() {
            "（未选择音乐）".to_string()
        } else {
            format!("@music {}", self.music_preview.selected)
        };
        let stop_syntax = "@stop_music";
        ui.label(
            egui::RichText::new("播放音乐：")
                .color(egui::Color32::from_rgb(180, 180, 180)),
        );
        ui.label(
            egui::RichText::new(&play_syntax)
                .monospace()
                .color(egui::Color32::from_rgb(200, 220, 255)),
        );
        ui.label(
            egui::RichText::new("关闭音乐：")
                .color(egui::Color32::from_rgb(180, 180, 180)),
        );
        ui.label(
            egui::RichText::new(stop_syntax)
                .monospace()
                .color(egui::Color32::from_rgb(200, 220, 255)),
        );

        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if ui.button("复制播放语法").clicked() {
                ctx.output_mut(|o| o.copied_text = play_syntax.clone());
                self.status = "播放语法已复制到剪贴板".to_string();
            }
            if ui.button("追加播放语法").clicked() {
                self.editor_content.push_str(&format!("{}\n", play_syntax));
                self.status = "播放语法已追加到脚本末尾".to_string();
            }
            if ui.button("替换插入播放语法").clicked() {
                self.find_replace_syntax = play_syntax.clone();
                self.show_find_replace_dialog = true;
            }
        });
        ui.horizontal(|ui| {
            if ui.button("复制关闭语法").clicked() {
                ctx.output_mut(|o| o.copied_text = stop_syntax.to_string());
                self.status = "关闭语法已复制到剪贴板".to_string();
            }
            if ui.button("追加关闭语法").clicked() {
                self.editor_content.push_str(&format!("{}\n", stop_syntax));
                self.status = "关闭语法已追加到脚本末尾".to_string();
            }
            if ui.button("替换插入关闭语法").clicked() {
                self.find_replace_syntax = stop_syntax.to_string();
                self.show_find_replace_dialog = true;
            }
        });
    }

    // -- Ren'Py 剧本导入 -------------------------------------------------------

    /// 从 Ren'Py .rpy 文件转换为 .akrs 格式并保存。
    /// 仅转换可直接映射的语法，不支持的功能会产生警告。
    fn convert_rpy_to_akrs(&mut self, source: &Path, target: &Path) {
        // 读取源文件
        let content = match std::fs::read_to_string(source) {
            Ok(c) => c,
            Err(e) => {
                self.status = format!("无法读取源文件：{}", e);
                return;
            }
        };

        let mut akrs_lines: Vec<String> = Vec::new();
        let mut warnings: Vec<String> = Vec::new();

        // 检测剧本主题（label/start）
        let has_label = content.contains("label start:") || content.contains("label start:");
        if !has_label {
            warnings.push("未检测到 'label start:'，可能不是标准的 Ren'Py 剧本文件".to_string());
        }

        // 逐行转换
        for line in content.lines() {
            let trimmed = line.trim();

            // 跳过空行和 Python 代码块
            if trimmed.is_empty() || trimmed.starts_with("python:") || trimmed.starts_with("$ ") {
                continue;
            }

            // label -> # 章节
            if trimmed.starts_with("label ") {
                let label_name = trimmed
                    .strip_prefix("label ")
                    .unwrap_or("")
                    .trim_end_matches(':');
                akrs_lines.push(format!("# {}", label_name));
                continue;
            }

            // scene -> @bg
            if trimmed.starts_with("scene ") {
                let bg_name = trimmed
                    .strip_prefix("scene ")
                    .unwrap_or("")
                    .split_whitespace()
                    .next()
                    .unwrap_or("");
                // Ren'Py 的 with 过渡（如 "with fade"）可尝试映射
                if trimmed.contains("with fade") {
                    akrs_lines.push(format!("@bg {} with fade", bg_name));
                } else if trimmed.contains("with dissolve") {
                    akrs_lines.push(format!("@bg {} with dissolve", bg_name));
                } else {
                    akrs_lines.push(format!("@bg {}", bg_name));
                }
                continue;
            }

            // show -> + 立绘上场（简化映射）
            if trimmed.starts_with("show ") {
                let rest = trimmed.strip_prefix("show ").unwrap_or("");
                // 简化：取第一个词作为角色名
                let char_name = rest.split_whitespace().next().unwrap_or("");
                // Ren'Py 的 at 位置（如 "at left"）可映射
                if trimmed.contains("at left") {
                    akrs_lines.push(format!("+ {} at left", char_name));
                } else if trimmed.contains("at right") {
                    akrs_lines.push(format!("+ {} at right", char_name));
                } else if trimmed.contains("at center") {
                    akrs_lines.push(format!("+ {}", char_name));
                } else {
                    akrs_lines.push(format!("+ {}", char_name));
                }
                continue;
            }

            // hide -> - 立绘下场
            if trimmed.starts_with("hide ") {
                let char_name = trimmed
                    .strip_prefix("hide ")
                    .unwrap_or("")
                    .split_whitespace()
                    .next()
                    .unwrap_or("");
                akrs_lines.push(format!("- {}", char_name));
                continue;
            }

            // 对话 "说话人 \"对话内容\""
            if trimmed.starts_with('"') && trimmed.contains("\" \"") {
                // 格式如: "说话人 \"对话\""
                // 简化处理：提取说话人和对话
                let parts: Vec<&str> = trimmed.splitn(2, "\" \"").collect();
                if parts.len() == 2 {
                    let speaker = parts[0].trim_start_matches('"').trim();
                    let dialogue = parts[1].trim_end_matches('"').trim();
                    akrs_lines.push(format!("{}: \"{}\"", speaker, dialogue));
                    continue;
                }
            }

            // narrate 旁白（无说话人的字符串）
            if trimmed.starts_with('"') && trimmed.ends_with('"') {
                let narration = trimmed.trim_matches('"');
                akrs_lines.push(format!("\"{}\"", narration));
                continue;
            }

            // menu -> ? 选择分支
            if trimmed.starts_with("menu:") {
                akrs_lines.push("? \"\"".to_string());
                continue;
            }

            // 菜单选项（以字符串开头后冒号）-> | 选项
            if trimmed.starts_with('"') && trimmed.contains(':') && !trimmed.contains("\" \"") {
                // 格式如: "选项文本":（后接 jump）
                let parts: Vec<&str> = trimmed.splitn(2, ':').collect();
                if parts.len() == 2 {
                    let option_text = parts[0].trim_matches('"').trim();
                    akrs_lines.push(format!("| \"{}\"", option_text));
                    // 如果有 jump，映射为 ->
                    let rest = parts[1].trim();
                    if rest.starts_with("jump ") {
                        let target_label = rest.strip_prefix("jump ").unwrap_or("").trim();
                        akrs_lines.push(format!("    -> {}", target_label));
                    }
                    continue;
                }
            }

            // return / jump -> -> 或结束
            if trimmed.starts_with("return") {
                akrs_lines.push("~~".to_string());
                continue;
            }
            if trimmed.starts_with("jump ") {
                let target = trimmed.strip_prefix("jump ").unwrap_or("").trim();
                akrs_lines.push(format!("-> {}", target));
                continue;
            }

            // 不支持的 Ren'Py 特性产生警告
            if trimmed.starts_with("play music")
                || trimmed.starts_with("play sound")
                || trimmed.starts_with("stop music")
                || trimmed.starts_with("stop sound")
            {
                warnings.push(format!("音频播放指令未转换：{}", trimmed));
                continue;
            }
            if trimmed.starts_with("with ") {
                warnings.push(format!("独立过渡指令未转换：{}", trimmed));
                continue;
            }
            if trimmed.starts_with("call ")
                || trimmed.starts_with("if ")
                || trimmed.starts_with("while ")
                || trimmed.starts_with("for ")
            {
                warnings.push(format!("复杂流程控制未转换：{}", trimmed));
                continue;
            }
            if trimmed.starts_with("define ")
                || trimmed.starts_with("default ")
                || trimmed.starts_with("init ")
            {
                warnings.push(format!("定义/初始化块未转换：{}", trimmed));
                continue;
            }
            if trimmed.starts_with("image ")
                || trimmed.starts_with("transform ")
                || trimmed.starts_with("animation ")
            {
                warnings.push(format!("图像/动画定义未转换：{}", trimmed));
                continue;
            }
            if trimmed.contains("renpy.")
                || trimmed.contains("Ren'Py")
            {
                warnings.push(format!("Ren'Py 内置函数/变量未转换：{}", trimmed));
                continue;
            }
            // 注释行保留
            if trimmed.starts_with('#') {
                akrs_lines.push(format!("-- {}", trimmed.trim_start_matches('#').trim()));
                continue;
            }

            // 未识别的行保留为注释
            if !trimmed.is_empty() {
                akrs_lines.push(format!("-- 未转换: {}", trimmed));
                warnings.push(format!("未识别的行：{}", trimmed));
            }
        }

        // 构建结果
        let akrs_content = akrs_lines.join("\n");

        // 确保目标目录存在
        if let Some(parent) = target.parent() {
            let _ = std::fs::create_dir_all(parent);
        }

        // 写入目标文件
        match std::fs::write(target, &akrs_content) {
            Ok(_) => {
                self.status = format!("已导入 {} -> {}", source.display(), target.display());
                self.editor_content = akrs_content;
                self.current_file = Some(target.to_path_buf());
                self.file_name_input = target
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "imported.akrs".to_string());
            }
            Err(e) => {
                self.status = format!("写入失败：{}", e);
            }
        }

        // 保存警告
        self.rpy_import_warnings = warnings.clone();
        if warnings.is_empty() {
            self.rpy_import_warnings.push("转换完成，无警告".to_string());
        }
    }
}

impl eframe::App for EditorApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if !self.theme_applied {
            ctx.set_visuals(egui::Visuals::dark());
            self.theme_applied = true;
        }

        // 引擎动画的帧增量（打字机 / 过渡）。
        let now: f64 = ctx.input(|i| i.time);
        let dt = if self.last_time > 0.0 {
            ((now - self.last_time) as f32).clamp(0.0, 0.1)
        } else {
            0.0
        };
        self.last_time = now;

        // 键盘快捷键
        ctx.input(|i| {
            if i.modifiers.ctrl && !i.modifiers.shift {
                if i.key_pressed(egui::Key::N) {
                    self.new_file();
                }
                if i.key_pressed(egui::Key::O) {
                    if self.show_welcome {
                        self.show_welcome = false;
                        self.status = "请从左侧文件列表选择文件".to_string();
                    } else {
                        self.open_current_name();
                    }
                }
                if i.key_pressed(egui::Key::S) {
                    self.save_file();
                }
                if i.key_pressed(egui::Key::R) {
                    self.run_script();
                }
                // Ctrl+1/2/3/4：快速插入语法
                if i.key_pressed(egui::Key::Num1) {
                    self.editor_content.push_str("+ 角色\n");
                    self.status = "已插入：+ 角色（立绘上场）".to_string();
                }
                if i.key_pressed(egui::Key::Num2) {
                    self.editor_content.push_str("- 角色\n");
                    self.status = "已插入：- 角色（立绘下场）".to_string();
                }
                if i.key_pressed(egui::Key::Num3) {
                    self.editor_content.push_str("# 章节\n");
                    self.status = "已插入：# 章节（章节标题）".to_string();
                }
                if i.key_pressed(egui::Key::Num4) {
                    self.editor_content.push_str("@bg 背景\n");
                    self.status = "已插入：@bg 背景（背景指令）".to_string();
                }
                // Ctrl+H：显示帮助窗口
                if i.key_pressed(egui::Key::H) {
                    self.show_shortcuts = true;
                }
            }
        });

        if let Some(engine) = self.engine.as_mut() {
            let _ = engine.update(dt);
        }
        // 在引擎过渡中 / 等待中或打字机未完成时持续动画。
        if let Some(engine) = &self.engine {
            let animating = matches!(
                engine.phase(),
                EnginePhase::Transitioning | EnginePhase::Waiting
            ) || engine
                .scene()
                .dialogue
                .as_ref()
                .is_some_and(|d| !d.complete);
            if animating {
                ctx.request_repaint();
            }
        }

        // 轮询游戏预览子进程
        self.poll_game_process();
        // 推进打包队列
        if self.build.is_building() {
            self.poll_build();
            ctx.request_repaint();
        }

        // ---- 顶部工具栏 --------------------------------------------------
        egui::TopBottomPanel::top("toolbar").show(ctx, |ui| {
            ui.horizontal_wrapped(|ui| {
                if ui.button("打开项目").clicked() {
                    self.open_dir_picker();
                }
                if ui.button("项目设置").clicked() {
                    self.show_project_settings = true;
                }
                ui.separator();
                if ui.button("新建 (Ctrl+N)").clicked() {
                    self.new_file();
                }
                if ui.button("打开 (Ctrl+O)").clicked() {
                    self.open_file_picker(FilePickerMode::Open);
                }
                if ui.button("保存 (Ctrl+S)").clicked() {
                    self.save_file();
                }
                ui.separator();
                // 快速插入语法按钮（鼠标悬停显示提示）
                ui.label("插入:");
                let insert_buttons = [
                    ("+", "+ 角色", "立绘上场 (Ctrl+1)"),
                    ("-", "- 角色", "立绘下场 (Ctrl+2)"),
                    ("#", "# 章节", "章节标题 (Ctrl+3)"),
                    ("@", "@bg 背景", "背景指令 (Ctrl+4)"),
                    ("?", "? 选项", "选择分支"),
                    ("$", "$变量", "变量操作"),
                ];
                for (icon, syntax, tooltip) in &insert_buttons {
                    let btn = ui.add(
                        egui::Button::new(egui::RichText::new(*icon).monospace().strong())
                            .small()
                    );
                    let clicked = btn.clicked();
                    btn.on_hover_text(*tooltip);
                    if clicked {
                        self.editor_content.push_str(&format!("{}\n", syntax));
                        self.status = format!("已插入：{}", syntax);
                    }
                }
                ui.separator();
                if ui.button("运行 (Ctrl+R)").clicked() {
                    self.run_script();
                }
                if self.engine.is_some() {
                    if ui.button("停止").clicked() {
                        self.engine = None;
                        self.status = "预览已停止".to_string();
                    }
                }
                ui.separator();
                if ui.button("预览游戏").clicked() {
                    self.start_game_preview();
                }
                if ui.button("打包").clicked() {
                    self.build.show = true;
                }
                ui.separator();
                if ui.button("导入rpy").clicked() {
                    self.show_rpy_import = true;
                    self.rpy_import_warnings.clear();
                    self.rpy_import_source = None;
                    // 默认保存到项目的 scripts 目录，或工作目录
                    if self.project_loaded {
                        self.rpy_import_target = self.work_dir.join("scripts").join("imported.akrs");
                    } else {
                        self.rpy_import_target = self.work_dir.join("imported.akrs");
                    }
                }
                ui.separator();
                if ui.button("首页").clicked() {
                    self.show_welcome = true;
                    self.engine = None;
                    self.status = "就绪".to_string();
                }
                if ui.button("帮助").clicked() {
                    self.show_shortcuts = true;
                }
                if ui.button("关于").clicked() {
                    self.show_about = true;
                }
                ui.separator();
                if self.translation_mode {
                    if ui.button("退出对照翻译").clicked() {
                        self.toggle_translation_mode();
                    }
                } else {
                    if ui.button("对照翻译").clicked() {
                        self.toggle_translation_mode();
                    }
                }
                ui.separator();
                ui.label(format!("文件：{}", self.file_name_input));
            });
        });

        // ---- 底部状态栏 --------------------------------------------------
        egui::TopBottomPanel::bottom("status").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label(format!("状态：{}", self.status));
                ui.separator();
                ui.label(format!("诊断：{}", self.diagnostics.len()));
                ui.separator();
                match &self.current_file {
                    Some(path) => ui.label(format!("路径：{}", path.display())),
                    None => ui.label("路径：（未保存）"),
                };
            });
            if !self.diagnostics.is_empty() {
                ui.separator();
                let shown = self.diagnostics.len().min(8);
                for msg in self.diagnostics.iter().take(shown) {
                    let color = if msg.starts_with("[错误]") {
                        egui::Color32::from_rgb(255, 120, 120)
                    } else if msg.starts_with("[警告]") {
                        egui::Color32::from_rgb(220, 200, 120)
                    } else {
                        egui::Color32::from_rgb(150, 170, 200)
                    };
                    ui.label(egui::RichText::new(msg).color(color).monospace());
                }
                if self.diagnostics.len() > shown {
                    ui.label(format!("……还有 {} 条", self.diagnostics.len() - shown));
                }
            }
        });

        // ---- 左栏：文件列表 ---------------------------------------------
        egui::SidePanel::left("files")
            .resizable(true)
            .default_width(210.0)
            .show(ctx, |ui| {
                ui.heading("文件");
                ui.label(format!("目录：{}", self.work_dir.display()));
                ui.horizontal(|ui| {
                    ui.label("文件名：");
                    ui.text_edit_singleline(&mut self.file_name_input);
                });
                ui.horizontal(|ui| {
                    if ui.button("新建").clicked() {
                        self.new_file();
                    }
                    if ui.button("保存").clicked() {
                        self.save_file();
                    }
                    if ui.button("刷新").clicked() {
                        self.refresh_file_list();
                    }
                });
                ui.separator();
                ui.label("打开文件：");
                egui::ScrollArea::vertical().show(ui, |ui| {
                    if self.file_list.is_empty() {
                        ui.label("（无 .akrs 文件）");
                    }
                    // 克隆以便在迭代时修改 `self`。
                    let files = self.file_list.clone();
                    for name in &files {
                        let selected = self
                            .current_file
                            .as_ref()
                            .and_then(|p| p.file_name())
                            .is_some_and(|n| n == name.as_str());
                        if ui.selectable_label(selected, name.as_str()).clicked() {
                            self.open_file(name);
                        }
                    }
                });
            });

        // ---- 右栏：预览 --------------------------------------------------
        // 翻译模式时隐藏右侧边栏（防呆设计）
        if !self.translation_mode {
            egui::SidePanel::right("preview")
                .resizable(true)
                .default_width(330.0)
                .show(ctx, |ui| {
                    ui.heading("预览");
                    ui.horizontal(|ui| {
                        ui.selectable_value(&mut self.preview_tab, PreviewTab::Script, "剧本");
                        ui.selectable_value(&mut self.preview_tab, PreviewTab::Sprite, "立绘");
                        ui.selectable_value(&mut self.preview_tab, PreviewTab::Background, "背景");
                        ui.selectable_value(&mut self.preview_tab, PreviewTab::Music, "音乐");
                    });
                    ui.separator();
                    match self.preview_tab {
                        PreviewTab::Script => {
                            ui.horizontal(|ui| {
                                if ui.button("运行").clicked() {
                                    self.run_script();
                                }
                                if self.engine.is_some() {
                                    if ui.button("前进").clicked() {
                                        if let Some(engine) = self.engine.as_mut() {
                                            let _ = engine.advance();
                                        }
                                    }
                                    if ui.button("停止").clicked() {
                                        self.engine = None;
                                        self.status = "预览已停止".to_string();
                                    }
                                }
                            });
                            ui.separator();
                            self.show_preview(ui);
                        }
                        PreviewTab::Sprite => {
                            self.show_sprite_preview(ui);
                        }
                        PreviewTab::Background => {
                            self.show_bg_preview(ui);
                        }
                        PreviewTab::Music => {
                            self.show_music_preview(ui);
                        }
                    }
                });
        }

        // ---- 中央面板：编辑器或欢迎页 -----------------------------------
        egui::CentralPanel::default().show(ctx, |ui| {
            if self.show_welcome {
                self.show_welcome_panel(ui);
            } else if self.translation_mode {
                self.show_translation_view(ui);
            } else {
                self.show_editor(ui);
            }
        });

        // ---- 关于对话框 --------------------------------------------------
        if self.show_about {
            egui::Window::new("关于")
                .open(&mut self.show_about)
                .resizable(false)
                .collapsible(false)
                .default_width(380.0)
                .show(ctx, |ui| {
                    ui.add_space(8.0);
                    ui.heading("Akizuki*Rustgal 剧本编辑器");
                    ui.add_space(12.0);
                    ui.label(format!("引擎版本：v{}", env!("CARGO_PKG_VERSION")));
                    ui.label(format!("编辑器版本：v{}", env!("CARGO_PKG_VERSION")));
                    ui.add_space(8.0);
                    ui.label("为视觉小说设计的轻量级剧本编写工具。");
                    ui.add_space(12.0);
                    ui.horizontal(|ui| {
                        ui.label("GitHub：");
                        ui.hyperlink_to("AkizukiKokona/Akizuki-Rustgal", GITHUB_URL);
                    });
                });
        }

        // ---- 快捷键帮助窗口 --------------------------------------------------
        if self.show_shortcuts {
            egui::Window::new("快捷键帮助")
                .open(&mut self.show_shortcuts)
                .resizable(false)
                .collapsible(false)
                .default_width(450.0)
                .show(ctx, |ui| {
                    ui.heading("键盘快捷键");
                    ui.add_space(12.0);
                    ui.label(egui::RichText::new("文件操作").strong());
                    ui.separator();
                    let file_shortcuts = [
                        ("Ctrl+N", "新建剧本"),
                        ("Ctrl+O", "打开文件"),
                        ("Ctrl+S", "保存文件"),
                        ("Ctrl+R", "运行剧本"),
                        ("Ctrl+H", "显示此帮助窗口"),
                    ];
                    for (key, desc) in &file_shortcuts {
                        ui.horizontal(|ui| {
                            ui.add_space(8.0);
                            ui.label(egui::RichText::new(*key).monospace().color(egui::Color32::from_rgb(200, 180, 255)));
                            ui.label("—");
                            ui.label(*desc);
                        });
                    }
                    ui.add_space(8.0);
                    ui.label(egui::RichText::new("快速插入语法").strong());
                    ui.separator();
                    let insert_shortcuts = [
                        ("Ctrl+1", "+ 角色", "立绘上场"),
                        ("Ctrl+2", "- 角色", "立绘下场"),
                        ("Ctrl+3", "# 章节", "章节标题"),
                        ("Ctrl+4", "@bg 背景", "背景指令"),
                    ];
                    for (key, syntax, desc) in &insert_shortcuts {
                        ui.horizontal(|ui| {
                            ui.add_space(8.0);
                            ui.label(egui::RichText::new(*key).monospace().color(egui::Color32::from_rgb(180, 220, 255)));
                            ui.label("—");
                            ui.label(egui::RichText::new(*syntax).monospace().strong());
                            ui.label(format!("（{}）", desc));
                        });
                    }
                    ui.add_space(12.0);
                    ui.label("也可在顶部工具栏的「插入」按钮区域点击插入语法。");
                });
        }

        // ---- rpy 导入窗口 --------------------------------------------------
        if self.show_rpy_import {
            let mut close_window = false;
            let mut open = true;
            egui::Window::new("导入 Ren'Py 剧本")
                .open(&mut open)
                .resizable(true)
                .collapsible(false)
                .default_width(500.0)
                .default_height(400.0)
                .show(ctx, |ui| {
                    ui.heading("从 .rpy 文件导入");
                    ui.add_space(8.0);
                    ui.label("选择 Ren'Py 剧本文件（.rpy），转换后保存为 .akrs 格式。");
                    ui.add_space(12.0);

                    // 源文件选择
                    ui.label(egui::RichText::new("源文件：").strong());
                    ui.horizontal(|ui| {
                        let source_text = self.rpy_import_source
                            .as_ref()
                            .map(|p| p.display().to_string())
                            .unwrap_or_else(|| "（未选择）".to_string());
                        ui.label(&source_text);
                        if ui.button("选择文件...").clicked() {
                            // 打开文件选择对话框
                            let start_dir = self.work_dir.clone();
                            if let Ok(entries) = std::fs::read_dir(&start_dir) {
                                let mut picked_entries: Vec<_> = entries
                                    .filter_map(|e| e.ok())
                                    .filter_map(|e| {
                                        let p = e.path();
                                        let name = p.file_name()?.to_string_lossy().into_owned();
                                        Some(PickerEntry { name, is_dir: p.is_dir() })
                                    })
                                    .collect();
                                picked_entries.sort_by(|a, b| {
                                    b.is_dir.cmp(&a.is_dir).then_with(|| a.name.cmp(&b.name))
                                });
                                self.file_picker = Some(FilePickerState {
                                    mode: FilePickerMode::Open,
                                    current_dir: start_dir,
                                    entries: picked_entries,
                                    selected: None,
                                    filter: "rpy".to_string(),
                                });
                            }
                        }
                    });
                    ui.add_space(8.0);

                    // 目标路径
                    ui.label(egui::RichText::new("保存位置：").strong());
                    ui.horizontal(|ui| {
                        ui.text_edit_singleline(&mut self.rpy_import_target.display().to_string());
                        if ui.button("选择路径...").clicked() {
                            // 简化：直接使用工作目录
                            self.rpy_import_target = self.work_dir.join("scripts").join("imported.akrs");
                        }
                    });
                    ui.label(egui::RichText::new("提示：默认保存到项目的 scripts 目录，或桌面").color(egui::Color32::GRAY));
                    ui.add_space(12.0);

                    // 警告显示
                    if !self.rpy_import_warnings.is_empty() {
                        ui.label(egui::RichText::new("转换警告：").strong().color(egui::Color32::from_rgb(220, 180, 120)));
                        ui.separator();
                        for warn in &self.rpy_import_warnings {
                            ui.label(egui::RichText::new(warn).color(egui::Color32::from_rgb(200, 160, 100)));
                        }
                        ui.add_space(8.0);
                    }

                    // 导入按钮
                    ui.horizontal(|ui| {
                        if ui.button("导入").clicked() {
                            if let Some(source) = self.rpy_import_source.clone() {
                                // 执行转换
                                let target = self.rpy_import_target.clone();
                                self.convert_rpy_to_akrs(&source, &target);
                                close_window = true;
                            } else {
                                self.status = "请先选择 .rpy 文件".to_string();
                            }
                        }
                        if ui.button("取消").clicked() {
                            close_window = true;
                        }
                    });
                });
            if close_window {
                self.show_rpy_import = false;
            }
            self.show_rpy_import = self.show_rpy_import && open;
        }

        // ---- 文件选择对话框 ----------------------------------------------
        if self.file_picker.is_some() {
            let mut close = false;
            let mut confirm: Option<PathBuf> = None;
            let mut open = true;

            egui::Window::new("打开文件")
                .open(&mut open)
                .collapsible(false)
                .resizable(true)
                .default_size(egui::Vec2::new(560.0, 420.0))
                .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
                .show(ctx, |ui| {
                    // 顶部：当前路径 + 上一级
                    ui.horizontal(|ui| {
                        ui.label("路径：");
                        ui.label(egui::RichText::new(self.file_picker.as_ref().unwrap().current_dir.display().to_string()).monospace());
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.button("↑ 上一级").clicked() {
                                if let Some(parent) = self.file_picker.as_ref().unwrap().current_dir.parent() {
                                    let p = parent.to_path_buf();
                                    let entries = Self::read_picker_entries(&p, "");
                                    let fp = self.file_picker.as_mut().unwrap();
                                    fp.current_dir = p;
                                    fp.entries = entries;
                                    fp.selected = None;
                                    fp.filter.clear();
                                }
                            }
                        });
                    });
                    ui.add_space(6.0);

                    // 过滤输入
                    ui.horizontal(|ui| {
                        ui.label("过滤：");
                        let resp = ui.add_sized(
                            [ui.available_width(), 24.0],
                            egui::TextEdit::singleline(&mut self.file_picker.as_mut().unwrap().filter),
                        );
                        if resp.changed() {
                            let fp = self.file_picker.as_ref().unwrap();
                            let entries = Self::read_picker_entries(&fp.current_dir, &fp.filter);
                            self.file_picker.as_mut().unwrap().entries = entries;
                        }
                    });
                    ui.add_space(6.0);

                    // 文件列表
                    egui::ScrollArea::vertical()
                        .max_height(260.0)
                        .show(ui, |ui| {
                            let fp = self.file_picker.as_ref().unwrap();
                            let current_dir = fp.current_dir.clone();
                            let entries = fp.entries.clone();
                            let selected = fp.selected.clone();

                            for entry in &entries {
                                let is_selected = selected.as_ref().is_some_and(|s| s == &entry.name);
                                let label = if entry.is_dir {
                                    format!("📁  {}", entry.name)
                                } else {
                                    format!("📄  {}", entry.name)
                                };
                                let resp = ui.selectable_label(is_selected, label);
                                if resp.clicked() {
                                    let fp = self.file_picker.as_mut().unwrap();
                                    if entry.is_dir {
                                        // 单击进入目录
                                        let new_dir = current_dir.join(&entry.name);
                                        let new_entries = Self::read_picker_entries(&new_dir, &fp.filter);
                                        fp.current_dir = new_dir;
                                        fp.entries = new_entries;
                                        fp.selected = None;
                                    } else {
                                        fp.selected = Some(entry.name.clone());
                                    }
                                }
                                if resp.double_clicked() && !entry.is_dir {
                                    confirm = Some(current_dir.join(&entry.name));
                                }
                            }
                        });

                    ui.add_space(8.0);
                    ui.separator();
                    ui.add_space(8.0);

                    // 底部按钮
                    ui.horizontal(|ui| {
                        let has_selection = self.file_picker.as_ref().unwrap().selected.is_some();
                        if ui.add_enabled(has_selection, egui::Button::new("打开")).clicked() {
                            if let Some(name) = &self.file_picker.as_ref().unwrap().selected.clone() {
                                let path = self.file_picker.as_ref().unwrap().current_dir.join(name);
                                confirm = Some(path);
                            }
                        }
                        if ui.button("取消").clicked() {
                            close = true;
                        }
                    });
                });

            // 处理结果
            if !open {
                close = true;
            }
            if let Some(path) = confirm {
                if path.is_file() {
                    self.open_file_path(&path);
                }
                self.file_picker = None;
            }
            if close {
                self.file_picker = None;
            }
        }

        // ---- 目录选择对话框（打开项目） -----------------------------------
        if self.dir_picker.is_some() {
            let mut close = false;
            let mut confirm: Option<PathBuf> = None;
            let mut open = true;

            egui::Window::new("选择项目文件夹")
                .open(&mut open)
                .collapsible(false)
                .resizable(true)
                .default_size(egui::Vec2::new(560.0, 420.0))
                .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
                .show(ctx, |ui| {
                    // 顶部：当前路径 + 上一级
                    ui.horizontal(|ui| {
                        ui.label("路径：");
                        ui.label(egui::RichText::new(self.dir_picker.as_ref().unwrap().current_dir.display().to_string()).monospace());
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.button("↑ 上一级").clicked() {
                                if let Some(parent) = self.dir_picker.as_ref().unwrap().current_dir.parent() {
                                    let p = parent.to_path_buf();
                                    let entries = Self::read_picker_entries(&p, "");
                                    let dp = self.dir_picker.as_mut().unwrap();
                                    dp.current_dir = p;
                                    dp.entries = entries;
                                    dp.filter.clear();
                                }
                            }
                        });
                    });
                    ui.add_space(6.0);

                    // 过滤输入
                    ui.horizontal(|ui| {
                        ui.label("过滤：");
                        let resp = ui.add_sized(
                            [ui.available_width(), 24.0],
                            egui::TextEdit::singleline(&mut self.dir_picker.as_mut().unwrap().filter),
                        );
                        if resp.changed() {
                            let dp = self.dir_picker.as_ref().unwrap();
                            let entries = Self::read_picker_entries(&dp.current_dir, &dp.filter);
                            self.dir_picker.as_mut().unwrap().entries = entries;
                        }
                    });
                    ui.add_space(6.0);

                    // 目录列表（只显示目录）
                    egui::ScrollArea::vertical()
                        .max_height(260.0)
                        .show(ui, |ui| {
                            let dp = self.dir_picker.as_ref().unwrap();
                            let current_dir = dp.current_dir.clone();
                            let entries = dp.entries.clone();
                            let filter = dp.filter.clone();

                            for entry in &entries {
                                if !entry.is_dir {
                                    continue;
                                }
                                let label = format!("📁  {}", entry.name);
                                let resp = ui.selectable_label(false, label);
                                if resp.clicked() {
                                    let new_dir = current_dir.join(&entry.name);
                                    let new_entries = Self::read_picker_entries(&new_dir, &filter);
                                    let dp = self.dir_picker.as_mut().unwrap();
                                    dp.current_dir = new_dir;
                                    dp.entries = new_entries;
                                    dp.filter.clear();
                                }
                                if resp.double_clicked() {
                                    confirm = Some(current_dir.join(&entry.name));
                                }
                            }
                        });

                    ui.add_space(8.0);
                    ui.separator();
                    ui.add_space(8.0);

                    // 底部按钮
                    ui.horizontal(|ui| {
                        if ui.button("选择此目录").clicked() {
                            let dp = self.dir_picker.as_ref().unwrap();
                            confirm = Some(dp.current_dir.clone());
                        }
                        if ui.button("取消").clicked() {
                            close = true;
                        }
                    });
                });

            // 处理结果
            if !open {
                close = true;
            }
            if let Some(path) = confirm {
                if path.is_dir() {
                    self.open_project(&path);
                }
                self.dir_picker = None;
            }
            if close {
                self.dir_picker = None;
            }
        }

        // ---- 项目设置对话框 ----------------------------------------------
        if self.show_project_settings {
            let mut close = false;
            let mut open = self.show_project_settings;
            let mut title_changed = false;
            let mut new_title_val = self.project_config.title.clone();
            let mut subtitle_changed = false;
            let mut new_subtitle_val = self.project_config.subtitle.clone();

            egui::Window::new("项目设置")
                .open(&mut open)
                .collapsible(false)
                .resizable(false)
                .default_width(480.0)
                .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
                .show(ctx, |ui| {
                    ui.add_space(8.0);

                    ui.heading("标题设置");
                    ui.add_space(8.0);

                    // 主标题输入
                    ui.horizontal(|ui| {
                        ui.label("主标题：");
                        let resp = ui.add_sized(
                            [ui.available_width(), 28.0],
                            egui::TextEdit::singleline(&mut new_title_val),
                        );
                        if resp.lost_focus() && resp.changed() {
                            title_changed = true;
                        }
                    });
                    ui.add_space(4.0);

                    // 副标题输入
                    ui.horizontal(|ui| {
                        ui.label("副标题：");
                        let resp = ui.add_sized(
                            [ui.available_width(), 28.0],
                            egui::TextEdit::singleline(&mut new_subtitle_val),
                        );
                        if resp.lost_focus() && resp.changed() {
                            subtitle_changed = true;
                        }
                    });
                    ui.add_space(4.0);

                    ui.label(
                        egui::RichText::new("提示：标题过长（超过屏幕宽度约 1/4）时会显示警告。")
                            .small()
                            .color(egui::Color32::from_rgb(140, 150, 170)),
                    );

                    ui.add_space(12.0);
                    ui.separator();
                    ui.add_space(12.0);

                    ui.heading("项目信息");
                    ui.add_space(8.0);

                    ui.horizontal(|ui| {
                        ui.label("项目描述：");
                    });
                    ui.add_space(4.0);
                    ui.add_sized(
                        [ui.available_width(), 60.0],
                        egui::TextEdit::multiline(&mut self.project_config.description),
                    );
                    ui.add_space(8.0);

                    ui.horizontal(|ui| {
                        ui.label("作者：");
                        ui.add_sized(
                            [ui.available_width(), 28.0],
                            egui::TextEdit::singleline(&mut self.project_config.author),
                        );
                    });
                    ui.add_space(8.0);

                    ui.horizontal(|ui| {
                        ui.label("主剧本文件：");
                        ui.add_sized(
                            [ui.available_width(), 28.0],
                            egui::TextEdit::singleline(&mut self.project_config.main_script),
                        );
                    });

                    ui.add_space(16.0);
                    ui.separator();
                    ui.add_space(12.0);

                    ui.heading("运行设置");
                    ui.add_space(8.0);

                    ui.horizontal(|ui| {
                        ui.label("窗口标题：");
                        ui.add_sized(
                            [ui.available_width(), 28.0],
                            egui::TextEdit::singleline(&mut self.project_config.window_title)
                                .hint_text("留空则使用主标题"),
                        );
                    });
                    ui.add_space(8.0);

                    ui.horizontal(|ui| {
                        ui.label("默认语言：");
                        ui.add_sized(
                            [ui.available_width(), 28.0],
                            egui::TextEdit::singleline(&mut self.project_config.language)
                                .hint_text("zh-CN / en-US / ja-JP 等，留空为系统默认"),
                        );
                    });
                    ui.add_space(8.0);

                    ui.horizontal(|ui| {
                        ui.label("版本号：");
                        ui.add_sized(
                            [ui.available_width(), 28.0],
                            egui::TextEdit::singleline(&mut self.project_config.version)
                                .hint_text("例如 v1.0.0"),
                        );
                    });
                    ui.add_space(8.0);

                    let mut res_w = self.project_config.default_resolution.0.to_string();
                    let mut res_h = self.project_config.default_resolution.1.to_string();
                    ui.horizontal(|ui| {
                        ui.label("默认分辨率：");
                        ui.add(egui::TextEdit::singleline(&mut res_w).desired_width(80.0));
                        ui.label("×");
                        ui.add(egui::TextEdit::singleline(&mut res_h).desired_width(80.0));
                    });
                    if let Ok(w) = res_w.parse::<u32>() {
                        if let Ok(h) = res_h.parse::<u32>() {
                            self.project_config.default_resolution = (w, h);
                        }
                    }
                    ui.add_space(8.0);

                    ui.horizontal(|ui| {
                        ui.checkbox(&mut self.project_config.start_fullscreen, "启动时全屏");
                    });

                    ui.add_space(16.0);
                    ui.separator();
                    ui.add_space(8.0);

                    ui.horizontal(|ui| {
                        if ui.button("保存并关闭").clicked() {
                            close = true;
                        }
                        if ui.button("取消").clicked() {
                            close = true;
                        }
                    });
                });

            // 处理标题更改
            if title_changed {
                self.try_set_title(new_title_val.clone());
            }
            if subtitle_changed {
                self.try_set_subtitle(new_subtitle_val.clone());
            }

            if !open {
                close = true;
            }
            if close {
                self.save_project_config();
                self.show_project_settings = false;
            }
        }

        // ---- 标题过长警告对话框 -------------------------------------------
        if self.title_warning.is_some() {
            let mut confirm = false;
            let mut cancel = false;

            egui::Window::new("标题过长警告")
                .collapsible(false)
                .resizable(false)
                .default_width(420.0)
                .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
                .show(ctx, |ui| {
                    ui.add_space(8.0);

                    ui.label(
                        egui::RichText::new("⚠️ 警告")
                            .size(20.0)
                            .color(egui::Color32::from_rgb(255, 200, 100)),
                    );
                    ui.add_space(8.0);

                    ui.label(
                        "标题长度超过了屏幕宽度的约 1/4，可能会影响显示效果。",
                    );
                    ui.add_space(4.0);
                    ui.label("是否仍然要使用这个标题？");

                    ui.add_space(12.0);
                    ui.separator();
                    ui.add_space(8.0);

                    ui.horizontal(|ui| {
                        if ui.button("确认使用").clicked() {
                            confirm = true;
                        }
                        if ui.button("取消").clicked() {
                            cancel = true;
                        }
                    });
                });

            if confirm {
                self.confirm_title_warning();
            }
            if cancel {
                self.cancel_title_warning();
            }
        }

        // ---- 打包对话框 --------------------------------------------------
        if self.build.show {
            let mut close = false;
            let mut start = false;
            let mut open_dir = false;

            egui::Window::new("打包游戏")
                .collapsible(false)
                .resizable(true)
                .default_width(560.0)
                .default_height(420.0)
                .show(ctx, |ui| {
                    ui.add_space(4.0);
                    ui.label("选择目标平台：");

                    ui.horizontal(|ui| {
                        for platform in BuildPlatform::all() {
                            let mut checked = *self.build.selected.get(&platform).unwrap_or(&false);
                            if ui.checkbox(&mut checked, platform.label()).changed() {
                                self.build.selected.insert(platform, checked);
                            }
                        }
                    });

                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        ui.label("导出目录：");
                        ui.label(self.build.output_dir.display().to_string());
                    });

                    ui.add_space(8.0);
                    ui.separator();
                    ui.add_space(4.0);

                    // 构建日志
                    ui.horizontal(|ui| {
                        ui.label("构建日志：");
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            let follow = &mut self.build.log_follow;
                            if ui.checkbox(follow, "自动跟底").changed() && *follow {
                                // 重新开启跟底时立即请求重绘
                                ui.ctx().request_repaint();
                            }
                        });
                    });
                    let follow = self.build.log_follow;
                    egui::ScrollArea::vertical()
                        .max_height(200.0)
                        .stick_to_bottom(follow)
                        .auto_shrink([false, true])
                        .show(ui, |ui| {
                            ui.add(
                                egui::TextEdit::multiline(&mut self.build.log.as_str())
                                    .desired_width(f32::MAX)
                                    .font(egui::TextStyle::Monospace),
                            );
                        });

                    ui.add_space(8.0);

                    // 按钮区
                    ui.horizontal(|ui| {
                        let building = self.build.is_building();
                        if building {
                            ui.add_enabled(false, egui::Button::new("构建中..."));
                        } else if self.build.done {
                            if ui.button("重新打包").clicked() {
                                start = true;
                            }
                        } else {
                            if ui.button("开始打包").clicked() {
                                start = true;
                            }
                        }

                        if self.build.done {
                            if ui.button("打开目录").clicked() {
                                open_dir = true;
                            }
                        }

                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.button("关闭").clicked() {
                                close = true;
                            }
                        });
                    });
                });

            if start {
                self.start_build();
            }
            if open_dir {
                let abs_path = if self.build.output_dir.is_absolute() {
                    self.build.output_dir.clone()
                } else {
                    self.work_dir.join(&self.build.output_dir)
                };
                open_path_in_file_manager(&abs_path);
            }
            if close {
                // 不允许在构建中关闭
                if !self.build.is_building() {
                    self.build.show = false;
                }
            }
        }

        // ---- 放大预览弹窗 ------------------------------------------------
        if self.show_enlarged_preview {
            let mut close = false;
            let mut open = true;
            egui::Window::new("放大预览")
                .open(&mut open)
                .collapsible(false)
                .resizable(true)
                .default_size(egui::Vec2::new(960.0, 540.0))
                .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
                .show(ctx, |ui| {
                    // 根据当前标签页显示对应的放大预览
                    match self.preview_tab {
                        PreviewTab::Sprite => {
                            // 显示立绘放大预览
                            self.show_sprite_preview(ui);
                        }
                        PreviewTab::Background => {
                            // 显示背景放大预览
                            self.show_bg_preview(ui);
                        }
                        _ => {
                            ui.label("当前标签页不支持放大预览");
                        }
                    }
                    ui.separator();
                    if ui.button("关闭").clicked() {
                        close = true;
                    }
                });
            if close || !open {
                self.show_enlarged_preview = false;
            }
        }

        // ---- 查找替换对话框 ------------------------------------------------
        if self.show_find_replace_dialog {
            let mut close = false;
            let mut do_replace = false;
            let syntax = self.find_replace_syntax.clone();

            egui::Window::new("替换插入")
                .open(&mut self.show_find_replace_dialog)
                .collapsible(false)
                .resizable(false)
                .default_width(420.0)
                .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
                .show(ctx, |ui| {
                    ui.add_space(8.0);
                    ui.label("请在下方输入要替换的内容：");
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        ui.label("查找：");
                        ui.text_edit_singleline(&mut self.find_replace_target);
                    });
                    ui.add_space(4.0);
                    ui.label(
                        egui::RichText::new("替换为（生成的语法）：")
                            .color(egui::Color32::from_rgb(180, 180, 180)),
                    );
                    ui.label(
                        egui::RichText::new(&syntax)
                            .monospace()
                            .color(egui::Color32::from_rgb(200, 220, 255)),
                    );
                    ui.add_space(12.0);
                    ui.separator();
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.button("替换").clicked() {
                            do_replace = true;
                        }
                        if ui.button("取消").clicked() {
                            close = true;
                        }
                    });
                    ui.add_space(4.0);
                    ui.label(
                        egui::RichText::new("提示：直接追加到脚本末尾请点击「追加」按钮")
                            .small()
                            .color(egui::Color32::from_rgb(140, 150, 170)),
                    );
                });

            if do_replace {
                let target = self.find_replace_target.clone();
                if self.find_and_replace(&target, &syntax) {
                    self.status = format!("已将「{}」替换为生成的语法", target);
                } else {
                    self.editor_content.push_str(&format!("{}\n", syntax));
                    self.status = format!("未找到「{}」，已追加到脚本末尾", target);
                }
                close = true;
            }

            if close {
                self.show_find_replace_dialog = false;
                self.find_replace_target.clear();
                self.find_replace_syntax.clear();
            }
        }

        // ---- Cargo 未安装引导 -------------------------------------------
        if self.show_cargo_guide {
            let mut close = false;

            egui::Window::new("未检测到 Cargo")
                .collapsible(false)
                .resizable(false)
                .default_width(480.0)
                .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
                .show(ctx, |ui| {
                    ui.add_space(8.0);

                    ui.label(
                        egui::RichText::new("⚠️ 系统未检测到 Rust/Cargo")
                            .size(18.0)
                            .color(egui::Color32::from_rgb(255, 180, 80)),
                    );
                    ui.add_space(8.0);

                    ui.label("打包和预览功能需要 Rust 工具链。请按以下步骤安装：");
                    ui.add_space(6.0);

                    ui.label("1. 使用清华源加速安装（推荐）：");
                    ui.add_space(2.0);
                    ui.horizontal(|ui| {
                        let cmd = "export RUSTUP_DIST_SERVER=https://mirrors.tuna.tsinghua.edu.cn/rustup";
                        ui.label(egui::RichText::new(cmd).monospace().color(egui::Color32::from_rgb(120, 200, 120)));
                        if ui.small_button("复制").clicked() {
                            ui.output_mut(|o| o.copied_text = cmd.to_string());
                        }
                    });

                    ui.add_space(4.0);
                    ui.label("2. 安装 Rust（Linux/macOS）：");
                    ui.add_space(2.0);
                    ui.horizontal(|ui| {
                        let cmd = "curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh";
                        ui.label(egui::RichText::new(cmd).monospace().color(egui::Color32::from_rgb(120, 200, 120)));
                        if ui.small_button("复制").clicked() {
                            ui.output_mut(|o| o.copied_text = cmd.to_string());
                        }
                    });

                    ui.add_space(4.0);
                    ui.label("3. 或访问官网安装：");
                    ui.add_space(2.0);
                    ui.horizontal(|ui| {
                        if ui.link("https://rustup.rs").clicked() {
                            ui.output_mut(|o| o.open_url = Some(egui::output::OpenUrl::same_tab("https://rustup.rs")));
                        }
                    });

                    ui.add_space(8.0);
                    ui.label("安装完成后重启编辑器即可使用打包和预览功能。");

                    ui.add_space(12.0);
                    ui.separator();
                    ui.add_space(8.0);

                    ui.horizontal(|ui| {
                        if ui.button("我知道了").clicked() {
                            close = true;
                        }
                    });
                });

            if close {
                self.show_cargo_guide = false;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// 入口点
// ---------------------------------------------------------------------------

/// 启动 GUI 编辑器应用。
pub fn run_editor() -> Result<(), eframe::Error> {
    let icon = load_icon();
    let options = eframe::NativeOptions {
        initial_window_size: Some(egui::Vec2::new(1280.0, 820.0)),
        icon_data: icon,
        ..Default::default()
    };
    eframe::run_native(
        "Akizuki*Rustgal 剧本编辑器",
        options,
        Box::new(|cc| {
            install_cjk_fonts(&cc.egui_ctx);
            Box::new(EditorApp::default())
        }),
    )
}

/// 加载外部中文字体并安装到 egui 上下文（优先加载运行时字体文件，其次系统字体）。
fn install_cjk_fonts(ctx: &egui::Context) {
    let mut font_data: Option<Vec<u8>> = None;

    // 1. 运行时外部字体文件（最高优先级）
    let runtime_font_path = "assets/fonts/SourceHanSansSC-Regular-2.otf";
    if let Ok(bytes) = std::fs::read(runtime_font_path) {
        eprintln!("[editor] 中文字体已加载（运行时 OTF）");
        font_data = Some(bytes);
    }

    // 2. 系统字体回退（按平台分别列出候选路径）
    if font_data.is_none() {
        let mut sys_candidates: Vec<String> = Vec::new();
        #[cfg(target_os = "windows")]
        {
            let win_dir = std::env::var("WINDIR").unwrap_or_else(|_| "C:\\Windows".to_string());
            let fonts_dir = std::path::Path::new(&win_dir).join("Fonts");
            for name in &["msyh.ttc", "msyh.ttf", "msyhbd.ttc", "simsun.ttc", "simhei.ttf"] {
                sys_candidates.push(fonts_dir.join(name).to_string_lossy().into_owned());
            }
            if let Some(home) = std::env::var_os("USERPROFILE") {
                let user_fonts = std::path::Path::new(&home)
                    .join("AppData/Local/Microsoft/Windows/Fonts/msyh.ttc");
                sys_candidates.push(user_fonts.to_string_lossy().into_owned());
            }
        }
        #[cfg(target_os = "macos")]
        {
            for path in &[
                "/System/Library/Fonts/PingFang.ttc",
                "/Library/Fonts/PingFang.ttc",
                "/System/Library/Fonts/STHeiti Light.ttc",
            ] {
                sys_candidates.push((*path).to_string());
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
                sys_candidates.push((*path).to_string());
            }
        }
        for path in &sys_candidates {
            if let Ok(bytes) = std::fs::read(path) {
                eprintln!("[editor] 使用系统中文字体: {}", path);
                font_data = Some(bytes);
                break;
            }
        }
    }

    if let Some(bytes) = font_data {
        let mut fonts = egui::FontDefinitions::default();
        fonts
            .font_data
            .insert("cjk_font".to_owned(), egui::FontData::from_owned(bytes));

        // 将 CJK 字体插入到 proportional 和 monospace 的字体列表头部
        fonts
            .families
            .entry(egui::FontFamily::Proportional)
            .or_default()
            .insert(0, "cjk_font".to_owned());
        fonts
            .families
            .entry(egui::FontFamily::Monospace)
            .or_default()
            .insert(0, "cjk_font".to_owned());

        ctx.set_fonts(fonts);
    } else {
        eprintln!("[editor] 警告：未找到中文字体，中文可能无法正确显示");
    }
}

/// 从嵌入的 RGBA 数据加载窗口图标。
fn load_icon() -> Option<eframe::IconData> {
    let data = include_bytes!("../../../assets/icon_kokona_64.bin");
    const W: u32 = 64;
    const H: u32 = 64;
    if data.len() == (W * H * 4) as usize {
        Some(eframe::IconData {
            rgba: data.to_vec(),
            width: W,
            height: H,
        })
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// 辅助函数：文件名清理、诊断格式化、标签
// ---------------------------------------------------------------------------

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

/// 角色位置的可读标签。
fn position_label(p: &Position) -> String {
    match p {
        Position::Left => "左侧".to_string(),
        Position::Center => "中央".to_string(),
        Position::Right => "右侧".to_string(),
        Position::Custom(x) => format!("自定义({:.2})", x),
    }
}

// ---------------------------------------------------------------------------
// 语法高亮
// ---------------------------------------------------------------------------

/// 为整个缓冲区构建带逐 token 着色的 `LayoutJob`。
fn highlight_code(text: &str) -> egui::text::LayoutJob {
    let mut job = egui::text::LayoutJob::default();
    let lines: Vec<&str> = text.split('\n').collect();
    for (idx, line) in lines.iter().enumerate() {
        highlight_line(&mut job, line);
        if idx + 1 < lines.len() {
            // 重新插入被 `split` 消耗的换行符。
            job.append("\n", 0.0, text_format(COLOR_DEFAULT));
        }
    }
    job
}

/// 根据行首非空白 token 选择基础颜色。
fn line_base_color(trimmed: &str) -> egui::Color32 {
    // 以 `-` 开头的双字符标记必须先于单字符 `-` 方向标记检查。
    if trimmed.starts_with('#') {
        COLOR_SECTION
    } else if trimmed.starts_with("--") {
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
    } else {
        COLOR_DEFAULT
    }
}

/// 将单行的着色片段追加到 `job`。
///
/// 在一行内，`"..."` 字符串字面量和行尾 `--` 注释始终使用各自的专用颜色；
/// 其余内容使用行的基础颜色。基于 `char` 操作（通过 `char_indices`）以保留多字节 UTF-8 内容。
fn highlight_line(job: &mut egui::text::LayoutJob, line: &str) {
    let trimmed = line.trim_start();
    let leading_ws = &line[..line.len() - trimmed.len()];
    if !leading_ws.is_empty() {
        job.append(leading_ws, 0.0, text_format(COLOR_DEFAULT));
    }

    let base = line_base_color(trimmed);
    let chars: Vec<(usize, char)> = trimmed.char_indices().collect();
    let n = chars.len();

    let mut buf_start: Option<usize> = None;
    let mut buf_end: usize = 0;
    let mut i = 0;

    while i < n {
        let (bofs, c) = chars[i];

        if c == '"' {
            // 刷新待处理的基础着色文本，然后消费字符串字面量。
            if let Some(s) = buf_start {
                job.append(&trimmed[s..buf_end], 0.0, text_format(base));
                buf_start = None;
            }
            let start = bofs;
            let mut end_byte = bofs + 1; // 包含开引号
            i += 1;
            while i < n {
                let (eb, cc) = chars[i];
                end_byte = eb + cc.len_utf8();
                i += 1;
                if cc == '"' {
                    break;
                }
            }
            job.append(&trimmed[start..end_byte], 0.0, text_format(COLOR_STRING));
            continue;
        }

        if c == '-' && i + 1 < n && chars[i + 1].1 == '-' {
            // `--` 注释延续到行尾。
            if let Some(s) = buf_start {
                job.append(&trimmed[s..buf_end], 0.0, text_format(base));
            }
            job.append(&trimmed[bofs..], 0.0, text_format(COLOR_COMMENT));
            return;
        }

        // 累积到基础着色段中。
        if buf_start.is_none() {
            buf_start = Some(bofs);
        }
        buf_end = bofs + c.len_utf8();
        i += 1;
    }

    if let Some(s) = buf_start {
        job.append(&trimmed[s..buf_end], 0.0, text_format(base));
    }
}

/// 构建带指定文字颜色的等宽 `TextFormat`。
fn text_format(color: egui::Color32) -> egui::text::TextFormat {
    egui::text::TextFormat {
        font_id: egui::FontId::monospace(FONT_SIZE),
        color,
        ..Default::default()
    }
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
    fn highlight_preserves_text() {
        // LayoutJob 的 `text` 必须等于输入，以使光标位置对 TextEdit 有效。
        for src in [
            "",
            "hello",
            "# Title\nAki: \"Hi\"\n-- comment\n$x = 1\n",
            "多行\n中文 \"字\" 符\n",
        ] {
            let job = highlight_code(src);
            assert_eq!(job.text, src, "mismatch for {:?}", src);
        }
    }

    #[test]
    fn phase_and_position_labels() {
        assert_eq!(phase_label(EnginePhase::ChoicePending), "等待选择");
        assert_eq!(position_label(&Position::Left), "左侧");
        assert_eq!(position_label(&Position::Custom(0.25)), "自定义(0.25)");
    }

    #[test]
    fn run_script_compiles_sample() {
        let mut app = EditorApp::default();
        app.editor_content = SAMPLE_SCRIPT.to_string();
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
        assert!(app.editor_content.is_empty(), "editor should be empty on first launch");
    }

    #[test]
    fn load_sample_hides_welcome() {
        let mut app = EditorApp::default();
        app.load_sample();
        assert!(!app.show_welcome, "welcome panel should hide after loading sample");
        assert!(!app.editor_content.is_empty(), "editor should have content after loading sample");
    }

    #[test]
    fn new_file_hides_welcome() {
        let mut app = EditorApp::default();
        app.new_file();
        assert!(!app.show_welcome, "welcome panel should hide after new file");
    }
}
