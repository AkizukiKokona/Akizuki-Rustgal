//! Akizuki*Rustgal 剧本编辑器
//!
//! 基于 [`iced 0.13`] 重建的可视化小说剧本编辑器。采用 Elm 架构
//! （State + Message + update + view），三栏布局：
//!
//! - **左栏**：工作目录中的 `.akrs` 文件列表，支持新建 / 打开 / 保存。
//! - **中栏**：文本编辑器（带语法高亮）或蓝图节点画布（可切换）。
//! - **右栏**：预览面板（脚本 / 诊断 / 立绘 / 背景 / 音乐）。
//!
//! # 字体加载策略
//! 优先加载运行时 OTF（`assets/fonts/SourceHanSansSC-Regular-2.otf`），
//! 其次按平台查找系统中文字体，最后回退到 iced 默认字体。通过
//! [`iced::application::Application::font`] 注册字节，`cosmic-text`
//! 做字形回退，彻底避免中文显示方框问题。
//!
//! # 文件操作
//! 使用 [`rfd`] 原生文件对话框（打开 / 保存 / 选目录），替代旧版自实现的
//! 文件浏览器，彻底避免「读不了文件」类 bug。

// ---- 纯逻辑模块（无 UI 依赖，可独立复用）----
pub mod flow;
pub mod fs_util;
pub mod highlight;
pub mod palette;
pub mod parse;
pub mod rpy;
pub mod state;
pub mod templates;
pub mod widgets;

// ---- 重导出常用类型 ----
pub use state::{BlueprintLink, BlueprintNode, BlueprintState, NodeKind};
pub use templates::{BUILD_NUMBER, FONT_SIZE, GITHUB_URL, NODE_TEMPLATES, SAMPLE_SCRIPT};
pub use widgets::{BlueprintMsg, BlueprintProgram};

use akrs_core::{compile, DismissedWarnings, ErrSeverity, ProjectConfig, RecentProjects};
use akrs_runtime::Translator;

use iced::widget::{
    button, canvas, checkbox, column, container, horizontal_space, row, scrollable, stack,
    text, text_editor, text_input, vertical_space,
};
use iced::{
    alignment, Alignment, Background, Border, Color, Element, Font, Length, Padding, Pixels,
    Point, Size, Subscription, Task, Theme,
};
use std::collections::HashMap;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// 加粗字体（iced 0.13 中 `text` 没有 `.font(BOLD)` 方法，需要通过 `.font()` 设置）。
const BOLD: Font = Font {
    weight: iced::font::Weight::Bold,
    ..Font::DEFAULT
};

// ===========================================================================
// 预览面板标签页
// ===========================================================================

/// 右栏预览面板的标签页类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreviewTab {
    /// 编译诊断信息（错误 / 警告 / 提示）。
    Diagnostics,
    /// 立绘预览。
    Sprite,
    /// 背景预览。
    Background,
    /// 音乐预览。
    Music,
}

impl Default for PreviewTab {
    fn default() -> Self {
        Self::Diagnostics
    }
}

impl PreviewTab {
    /// 标签页显示文字。
    fn label(self) -> &'static str {
        match self {
            Self::Diagnostics => "诊断",
            Self::Sprite => "立绘",
            Self::Background => "背景",
            Self::Music => "音乐",
        }
    }

    /// 所有标签页。
    const ALL: [Self; 4] = [Self::Diagnostics, Self::Sprite, Self::Background, Self::Music];
}

// ===========================================================================
// 构建平台
// ===========================================================================

/// 打包目标平台。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BuildPlatform {
    Windows,
    MacOS,
    Linux,
}

impl BuildPlatform {
    fn label(self) -> &'static str {
        match self {
            Self::Windows => "Windows",
            Self::MacOS => "macOS",
            Self::Linux => "Linux",
        }
    }

    fn target_triple(self) -> &'static str {
        match self {
            Self::Windows => "x86_64-pc-windows-gnu",
            Self::MacOS => "x86_64-apple-darwin",
            Self::Linux => "x86_64-unknown-linux-gnu",
        }
    }

    fn binary_name(self) -> &'static str {
        match self {
            Self::Windows => "akrs-game.exe",
            _ => "akrs-game",
        }
    }

    const ALL: [Self; 3] = [Self::Windows, Self::MacOS, Self::Linux];
}

/// 打包状态。
struct BuildState {
    /// 是否显示打包对话框。
    show: bool,
    /// 各平台是否勾选。
    selected: HashMap<BuildPlatform, bool>,
    /// 构建日志输出。
    log: String,
    /// 待构建平台队列。
    queue: Vec<BuildPlatform>,
    /// 当前正在构建的平台。
    building: Option<BuildPlatform>,
    /// 构建子进程。
    process: Option<Child>,
    /// 是否全部完成。
    done: bool,
    /// 导出目录。
    output_dir: PathBuf,
    /// 成功的平台列表。
    succeeded: Vec<BuildPlatform>,
    /// 失败的平台列表。
    failed: Vec<(BuildPlatform, String)>,
}

impl Default for BuildState {
    fn default() -> Self {
        let mut selected = HashMap::new();
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
            queue: Vec::new(),
            building: None,
            process: None,
            done: false,
            output_dir: PathBuf::from("build"),
            succeeded: Vec::new(),
            failed: Vec::new(),
        }
    }
}

impl BuildState {
    fn is_building(&self) -> bool {
        self.building.is_some() || !self.queue.is_empty()
    }

    fn log_line(&mut self, msg: impl AsRef<str>) {
        self.log.push_str(msg.as_ref());
        if !msg.as_ref().ends_with('\n') {
            self.log.push('\n');
        }
    }
}

// ===========================================================================
// 立绘 / 背景 / 音乐预览状态
// ===========================================================================

/// 立绘预览状态：列出工作目录中的立绘资源文件。
#[derive(Default)]
struct SpritePreview {
    /// 已扫描到的立绘文件名列表（相对 assets/sprites/）。
    items: Vec<String>,
    /// 当前选中的立绘文件名。
    selected: Option<String>,
}

/// 背景预览状态。
#[derive(Default)]
struct BackgroundPreview {
    items: Vec<String>,
    selected: Option<String>,
}

/// 音乐预览状态。
#[derive(Default)]
struct MusicPreview {
    items: Vec<String>,
    selected: Option<String>,
}

// ===========================================================================
// Message 枚举
// ===========================================================================

/// 编辑器的所有消息类型。
#[derive(Debug, Clone)]
pub enum Message {
    /// 文本编辑动作（输入、删除、光标移动等）。
    Edit(text_editor::Action),
    /// 文件名输入框内容变化。
    FileNameInputChanged(String),
    /// 点击文件列表中的某一项。
    FileListItemClicked(String),

    // ---- 文件操作 ----
    /// 新建文件。
    NewFile,
    /// 打开文件（弹出 rfd 对话框）。
    OpenFile,
    /// 保存文件（若已有路径则直接保存，否则另存为）。
    SaveFile,
    /// 另存为（弹出 rfd 对话框）。
    SaveAsFile,
    /// rfd 文件对话框结果：选中的文件路径（None 表示取消）。
    FilePicked(Option<PathBuf>),
    /// rfd 保存对话框结果：保存到的文件路径。
    FileSaved(Option<PathBuf>),

    // ---- 项目操作 ----
    /// 打开项目（弹出 rfd 选目录对话框）。
    OpenProject,
    /// rfd 目录选择结果。
    ProjectPicked(Option<PathBuf>),
    /// 点击最近项目列表中的某一项。
    RecentProjectClicked(PathBuf),

    // ---- 运行 / 预览 ----
    /// 编译当前剧本并尝试启动游戏预览。
    RunScript,
    /// 停止游戏预览进程。
    StopScript,
    /// 游戏预览进程退出。
    GameProcessExited,
    /// 引擎动画帧（用于打字机 / 过渡效果）。
    EngineTick,

    // ---- 构建打包 ----
    /// 打开 / 关闭打包对话框。
    ToggleBuildDialog,
    /// 切换某平台的勾选状态。
    ToggleBuildPlatform(BuildPlatform),
    /// 开始构建。
    StartBuild,
    /// 构建轮询：读取子进程输出 / 检查是否退出。
    BuildTick,
    /// 关闭打包对话框。
    CloseBuildDialog,

    // ---- 模式切换 ----
    /// 切换蓝图模式。
    ToggleBlueprint,
    /// 切换对照翻译模式。
    ToggleTranslation,
    /// 切换查找替换对话框。
    ToggleFindReplace,
    /// 从蓝图模式返回文本模式时同步脚本文本。
    SyncBlueprintToText,
    /// 从文本模式进入蓝图模式时同步脚本到节点。
    SyncTextToBlueprint,

    // ---- 蓝图交互 ----
    /// 蓝图画布消息。
    Blueprint(BlueprintMsg),
    /// 从模板添加蓝图节点。
    AddBlueprintNode(usize),

    // ---- 预览面板 ----
    /// 切换右栏预览标签页。
    TabSelected(PreviewTab),
    /// 选中立绘预览项。
    SpriteSelected(String),
    /// 选中背景预览项。
    BackgroundSelected(String),
    /// 选中音乐预览项。
    MusicSelected(String),

    // ---- 对话框 / 弹窗 ----
    ShowAbout,
    ShowShortcuts,
    ShowWelcome,
    CloseDialog,
    /// 关闭窗口请求（点叉）。
    ExitRequested,
    /// 确认退出（放弃未保存修改）。
    ConfirmExit,

    // ---- 快捷插入语法 ----
    InsertSnippet(String),
    /// 插入蓝图节点模板文本到编辑器。
    InsertTemplate(usize),

    // ---- 撤销 / 重做 ----
    Undo,
    Redo,

    // ---- 查找替换 ----
    FindReplaceTargetChanged(String),
    FindNext,
    ReplaceAll,

    // ---- rpy 导入 ----
    ToggleRpyImport,
    RpyFilePicked(Option<PathBuf>),
    ConvertRpy,

    // ---- 状态栏 ----
    /// 更新底部状态栏文字。
    SetStatus(String),

    // ---- 窗口 ----
    /// 窗口关闭事件（由 iced 的 subscription 产生）。
    WindowClosed,

    // ---- 无操作 ----
    None,
}

// ===========================================================================
// 模态弹窗叠加层辅助函数
// ===========================================================================

/// 构建模态弹窗叠加层：底层内容 + 半透明遮罩 + 居中的内容卡片。
///
/// iced 0.13 没有原生的 `overlay(inner, base, alignment)` 自由函数，模态弹窗通过
/// [`iced::widget::stack!`] 宏实现：栈中后入的元素覆盖在先入的元素之上。
///
/// # 参数
/// - `content`：底层被遮挡的内容（通常是主界面）。
/// - `panel`：弹窗内容卡片（已被 `max_width` 限制宽度）。
fn modal_overlay<'a>(
    content: Element<'a, Message>,
    panel: impl Into<Element<'a, Message>>,
) -> Element<'a, Message> {
    // 半透明背景遮罩：占满整个区域，拦截鼠标事件，避免穿透到下层。
    // iced 0.13 的 `Color::from_rgba8` 第 4 个参数是 f32（0.0–1.0），不是 u8。
    let backdrop = container(text(""))
        .width(Length::Fill)
        .height(Length::Fill)
        .style(|_theme: &Theme| container::Style {
            background: Some(Background::Color(Color::from_rgba(
                0.0,
                0.0,
                0.0,
                170.0 / 255.0,
            ))),
            ..Default::default()
        });

    // 内容卡片容器：填满整个区域后让 panel 居中，并加上背景色与圆角边框。
    // `Border::rounded` 接受 1 个参数（radius），通过 `.color()` / `.width()` 链式设置。
    let panel_wrap = container(panel)
        .width(Length::Fill)
        .height(Length::Fill)
        .center_x(Length::Fill)
        .center_y(Length::Fill)
        .style(|_theme: &Theme| container::Style {
            background: Some(Background::Color(Color::from_rgb(
                28.0 / 255.0,
                30.0 / 255.0,
                38.0 / 255.0,
            ))),
            border: iced::border::rounded(10.0)
                .color(Color::from_rgb(80.0 / 255.0, 80.0 / 255.0, 110.0 / 255.0))
                .width(1.0),
            text_color: Some(Color::WHITE),
            ..Default::default()
        });

    stack![content, backdrop, panel_wrap].into()
}

// ===========================================================================
// EditorApp
// ===========================================================================

/// 编辑器主应用状态。
pub struct EditorApp {
    // ---- 文本编辑 ----
    /// text_editor 组件的内容（iced 要求独立持有）。
    editor_content: text_editor::Content,
    /// 当前加载 / 保存的文件路径（未保存时为 None）。
    current_file: Option<PathBuf>,
    /// 扫描文件列表和保存 / 打开使用的工作目录。
    work_dir: PathBuf,
    /// 左栏中编辑的文件名输入。
    file_name_input: String,
    /// work_dir 中 `.akrs` 文件的缓存列表。
    file_list: Vec<String>,
    /// 底部状态栏文字。
    status: String,
    /// 格式化后的编译诊断信息。
    diagnostics: Vec<String>,

    // ---- UI 状态 ----
    /// 是否显示首次启动欢迎面板。
    show_welcome: bool,
    /// 是否显示「关于」对话框。
    show_about: bool,
    /// 是否显示快捷键帮助窗口。
    show_shortcuts: bool,
    /// 是否显示查找替换对话框。
    show_find_replace: bool,
    /// 是否显示 rpy 导入窗口。
    show_rpy_import: bool,
    /// 右栏预览的当前标签页。
    preview_tab: PreviewTab,

    // ---- 项目 ----
    /// 当前项目配置（project.json）。
    project_config: ProjectConfig,
    /// 是否已加载项目配置。
    project_loaded: bool,
    /// 项目根目录。
    project_dir: Option<PathBuf>,
    /// 最近打开的项目列表。
    recent_projects: RecentProjects,
    /// 本地「不再显示」的项目警告忽略列表。
    dismissed_warnings: DismissedWarnings,

    // ---- 模式 ----
    /// 是否处于蓝图模式。
    blueprint_mode: bool,
    /// 蓝图编辑器状态。
    blueprint: BlueprintState,
    /// 是否处于对照翻译模式。
    translation_mode: bool,
    /// 当前编辑的翻译目标语言代码。
    translation_target_lang: String,
    /// 当前加载的翻译文件。
    translation_file: Option<Translator>,

    // ---- 构建 ----
    /// 打包状态。
    build: BuildState,
    /// 游戏预览子进程。
    game_process: Option<Child>,

    // ---- 预览 ----
    /// 立绘预览状态。
    sprite_preview: SpritePreview,
    /// 背景预览状态。
    bg_preview: BackgroundPreview,
    /// 音乐预览状态。
    music_preview: MusicPreview,

    // ---- 撤销 / 重做 ----
    /// 撤销历史栈（旧 → 新），每项是某次提交时的编辑器内容快照。
    undo_stack: Vec<String>,
    /// 重做历史栈（旧 → 新）。
    redo_stack: Vec<String>,
    /// 上一次提交到历史栈的编辑器内容。
    last_committed: String,
    /// 当前是否有未提交到历史栈的编辑（连续输入合并用）。
    edit_dirty: bool,
    /// 最后一次编辑的时间戳。
    last_edit_time: Option<Instant>,
    /// 撤销 / 重做正在执行中，跳过变化检测。
    undo_redo_in_progress: bool,

    // ---- 保存状态 ----
    /// 上次保存 / 加载时的内容快照，用于判断是否有未保存修改。
    saved_content: String,

    // ---- 查找替换 ----
    find_replace_target: String,

    // ---- rpy 导入 ----
    rpy_import_source: Option<PathBuf>,
    rpy_import_warnings: Vec<String>,

    // ---- 退出 ----
    /// 是否正在等待退出确认。
    pending_exit: bool,
    /// 用户已确认退出。
    force_close: bool,
}

impl Default for EditorApp {
    fn default() -> Self {
        let work_dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let recent_projects = RecentProjects::load();
        let dismissed_warnings = DismissedWarnings::load();
        let project_config = ProjectConfig::default();

        let mut app = Self {
            editor_content: text_editor::Content::new(),
            current_file: None,
            work_dir: work_dir.clone(),
            file_name_input: "untitled.akrs".to_string(),
            file_list: Vec::new(),
            status: "就绪".to_string(),
            diagnostics: Vec::new(),
            show_welcome: true,
            show_about: false,
            show_shortcuts: false,
            show_find_replace: false,
            show_rpy_import: false,
            preview_tab: PreviewTab::Diagnostics,
            project_config,
            project_loaded: false,
            project_dir: None,
            recent_projects,
            dismissed_warnings,
            blueprint_mode: false,
            blueprint: BlueprintState::default(),
            translation_mode: false,
            translation_target_lang: "en-US".to_string(),
            translation_file: None,
            build: BuildState::default(),
            game_process: None,
            sprite_preview: SpritePreview::default(),
            bg_preview: BackgroundPreview::default(),
            music_preview: MusicPreview::default(),
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            last_committed: String::new(),
            edit_dirty: false,
            last_edit_time: None,
            undo_redo_in_progress: false,
            saved_content: String::new(),
            find_replace_target: String::new(),
            rpy_import_source: None,
            rpy_import_warnings: Vec::new(),
            pending_exit: false,
            force_close: false,
        };

        // 扫描工作目录中的 .akrs 文件。
        app.refresh_file_list();
        // 扫描预览资源。
        app.refresh_preview_resources();

        app
    }
}

impl EditorApp {
    // =======================================================================
    // 文件操作
    // =======================================================================

    /// 重新扫描工作目录中的 `.akrs` 文件列表。
    fn refresh_file_list(&mut self) {
        self.file_list.clear();
        if let Ok(entries) = std::fs::read_dir(&self.work_dir) {
            let mut files: Vec<String> = entries
                .filter_map(|e| e.ok())
                .filter_map(|e| {
                    let name = e.file_name().to_string_lossy().into_owned();
                    if name.ends_with(".akrs") {
                        Some(name)
                    } else {
                        None
                    }
                })
                .collect();
            files.sort();
            self.file_list = files;
        }
    }

    /// 重新扫描预览资源（立绘 / 背景 / 音乐）。
    fn refresh_preview_resources(&mut self) {
        self.sprite_preview.items = fs_util::scan_dir(&self.work_dir.join("assets/sprites"), &["png", "jpg", "webp"]);
        self.bg_preview.items = fs_util::scan_dir(&self.work_dir.join("assets/backgrounds"), &["png", "jpg", "webp"]);
        self.music_preview.items = fs_util::scan_dir(&self.work_dir.join("assets/music"), &["mp3", "ogg", "wav", "flac"]);
    }

    /// 获取编辑器当前文本内容。
    fn editor_text(&self) -> String {
        self.editor_content.text()
    }

    /// 设置编辑器文本内容（替换全部）。
    fn set_editor_text(&mut self, text: &str) {
        self.editor_content = text_editor::Content::with_text(text);
    }

    /// 新建空白文件。
    fn new_file(&mut self) {
        self.set_editor_text(templates::NEW_TEMPLATE);
        self.current_file = None;
        self.file_name_input = "untitled.akrs".to_string();
        self.saved_content = templates::NEW_TEMPLATE.to_string();
        self.undo_stack.clear();
        self.redo_stack.clear();
        self.last_committed = templates::NEW_TEMPLATE.to_string();
        self.status = "已新建空白文件".to_string();
        self.diagnostics.clear();
    }

    /// 打开指定路径的文件并加载到编辑器。
    fn open_file_path(&mut self, path: PathBuf) {
        match std::fs::read_to_string(&path) {
            Ok(content) => {
                self.set_editor_text(&content);
                self.saved_content = content.clone();
                self.current_file = Some(path.clone());
                if let Some(name) = path.file_name() {
                    self.file_name_input = name.to_string_lossy().into_owned();
                }
                if let Some(parent) = path.parent() {
                    self.work_dir = parent.to_path_buf();
                    self.refresh_file_list();
                    self.refresh_preview_resources();
                }
                self.undo_stack.clear();
                self.redo_stack.clear();
                self.last_committed = content;
                self.status = format!("已打开: {}", path.display());
                self.compile_and_show_diagnostics();
            }
            Err(e) => {
                self.status = format!("打开失败: {e}");
            }
        }
    }

    /// 将编辑器内容保存到指定路径。
    fn save_to_path(&mut self, path: PathBuf) {
        let content = self.editor_text();
        match std::fs::write(&path, &content) {
            Ok(()) => {
                self.saved_content = content.clone();
                self.current_file = Some(path.clone());
                if let Some(name) = path.file_name() {
                    self.file_name_input = name.to_string_lossy().into_owned();
                }
                if let Some(parent) = path.parent() {
                    self.work_dir = parent.to_path_buf();
                    self.refresh_file_list();
                }
                self.status = format!("已保存: {}", path.display());
            }
            Err(e) => {
                self.status = format!("保存失败: {e}");
            }
        }
    }

    /// 编译当前编辑器内容并更新诊断面板。
    fn compile_and_show_diagnostics(&mut self) {
        let source = self.editor_text();
        if source.trim().is_empty() {
            self.diagnostics.clear();
            return;
        }
        let (_, errors) = compile(&source);
        self.diagnostics = parse::format_errors(&errors);
        if self.diagnostics.is_empty() {
            self.status = "编译通过，无错误".to_string();
        } else {
            let errs = errors.iter().filter(|e| e.severity == ErrSeverity::Error).count();
            let warns = errors.iter().filter(|e| e.severity == ErrSeverity::Warning).count();
            self.status = format!("诊断: {errs} 错误, {warns} 警告");
        }
    }

    // =======================================================================
    // 运行 / 预览
    // =======================================================================

    /// 启动游戏预览进程。
    fn run_script(&mut self) {
        let content = self.editor_text();
        if content.trim().is_empty() {
            self.status = "内容为空，无法运行".to_string();
            return;
        }
        // 若当前文件未保存，提示先保存。
        if self.current_file.is_none() {
            self.status = "请先保存文件再运行".to_string();
            return;
        }
        // 保存当前文件（确保运行的是最新内容）。
        if let Some(path) = &self.current_file {
            let _ = std::fs::write(path, &content);
        }
        // 启动 akrs-game 进程。
        let cwd = self
            .project_dir
            .as_ref()
            .unwrap_or(&self.work_dir)
            .clone();
        let mut cmd = Command::new("./akrs-game");
        cmd.current_dir(&cwd);
        cmd.stdin(Stdio::null());
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());
        match cmd.spawn() {
            Ok(child) => {
                self.game_process = Some(child);
                self.status = "游戏预览已启动".to_string();
            }
            Err(e) => {
                self.status = format!("启动失败: {e}（请确保 akrs-game 在项目目录中）");
            }
        }
    }

    /// 停止游戏预览进程。
    fn stop_script(&mut self) {
        if let Some(mut child) = self.game_process.take() {
            let _ = child.kill();
            let _ = child.wait();
            self.status = "已停止游戏预览".to_string();
        }
    }

    // =======================================================================
    // 撤销 / 重做
    // =======================================================================

    /// 提交当前编辑器内容到撤销历史栈。
    fn commit_undo(&mut self) {
        if self.undo_redo_in_progress {
            return;
        }
        let current = self.editor_text();
        if current == self.last_committed {
            return;
        }
        self.undo_stack.push(self.last_committed.clone());
        if self.undo_stack.len() > templates::UNDO_LIMIT {
            self.undo_stack.remove(0);
        }
        self.last_committed = current;
        self.edit_dirty = false;
    }

    /// 撤销。
    fn undo(&mut self) {
        self.commit_undo();
        if let Some(prev) = self.undo_stack.pop() {
            let current = self.editor_text();
            self.redo_stack.push(current);
            self.undo_redo_in_progress = true;
            self.set_editor_text(&prev);
            self.undo_redo_in_progress = false;
            self.last_committed = prev;
            self.status = "已撤销".to_string();
        }
    }

    /// 重做。
    fn redo(&mut self) {
        if let Some(next) = self.redo_stack.pop() {
            let current = self.editor_text();
            self.undo_stack.push(current);
            self.undo_redo_in_progress = true;
            self.set_editor_text(&next);
            self.undo_redo_in_progress = false;
            self.last_committed = next;
            self.status = "已重做".to_string();
        }
    }

    // =======================================================================
    // 蓝图模式
    // =======================================================================

    /// 进入蓝图模式：从脚本文本生成节点。
    fn enter_blueprint_mode(&mut self) {
        let text = self.editor_text();
        self.blueprint = BlueprintState::default();
        self.blueprint.from_script(&text);
        self.blueprint_mode = true;
        self.status = "已进入蓝图模式".to_string();
    }

    /// 离开蓝图模式：从节点生成脚本回写到编辑器。
    fn leave_blueprint_mode(&mut self) {
        let script = self.blueprint.to_script();
        self.set_editor_text(&script);
        self.blueprint_mode = false;
        self.status = "已返回文本模式".to_string();
        self.compile_and_show_diagnostics();
    }

    // =======================================================================
    // 查找替换
    // =======================================================================

    /// 查找下一个匹配项。
    fn find_next(&self) -> Task<Message> {
        // 简化实现：仅更新状态。
        // 完整实现需要操作 text_editor::Content 的光标位置。
        Task::none()
    }

    // =======================================================================
    // 构建打包
    // =======================================================================

    /// 启动构建队列。
    fn start_build(&mut self) {
        let platforms: Vec<BuildPlatform> = BuildPlatform::ALL
            .into_iter()
            .filter(|p| *self.build.selected.get(p).unwrap_or(&false))
            .collect();
        if platforms.is_empty() {
            self.build.log_line("未选择任何平台");
            return;
        }
        self.build.queue = platforms;
        self.build.log.clear();
        self.build.succeeded.clear();
        self.build.failed.clear();
        self.build.done = false;
        self.build.log_line("开始构建...");
        self.spawn_next_build();
    }

    /// 启动队列中下一个平台的构建。
    fn spawn_next_build(&mut self) {
        if let Some(platform) = self.build.queue.first().copied() {
            self.build.building = Some(platform);
            let target = platform.target_triple();
            let cmd = Command::new("cargo")
                .args(["build", "--release", "--target", target, "-p", "akrs-game"])
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn();
            match cmd {
                Ok(child) => {
                    self.build.process = Some(child);
                    self.build.log_line(format!("正在构建 {} ({target})...", platform.label()));
                }
                Err(e) => {
                    self.build.failed.push((platform, e.to_string()));
                    self.build.log_line(format!("构建 {} 失败: {e}", platform.label()));
                    self.build.building = None;
                    self.build.queue.remove(0);
                }
            }
        } else {
            self.build.building = None;
            self.build.done = true;
            self.build.log_line("构建完成");
        }
    }

    /// 轮询构建子进程：读取输出 / 检查退出。
    fn poll_build(&mut self) {
        if let Some(platform) = self.build.building {
            if let Some(child) = self.build.process.as_mut() {
                // 尝试读取 stdout / stderr（非阻塞）。
                use std::io::Read;
                if let Some(stdout) = child.stdout.as_mut() {
                    let mut buf = [0u8; 4096];
                    if let Ok(n) = stdout.read(&mut buf) {
                        if n > 0 {
                            if let Ok(s) = std::str::from_utf8(&buf[..n]) {
                                self.build.log.push_str(s);
                            }
                        }
                    }
                }
                if let Some(stderr) = child.stderr.as_mut() {
                    let mut buf = [0u8; 4096];
                    if let Ok(n) = stderr.read(&mut buf) {
                        if n > 0 {
                            if let Ok(s) = std::str::from_utf8(&buf[..n]) {
                                self.build.log.push_str(s);
                            }
                        }
                    }
                }
                // 检查进程是否退出（非阻塞 try_wait）。
                match child.try_wait() {
                    Ok(Some(status)) => {
                        if status.success() {
                            self.build.succeeded.push(platform);
                            self.build.log_line(format!("构建 {} 成功", platform.label()));
                        } else {
                            self.build.failed.push((platform, status.to_string()));
                            self.build.log_line(format!("构建 {} 失败: {status}", platform.label()));
                        }
                        self.build.process = None;
                        self.build.building = None;
                        self.build.queue.remove(0);
                        self.spawn_next_build();
                    }
                    Ok(None) => {
                        // 仍在运行，继续等待。
                    }
                    Err(e) => {
                        self.build.failed.push((platform, e.to_string()));
                        self.build.log_line(format!("构建 {} 错误: {e}", platform.label()));
                        self.build.process = None;
                        self.build.building = None;
                        self.build.queue.remove(0);
                        self.spawn_next_build();
                    }
                }
            }
        }
    }

    // =======================================================================
    // 状态查询
    // =======================================================================

    /// 是否有未保存的修改。
    fn is_dirty(&self) -> bool {
        self.editor_text() != self.saved_content
    }

    /// 获取窗口标题。
    fn window_title(&self) -> String {
        let file_name = self
            .current_file
            .as_ref()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.file_name_input.clone());
        let dirty = if self.is_dirty() { " *" } else { "" };
        format!("Akizuki*Rustgal 剧本编辑器 - {file_name}{dirty}  (Build {BUILD_NUMBER})")
    }
}

// ===========================================================================
// Application 方法（供 iced::application builder 使用）
// ===========================================================================

impl EditorApp {
    /// 窗口标题（动态：含文件名与 build 号）。
    pub fn title(&self) -> String {
        self.window_title()
    }

    /// 更新逻辑：处理所有消息。
    pub fn update(&mut self, message: Message) -> Task<Message> {
        // 注：此处方法签名匹配 iced builder 的 Update trait。
        match message {
            Message::Edit(action) => {
                self.editor_content.perform(action);
                self.edit_dirty = true;
                self.last_edit_time = Some(Instant::now());
                // 延迟编译诊断（空闲后）。
                Task::none()
            }
            Message::FileNameInputChanged(s) => {
                self.file_name_input = s;
                Task::none()
            }
            Message::FileListItemClicked(name) => {
                let path = self.work_dir.join(&name);
                self.open_file_path(path);
                Task::none()
            }

            // ---- 文件操作 ----
            Message::NewFile => {
                self.commit_undo();
                self.new_file();
                Task::none()
            }
            Message::OpenFile => {
                // 使用 rfd 异步文件对话框。
                let task = Task::perform(
                    async move {
                        rfd::AsyncFileDialog::new()
                            .add_filter("AKRS 脚本", &["akrs"])
                            .pick_file()
                            .await
                            .map(|h| h.path().to_path_buf())
                    },
                    Message::FilePicked,
                );
                task
            }
            Message::SaveFile => {
                if let Some(path) = self.current_file.clone() {
                    self.commit_undo();
                    self.save_to_path(path);
                    Task::none()
                } else {
                    // 没有路径，走另存为。
                    let file_name = self.file_name_input.clone();
                    Task::perform(
                        async move {
                            rfd::AsyncFileDialog::new()
                                .set_file_name(file_name)
                                .add_filter("AKRS 脚本", &["akrs"])
                                .save_file()
                                .await
                                .map(|h| h.path().to_path_buf())
                        },
                        Message::FileSaved,
                    )
                }
            }
            Message::SaveAsFile => {
                let file_name = self.file_name_input.clone();
                Task::perform(
                    async move {
                        rfd::AsyncFileDialog::new()
                            .set_file_name(file_name)
                            .add_filter("AKRS 脚本", &["akrs"])
                            .save_file()
                            .await
                            .map(|h| h.path().to_path_buf())
                    },
                    Message::FileSaved,
                )
            }
            Message::FilePicked(path) => {
                if let Some(p) = path {
                    self.open_file_path(p);
                }
                Task::none()
            }
            Message::FileSaved(path) => {
                if let Some(p) = path {
                    self.commit_undo();
                    self.save_to_path(p);
                }
                Task::none()
            }

            // ---- 项目操作 ----
            Message::OpenProject => {
                Task::perform(
                    async {
                        rfd::AsyncFileDialog::new()
                            .pick_folder()
                            .await
                            .map(|h| h.path().to_path_buf())
                    },
                    Message::ProjectPicked,
                )
            }
            Message::ProjectPicked(path) => {
                if let Some(p) = path {
                    self.project_dir = Some(p.clone());
                    self.work_dir = p.clone();
                    // 尝试加载 project.json。
                    self.project_config = ProjectConfig::load(&p);
                    self.project_loaded = true;
                    // 添加到最近项目。
                    let name = self.project_config.title.clone();
                    self.recent_projects.add_project(&p, &name);
                    self.refresh_file_list();
                    self.refresh_preview_resources();
                    self.status = format!("已打开项目: {}", p.display());
                }
                Task::none()
            }
            Message::RecentProjectClicked(path) => {
                if path.exists() {
                    self.project_dir = Some(path.clone());
                    self.work_dir = path.clone();
                    self.refresh_file_list();
                    self.refresh_preview_resources();
                    self.status = format!("已切换到: {}", path.display());
                }
                Task::none()
            }

            // ---- 运行 / 预览 ----
            Message::RunScript => {
                self.run_script();
                Task::none()
            }
            Message::StopScript => {
                self.stop_script();
                Task::none()
            }
            Message::GameProcessExited => {
                self.game_process = None;
                self.status = "游戏预览已退出".to_string();
                Task::none()
            }
            Message::EngineTick => {
                // 引擎动画帧（目前无引擎实例，预留）。
                Task::none()
            }

            // ---- 构建打包 ----
            Message::ToggleBuildDialog => {
                self.build.show = !self.build.show;
                Task::none()
            }
            Message::ToggleBuildPlatform(p) => {
                let entry = self.build.selected.entry(p).or_insert(false);
                *entry = !*entry;
                Task::none()
            }
            Message::StartBuild => {
                self.start_build();
                Task::none()
            }
            Message::BuildTick => {
                self.poll_build();
                Task::none()
            }
            Message::CloseBuildDialog => {
                self.build.show = false;
                Task::none()
            }

            // ---- 模式切换 ----
            Message::ToggleBlueprint => {
                if self.blueprint_mode {
                    self.leave_blueprint_mode();
                } else {
                    self.enter_blueprint_mode();
                }
                Task::none()
            }
            Message::ToggleTranslation => {
                self.translation_mode = !self.translation_mode;
                Task::none()
            }
            Message::ToggleFindReplace => {
                self.show_find_replace = !self.show_find_replace;
                Task::none()
            }
            Message::SyncBlueprintToText => {
                self.leave_blueprint_mode();
                Task::none()
            }
            Message::SyncTextToBlueprint => {
                self.enter_blueprint_mode();
                Task::none()
            }

            // ---- 蓝图交互 ----
            Message::Blueprint(msg) => {
                self.handle_blueprint_msg(msg);
                Task::none()
            }
            Message::AddBlueprintNode(template_idx) => {
                if let Some((_, _, text)) = NODE_TEMPLATES.get(template_idx) {
                    let pos = self.blueprint.next_placement_pos();
                    let kind = BlueprintState::detect_kind(text);
                    let id = self.blueprint.add_node(kind, pos, text.to_string());
                    self.blueprint.selected = Some(id);
                    self.status = format!("已添加节点: {text}");
                }
                Task::none()
            }

            // ---- 预览面板 ----
            Message::TabSelected(tab) => {
                self.preview_tab = tab;
                Task::none()
            }
            Message::SpriteSelected(s) => {
                self.sprite_preview.selected = Some(s);
                Task::none()
            }
            Message::BackgroundSelected(s) => {
                self.bg_preview.selected = Some(s);
                Task::none()
            }
            Message::MusicSelected(s) => {
                self.music_preview.selected = Some(s);
                Task::none()
            }

            // ---- 对话框 ----
            Message::ShowAbout => {
                self.show_about = true;
                Task::none()
            }
            Message::ShowShortcuts => {
                self.show_shortcuts = true;
                Task::none()
            }
            Message::ShowWelcome => {
                self.show_welcome = true;
                Task::none()
            }
            Message::CloseDialog => {
                self.show_about = false;
                self.show_shortcuts = false;
                self.show_find_replace = false;
                self.show_rpy_import = false;
                self.build.show = false;
                Task::none()
            }
            Message::ExitRequested => {
                if self.is_dirty() && !self.force_close {
                    self.pending_exit = true;
                }
                Task::none()
            }
            Message::ConfirmExit => {
                self.force_close = true;
                Task::none()
            }

            // ---- 快捷插入 ----
            Message::InsertSnippet(snippet) => {
                let mut content = self.editor_text();
                content.push_str(&snippet);
                self.set_editor_text(&content);
                self.status = format!("已插入: {snippet}");
                Task::none()
            }
            Message::InsertTemplate(idx) => {
                if let Some((_, _, text)) = NODE_TEMPLATES.get(idx) {
                    let mut content = self.editor_text();
                    content.push_str(text);
                    content.push('\n');
                    self.set_editor_text(&content);
                    self.status = format!("已插入模板");
                }
                Task::none()
            }

            // ---- 撤销 / 重做 ----
            Message::Undo => {
                self.undo();
                Task::none()
            }
            Message::Redo => {
                self.redo();
                Task::none()
            }

            // ---- 查找替换 ----
            Message::FindReplaceTargetChanged(s) => {
                self.find_replace_target = s;
                Task::none()
            }
            Message::FindNext => self.find_next(),
            Message::ReplaceAll => {
                // 简化实现：全文替换。
                let target = self.find_replace_target.clone();
                if !target.is_empty() {
                    let content = self.editor_text().replace(&target, "");
                    self.set_editor_text(&content);
                    self.status = "已全部替换".to_string();
                }
                Task::none()
            }

            // ---- rpy 导入 ----
            Message::ToggleRpyImport => {
                self.show_rpy_import = !self.show_rpy_import;
                Task::none()
            }
            Message::RpyFilePicked(path) => {
                self.rpy_import_source = path;
                Task::none()
            }
            Message::ConvertRpy => {
                if let Some(src) = self.rpy_import_source.clone() {
                    // 写入临时目标文件，转换后读回编辑器。
                    let target = std::env::temp_dir().join("akrs_rpy_imported.akrs");
                    match rpy::convert_rpy_to_akrs(&src, &target) {
                        Ok(warnings) => {
                            self.rpy_import_warnings = warnings;
                            if let Ok(content) = std::fs::read_to_string(&target) {
                                self.set_editor_text(&content);
                                self.status = "rpy 转换完成".to_string();
                            }
                        }
                        Err(e) => {
                            self.status = format!("转换失败: {e}");
                        }
                    }
                }
                Task::none()
            }

            // ---- 状态栏 ----
            Message::SetStatus(s) => {
                self.status = s;
                Task::none()
            }

            // ---- 窗口 ----
            Message::WindowClosed => {
                self.stop_script();
                Task::none()
            }

            Message::None => Task::none(),
        }
    }

    pub fn view(&self) -> Element<Message> {
        // 主三栏布局：顶部工具栏 + (左栏 | 中栏 | 右栏) + 底部状态栏。
        let toolbar = self.view_toolbar();
        let left = self.view_left_panel();
        let center = self.view_center();
        let right = self.view_right_panel();
        let status_bar = self.view_status_bar();

        let main_row = row![left, center, right]
            .spacing(4)
            .width(Length::Fill)
            .height(Length::Fill);

        let content: Element<Message> = column![toolbar, main_row, status_bar]
            .spacing(4)
            .width(Length::Fill)
            .height(Length::Fill)
            .into();

        // 弹窗覆盖层：根据状态选择对应的弹窗叠加在 content 之上。
        if self.show_welcome {
            self.view_welcome_overlay(content)
        } else if self.show_about {
            self.view_about_overlay(content)
        } else if self.show_shortcuts {
            self.view_shortcuts_overlay(content)
        } else if self.show_find_replace {
            self.view_find_replace_overlay(content)
        } else if self.show_rpy_import {
            self.view_rpy_import_overlay(content)
        } else if self.build.show {
            self.view_build_overlay(content)
        } else if self.pending_exit {
            self.view_exit_overlay(content)
        } else {
            content
        }
    }

    pub fn subscription(&self) -> Subscription<Message> {
        // 构建中：每 100ms 轮询子进程。
        let build_sub = if self.build.is_building() {
            Some(iced::time::every(Duration::from_millis(100)).map(|_| Message::BuildTick))
        } else {
            None
        };
        // 游戏运行中：每 500ms 检查是否退出。
        let game_sub = if self.game_process.is_some() {
            Some(iced::time::every(Duration::from_millis(500)).map(|_| Message::GameProcessExited))
        } else {
            None
        };

        let subs: Vec<_> = [build_sub, game_sub].into_iter().flatten().collect();
        if subs.is_empty() {
            Subscription::none()
        } else {
            Subscription::batch(subs)
        }
    }
}

// ===========================================================================
// EditorApp 视图辅助方法
// ===========================================================================

impl EditorApp {
    /// 顶部工具栏：文件操作 + 运行 + 模式切换 + 帮助。
    fn view_toolbar(&self) -> Element<Message> {
        let btn = |label, msg| {
            button(text(label).size(13))
                .on_press(msg)
                .padding(Padding::new(4.0).bottom(2.0).top(2.0).left(8.0).right(8.0))
        };

        let blueprint_label = if self.blueprint_mode { "文本" } else { "蓝图" };

        let toolbar = row![
            btn("新建", Message::NewFile),
            btn("打开", Message::OpenFile),
            btn("保存", Message::SaveFile),
            btn("另存为", Message::SaveAsFile),
            text("").width(8),
            btn("运行", Message::RunScript),
            btn("停止", Message::StopScript),
            btn("打包", Message::ToggleBuildDialog),
            text("").width(8),
            btn(blueprint_label, Message::ToggleBlueprint),
            btn("翻译", Message::ToggleTranslation),
            btn("查找替换", Message::ToggleFindReplace),
            btn("rpy导入", Message::ToggleRpyImport),
            text("").width(8),
            btn("关于", Message::ShowAbout),
            btn("快捷键", Message::ShowShortcuts),
            horizontal_space(),
            text(format!("Build {BUILD_NUMBER}")).size(11),
        ]
        .spacing(4)
        .padding(Padding::new(4.0).left(4.0).right(4.0).top(2.0).bottom(2.0));

        container(toolbar)
            .style(container::rounded_box)
            .into()
    }

    /// 左栏：文件名输入 + 文件列表。
    fn view_left_panel(&self) -> Element<Message> {
        let file_input = text_input("文件名", &self.file_name_input)
            .on_input(Message::FileNameInputChanged)
            .size(13);

        let mut file_list_col = column![text("文件列表").size(12).font(BOLD)].spacing(2);
        for name in &self.file_list {
            let is_current = self
                .current_file
                .as_ref()
                .and_then(|p| p.file_name())
                .map(|n| n == name.as_str())
                .unwrap_or(false);
            let label = if is_current {
                format!("▶ {name}")
            } else {
                name.clone()
            };
            let item = button(text(label).size(12))
                .on_press(Message::FileListItemClicked(name.clone()))
                .padding(2);
            file_list_col = file_list_col.push(item);
        }

        let recent_label = if !self.recent_projects.projects.is_empty() {
            Some(text("最近项目").size(12).font(BOLD))
        } else {
            None
        };
        let mut recent_col = column![].spacing(2);
        if let Some(label) = recent_label {
            recent_col = recent_col.push(label);
        }
        for rp in self.recent_projects.projects.iter().take(5) {
            let path = rp.path.clone();
            let item = button(text(&rp.name).size(11))
                .on_press(Message::RecentProjectClicked(path))
                .padding(2);
            recent_col = recent_col.push(item);
        }

        let content = column![file_input, file_list_col, recent_col]
            .spacing(6)
            .padding(8)
            .width(Length::Fill)
            .height(Length::Fill);

        container(scrollable(content))
            .width(220)
            .height(Length::Fill)
            .style(container::rounded_box)
            .into()
    }

    /// 中栏：文本编辑器或蓝图画布。
    fn view_center(&self) -> Element<Message> {
        if self.blueprint_mode {
            self.view_blueprint_canvas()
        } else {
            self.view_text_editor()
        }
    }

    /// 文本编辑器（带语法高亮）。
    fn view_text_editor(&self) -> Element<Message> {
        let editor = text_editor(&self.editor_content)
            .on_action(Message::Edit)
            .highlight_with::<highlight::AkrsHighlighter>(
                highlight::AkrsHighlightSettings,
                highlight::akrs_highlight_to_format,
            )
            .font(Font::with_name("Source Han Sans SC"))
            .size(Pixels(FONT_SIZE))
            .padding(8);

        container(editor)
            .width(Length::Fill)
            .height(Length::Fill)
            .style(container::rounded_box)
            .into()
    }

    /// 蓝图画布。
    fn view_blueprint_canvas(&self) -> Element<Message> {
        let program = BlueprintProgram::new(self.blueprint.clone());
        let canvas_widget = canvas::Canvas::new(program)
            .width(Length::Fill)
            .height(Length::Fill);
        let canvas_el: Element<BlueprintMsg> = canvas_widget.into();

        // 蓝图右栏：节点模板列表。
        let mut templates_col = column![text("节点模板").size(12).font(BOLD)].spacing(2);
        for (i, (label, desc, _)) in NODE_TEMPLATES.iter().enumerate() {
            let item = button(
                column![text(*label).size(12), text(*desc).size(10)]
                    .spacing(1),
            )
            .on_press(Message::AddBlueprintNode(i))
            .padding(3);
            templates_col = templates_col.push(item);
        }
        let templates_panel = container(scrollable(templates_col))
            .width(180)
            .height(Length::Fill)
            .style(container::rounded_box);

        let layout = row![
            container(canvas_el.map(Message::Blueprint))
                .width(Length::Fill)
                .height(Length::Fill)
                .style(container::rounded_box),
            templates_panel,
        ]
        .spacing(4)
        .width(Length::Fill)
        .height(Length::Fill);

        layout.into()
    }

    /// 右栏：预览面板（诊断 / 立绘 / 背景 / 音乐）。
    fn view_right_panel(&self) -> Element<Message> {
        // 标签页按钮。
        let mut tabs = row![].spacing(2);
        for tab in PreviewTab::ALL {
            let active = self.preview_tab == tab;
            let style = if active {
                container::rounded_box
            } else {
                container::rounded_box
            };
            let _ = style;
            let btn = button(text(tab.label()).size(12))
                .on_press(Message::TabSelected(tab))
                .padding(4);
            tabs = tabs.push(btn);
        }

        // 标签页内容。
        let content: Element<Message> = match self.preview_tab {
            PreviewTab::Diagnostics => {
                let mut col = column![].spacing(2);
                if self.diagnostics.is_empty() {
                    col = col.push(text("无诊断信息").size(12));
                } else {
                    for diag in &self.diagnostics {
                        col = col.push(text(diag).size(11));
                    }
                }
                scrollable(col).into()
            }
            PreviewTab::Sprite => {
                let mut col = column![text("立绘预览").size(12).font(BOLD)].spacing(2);
                for name in &self.sprite_preview.items {
                    let item = button(text(name).size(11))
                        .on_press(Message::SpriteSelected(name.clone()))
                        .padding(2);
                    col = col.push(item);
                }
                scrollable(col).into()
            }
            PreviewTab::Background => {
                let mut col = column![text("背景预览").size(12).font(BOLD)].spacing(2);
                for name in &self.bg_preview.items {
                    let item = button(text(name).size(11))
                        .on_press(Message::BackgroundSelected(name.clone()))
                        .padding(2);
                    col = col.push(item);
                }
                scrollable(col).into()
            }
            PreviewTab::Music => {
                let mut col = column![text("音乐预览").size(12).font(BOLD)].spacing(2);
                for name in &self.music_preview.items {
                    let item = button(text(name).size(11))
                        .on_press(Message::MusicSelected(name.clone()))
                        .padding(2);
                    col = col.push(item);
                }
                scrollable(col).into()
            }
        };

        let panel = column![tabs, content]
            .spacing(4)
            .padding(8)
            .width(Length::Fill)
            .height(Length::Fill);

        container(panel)
            .width(260)
            .height(Length::Fill)
            .style(container::rounded_box)
            .into()
    }

    /// 底部状态栏。
    fn view_status_bar(&self) -> Element<Message> {
        let dirty = if self.is_dirty() { " [未保存]" } else { "" };
        let status = text(format!("{}{dirty}", self.status)).size(12);
        let file_info = text(
            self.current_file
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "未保存".to_string()),
        )
        .size(11);

        let bar = row![status, horizontal_space(), file_info]
            .spacing(4)
            .padding(Padding::new(4.0).left(8.0).right(8.0).top(2.0).bottom(2.0));

        container(bar)
            .width(Length::Fill)
            .style(container::rounded_box)
            .into()
    }

    // ---- 弹窗覆盖层 ----

    fn view_welcome_overlay<'a>(&'a self, content: Element<'a, Message>) -> Element<'a, Message> {
        let welcome = column![
            text("欢迎使用 Akizuki*Rustgal 剧本编辑器").size(20).font(BOLD),
            vertical_space().height(12),
            text(format!("Build {BUILD_NUMBER}")).size(13),
            vertical_space().height(8),
            text("快捷键: Ctrl+N 新建 | Ctrl+O 打开 | Ctrl+S 保存 | Ctrl+R 运行").size(12),
            text("Ctrl+B 蓝图模式 | Ctrl+T 翻译模式 | Ctrl+H 快捷键帮助").size(12),
            vertical_space().height(12),
            row![
                button(text("新建空白文件").size(13)).on_press(Message::NewFile).padding(8),
                button(text("加载示例剧本").size(13))
                    .on_press(Message::InsertSnippet(SAMPLE_SCRIPT.to_string()))
                    .padding(8),
                button(text("打开已有文件").size(13)).on_press(Message::OpenFile).padding(8),
            ]
            .spacing(8),
        ]
        .spacing(4)
        .padding(20)
        .max_width(500);

        modal_overlay(content, welcome)
    }

    fn view_about_overlay<'a>(&'a self, content: Element<'a, Message>) -> Element<'a, Message> {
        let about = column![
            text("关于 Akizuki*Rustgal 剧本编辑器").size(18).font(BOLD),
            vertical_space().height(8),
            text(format!("Build {BUILD_NUMBER}")).size(13),
            text("基于 iced 0.13 重构的可视化小说剧本编辑器").size(12),
            vertical_space().height(4),
            text("功能: 文本编辑 / 蓝图节点 / 语法高亮 / 编译诊断").size(12),
            text("      立绘背景音乐预览 / 构建打包 / 翻译辅助").size(12),
            vertical_space().height(8),
            text(format!("GitHub: {GITHUB_URL}")).size(11),
            vertical_space().height(12),
            button(text("关闭").size(13)).on_press(Message::CloseDialog).padding(8),
        ]
        .spacing(4)
        .padding(20)
        .max_width(460);

        modal_overlay(content, about)
    }

    fn view_shortcuts_overlay<'a>(&'a self, content: Element<'a, Message>) -> Element<'a, Message> {
        let shortcuts = column![
            text("键盘快捷键").size(18).font(BOLD),
            vertical_space().height(8),
            text("Ctrl+N  新建文件").size(12),
            text("Ctrl+O  打开文件").size(12),
            text("Ctrl+S  保存文件").size(12),
            text("Ctrl+R  运行剧本").size(12),
            text("Ctrl+B  切换蓝图模式").size(12),
            text("Ctrl+T  切换翻译模式").size(12),
            text("Ctrl+H  显示此帮助").size(12),
            text("Ctrl+Z  撤销").size(12),
            text("Ctrl+Y / Ctrl+Shift+Z  重做").size(12),
            text("Ctrl+1  插入 + 角色（立绘上场）").size(12),
            text("Ctrl+2  插入 - 角色（立绘下场）").size(12),
            text("Ctrl+3  插入 # 章节（章节标题）").size(12),
            text("Ctrl+4  插入 @bg 背景（背景指令）").size(12),
            vertical_space().height(12),
            button(text("关闭").size(13)).on_press(Message::CloseDialog).padding(8),
        ]
        .spacing(2)
        .padding(20)
        .max_width(420);

        modal_overlay(content, shortcuts)
    }

    fn view_find_replace_overlay<'a>(&'a self, content: Element<'a, Message>) -> Element<'a, Message> {
        let panel = column![
            text("查找替换").size(16).font(BOLD),
            vertical_space().height(8),
            text_input("查找内容", &self.find_replace_target)
                .on_input(Message::FindReplaceTargetChanged)
                .size(13),
            row![
                button(text("查找下一个").size(12)).on_press(Message::FindNext).padding(4),
                button(text("全部替换为空").size(12)).on_press(Message::ReplaceAll).padding(4),
                button(text("关闭").size(12)).on_press(Message::CloseDialog).padding(4),
            ]
            .spacing(4),
        ]
        .spacing(4)
        .padding(16)
        .max_width(400);

        modal_overlay(content, panel)
    }

    fn view_rpy_import_overlay<'a>(&'a self, content: Element<'a, Message>) -> Element<'a, Message> {
        let source_text = self
            .rpy_import_source
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "未选择".to_string());

        let panel = column![
            text("Ren'Py 脚本导入").size(16).font(BOLD),
            vertical_space().height(8),
            text(format!("源文件: {source_text}")).size(12),
            row![
                button(text("选择 .rpy 文件").size(12))
                    .on_press(Message::None)
                    .padding(4),
                button(text("转换").size(12)).on_press(Message::ConvertRpy).padding(4),
                button(text("关闭").size(12)).on_press(Message::CloseDialog).padding(4),
            ]
            .spacing(4),
            vertical_space().height(4),
            text("转换警告:").size(12),
        ]
        .spacing(2)
        .padding(16)
        .max_width(500);

        let mut panel = panel;
        if !self.rpy_import_warnings.is_empty() {
            for w in &self.rpy_import_warnings {
                panel = panel.push(text(format!("  ⚠ {w}")).size(11));
            }
        }

        modal_overlay(content, panel)
    }

    fn view_build_overlay<'a>(&'a self, content: Element<'a, Message>) -> Element<'a, Message> {
        let mut platform_list = column![].spacing(4);
        for platform in BuildPlatform::ALL {
            let checked = *self.build.selected.get(&platform).unwrap_or(&false);
            // iced 0.13 中 checkbox 的 `on_toggle` 接受 `Fn(bool) -> Message`，不是 `Message`。
            let cb = checkbox(platform.label(), checked)
                .on_toggle(move |_checked| Message::ToggleBuildPlatform(platform));
            platform_list = platform_list.push(cb);
        }

        let build_btn = if self.build.is_building() {
            button(text("构建中...").size(12)).padding(8)
        } else {
            button(text("开始构建").size(12)).on_press(Message::StartBuild).padding(8)
        };

        let log_scroll = scrollable(text(&self.build.log).size(11).font(Font::MONOSPACE))
            .height(200);

        let status_text = if self.build.done {
            format!(
                "完成: 成功 {} 个, 失败 {} 个",
                self.build.succeeded.len(),
                self.build.failed.len()
            )
        } else if self.build.is_building() {
            "构建中...".to_string()
        } else {
            "就绪".to_string()
        };

        let panel = column![
            text("构建打包").size(16).font(BOLD),
            vertical_space().height(8),
            platform_list,
            vertical_space().height(8),
            row![build_btn, button(text("关闭").size(12)).on_press(Message::CloseBuildDialog).padding(8)]
                .spacing(4),
            vertical_space().height(8),
            text(format!("状态: {status_text}")).size(12),
            vertical_space().height(4),
            text("构建日志:").size(12),
            log_scroll,
        ]
        .spacing(4)
        .padding(16)
        .max_width(600);

        modal_overlay(content, panel)
    }

    fn view_exit_overlay<'a>(&'a self, content: Element<'a, Message>) -> Element<'a, Message> {
        let panel = column![
            text("有未保存的修改").size(16).font(BOLD),
            vertical_space().height(8),
            text("当前文件有未保存的修改，确定要退出吗？").size(13),
            vertical_space().height(12),
            row![
                button(text("保存并退出").size(13)).on_press(Message::SaveFile).padding(8),
                button(text("放弃修改退出").size(13)).on_press(Message::ConfirmExit).padding(8),
                button(text("取消").size(13)).on_press(Message::CloseDialog).padding(8),
            ]
            .spacing(8),
        ]
        .spacing(4)
        .padding(20)
        .max_width(400);

        modal_overlay(content, panel)
    }

    // ---- 蓝图消息处理 ----

    fn handle_blueprint_msg(&mut self, msg: BlueprintMsg) {
        match msg {
            BlueprintMsg::DragStarted { id, offset } => {
                self.blueprint.selected = Some(id);
                self.blueprint.drag_node = Some(id);
                self.blueprint.drag_offset = offset;
            }
            BlueprintMsg::DragMoved { pos } => {
                if let Some(id) = self.blueprint.drag_node {
                    if let Some(node) = self.blueprint.nodes.iter_mut().find(|n| n.id == id) {
                        node.pos = Point::new(pos.x - self.blueprint.drag_offset.x, pos.y - self.blueprint.drag_offset.y);
                    }
                }
            }
            BlueprintMsg::DragEnded => {
                self.blueprint.drag_node = None;
            }
            BlueprintMsg::ConnectStarted { from, pos } => {
                self.blueprint.connecting_from = Some(from);
                self.blueprint.connecting_pos = pos;
            }
            BlueprintMsg::ConnectMoved { pos } => {
                self.blueprint.connecting_pos = pos;
            }
            BlueprintMsg::ConnectEnded { target } => {
                if let (Some(from), Some(to)) = (self.blueprint.connecting_from, target) {
                    self.blueprint.add_link(from, to);
                }
                self.blueprint.connecting_from = None;
            }
            BlueprintMsg::PanStarted { origin: _ } => {
                // Pan 由 delta 累积。
            }
            BlueprintMsg::PanMoved { delta } => {
                self.blueprint.pan.x += delta.x;
                self.blueprint.pan.y += delta.y;
            }
            BlueprintMsg::PanEnded => {}
            BlueprintMsg::Deselected => {
                self.blueprint.selected = None;
            }
            BlueprintMsg::RightClicked { pos } => {
                self.blueprint.context_menu_pos = Some(pos);
            }
            BlueprintMsg::DoubleClicked { id } => {
                self.blueprint.editing_node = Some(id);
            }
            BlueprintMsg::NodeSelected { id } => {
                self.blueprint.selected = Some(id);
            }
        }
    }
}

// ===========================================================================
// 字体加载
// ===========================================================================

/// 加载 CJK 字体字节（优先运行时 OTF，其次系统字体）。
///
/// 返回 `Some(Vec<u8>)` 表示成功加载到字体文件字节；
/// `None` 表示未找到任何中文字体（此时将回退到 iced 默认字体，
/// 中文可能显示为方框）。
fn load_cjk_font() -> Option<Vec<u8>> {
    // 1. 运行时外部字体文件（最高优先级）。
    let runtime_font_path = "assets/fonts/SourceHanSansSC-Regular-2.otf";
    if let Ok(bytes) = std::fs::read(runtime_font_path) {
        eprintln!("[editor] 中文字体已加载（运行时 OTF）");
        return Some(bytes);
    }

    // 2. 系统字体回退（按平台分别列出候选路径）。
    let mut sys_candidates: Vec<String> = Vec::new();
    #[cfg(target_os = "windows")]
    {
        let win_dir = std::env::var("WINDIR").unwrap_or_else(|_| "C:\\Windows".to_string());
        let fonts_dir = std::path::Path::new(&win_dir).join("Fonts");
        for name in &["msyh.ttc", "msyh.ttf", "msyhbd.ttc", "simsun.ttc", "simhei.ttf"] {
            sys_candidates.push(fonts_dir.join(name).to_string_lossy().into_owned());
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
            "/usr/share/fonts/truetype/wqy/wqy-zenhei.ttc",
        ] {
            sys_candidates.push((*path).to_string());
        }
    }

    for path in &sys_candidates {
        if let Ok(bytes) = std::fs::read(path) {
            eprintln!("[editor] 使用系统中文字体: {path}");
            return Some(bytes);
        }
    }

    eprintln!("[editor] 警告：未找到中文字体，中文可能显示为方框");
    None
}

/// 从嵌入的 RGBA 数据加载窗口图标。
fn load_icon() -> Option<iced::window::icon::Icon> {
    let data = include_bytes!("../../../assets/icon_kokona_64.bin");
    const W: u32 = 64;
    const H: u32 = 64;
    if data.len() == (W * H * 4) as usize {
        iced::window::icon::from_rgba(data.to_vec(), W, H).ok()
    } else {
        None
    }
}

// ===========================================================================
// 入口函数
// ===========================================================================

/// 启动编辑器。
///
/// 加载 CJK 字体、配置窗口、运行 iced 应用。
pub fn run_editor() -> Result<(), iced::Error> {
    let font_bytes = load_cjk_font();

    // iced 0.13 的 `iced::application(title, update, view)` builder：
    // - `title` 实现了 `Title<State>` trait（`&'static str` 或 `Fn(&State) -> String`）。
    // - `update` 是 `Fn(&mut State, Message) -> Task<Message>`。
    // - `view` 是 `Fn(&State) -> Element<Message>`。
    // 注意：`.title(...)` builder 方法是 `pub(crate)`，外部不能调用，标题必须在此处设置。
    // 注意：`subscription` / `theme` 等方法返回新类型（`impl Program`），必须链式调用。
    let app = iced::application(
        |state: &EditorApp| state.title(),
        EditorApp::update,
        EditorApp::view,
    );

    // 注册字体（若加载成功）。`font` / `default_font` 返回 `Self`，可分步赋值。
    let app = if let Some(bytes) = font_bytes {
        app.font(bytes)
            .default_font(Font::with_name("Source Han Sans SC"))
    } else {
        app
    };

    // 窗口配置 + 订阅 + 主题：`window_size` / `resizable` 返回 `Self`，
    // `subscription` / `theme` 返回新类型，必须链式调用到最后。
    app.window_size(Size::new(1280.0, 820.0))
        .resizable(true)
        .subscription(|state: &EditorApp| state.subscription())
        .theme(|_state: &EditorApp| Theme::Dark)
        .run()
}
