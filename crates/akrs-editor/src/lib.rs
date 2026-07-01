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

use eframe::egui;
use egui::{ColorImage, TextureHandle};

use akrs_core::{compile, format_location, CompileError, ErrSeverity, Position};
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
    /// 文件选择对话框状态（None 表示未打开）。
    file_picker: Option<FilePickerState>,
}

/// 文件选择对话框模式。
#[derive(Debug, Clone, Copy, PartialEq)]
#[allow(dead_code)]
enum FilePickerMode {
    Open,
}

/// 文件选择对话框状态。
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

impl Default for EditorApp {
    fn default() -> Self {
        let work_dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
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
            file_picker: None,
        };
        app.refresh_file_list();
        app
    }
}

impl EditorApp {
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

            ui.add_space(36.0);

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
        });
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

        // ---- 顶部工具栏 --------------------------------------------------
        egui::TopBottomPanel::top("toolbar").show(ctx, |ui| {
            ui.horizontal_wrapped(|ui| {
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
                if ui.button("首页").clicked() {
                    self.show_welcome = true;
                    self.engine = None;
                    self.status = "就绪".to_string();
                }
                if ui.button("关于").clicked() {
                    self.show_about = true;
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
        egui::SidePanel::right("preview")
            .resizable(true)
            .default_width(330.0)
            .show(ctx, |ui| {
                ui.heading("预览");
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut self.preview_tab, PreviewTab::Script, "剧本");
                    ui.selectable_value(&mut self.preview_tab, PreviewTab::Sprite, "立绘");
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
                }
            });

        // ---- 中央面板：编辑器或欢迎页 -----------------------------------
        egui::CentralPanel::default().show(ctx, |ui| {
            if self.show_welcome {
                self.show_welcome_panel(ui);
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

    // 2. 系统字体回退
    if font_data.is_none() {
        let sys_candidates: &[&str] = &[
            "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
            "/usr/share/fonts/truetype/noto/NotoSansCJK-Regular.ttc",
            "/usr/share/fonts/truetype/wqy/wqy-microhei.ttc",
            "/System/Library/Fonts/PingFang.ttc",
            "/System/Library/Fonts/STHeiti Light.ttc",
        ];
        for path in sys_candidates {
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
