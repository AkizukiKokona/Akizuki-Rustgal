//! Akizuki*Rustgal 剧本编辑器
//!
//! 基于 `egui 0.29` + `eframe 0.29`（wgpu 22 后端）构建的视觉小说剧本编辑器。提供三栏布局：
//!
//! - **左栏**：工作目录中的 `.akrs` 文件列表，支持新建 / 打开 / 保存。
//! - **中栏**：多行脚本编辑器，带基础语法高亮。
//! - **右栏**：由 `akrs_runtime::Engine` 驱动的实时预览。
//! - **顶部工具栏**：新建 / 打开 / 保存 / 运行等操作。
//! - **底部状态栏**：编译诊断信息（错误 / 警告 / 提示）。
//!
//! 所有文件操作使用 `Result` 风格的错误处理，不会 panic；失败信息显示在底部状态栏。

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::io::Read;

use eframe::egui;
use egui::{ColorImage, TextureHandle};

use akrs_core::{compile, format_location, CompileError, dirs_data_dir, DismissedWarnings, ErrSeverity, Position, ProjectConfig, RecentProjects};
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
/// `//` 注释 -> (0.4, 0.4, 0.4)
const COLOR_COMMENT: egui::Color32 = egui::Color32::from_rgb(102, 102, 102);
/// `"..."` 字符串 -> (0.9, 0.9, 0.4)
const COLOR_STRING: egui::Color32 = egui::Color32::from_rgb(229, 229, 102);
/// 默认文字 -> 白色
const COLOR_DEFAULT: egui::Color32 = egui::Color32::from_rgb(255, 255, 255);
/// 配对高亮背景色：光标停在 `=>`/`<=` 时，该指令及其配对指令的背景。
const COLOR_PAIR_HIGHLIGHT: egui::Color32 = egui::Color32::from_rgb(255, 220, 0);

const FONT_SIZE: f32 = 14.0;

/// 章节显示标题（`# name title` 中的 title，无标题时取 name）的最大建议字符数。
/// 超过此值时编辑器给出警告（非阻断，仍可强制运行；运行时通知会自动缩字显示）。
/// 取值依据：顶部章节通知宽度约为屏幕 80%，基准字号下可舒适显示约 24 个字符。
const MAX_CHAPTER_TITLE_CHARS: usize = 24;

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

/// 右栏编辑器工具的标签页。
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
    /// 大纲视图：按缩进展示章节、分支选项与 `=>`/`<=` 配对结构。
    Outline,
    /// 蓝图积木：可拖拽的节点模板列表（仅在蓝图模式显示）。
    Blocks,
}

/// 左栏（资源文件系统分类）的标签页。
#[derive(PartialEq, Clone, Copy)]
enum LeftPanelTab {
    /// 脚本文件（`.akrs`，保持原有逻辑）。
    Script,
    /// 立绘资源（`assets/characters/` 下的 PNG）。
    Sprite,
    /// 背景资源（`assets/bg/` 下的 PNG/JPG/JPEG）。
    Background,
    /// 音乐资源（`assets/music/` 下的 OGG/MP3/WAV）。
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
// 蓝图模式
// ---------------------------------------------------------------------------

/// 蓝图节点类型。由节点文本的首字符/首词推断，仅影响颜色和标签。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum NodeKind {
    Section,
    Dialogue,
    Narration,
    Command,
    Direction,
    Choice,
    ChoiceOption,
    Flow,
    Visit,
    Return,
    Wait,
    StoryEnd,
    Ending,
    Unlock,
    VarOp,
    Comment,
    Other,
}

/// 蓝图节点：对应剧本中的一行（或多行）指令。
struct BlueprintNode {
    id: usize,
    kind: NodeKind,
    /// 画布坐标（不含 pan 偏移）。
    pos: egui::Pos2,
    /// 节点文本内容（即 `.akrs` 脚本行）。
    text: String,
}

/// 蓝图连线：从某节点的输出引脚连到另一节点的输入引脚。
struct BlueprintLink {
    from: usize,
    to: usize,
}

/// 蓝图编辑器状态。
struct BlueprintState {
    nodes: Vec<BlueprintNode>,
    links: Vec<BlueprintLink>,
    next_id: usize,
    /// 画布平移偏移。
    pan: egui::Vec2,
    /// 当前选中的节点 ID。
    selected: Option<usize>,
    /// 正在拖动的节点 ID。
    drag_node: Option<usize>,
    /// 拖动偏移（鼠标位置 - 节点位置）。
    drag_offset: egui::Vec2,
    /// 右键按下时的位置（用于区分"点击"与"拖动"）。
    right_press_pos: Option<egui::Pos2>,
    /// 右键按下后是否发生了拖动。
    right_moved: bool,
    /// 正在从某节点输出引脚拉线。
    connecting_from: Option<usize>,
    /// 拉线时鼠标当前位置（画布坐标）。
    connecting_pos: egui::Pos2,
    /// 右键菜单弹出位置（屏幕坐标）。
    context_menu_pos: Option<egui::Pos2>,
    /// 正在编辑文本的节点 ID（双击触发，就地编辑）。
    editing_node: Option<usize>,
    /// 注释是否折叠隐藏到下一个非注释积木中（true=折叠，false=独立显示）。
    collapse_comments: bool,
    /// 画布缩放系数（1.0 = 100%）。
    zoom: f32,
    /// 是否处于触摸模式（检测到 Event::Touch 后置 true，粘性保持）。
    /// 触摸模式下：放大引脚便于点按、空白处单指拖拽平移画布、
    /// 工具栏显示触摸操作提示。
    touch_mode: bool,
    /// 触摸模式下空白处单指拖拽平移进行中（primary 按下在空白处时置 true）。
    touch_panning: bool,
    /// 框选多选产生的节点 ID 集合（与 `selected` 并存，`selected` 用于单选高亮）。
    selected_set: std::collections::HashSet<usize>,
}

impl Default for BlueprintState {
    fn default() -> Self {
        Self {
            nodes: Vec::new(),
            links: Vec::new(),
            next_id: 0,
            pan: egui::Vec2::ZERO,
            selected: None,
            drag_node: None,
            drag_offset: egui::Vec2::ZERO,
            right_press_pos: None,
            right_moved: false,
            connecting_from: None,
            connecting_pos: egui::Pos2::ZERO,
            context_menu_pos: None,
            editing_node: None,
            collapse_comments: true,
            zoom: 1.0,
            touch_mode: false,
            touch_panning: false,
            selected_set: std::collections::HashSet::new(),
        }
    }
}

/// 蓝图填空式编辑器的临时编辑状态。
///
/// 双击节点进入编辑时从节点 text 解析填充各字段；编辑过程中实时更新；
/// 每次字段变化时根据节点类型重新生成语法文本写回 `node.text`，并刷新 `node.kind`。
struct BlueprintEditState {
    /// 通用文本（旁白/其他类型兜底；同时保留原始文本用于判断立绘 +/- 与 @ 子命令）。
    text: String,
    /// 对话角色名。
    speaker: String,
    /// 对话/旁白内容。
    content: String,
    /// 选中的立绘名。
    sprite_name: String,
    /// 立绘水平位置百分比（0.0=最左，1.0=最右）。
    x_percent: f32,
    /// 立绘垂直位置百分比（0.0=最上，1.0=底部站立）。
    y_percent: f32,
    /// 立绘大小倍数。
    scale: f32,
    /// 选中的背景名。
    bg_name: String,
    /// 背景过渡效果。
    transition: String,
    /// 选中的音乐名。
    music_name: String,
    /// 选中的章节名（跳转/访问）。
    section_name: String,
    /// 当前编辑的节点类型。
    kind: NodeKind,
}

impl Default for BlueprintEditState {
    fn default() -> Self {
        Self {
            text: String::new(),
            speaker: String::new(),
            content: String::new(),
            sprite_name: String::new(),
            x_percent: 0.5,
            y_percent: 1.0,
            scale: 1.0,
            bg_name: String::new(),
            transition: "fade".to_string(),
            music_name: String::new(),
            section_name: String::new(),
            kind: NodeKind::Other,
        }
    }
}

impl BlueprintEditState {
    /// 从节点文本和类型解析填充各字段。
    /// `text` 始终保留原始（去首尾空白）文本，用于判断立绘 +/- 与 @ 子命令等不变前缀。
    fn init_from(&mut self, text: &str, kind: NodeKind) {
        self.kind = kind;
        let t = text.trim();
        self.text = t.to_string();
        match kind {
            NodeKind::Direction => {
                if t.starts_with('+') {
                    // 立绘上场：+ name at x,y size s
                    let rest = t.strip_prefix('+').unwrap_or(t).trim();
                    if let Some(at_pos) = rest.find(" at ") {
                        self.sprite_name = rest[..at_pos].trim().to_string();
                        let after = &rest[at_pos + 4..];
                        let (coords, scale_str) = if let Some(sp) = after.find(" size ") {
                            (after[..sp].trim(), after[sp + 6..].trim())
                        } else {
                            (after.trim(), "")
                        };
                        let mut it = coords.split(',');
                        self.x_percent = it
                            .next()
                            .and_then(|s| s.trim().parse::<f32>().ok())
                            .unwrap_or(0.5);
                        self.y_percent = it
                            .next()
                            .and_then(|s| s.trim().parse::<f32>().ok())
                            .unwrap_or(1.0);
                        self.scale = scale_str.parse::<f32>().ok().unwrap_or(1.0);
                    } else {
                        // 旧格式或不完整：取首词作为立绘名，坐标用默认值
                        self.sprite_name =
                            rest.split_whitespace().next().unwrap_or("").to_string();
                        self.x_percent = 0.5;
                        self.y_percent = 1.0;
                        self.scale = 1.0;
                    }
                } else {
                    // 立绘下场：- name
                    self.sprite_name = t.strip_prefix('-').unwrap_or(t).trim().to_string();
                }
            }
            NodeKind::Command => {
                let rest = t.strip_prefix('@').unwrap_or(t).trim_start();
                let cmd: String = rest.chars().take_while(|c| !c.is_whitespace()).collect();
                match cmd.as_str() {
                    "bg" => {
                        let args = rest.strip_prefix("bg").unwrap_or("").trim();
                        if let Some(wp) = args.find(" with ") {
                            self.bg_name = args[..wp].trim().to_string();
                            self.transition = args[wp + 6..].trim().to_string();
                        } else {
                            self.bg_name =
                                args.split_whitespace().next().unwrap_or("").to_string();
                            self.transition = "fade".to_string();
                        }
                    }
                    "music" => {
                        self.music_name =
                            rest.strip_prefix("music").unwrap_or("").trim().to_string();
                    }
                    _ => {
                        // 其他命令（含 stop_music）由渲染层按子命令分发
                    }
                }
            }
            NodeKind::Flow => {
                self.section_name = t.strip_prefix("->").unwrap_or(t).trim().to_string();
            }
            NodeKind::Visit => {
                self.section_name = t.strip_prefix("=>").unwrap_or(t).trim().to_string();
            }
            NodeKind::Dialogue => {
                if let Some(colon) = t.find(':') {
                    self.speaker = t[..colon].trim().to_string();
                    let after = t[colon + 1..].trim();
                    self.content = after.trim_matches('"').to_string();
                } else {
                    self.speaker.clear();
                    self.content = t.to_string();
                }
            }
            NodeKind::Narration => {
                self.content = t.trim_matches('"').to_string();
            }
            _ => {
                // 其他类型（章节/选择/选项/等待/结局/变量/注释等）走通用文本兜底
            }
        }
    }

    /// 根据当前各字段和节点类型重新生成脚本文本。
    fn generate_text(&self) -> String {
        match self.kind {
            NodeKind::Direction => {
                if self.text.trim_start().starts_with('+') {
                    format!(
                        "+ {} at {:.2},{:.2} size {:.2}",
                        self.sprite_name, self.x_percent, self.y_percent, self.scale
                    )
                } else {
                    format!("- {}", self.sprite_name)
                }
            }
            NodeKind::Command => {
                let rest = self
                    .text
                    .trim_start()
                    .strip_prefix('@')
                    .unwrap_or("")
                    .trim_start();
                let cmd: String = rest.chars().take_while(|c| !c.is_whitespace()).collect();
                match cmd.as_str() {
                    "bg" => format!("@bg {} with {}", self.bg_name, self.transition),
                    "music" => format!("@music {}", self.music_name),
                    _ => self.text.clone(),
                }
            }
            NodeKind::Flow => format!("-> {}", self.section_name),
            NodeKind::Visit => format!("=> {}", self.section_name),
            NodeKind::Dialogue => format!("{}: \"{}\"", self.speaker, self.content),
            NodeKind::Narration => format!("\"{}\"", self.content),
            _ => self.text.clone(),
        }
    }
}

impl BlueprintState {
    /// 添加一个新节点，返回其 ID。
    fn add_node(&mut self, kind: NodeKind, pos: egui::Pos2, text: String) -> usize {
        let id = self.next_id;
        self.next_id += 1;
        self.nodes.push(BlueprintNode { id, kind, pos, text });
        id
    }

    /// 计算下一个新节点的合适放置位置（最后一个节点下方，或默认起点）。
    /// 用于从预览面板"添加为节点"等无需指定位置的添加操作。
    fn next_placement_pos(&self) -> egui::Pos2 {
        if let Some(last) = self.nodes.last() {
            let size = Self::node_size(&last.text);
            egui::pos2(last.pos.x, last.pos.y + size.y + 24.0)
        } else {
            egui::pos2(80.0, 80.0)
        }
    }

    /// 用新文本替换选中节点的文本（并重新推断类型）。
    /// 用于蓝图模式下"替换选中节点"操作。若无选中节点则返回 false。
    fn replace_selected_text(&mut self, text: String) -> bool {
        if let Some(id) = self.selected {
            if let Some(node) = self.nodes.iter_mut().find(|n| n.id == id) {
                node.kind = Self::detect_kind(&text);
                node.text = text;
                return true;
            }
        }
        false
    }

    /// 删除指定节点及其所有连线。
    fn remove_node(&mut self, id: usize) {
        self.nodes.retain(|n| n.id != id);
        self.links.retain(|l| l.from != id && l.to != id);
        if self.selected == Some(id) {
            self.selected = None;
        }
    }

    /// 添加一条连线（去重：同 from→to 只保留一条）。
    fn add_link(&mut self, from: usize, to: usize) {
        if from == to {
            return;
        }
        if self.links.iter().any(|l| l.from == from && l.to == to) {
            return;
        }
        // 每个输入引脚只接受一条连线：移除已有的入线。
        self.links.retain(|l| l.to != to);
        self.links.push(BlueprintLink { from, to });
    }

    /// 沿入线回溯，收集上游所有节点（深度优先）。
    /// 返回的引用借用自 `self.nodes`，调用方需注意借用生命周期。
    fn upstream_nodes(&self, start_id: usize) -> Vec<&BlueprintNode> {
        let mut result = Vec::new();
        let mut visited = std::collections::HashSet::new();
        let mut stack = vec![start_id];
        while let Some(id) = stack.pop() {
            if !visited.insert(id) {
                continue;
            }
            // 找入线：to == id 的 link 的 from 即上游节点
            for link in &self.links {
                if link.to == id {
                    if let Some(n) = self.nodes.iter().find(|n| n.id == link.from) {
                        result.push(n);
                        stack.push(n.id);
                    }
                }
            }
        }
        result
    }

    /// 从上游节点中提取立绘角色名（从 `+` 开头的立绘上场节点）。
    /// 返回 (角色名列表, 是否有直接连接的上游)。
    /// 角色名列表为 owned，调用后不持有 self 的借用。
    fn sprite_names_from_upstream(&self, start_id: usize) -> (Vec<String>, bool) {
        let upstream = self.upstream_nodes(start_id);
        let has_direct = !upstream.is_empty();
        let names: Vec<String> = upstream
            .iter()
            .filter(|n| n.kind == NodeKind::Direction && n.text.trim_start().starts_with('+'))
            .filter_map(|n| {
                // + 心夏 at 0.5,1.0 size 1.0 → 心夏
                let t = n.text.trim_start().trim_start_matches('+').trim();
                t.split_whitespace().next().map(|s| s.to_string())
            })
            .collect();
        (names, has_direct)
    }

    /// 从全画布所有立绘上场节点（`+` 开头）提取角色名。
    fn all_sprite_names(&self) -> Vec<String> {
        self.nodes
            .iter()
            .filter(|n| n.kind == NodeKind::Direction && n.text.trim_start().starts_with('+'))
            .filter_map(|n| {
                let t = n.text.trim_start().trim_start_matches('+').trim();
                t.split_whitespace().next().map(|s| s.to_string())
            })
            .collect()
    }

    /// 根据文本推断节点类型。
    fn detect_kind(text: &str) -> NodeKind {
        let t = text.trim_start();
        if t.starts_with("//") {
            NodeKind::Comment
        } else if t.starts_with('#') {
            NodeKind::Section
        } else if t.starts_with('+') {
            NodeKind::Direction
        } else if t.starts_with('-') {
            NodeKind::Direction
        } else if t.starts_with('@') {
            NodeKind::Command
        } else if t.starts_with('?') {
            NodeKind::Choice
        } else if t.starts_with('|') {
            NodeKind::ChoiceOption
        } else if t.starts_with("->") {
            NodeKind::Flow
        } else if t.starts_with("=>") {
            NodeKind::Visit
        } else if t.starts_with("<=") {
            NodeKind::Return
        } else if t.starts_with("~~") {
            NodeKind::Wait
        } else if t.starts_with("end") {
            NodeKind::StoryEnd
        } else if t.starts_with("ending") {
            NodeKind::Ending
        } else if t.starts_with("unlock") {
            NodeKind::Unlock
        } else if t.starts_with('$') {
            NodeKind::VarOp
        } else if t.starts_with('"') {
            NodeKind::Narration
        } else if t.contains(':') && t.contains('"') {
            NodeKind::Dialogue
        } else {
            NodeKind::Other
        }
    }

    /// 节点类型对应的标题栏颜色。
    fn kind_color(kind: NodeKind) -> egui::Color32 {
        match kind {
            NodeKind::Section => egui::Color32::from_rgb(70, 110, 200),
            NodeKind::Dialogue => egui::Color32::from_rgb(60, 140, 80),
            NodeKind::Narration => egui::Color32::from_rgb(90, 160, 100),
            NodeKind::Command => egui::Color32::from_rgb(200, 140, 50),
            NodeKind::Direction => egui::Color32::from_rgb(150, 80, 200),
            NodeKind::Choice => egui::Color32::from_rgb(200, 180, 50),
            NodeKind::ChoiceOption => egui::Color32::from_rgb(220, 200, 80),
            NodeKind::Flow => egui::Color32::from_rgb(50, 180, 200),
            NodeKind::Visit => egui::Color32::from_rgb(80, 150, 200),
            NodeKind::Return => egui::Color32::from_rgb(120, 120, 130),
            NodeKind::Wait => egui::Color32::from_rgb(200, 100, 150),
            NodeKind::StoryEnd => egui::Color32::from_rgb(200, 60, 60),
            NodeKind::Ending => egui::Color32::from_rgb(180, 80, 160),
            NodeKind::Unlock => egui::Color32::from_rgb(160, 100, 180),
            NodeKind::VarOp => egui::Color32::from_rgb(100, 130, 160),
            NodeKind::Comment => egui::Color32::from_rgb(110, 110, 120),
            NodeKind::Other => egui::Color32::from_rgb(100, 100, 110),
        }
    }

    /// 节点类型对应的中文标签。
    /// 对于命令行（@开头），返回具体命令名（背景/音乐等），而非笼统的"命令"。
    fn node_label(text: &str, kind: NodeKind) -> String {
        let t = text.trim_start();
        match kind {
            NodeKind::Section => "章节".to_string(),
            NodeKind::Dialogue => "对话".to_string(),
            NodeKind::Narration => "旁白".to_string(),
            NodeKind::Command => {
                // @bg → 背景，@music → 音乐，@stop_music → 停止音乐，其他用命令名
                let rest = t.strip_prefix('@').unwrap_or(t).trim_start();
                let cmd: String = rest.chars().take_while(|c| !c.is_whitespace()).collect();
                match cmd.as_str() {
                    "bg" => "背景".to_string(),
                    "music" => "音乐".to_string(),
                    "stop_music" => "停止音乐".to_string(),
                    other => other.to_string(),
                }
            }
            NodeKind::Direction => {
                if t.starts_with('+') {
                    "立绘上场".to_string()
                } else {
                    "立绘下场".to_string()
                }
            }
            NodeKind::Choice => "选择".to_string(),
            NodeKind::ChoiceOption => "选项".to_string(),
            NodeKind::Flow => "跳转".to_string(),
            NodeKind::Visit => "访问".to_string(),
            NodeKind::Return => "返回".to_string(),
            NodeKind::Wait => "等待".to_string(),
            NodeKind::StoryEnd => "结局".to_string(),
            NodeKind::Ending => "结局声明".to_string(),
            NodeKind::Unlock => "解锁".to_string(),
            NodeKind::VarOp => "变量".to_string(),
            NodeKind::Comment => "注释".to_string(),
            NodeKind::Other => "其他".to_string(),
        }
    }

    /// 估算节点尺寸：宽度随最长行自适应，高度随软换行后行数自适应（最多 8 行）。
    /// 单行节点保持单行高度；多行/超宽文本按软换行后的实际行数增高，上限 8 行。
    fn node_size(text: &str) -> egui::Vec2 {
        // 估算每字符宽度：中文约 12px，ASCII 约 7px。
        let char_width = |c: char| if c.is_ascii() { 7.0 } else { 12.0 };
        let max_line_w = text
            .lines()
            .map(|line| line.chars().map(char_width).sum::<f32>())
            .fold(0.0f32, f32::max);
        // 宽度 = 正文最大行宽 + 左右内边距，限制在 [200, 420]。
        let width = (max_line_w + 24.0).clamp(200.0, 420.0);
        let header_h = 22.0;
        // 估算软换行后的总行数：每行按可用宽度向上取整折算。
        let avail_w = (width - 24.0).max(1.0);
        let mut total_lines = 0usize;
        for line in text.lines() {
            let line_w: f32 = line.chars().map(char_width).sum();
            let wrapped = ((line_w / avail_w).ceil() as usize).max(1);
            total_lines += wrapped;
        }
        // 高度随行数自适应，上限 8 行（避免单个节点过高）。
        let line_count = total_lines.max(1).min(8);
        let body_h = line_count as f32 * 15.0 + 12.0;
        egui::Vec2::new(width, header_h + body_h)
    }

    /// 计算从 from_pin 到 to_pin 的正交路径，绕开所有节点矩形。
    /// 返回路径上的点序列（含首尾），用折线（PathShape）绘制。
    /// 坐标空间：逻辑坐标（不含 zoom/pan）。仅允许在首尾引脚处接触节点，
    /// 中间水平通道必须不穿过任何障碍节点矩形。
    fn route_link(
        &self,
        from_pin: egui::Pos2, // 逻辑坐标
        to_pin: egui::Pos2,   // 逻辑坐标
        skip_ids: &[usize],   // 排除的节点ID（源和目标节点本身）
    ) -> Vec<egui::Pos2> {
        // 同列连线（x 差值很小）直接画直线，保持逻辑顺序清晰
        if (from_pin.x - to_pin.x).abs() < 10.0 {
            return vec![from_pin, to_pin];
        }

        let gap = 15.0; // 逻辑坐标下连线与节点的间距

        // 收集障碍节点矩形（逻辑坐标）：排除源/目标节点；
        // 折叠注释时注释节点不参与阻挡检测。
        let obstacles: Vec<egui::Rect> = self
            .nodes
            .iter()
            .filter(|n| !skip_ids.contains(&n.id))
            .filter(|n| !(self.collapse_comments && n.kind == NodeKind::Comment))
            .map(|n| {
                let size = BlueprintState::node_size(&n.text);
                egui::Rect::from_min_size(n.pos, size)
            })
            .collect();

        // 水平通道段的 x 范围
        let x_min = from_pin.x.min(to_pin.x);
        let x_max = from_pin.x.max(to_pin.x);

        // 判断 y=channel_y 处的水平段 [x_min, x_max] 是否与任何障碍矩形相交
        let intersects_any = |channel_y: f32| -> bool {
            obstacles.iter().any(|r| {
                x_min < r.right()
                    && x_max > r.left()
                    && channel_y > r.top()
                    && channel_y < r.bottom()
            })
        };

        // 跨列连线优先走"源下方"通道（保持从上到下的逻辑顺序：章节A尾部→章节B头部）。
        // 只有源下方通道被阻挡时才尝试其他通道，且按"离源下方近"到"远"排序。
        // 这样跨列连线视觉上总是从源节点底部向下出发，再水平到目标列，再向上到目标，
        // 保持首尾相连的逻辑顺序感，不会忽上忽下。
        let mut candidates: Vec<f32> = Vec::new();
        // 优先级1：源下方（最符合"首尾相连"的视觉直觉）
        candidates.push(from_pin.y + gap);
        // 优先级2：目标上方（次选，当源下方被挡时）
        candidates.push(to_pin.y - gap);
        // 优先级3：所有节点之间的间隙（从源下方开始往下找）
        for r in &obstacles {
            let y = r.bottom() + gap;
            if y > from_pin.y {
                candidates.push(y);
            }
        }
        // 优先级4：所有节点之间的间隙（从源上方开始往上找，作为最后手段）
        for r in &obstacles {
            let y = r.top() - gap;
            if y < from_pin.y {
                candidates.push(y);
            }
        }

        // 选择第一个不与任何障碍相交的候选 y
        let channel_y = candidates
            .iter()
            .cloned()
            .find(|&y| !intersects_any(y))
            .unwrap_or(from_pin.y + gap); // 全部被挡时退化为源下方（允许穿过）

        // 生成正交折线路径：源引脚 → 向下到通道 → 水平到目标列 → 到目标引脚
        // 保持"从源底部出发向下，再水平，再到目标"的流向，体现首尾相连的逻辑顺序
        vec![
            from_pin,
            egui::pos2(from_pin.x, channel_y),
            egui::pos2(to_pin.x, channel_y),
            to_pin,
        ]
    }

    /// 判断两个节点是否在不同章节（即不同列）。
    /// auto_layout 按章节分列，同列节点居中对齐后中心 x 一致。
    /// 基于节点 x 坐标判断是否跨列，比基于 Vec 索引更可靠：
    /// 折叠注释后连线桥接了 Comment 节点，Vec 索引法会误判。
    fn is_cross_section_link(&self, from_id: usize, to_id: usize) -> bool {
        let from = self.nodes.iter().find(|n| n.id == from_id);
        let to = self.nodes.iter().find(|n| n.id == to_id);
        match (from, to) {
            (Some(f), Some(t)) => {
                let f_size = Self::node_size(&f.text);
                let t_size = Self::node_size(&t.text);
                let f_cx = f.pos.x + f_size.x / 2.0;
                let t_cx = t.pos.x + t_size.x / 2.0;
                // 中心 x 差值大于 20 认为是跨列（同列节点居中对齐后中心 x 一致）
                (f_cx - t_cx).abs() > 20.0
            }
            _ => false,
        }
    }

    /// 三次贝塞尔曲线上的点（t∈[0,1]）。
    fn bezier_point(
        p0: egui::Pos2,
        p1: egui::Pos2,
        p2: egui::Pos2,
        p3: egui::Pos2,
        t: f32,
    ) -> egui::Pos2 {
        let u = 1.0 - t;
        let tt = t * t;
        let uu = u * u;
        // egui 中 Pos2 未实现 Add<Pos2>（仅 Pos2 + Vec2），转 Vec2 加权求和后再转回
        (p0.to_vec2() * (uu * u)
            + p1.to_vec2() * (3.0 * uu * t)
            + p2.to_vec2() * (3.0 * u * tt)
            + p3.to_vec2() * (tt * t))
            .to_pos2()
    }

    /// 计算跨章节连线的贝塞尔曲线路径，绕开所有节点矩形。
    /// 返回 4 个点（起点、2 个控制点、终点），用于 CubicBezierShape。
    /// 坐标空间：逻辑坐标（不含 zoom/pan）。仅在首尾引脚处允许接触节点，
    /// 曲线中段必须不穿过任何障碍节点矩形（采样 100 点检测）。
    ///
    /// 控制点策略：放在 from 和 to 的 y 中点（不向外延伸），
    /// 这样曲线从 from 平滑弯向 to，不会绕大圈。
    /// 若穿过节点，从障碍节点的上方/下方找最近的空隙穿过。
    fn route_bezier_around(
        &self,
        from_pin: egui::Pos2, // 逻辑坐标
        to_pin: egui::Pos2,   // 逻辑坐标
        skip_ids: &[usize],   // 排除的节点ID（源和目标节点本身）
    ) -> [egui::Pos2; 4] {
        // 收集障碍节点矩形（逻辑坐标）：排除源/目标节点；
        // 折叠注释时注释节点不参与阻挡检测。
        let obstacles: Vec<egui::Rect> = self
            .nodes
            .iter()
            .filter(|n| !skip_ids.contains(&n.id))
            .filter(|n| !(self.collapse_comments && n.kind == NodeKind::Comment))
            .map(|n| {
                let size = BlueprintState::node_size(&n.text);
                egui::Rect::from_min_size(n.pos, size)
            })
            .collect();

        // 采样曲线上的点，检查是否穿过任何障碍矩形
        let curve_ok = |c1: egui::Pos2, c2: egui::Pos2| -> bool {
            for t in (0..=100).map(|i| i as f32 / 100.0) {
                let p = Self::bezier_point(from_pin, c1, c2, to_pin, t);
                if obstacles.iter().any(|r| r.contains(p)) {
                    return false;
                }
            }
            true
        };

        // 生成控制点：两个控制点的 y 坐标相同（在 from 和 to 的 y 中间），
        // x 分别保持 from 和 to 的 x，形成平滑 S 形曲线。
        let make_ctrls = |y: f32| -> [egui::Pos2; 2] {
            [egui::pos2(from_pin.x, y), egui::pos2(to_pin.x, y)]
        };

        // 默认：控制点在 from 和 to 的 y 中点
        let mid_y = (from_pin.y + to_pin.y) * 0.5;
        let default_ctrls = make_ctrls(mid_y);
        if curve_ok(default_ctrls[0], default_ctrls[1]) {
            return [from_pin, default_ctrls[0], default_ctrls[1], to_pin];
        }

        // 默认曲线穿过节点，收集 x 范围内障碍节点的 top/bottom，
        // 尝试从障碍上方或下方的空隙穿过。
        let x_min = from_pin.x.min(to_pin.x);
        let x_max = from_pin.x.max(to_pin.x);
        let gap = 15.0; // 与障碍节点的垂直间距

        let mut candidates: Vec<f32> = Vec::new();
        for r in &obstacles {
            // 只考虑 x 范围与曲线水平跨度有重叠的障碍
            if r.right() > x_min && r.left() < x_max {
                candidates.push(r.top() - gap);    // 障碍上方
                candidates.push(r.bottom() + gap); // 障碍下方
            }
        }

        // 按离 mid_y 近到远排序，优先选择最短绕行
        candidates.sort_by(|a, b| {
            (a - mid_y)
                .abs()
                .partial_cmp(&(b - mid_y).abs())
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        for &y in &candidates {
            let ctrls = make_ctrls(y);
            if curve_ok(ctrls[0], ctrls[1]) {
                return [from_pin, ctrls[0], ctrls[1], to_pin];
            }
        }

        // 全部候选都穿过，退化为默认中点（允许穿过，至少视觉平滑）
        [from_pin, default_ctrls[0], default_ctrls[1], to_pin]
    }

    /// 返回节点的友好显示文本（去掉脚本前缀符号，不暴露代码语法）。
    /// 例如 `+ 角色 at 0.5,1.0` → `角色 at 0.5,1.0`，`? 提示` → `提示`。
    /// 仅影响画布显示，节点 text 字段仍保留原始脚本文本用于导出。
    fn node_display_text(text: &str, kind: NodeKind) -> String {
        let t = text.trim();
        match kind {
            NodeKind::Section => t.strip_prefix('#').unwrap_or(t).trim().to_string(),
            NodeKind::Dialogue => {
                // 角色: "对话" → 角色：对话
                if let Some(colon) = t.find(':') {
                    let speaker = t[..colon].trim();
                    let rest = t[colon + 1..].trim();
                    let content = rest.trim_matches('"');
                    format!("{}：{}", speaker, content)
                } else {
                    t.to_string()
                }
            }
            NodeKind::Narration => t.trim_matches('"').to_string(),
            NodeKind::Command => t.strip_prefix('@').unwrap_or(t).trim().to_string(),
            NodeKind::Direction => {
                // +/- 开头的立绘上场/下场
                if t.starts_with('+') {
                    format!("上场：{}", t.trim_start_matches('+').trim())
                } else {
                    format!("下场：{}", t.trim_start_matches('-').trim())
                }
            }
            NodeKind::Choice => t.strip_prefix('?').unwrap_or(t).trim().to_string(),
            NodeKind::ChoiceOption => t.strip_prefix('|').unwrap_or(t).trim().to_string(),
            NodeKind::Flow => t.strip_prefix("->").unwrap_or(t).trim().to_string(),
            NodeKind::Visit => t.strip_prefix("=>").unwrap_or(t).trim().to_string(),
            NodeKind::Return => "返回".to_string(),
            NodeKind::Wait => {
                let val = t.strip_prefix("~~").unwrap_or(t).trim();
                if val.is_empty() {
                    "等待".to_string()
                } else {
                    format!("等待 {} 秒", val)
                }
            }
            NodeKind::StoryEnd => "故事结束".to_string(),
            NodeKind::Ending => t.strip_prefix("ending").unwrap_or(t).trim().to_string(),
            NodeKind::Unlock => t.strip_prefix("unlock").unwrap_or(t).trim().to_string(),
            NodeKind::VarOp => t.strip_prefix('$').unwrap_or(t).trim().to_string(),
            NodeKind::Comment => t.strip_prefix("//").unwrap_or(t).trim().to_string(),
            NodeKind::Other => t.to_string(),
        }
    }

    /// 从脚本文本生成蓝图节点（自动布局 + 顺序连线）。
    /// 选择块结尾的 `?` 不生成节点（to_script 会自动补上）。
    /// 生成后调用 auto_layout 整理布局，避免节点堆叠。
    fn from_script(&mut self, text: &str) {
        self.nodes.clear();
        self.links.clear();
        self.next_id = 0;
        self.selected = None;

        // 先生成所有节点（位置暂为默认），同时建立顺序连线。
        let mut prev_id: Option<usize> = None;
        let mut in_choice = false; // 是否在选择块内（用于跳过结尾 ?）
        for line in text.lines() {
            if line.trim().is_empty() {
                continue;
            }
            let kind = Self::detect_kind(line);
            // 选择块结尾的 ? 跳过（to_script 遇到 Choice 后自动补 ?）
            if in_choice && kind == NodeKind::Choice {
                in_choice = false;
                continue;
            }
            let id = self.add_node(kind, egui::Pos2::ZERO, line.to_string());
            // 与上一个节点顺序连线（含章节节点→本章第一个积木，保持流程连贯）。
            if let Some(prev) = prev_id {
                self.links.push(BlueprintLink { from: prev, to: id });
            }
            prev_id = Some(id);
            if kind == NodeKind::Choice {
                in_choice = true;
            }
        }
        // 生成完毕后统一整理布局。
        self.auto_layout();
    }

    /// 从蓝图节点生成脚本文本。
    ///
    /// 注释节点（`//` 开头）按连线状态分类处理：
    /// - **完全连线**（上下都连）：作为普通节点参与拓扑排序，在流程位置输出。
    /// - **只连上面**（有入线无出线）：作为前驱节点的下一行注释输出（紧跟前驱之后）。
    /// - **只连下面**（有出线无入线）：作为后继节点的上一行注释输出（紧邻后继之前）。
    /// - **都没连**（孤立）：不导出到脚本中（调用方可通过 [`orphan_comments`] 获取
    ///   孤立注释列表，在保存/离开时警告用户这些注释无法迁移）。
    ///
    /// 非注释节点按连线顺序（DFS 遍历）输出；无连线则按创建顺序输出。
    fn to_script(&self) -> String {
        if self.nodes.is_empty() {
            return String::new();
        }

        // ---- 分类注释节点 ----
        // after_comments[pred_id]：只连上面的注释，附在前驱之后输出。
        // before_comments[succ_id]：只连下面的注释，附在后继之前输出。
        // skip_ids：不参与主流程拓扑输出的注释节点（只连上/只连下/都没连）。
        let mut after_comments: HashMap<usize, Vec<String>> = HashMap::new();
        let mut before_comments: HashMap<usize, Vec<String>> = HashMap::new();
        let mut skip_ids: HashSet<usize> = HashSet::new();

        // 第一遍：收集所有需要跳过的注释节点（非完全连线的）。
        // 必须先完整构建 skip_ids，第二遍才能正确穿过被跳过的注释节点解析链。
        for node in &self.nodes {
            if node.kind != NodeKind::Comment {
                continue;
            }
            let has_incoming = self.links.iter().any(|l| l.to == node.id);
            let has_outgoing = self.links.iter().any(|l| l.from == node.id);
            if !has_incoming || !has_outgoing {
                skip_ids.insert(node.id);
            }
        }

        // 第二遍：分类并附到前驱/后继上。
        for node in &self.nodes {
            if node.kind != NodeKind::Comment {
                continue;
            }
            let has_incoming = self.links.iter().any(|l| l.to == node.id);
            let has_outgoing = self.links.iter().any(|l| l.from == node.id);
            match (has_incoming, has_outgoing) {
                (true, false) => {
                    // 只连上面：找到前驱（穿过其他被跳过的注释节点）。
                    if let Some(pred) = self.resolve_chain(node.id, true, &skip_ids) {
                        after_comments.entry(pred).or_default().push(node.text.clone());
                    }
                }
                (false, true) => {
                    // 只连下面：找到后继（穿过其他被跳过的注释节点）。
                    if let Some(succ) = self.resolve_chain(node.id, false, &skip_ids) {
                        before_comments.entry(succ).or_default().push(node.text.clone());
                    }
                }
                // 完全连线或都没连：不在第二遍处理（完全连线参与拓扑，都没连由 orphan_comments 报告）。
                _ => {}
            }
        }

        // ---- 拓扑排序（跳过被分类的注释节点，但完全连线注释仍参与）----
        let order = self.topo_order(&skip_ids);

        // ---- 构建输出行 ----
        // 对每个节点：先输出其 before_comments，再输出节点文本，再输出 after_comments。
        // 选择块（Choice）特殊处理：输出 Choice 后沿出线找 ChoiceOption 链逐个输出，
        // 最后自动补结尾 `?`（from_script 跳过了结尾 ?，这里负责还原）。
        let mut lines: Vec<String> = Vec::new();
        let mut processed_options: std::collections::HashSet<usize> = std::collections::HashSet::new();
        for id in &order {
            if processed_options.contains(id) {
                continue;
            }
            if let Some(comments) = before_comments.get(id) {
                for c in comments {
                    lines.push(c.clone());
                }
            }
            if let Some(node) = self.nodes.iter().find(|n| n.id == *id) {
                lines.push(node.text.clone());
                // 选择块：沿出线找 ChoiceOption 链，逐个输出后补 ?
                if node.kind == NodeKind::Choice {
                    let mut cur = *id;
                    loop {
                        // 找 cur 的出线中第一个目标是 ChoiceOption 的
                        let next_opt = self.links.iter()
                            .find(|l| l.from == cur)
                            .and_then(|l| {
                                self.nodes.iter().find(|n| n.id == l.to && n.kind == NodeKind::ChoiceOption)
                            });
                        match next_opt {
                            Some(opt) => {
                                lines.push(opt.text.clone());
                                processed_options.insert(opt.id);
                                cur = opt.id;
                            }
                            None => break, // 没有 ChoiceOption 后继了，选择块结束
                        }
                    }
                    lines.push("?".to_string()); // 自动补结尾 ?
                }
            }
            if let Some(comments) = after_comments.get(id) {
                for c in comments {
                    lines.push(c.clone());
                }
            }
        }

        lines.join("\n")
    }

    /// 返回所有「都没连」（既无入线也无出线）的孤立注释节点文本列表。
    /// 这些注释不会出现在导出的脚本中，调用方应在保存/离开蓝图时警告用户。
    fn orphan_comments(&self) -> Vec<String> {
        self.nodes
            .iter()
            .filter(|n| n.kind == NodeKind::Comment)
            .filter(|n| {
                let has_incoming = self.links.iter().any(|l| l.to == n.id);
                let has_outgoing = self.links.iter().any(|l| l.from == n.id);
                !has_incoming && !has_outgoing
            })
            .map(|n| n.text.clone())
            .collect()
    }

    /// 沿连线链解析：穿过其他被跳过的注释节点，找到最近的未被跳过的前驱或后继。
    /// - `up = true`：向上找前驱（follow `to == id` → `from`）。
    /// - `up = false`：向下找后继（follow `from == id` → `to`）。
    fn resolve_chain(
        &self,
        start: usize,
        up: bool,
        skip_ids: &HashSet<usize>,
    ) -> Option<usize> {
        let mut current = start;
        let mut guard = 0usize;
        const MAX_DEPTH: usize = 64;
        loop {
            guard += 1;
            if guard > MAX_DEPTH {
                break;
            }
            let next = if up {
                self.links.iter().find(|l| l.to == current).map(|l| l.from)
            } else {
                self.links.iter().find(|l| l.from == current).map(|l| l.to)
            };
            match next {
                Some(n) => {
                    if skip_ids.contains(&n) {
                        // 该前驱/后继也是被跳过的注释，继续沿链查找。
                        current = n;
                    } else {
                        return Some(n);
                    }
                }
                None => return None,
            }
        }
        None
    }

    /// 拓扑排序：DFS 从无入线节点出发，跳过 `skip_ids` 中的节点（不加入输出顺序，
    /// 但仍遍历其连线以桥接后续节点）。无连线时按创建顺序输出。
    fn topo_order(&self, skip_ids: &HashSet<usize>) -> Vec<usize> {
        let has_incoming = |id: usize| self.links.iter().any(|l| l.to == id);
        let mut order: Vec<usize> = Vec::new();
        let mut visited: Vec<usize> = Vec::new();
        let starts: Vec<usize> = self
            .nodes
            .iter()
            .filter(|n| !has_incoming(n.id))
            .map(|n| n.id)
            .collect();
        let starts = if starts.is_empty() {
            self.nodes.first().map(|n| vec![n.id]).unwrap_or_default()
        } else {
            starts
        };
        let mut stack: Vec<usize> = starts.into_iter().rev().collect();
        while let Some(id) = stack.pop() {
            if visited.contains(&id) {
                continue;
            }
            visited.push(id);
            if !skip_ids.contains(&id) {
                order.push(id);
            }
            let nexts: Vec<usize> = self
                .links
                .iter()
                .filter(|l| l.from == id)
                .map(|l| l.to)
                .collect();
            for next in nexts.into_iter().rev() {
                stack.push(next);
            }
        }
        // 追加未访问的节点（跳过的除外）。
        for node in &self.nodes {
            if !visited.contains(&node.id) && !skip_ids.contains(&node.id) {
                order.push(node.id);
            }
        }
        order
    }

    /// 自动整理布局：按章节分列，一列纵向不限（不换列），章节间水平排列。
    /// 节点在列内水平居中对齐——同列所有节点中心 x 一致，连线是垂直直线不弯曲。
    /// 节点间距根据节点实际高度累加，确保不重叠。
    fn auto_layout(&mut self) {
        if self.nodes.is_empty() {
            return;
        }
        // 整理策略：仅按章节分列（一列纵向不限），每列宽度取该列最宽节点自适应。
        // 节点在列内水平居中对齐，使同列节点中心 x 一致，连线保持为垂直直线。
        // 折叠注释时跳过注释节点（它们隐藏到下一个非注释积木中，不占布局位置）。
        let collapse = self.collapse_comments;
        let skip = |n: &BlueprintNode| collapse && n.kind == NodeKind::Comment;
        let gap = 24.0; // 同列节点之间的垂直间距
        let col_gap = 40.0; // 列之间的水平间距
        let top_margin = 20.0;
        let left_margin = 20.0;

        // 第一遍：计算所有节点尺寸。
        let sizes: Vec<egui::Vec2> = self
            .nodes
            .iter()
            .map(|n| Self::node_size(&n.text))
            .collect();

        // 确定每个节点所属的列号（跳过注释）。
        // 仅按章节分列，一列纵向不限，不再因行数换列。
        let mut col_of: Vec<usize> = vec![0; self.nodes.len()];
        let mut cur_col = 0usize;
        let mut any_placed = false;
        for (i, node) in self.nodes.iter().enumerate() {
            if skip(node) {
                col_of[i] = cur_col; // 注释归到当前列（不显示，位置无意义）
                continue;
            }
            if node.kind == NodeKind::Section && any_placed {
                cur_col += 1;
            }
            col_of[i] = cur_col;
            any_placed = true;
        }
        let num_cols = cur_col + 1;

        // 计算每列最宽节点宽度（下限 200，避免列过窄）。跳过注释。
        let mut col_widths = vec![200.0f32; num_cols];
        for (i, size) in sizes.iter().enumerate() {
            if skip(&self.nodes[i]) {
                continue;
            }
            let c = col_of[i];
            if size.x > col_widths[c] {
                col_widths[c] = size.x;
            }
        }

        // 计算每列的 x 起点（累加列宽 + 列间距）。
        let mut col_x: Vec<f32> = Vec::with_capacity(num_cols);
        let mut x = left_margin;
        for w in &col_widths {
            col_x.push(x);
            x += w + col_gap;
        }

        // 第二遍：放置节点，同列内水平居中对齐，y 坐标按实际高度累加。
        // 居中对齐后同列节点中心 x 一致，连线（底部中心→顶部中心）为垂直直线。
        // 跳过注释（不占垂直空间）。
        let mut col_y = vec![top_margin; num_cols];
        for (i, node) in self.nodes.iter_mut().enumerate() {
            if skip(node) {
                continue;
            }
            let c = col_of[i];
            let node_w = sizes[i].x;
            let col_w = col_widths[c];
            // 居中对齐：节点 x = 列起点 + (列宽 - 节点宽) / 2
            node.pos = egui::pos2(col_x[c] + (col_w - node_w) * 0.5, col_y[c]);
            col_y[c] += sizes[i].y + gap;
        }
    }

    /// 命中测试：返回指定画布坐标处的节点 ID（优先命中标题栏）。
    fn node_at(&self, canvas_pos: egui::Pos2) -> Option<usize> {
        // 从后往前测试（后绘制的在上层）。
        for node in self.nodes.iter().rev() {
            // 折叠注释时不参与命中。
            if self.collapse_comments && node.kind == NodeKind::Comment {
                continue;
            }
            let size = Self::node_size(&node.text);
            let rect = egui::Rect::from_min_size(node.pos, size);
            if rect.contains(canvas_pos) {
                return Some(node.id);
            }
        }
        None
    }

    /// 命中测试：返回指定画布坐标处的输出引脚所属节点 ID。
    /// `r` 为引脚命中半径（触摸模式下应放大以便点按）。
    fn output_pin_at(&self, canvas_pos: egui::Pos2, r: f32) -> Option<usize> {
        for node in &self.nodes {
            if self.collapse_comments && node.kind == NodeKind::Comment {
                continue;
            }
            // 结局/结局声明模块无输出引脚（流程到此终止，不再继续连线）。
            if node.kind == NodeKind::StoryEnd || node.kind == NodeKind::Ending {
                continue;
            }
            let size = Self::node_size(&node.text);
            let rect = egui::Rect::from_min_size(node.pos, size);
            let pin = egui::pos2(rect.center().x, rect.bottom());
            if canvas_pos.distance(pin) <= r {
                return Some(node.id);
            }
        }
        None
    }

    /// 命中测试：返回指定画布坐标处的输入引脚所属节点 ID。
    /// `r` 为引脚命中半径（触摸模式下应放大以便点按）。
    fn input_pin_at(&self, canvas_pos: egui::Pos2, r: f32) -> Option<usize> {
        for node in &self.nodes {
            if self.collapse_comments && node.kind == NodeKind::Comment {
                continue;
            }
            let size = Self::node_size(&node.text);
            let rect = egui::Rect::from_min_size(node.pos, size);
            let pin = egui::pos2(rect.center().x, rect.top());
            if canvas_pos.distance(pin) <= r {
                return Some(node.id);
            }
        }
        None
    }

    /// 返回指定节点的入线来源节点 ID（取第一条）。
    fn incoming(&self, id: usize) -> Option<usize> {
        self.links.iter().find(|l| l.to == id).map(|l| l.from)
    }

    /// 返回指定节点的出线目标节点 ID（取第一条）。
    fn outgoing(&self, id: usize) -> Option<usize> {
        self.links.iter().find(|l| l.from == id).map(|l| l.to)
    }

    /// 折叠注释时，把一个节点（可能是注释）解析为它对应的非注释"来源"节点：
    /// 若该节点本身非注释则返回它；否则沿入线回溯到最近的非注释节点。
    fn resolve_from(&self, id: usize) -> Option<usize> {
        let mut cur = id;
        let mut guard = 0usize;
        loop {
            if guard > self.nodes.len() + 1 {
                return None;
            }
            guard += 1;
            let kind = self.nodes.iter().find(|n| n.id == cur).map(|n| n.kind);
            match kind {
                Some(NodeKind::Comment) => {
                    cur = self.incoming(cur)?;
                }
                _ => return Some(cur),
            }
        }
    }

    /// 折叠注释时，把一个节点（可能是注释）解析为它对应的非注释"去向"节点：
    /// 若该节点本身非注释则返回它；否则沿出线前进到最近的非注释节点。
    fn resolve_to(&self, id: usize) -> Option<usize> {
        let mut cur = id;
        let mut guard = 0usize;
        loop {
            if guard > self.nodes.len() + 1 {
                return None;
            }
            guard += 1;
            let kind = self.nodes.iter().find(|n| n.id == cur).map(|n| n.kind);
            match kind {
                Some(NodeKind::Comment) => {
                    cur = self.outgoing(cur)?;
                }
                _ => return Some(cur),
            }
        }
    }

    /// 返回折叠隐藏到指定非注释积木中的注释文本列表（按脚本顺序）。
    /// 即：紧接在该积木之前、连续的注释行。默认隐藏逻辑——注释归到"下一句"积木。
    fn comments_for_node(&self, id: usize) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        let mut cur = match self.incoming(id) {
            Some(p) => p,
            None => return out,
        };
        loop {
            let node = match self.nodes.iter().find(|n| n.id == cur) {
                Some(n) => n,
                None => break,
            };
            if node.kind != NodeKind::Comment {
                break;
            }
            out.push(node.text.clone());
            cur = match self.incoming(cur) {
                Some(p) => p,
                None => break,
            };
        }
        out.reverse();
        out
    }
}

/// 蓝图右键菜单中可选的节点模板。
/// (标签, 简短描述, 模板文本)
/// 注意：选择模板拖出时会自动生成 Choice + 2 个 Option 节点 + 连线（见 show_blueprint 拖拽放置逻辑）。
const NODE_TEMPLATES: &[(&str, &str, &str)] = &[
    ("章节", "章节标题分隔", "# NewSection 章节标题"),
    ("对话", "角色说话", "角色: \"对话内容\""),
    ("旁白", "叙述文字", "\"旁白内容\""),
    ("背景", "切换背景图", "@bg background"),
    ("音乐", "播放音乐", "@music music"),
    ("立绘上场", "角色立绘登场", "+ 角色 at 0.5,1.0 size 1.0"),
    ("立绘下场", "角色立绘退场", "- 角色"),
    ("选择", "分支选项", "? 提示"),
    ("选项", "选择分支选项", "| 选项"),
    ("跳转", "跳转到章节", "-> TargetSection"),
    ("访问", "访问子章节", "=> TargetSection"),
    ("返回", "从子章节返回", "<="),
    ("等待", "暂停若干秒", "~~ 1.0"),
    ("结局", "故事结束", "end"),
];

/// 渲染单个积木卡片的内部内容（色块 + 标签 + 描述 + 模板文本）。
/// 供积木面板与拖拽预览复用，保证「所见即所得」——拖拽时看到的预览与面板里完全一致。
/// 调用方负责提供包裹的 `Frame`（填色 + 边框）。
fn paint_block_card(ui: &mut egui::Ui, label: &str, desc: &str, template: &str, color: egui::Color32) {
    ui.horizontal(|ui| {
        // 色块标识节点类型
        let (rect, _) = ui.allocate_exact_size(
            egui::Vec2::new(10.0, 10.0),
            egui::Sense::hover(),
        );
        ui.painter().rect_filled(rect, 2.0, color);
        ui.vertical(|ui| {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(label).strong().color(color));
                ui.label(
                    egui::RichText::new(desc)
                        .small()
                        .color(egui::Color32::from_rgb(140, 145, 160)),
                );
            });
            ui.label(
                egui::RichText::new(template)
                    .small()
                    .monospace()
                    .color(egui::Color32::from_rgb(180, 185, 195)),
            );
        });
    });
}

/// 构造积木卡片用的 Frame：半透明色填充 + 彩色边框 + 内边距。
fn block_card_frame(ui: &egui::Ui, color: egui::Color32, outer_y: f32) -> egui::Frame {
    egui::Frame::group(ui.style())
        .fill(color.linear_multiply(0.15))
        .stroke(egui::Stroke::new(1.0, color))
        .inner_margin(egui::Margin::same(6.0))
        .outer_margin(egui::Margin::symmetric(0.0, outer_y))
}

/// 记录哪些剧本文件已经在蓝图模式下做过首次自动布局整理。
/// 持久化到编辑器数据目录，避免每次进入蓝图模式都重新打乱用户手动调整过的布局。
/// 用纯文本文件存储（每行一个规范化路径），不依赖 serde。
#[derive(Default, Clone)]
struct BlueprintLayoutDone {
    /// 已整理过布局的文件规范化路径列表。
    files: Vec<String>,
}

impl BlueprintLayoutDone {
    fn load() -> Self {
        if let Some(path) = dirs_data_dir().map(|d| d.join("akrs-editor").join("blueprint_layout_done.txt")) {
            if let Ok(content) = std::fs::read_to_string(&path) {
                let files: Vec<String> = content
                    .lines()
                    .map(|l| l.trim().to_string())
                    .filter(|l| !l.is_empty())
                    .collect();
                return Self { files };
            }
        }
        Self::default()
    }

    fn save(&self) {
        if let Some(path) = dirs_data_dir().map(|d| d.join("akrs-editor").join("blueprint_layout_done.txt")) {
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let content = self.files.join("\n");
            let _ = std::fs::write(&path, content);
        }
    }

    /// 判断指定文件路径是否已整理过布局。
    fn is_done(&self, file_path: &std::path::Path) -> bool {
        let key = match std::fs::canonicalize(file_path) {
            Ok(p) => p.to_string_lossy().into_owned(),
            Err(_) => file_path.to_string_lossy().into_owned(),
        };
        self.files.iter().any(|f| *f == key)
    }

    /// 标记文件已整理过布局并持久化。
    fn mark_done(&mut self, file_path: &std::path::Path) {
        let key = match std::fs::canonicalize(file_path) {
            Ok(p) => p.to_string_lossy().into_owned(),
            Err(_) => file_path.to_string_lossy().into_owned(),
        };
        if !self.files.iter().any(|f| *f == key) {
            self.files.push(key);
            self.save();
        }
    }
}

// ---------------------------------------------------------------------------
// 编辑器设置（持久化到 editor_settings.json）
// ---------------------------------------------------------------------------

/// 编辑器设置：持久化到编辑器数据目录的 `editor_settings.json`。
///
/// 包含启动行为与退出行为相关开关，以及上次打开的文件路径（用于启动时自动恢复）。
#[derive(serde::Serialize, serde::Deserialize, Clone)]
struct EditorSettings {
    /// 进入编辑器默认打开上一次文件（默认 true）。
    #[serde(default = "EditorSettings::default_true")]
    open_last_file_on_startup: bool,
    /// 退出时自动保存文件不询问（默认 false）。
    #[serde(default)]
    auto_save_on_exit: bool,
    /// 进入编辑器自动启动蓝图模式（默认 true）。
    #[serde(default = "EditorSettings::default_true")]
    auto_blueprint_on_startup: bool,
    /// 上次打开的文件规范化路径（用于启动时自动恢复），无则为 None。
    #[serde(default)]
    last_file: Option<String>,
}

impl EditorSettings {
    /// serde 默认值辅助函数：返回 true。
    fn default_true() -> bool {
        true
    }

    /// 设置文件路径：`{数据目录}/akrs-editor/editor_settings.json`。
    fn path() -> Option<PathBuf> {
        dirs_data_dir().map(|d| d.join("akrs-editor").join("editor_settings.json"))
    }

    /// 从磁盘加载设置；文件缺失或解析失败时返回默认值。
    fn load() -> Self {
        let path = match Self::path() {
            Some(p) => p,
            None => return Self::default(),
        };
        std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    /// 将设置写入磁盘（覆盖式）。
    fn save(&self) {
        let path = match Self::path() {
            Some(p) => p,
            None => return,
        };
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(json) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(&path, json);
        }
    }

    /// 记录上次打开的文件路径并立即持久化。
    fn set_last_file(&mut self, path: &Path) {
        let key = std::fs::canonicalize(path)
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|_| path.to_string_lossy().into_owned());
        if self.last_file.as_deref() != Some(key.as_str()) {
            self.last_file = Some(key);
            self.save();
        }
    }
}

impl Default for EditorSettings {
    fn default() -> Self {
        Self {
            open_last_file_on_startup: true,
            auto_save_on_exit: false,
            auto_blueprint_on_startup: true,
            last_file: None,
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
    /// 项目根目录（open_project 时记录，预览子进程的 CWD 用此而非 work_dir）。
    /// 注意：work_dir 会被 open_file_path 覆盖为「打开文件的父目录」用于文件浏览，
    /// 但预览子进程必须以项目根为 CWD，否则 saves/*.json、project.json、assets/
    /// 都会从错误的子目录读取，导致尾声按钮、已读历史、设置等状态漂移。
    project_dir: Option<PathBuf>,
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
    /// 是否显示项目警告弹窗（打开项目时若作者留有警告且未被本地忽略则置 true）。
    show_project_warning: bool,
    /// 是否显示「请先保存再预览」提示弹窗（点预览时若当前文件未存档则置 true）。
    show_save_reminder: bool,
    /// 是否显示「孤立注释无法迁移」警告弹窗（保存/离开蓝图时若存在未连线注释则置 true）。
    show_comment_warning: bool,
    /// 触发警告的孤立注释文本列表（供弹窗展示）。
    comment_warning_list: Vec<String>,
    /// 本地「不再显示」的项目警告忽略列表（持久化到编辑器数据目录）。
    dismissed_warnings: DismissedWarnings,
    /// 是否处于蓝图模式（可视化节点编辑）。
    blueprint_mode: bool,
    /// 蓝图编辑器状态。
    blueprint: BlueprintState,
    /// 蓝图填空式编辑器的临时编辑状态。
    bp_edit: BlueprintEditState,
    /// 当前 `bp_edit` 已初始化对应的节点 ID（用于检测切换编辑节点时重新解析）。
    bp_edit_id: Option<usize>,
    /// 框选起点（画布逻辑坐标），非触摸模式下空白处左键拖拽时记录。
    box_select_start: Option<egui::Pos2>,
    /// 框选终点（画布逻辑坐标），随鼠标移动更新。
    box_select_end: Option<egui::Pos2>,
    /// 上一帧编辑面板的屏幕矩形，用于"点击面板内不退出编辑"判定。
    bp_edit_panel_rect: Option<egui::Rect>,
    /// 已在蓝图模式下做过首次自动布局整理的文件列表（持久化）。
    blueprint_layout_done: BlueprintLayoutDone,
    /// 正在从积木面板拖拽的模板索引（拖拽添加积木用）。
    drag_template: Option<usize>,
    /// 正在从预览面板拖拽的语法文本（拖拽添加节点用）。
    drag_syntax: Option<String>,
    /// 缩放百分比输入缓冲（右下角缩放控件用）。
    zoom_input: String,
    /// 上次保存/加载时的内容快照，用于判断是否有未保存修改。
    saved_content: String,
    /// 撤销历史栈（旧→新），每项是某次提交时的编辑器内容快照。
    undo_stack: Vec<String>,
    /// 重做历史栈（旧→新），撤销后可重做的内容快照。
    redo_stack: Vec<String>,
    /// 上一次提交到历史栈的编辑器内容（用于检测变化）。
    last_committed: String,
    /// 当前是否有未提交到历史栈的编辑（连续输入合并用）。
    edit_dirty: bool,
    /// 最后一次编辑（内容变化）的时间戳（秒），用于空闲合并提交。
    last_edit_time: f64,
    /// 撤销/重做正在执行中，本帧跳过变化检测，避免把 undo/redo 本身又压入历史。
    undo_redo_in_progress: bool,
    /// 是否正在等待退出确认对话框（点叉退出且有未保存修改时置 true）。
    pending_exit: bool,
    /// 用户已在对话框确认退出（保存或放弃），on_close_event 据此放行关闭。
    force_close: bool,
    /// 左栏当前标签页（脚本 / 立绘 / 背景 / 音乐）。
    left_panel_tab: LeftPanelTab,
    /// 立绘资源文件列表（含扩展名，扫描自 `assets/characters/`）。
    resource_sprites: Vec<String>,
    /// 背景资源文件列表（含扩展名，扫描自 `assets/bg/`）。
    resource_backgrounds: Vec<String>,
    /// 音乐资源文件列表（含扩展名，扫描自 `assets/music/`）。
    resource_musics: Vec<String>,
    /// 双击左栏图片资源时触发的预览弹窗目标路径（`None` 表示未打开）。
    resource_preview_path: Option<PathBuf>,
    /// 资源预览弹窗的纹理缓存（与 `resource_preview_path` 对应，避免每帧重新解码）。
    resource_preview_texture: Option<TextureHandle>,
    /// 编辑器设置（持久化到 editor_settings.json）。
    editor_settings: EditorSettings,
    /// 是否显示「编辑器设置」弹窗。
    show_editor_settings: bool,
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
        let dismissed_warnings = DismissedWarnings::load();
        let editor_settings = EditorSettings::load();
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
            project_dir: None,
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
            show_project_warning: false,
            show_save_reminder: false,
            show_comment_warning: false,
            comment_warning_list: Vec::new(),
            dismissed_warnings,
            blueprint_mode: false,
            blueprint: BlueprintState::default(),
            bp_edit: BlueprintEditState::default(),
            bp_edit_id: None,
            box_select_start: None,
            box_select_end: None,
            bp_edit_panel_rect: None,
            blueprint_layout_done: BlueprintLayoutDone::load(),
            drag_template: None,
            drag_syntax: None,
            zoom_input: "100".to_string(),
            saved_content: String::new(),
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            last_committed: String::new(),
            edit_dirty: false,
            last_edit_time: 0.0,
            undo_redo_in_progress: false,
            pending_exit: false,
            force_close: false,
            left_panel_tab: LeftPanelTab::Script,
            resource_sprites: Vec::new(),
            resource_backgrounds: Vec::new(),
            resource_musics: Vec::new(),
            resource_preview_path: None,
            resource_preview_texture: None,
            editor_settings,
            show_editor_settings: false,
        };
        app.refresh_file_list();

        // 启动行为应用：根据编辑器设置自动恢复上次文件并进入蓝图模式。
        // open_last_file_on_startup：若启用且记录了上次文件路径且文件仍存在，则自动打开。
        let mut opened_file = false;
        if app.editor_settings.open_last_file_on_startup {
            if let Some(last) = app.editor_settings.last_file.clone() {
                let path = PathBuf::from(&last);
                if path.exists() {
                    app.open_file_path(&path);
                    opened_file = app.current_file.is_some();
                }
            }
        }
        // auto_blueprint_on_startup：若启动时成功打开了文件且设置启用，则自动进入蓝图模式。
        // toggle_blueprint_mode 内部会检测空内容并拒绝，所以仅在有内容时调用。
        if opened_file && app.editor_settings.auto_blueprint_on_startup
            && !app.editor_content.trim().is_empty()
        {
            app.toggle_blueprint_mode();
        }

        app
    }
}

impl EditorApp {
    // -- 辅助方法：插入语法 -----------------------------------------------

    // -- 辅助方法：撤销 / 重做 -----------------------------------------------

    /// 历史栈最大容量，超出丢弃最旧项。
    const UNDO_LIMIT: usize = 100;
    /// 空闲合并提交阈值（秒）：用户停止输入超过此时长后，把当前内容提交到撤销栈。
    const UNDO_IDLE_SECS: f64 = 0.6;

    /// 把当前编辑器内容提交到撤销历史栈。
    /// 仅当内容相对上次提交有变化时才入栈，并清空重做栈（新编辑使重做失效）。
    /// 容量超过 `UNDO_LIMIT` 时丢弃最旧项。
    fn commit_history(&mut self) {
        if self.editor_content != self.last_committed {
            self.undo_stack.push(self.last_committed.clone());
            if self.undo_stack.len() > Self::UNDO_LIMIT {
                self.undo_stack.remove(0);
            }
            self.redo_stack.clear();
            self.last_committed = self.editor_content.clone();
        }
        self.edit_dirty = false;
    }

    /// 撤销：先把当前未提交编辑入栈，再弹出上一个历史状态。
    fn undo(&mut self) {
        // 先提交当前编辑，确保连续输入能被撤销到上一个稳定状态。
        self.commit_history();
        if let Some(prev) = self.undo_stack.pop() {
            self.redo_stack.push(self.last_committed.clone());
            self.editor_content = prev.clone();
            self.last_committed = prev;
            self.undo_redo_in_progress = true;
            self.status = "已撤销".to_string();
        } else {
            self.status = "没有可撤销的操作".to_string();
        }
    }

    /// 重做：把当前内容压回撤销栈，弹出重做栈顶恢复。
    fn redo(&mut self) {
        if let Some(next) = self.redo_stack.pop() {
            self.undo_stack.push(self.last_committed.clone());
            if self.undo_stack.len() > Self::UNDO_LIMIT {
                self.undo_stack.remove(0);
            }
            self.editor_content = next.clone();
            self.last_committed = next;
            self.undo_redo_in_progress = true;
            self.status = "已重做".to_string();
        } else {
            self.status = "没有可重做的操作".to_string();
        }
    }

    /// 加载/新建/打开文件等整体内容替换时重置历史基线：
    /// 清空撤销与重做栈，把 last_committed 同步为新内容，避免跨文件撤销穿越。
    fn reset_history(&mut self) {
        self.undo_stack.clear();
        self.redo_stack.clear();
        self.last_committed = self.editor_content.clone();
        self.edit_dirty = false;
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
        self.saved_content = self.editor_content.clone();
        self.current_file = None;
        self.file_name_input = "untitled.akrs".to_string();
        self.engine = None;
        self.diagnostics.clear();
        self.show_welcome = false;
        self.reset_history();
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
                self.saved_content = self.editor_content.clone();
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
                self.reset_history();
                self.status = format!("已打开 {}", name);
                // 记录上次打开的文件路径，供下次启动时自动恢复。
                self.editor_settings.set_last_file(path);
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
        self.saved_content = self.editor_content.clone();
        self.current_file = None;
        self.file_name_input = "sample.akrs".to_string();
        self.engine = None;
        self.diagnostics.clear();
        self.show_welcome = false;
        self.reset_history();
        self.status = "已加载示例剧本".to_string();
    }

    /// 将编辑器内容保存到 `work_dir/file_name_input`。
    fn save_file(&mut self) {
        // 蓝图模式下保存：先同步蓝图到编辑器内容，确保保存的是最新蓝图状态。
        // 同时检查孤立注释（未连线的注释节点），这些注释无法迁移到脚本中。
        if self.blueprint_mode {
            let orphans = self.blueprint.orphan_comments();
            self.editor_content = self.blueprint.to_script();
            if !orphans.is_empty() {
                self.comment_warning_list = orphans;
                self.show_comment_warning = true;
            }
        }
        let name = sanitize_filename(&self.file_name_input);
        let path = self.work_dir.join(&name);
        match std::fs::write(&path, &self.editor_content) {
            Ok(()) => {
                // 记录上次保存的文件路径，供下次启动时自动恢复。
                self.editor_settings.set_last_file(&path);
                self.current_file = Some(path);
                self.file_name_input = name.clone();
                self.saved_content = self.editor_content.clone();
                self.status = format!("已保存 {}", name);
                self.refresh_file_list();
                // main_script 一致性检查：若保存的文件名与 project.json 的
                // main_script 不一致，状态栏追加提示（非阻断）。
                // 直接 `cargo run -p akrs-game`（不带 --script）会按 main_script
                // 加载剧本，可能跑到别的文件而非用户当前编辑的文件。
                if self.project_loaded && name != self.project_config.main_script {
                    self.status.push_str(&format!(
                        " ｜ 提示：当前文件不是 project.json 的 main_script（{}），直接启动游戏将运行 {}；编辑器预览不受影响",
                        self.project_config.main_script, self.project_config.main_script
                    ));
                }
            }
            Err(e) => {
                self.status = format!("保存失败：{} - {}", name, e);
            }
        }
    }

    /// 当前剧本是否有未保存的修改（与上次保存/加载的快照不一致）。
    fn is_dirty(&self) -> bool {
        self.editor_content != self.saved_content
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

        // 项目警告：作者在 project.json 里留的提示文字，打开项目时弹出。
        // 已被玩家本地「不再显示」忽略过的不弹。
        if !self.project_config.warning.trim().is_empty()
            && !self.dismissed_warnings.is_dismissed(project_dir)
        {
            self.show_project_warning = true;
        }
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

    /// 返回项目根目录路径。
    ///
    /// 优先返回 `project_dir`（`open_project` 时记录的项目根）；
    /// 若未加载项目则回退到 `work_dir`。
    ///
    /// 资源扫描、打包复制、翻译文件定位等**项目级路径**必须用此方法，
    /// 而非直接用 `work_dir`——后者会被 `open_file_path` 覆盖为
    /// 「打开文件的父目录」（如 `scripts/`），导致资源路径漂移。
    fn project_root(&self) -> &Path {
        self.project_dir.as_deref().unwrap_or(&self.work_dir)
    }

    /// 翻译文件的路径（assets/scripts/languages/{lang}.json）。
    fn translation_file_path(&self, lang: &str) -> PathBuf {
        self.project_root().join("assets").join("scripts").join("languages").join(format!("{}.json", lang))
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
        // 原因：open_file_path 会把 work_dir 覆盖为「打开文件的父目录」（如 scripts/），
        // 若以 work_dir 为 CWD，游戏会从错误的子目录读取 saves/endings.json、
        // saves/read_history.json、project.json、assets/ 等，导致尾声按钮异常显示、
        // 已读历史丢失、标题/副标题丢失等状态漂移问题。
        // 优先用 open_project 时记录的 project_dir；未加载项目时回退 work_dir。
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
        let scripts_src = self.project_root().join("scripts");
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
                        let target_dir = self.project_root().join(format!(
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
            .unwrap_or_else(|| self.project_root().join("scripts"));
        if scripts_src.exists() {
            let scripts_dst = output_dir.join("scripts");
            copy_dir_recursive(&scripts_src, &scripts_dst);
        }

        // 资源始终从项目目录复制（资源不在快照范围内）
        let assets_src = self.project_root().join("assets");
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
                        + 角色名 (pose1) at 0.5,1.0 size 1.0\n\
                        角色名: \"对话内容\"\n\
                        + 角色名 (pose2) swap  // 差分更换（无过渡）\n\
                        $变量 = 1\n\
                        ? \"选择提示\"\n\
                        | \"选项1\"  -> 分支A\n\
                        | \"选项2\"  -> 分支B\n\
                        ?\n\
                        ~~  // 章节结束";
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
                    "提示：# 定义章节  @ 场景指令  + 角色上场  +...swap 差分更换  - 角色下场  $ 变量操作  ? 选择分支  ~~ 章节结束",
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
                // 在布局前从持久化状态读取光标字符位置，用于配对高亮（无帧延迟）。
                // id_source 与下方 TextEdit 的 .id_source 必须一致。
                let editor_id = ui.make_persistent_id(egui::Id::new("main_editor"));
                let cursor_ccursor = egui::text_edit::TextEditState::load(ui.ctx(), editor_id)
                    .and_then(|s| s.cursor.char_range())
                    .map(|r| r.primary.index);

                // 计算配对高亮区间：光标所在 mark 及其配对 mark 的字符范围。
                let marks = scan_flow_marks(&self.editor_content);
                let pairs = compute_flow_pairs(&marks);
                let mut highlight_ranges: Vec<(usize, usize)> = Vec::new();
                if let Some(ci) = cursor_ccursor {
                    if let Some(idx) = mark_at_cursor(&marks, ci) {
                        let m = &marks[idx];
                        highlight_ranges.push((m.char_start, m.char_end));
                        if let Some(p) = pairs[idx] {
                            let pm = &marks[p];
                            highlight_ranges.push((pm.char_start, pm.char_end));
                        }
                    }
                }

                let mut layouter = |ui: &egui::Ui, string: &str, wrap_width: f32| {
                    let mut job = highlight_code(string, &highlight_ranges);
                    job.wrap.max_width = wrap_width;
                    ui.fonts(|f| f.layout_job(job))
                };
                let output = egui::TextEdit::multiline(&mut self.editor_content)
                    .code_editor()
                    .desired_width(f32::MAX)
                    .id_source(egui::Id::new("main_editor"))
                    .layouter(&mut layouter)
                    .show(ui);

                // 悬停 tooltip：鼠标在 => / <= 上时显示配对信息。
                self.show_flow_hover_tooltip(ui.ctx(), &output, &marks, &pairs);

                // Ctrl+点击剧本中的指令行时，右侧预览跳转到对应参数。
                // 不使用 output.response.clicked()——egui 的 TextEdit 会将
                // Ctrl+点击解释为"选词"操作并消费点击事件，导致 clicked() 不触发。
                // 改用原始输入检测：button_pressed + interact_pos（点击瞬间位置），
                // 再通过 galley 将屏幕坐标转换为字符索引，找到所在行。
                let ctrl_click = ui.input(|i| {
                    i.pointer.button_pressed(egui::PointerButton::Primary) && i.modifiers.ctrl
                });
                if ctrl_click {
                    if let Some(click_pos) = ui.input(|i| i.pointer.interact_pos()) {
                        if output.response.rect.contains(click_pos) {
                            // 将屏幕坐标转为文本局部坐标，再用 galley 找到字符索引。
                            let local = click_pos - output.galley_pos;
                            let cursor = output.galley.cursor_from_pos(local);
                            let ci = cursor.ccursor.index;
                            let ci_clamped = ci.min(self.editor_content.len());
                            let line_start = self.editor_content[..ci_clamped]
                                .rfind('\n')
                                .map(|p| p + 1)
                                .unwrap_or(0);
                            let line_end = self.editor_content[ci_clamped..]
                                .find('\n')
                                .map(|p| ci_clamped + p)
                                .unwrap_or(self.editor_content.len());
                            let line = &self.editor_content[line_start..line_end];
                            // 尝试解析为立绘指令（+ ...）。
                            if let Some(preview) = parse_sprite_line(line) {
                                self.status = format!(
                                    "已跳转到立绘预览：{} ({})",
                                    preview.character_name, preview.selected
                                );
                                self.sprite_preview.character_name = preview.character_name;
                                self.sprite_preview.selected = preview.selected;
                                self.sprite_preview.x_percent = preview.x;
                                self.sprite_preview.y_percent = preview.y;
                                self.sprite_preview.scale = preview.scale;
                                self.sprite_preview.load_error = None;
                                self.preview_tab = PreviewTab::Sprite;
                            }
                            // 尝试解析为命令行（@bg / @music ...）。
                            else if let Some(cmd) = parse_command_line(line) {
                                match cmd.kind.as_str() {
                                    "bg" => {
                                        self.bg_preview.selected = cmd.target.clone();
                                        self.bg_preview.load_error = None;
                                        self.preview_tab = PreviewTab::Background;
                                        self.status = format!(
                                            "已跳转到背景预览：{}",
                                            cmd.target
                                        );
                                    }
                                    "music" => {
                                        self.music_preview.selected = cmd.target.clone();
                                        self.preview_tab = PreviewTab::Music;
                                        self.status = format!(
                                            "已跳转到音乐预览：{}",
                                            cmd.target
                                        );
                                    }
                                    _ => {}
                                }
                            }
                        }
                    }
            }
        });

        // ---- 撤销/重做历史追踪 ----
        // 检测 editor_content 相对 last_committed 的变化：
        // - 有变化：标记 dirty 并刷新 last_edit_time（连续输入会不断刷新，从而合并为一条历史）。
        // - 空闲超过 UNDO_IDLE_SECS：自动提交，把上一个稳定状态压入撤销栈。
        // undo/redo 本身修改了 editor_content，本帧通过 undo_redo_in_progress 跳过检测，
        // 并在帧末复位该标志（下一帧恢复正常追踪）。
        let now = ui.input(|i| i.time);
        if self.undo_redo_in_progress {
            self.undo_redo_in_progress = false;
        } else {
            if self.editor_content != self.last_committed {
                if !self.edit_dirty {
                    self.edit_dirty = true;
                }
                self.last_edit_time = now;
            }
            if self.edit_dirty && now - self.last_edit_time > Self::UNDO_IDLE_SECS {
                self.commit_history();
            }
        }
    }

    /// 切换蓝图模式与文本模式。
    ///
    /// 切换到蓝图时从当前脚本导入节点；切换回文本时将蓝图导出为脚本。
    /// 首次用蓝图模式打开某剧本文件时自动整理布局（持久化记录，之后不再自动整理，
    /// 避免打乱用户手动调整过的布局）。
    fn toggle_blueprint_mode(&mut self) {
        if self.blueprint_mode {
            // 离开蓝图模式：导出脚本，并检查孤立注释（未连线的注释无法迁移到脚本）。
            let orphans = self.blueprint.orphan_comments();
            let script = self.blueprint.to_script();
            if !script.is_empty() {
                self.editor_content = script;
            }
            if !orphans.is_empty() {
                self.comment_warning_list = orphans;
                self.show_comment_warning = true;
            }
            self.blueprint_mode = false;
            self.status = "已切换到文本模式".to_string();
        } else {
            // 未打开/新建剧本（内容为空）时不允许进入蓝图模式。
            if self.editor_content.trim().is_empty() {
                self.status = "请先打开或新建剧本再进入蓝图模式".to_string();
                return;
            }
            self.blueprint.from_script(&self.editor_content);
            // 首次进入该文件的蓝图模式时自动整理布局。
            if let Some(file_path) = &self.current_file {
                if !self.blueprint_layout_done.is_done(file_path) {
                    self.blueprint.auto_layout();
                    self.blueprint_layout_done.mark_done(file_path);
                    self.status = "已切换到蓝图模式（首次自动整理布局）".to_string();
                } else {
                    self.status = "已切换到蓝图模式".to_string();
                }
            } else {
                // 未保存的新文件也整理一次（但不持久化标记）。
                self.blueprint.auto_layout();
                self.status = "已切换到蓝图模式".to_string();
            }
            self.blueprint_mode = true;
        }
    }

    /// 从脚本中解析所有 `#` 开头的章节名（取 `#` 后第一个空白分隔 token）。
    /// 用于跳转/访问节点的章节下拉选择。
    fn section_names_from_script(text: &str) -> Vec<String> {
        text.lines()
            .filter_map(|l| {
                let t = l.trim();
                t.strip_prefix('#')
                    .map(|r| r.split_whitespace().next().unwrap_or("").to_string())
            })
            .filter(|s| !s.is_empty())
            .collect()
    }

    /// 根据当前蓝图编辑状态重新生成脚本文本，写回指定节点并刷新其类型。
    /// 在填空式编辑器中各控件值变化时调用。
    fn bp_writeback(&mut self, edit_id: usize) {
        let new_text = self.bp_edit.generate_text();
        if let Some(node) = self.blueprint.nodes.iter_mut().find(|n| n.id == edit_id) {
            node.text = new_text;
            node.kind = BlueprintState::detect_kind(&node.text);
        }
    }

    /// 通用文本兜底编辑器：对节点原始文本进行多行 TextEdit（保留原有编辑行为）。
    /// 用于章节/选择/选项/等待/结局/变量/注释等未提供填空式编辑器的节点类型。
    /// 面板为固定尺寸 UI，字号固定 13.0（不随 zoom 缩放）。
    fn bp_edit_textedit_fallback(
        &mut self,
        ui: &mut egui::Ui,
        edit_id: usize,
        body_rect: egui::Rect,
        _zoom: f32,
    ) {
        if let Some(node) = self.blueprint.nodes.iter_mut().find(|n| n.id == edit_id) {
            let resp = ui.add(
                egui::TextEdit::multiline(&mut node.text)
                    .desired_width(body_rect.width())
                    .desired_rows(4)
                    .font(egui::FontId::monospace(13.0)),
            );
            // 编辑后重新推断类型
            if resp.changed() {
                node.kind = BlueprintState::detect_kind(&node.text);
            }
            // 失焦或 Enter 退出编辑（Shift+Enter 换行）
            if resp.lost_focus()
                && ui.input(|i| i.key_pressed(egui::Key::Enter))
                && !ui.input(|i| i.modifiers.shift)
            {
                self.blueprint.editing_node = None;
            }
        }
    }

    /// 蓝图模式画布：可视化节点编辑。
    ///
    /// 交互方式（参考 UE 蓝图，精简版）：
    /// - **左键拖动节点**：移动节点位置
    /// - **左键点击节点**：选中（底部出现文本编辑栏）
    /// - **左键拖动输出引脚→输入引脚**：创建连线
    /// - **右键拖动空白**：平移画布
    /// - **右键点击空白**（不拖动）：弹出添加节点菜单
    /// - **Delete 键**：删除选中节点
    /// - **Escape 键**：关闭右键菜单
    fn show_blueprint(&mut self, ui: &mut egui::Ui) {
        // ---- 工具栏 ----
        ui.horizontal_wrapped(|ui| {
            if ui.button("从脚本导入").clicked() {
                self.blueprint.from_script(&self.editor_content);
                self.status = "已从脚本导入蓝图".to_string();
            }
            if ui.button("生成脚本").clicked() {
                let orphans = self.blueprint.orphan_comments();
                self.editor_content = self.blueprint.to_script();
                self.status = "已从蓝图生成脚本".to_string();
                if !orphans.is_empty() {
                    self.comment_warning_list = orphans;
                    self.show_comment_warning = true;
                }
            }
            if ui.button("整理布局").clicked() {
                self.blueprint.auto_layout();
                self.status = "已整理布局".to_string();
            }
            if ui.button("清空").clicked() {
                self.blueprint.nodes.clear();
                self.blueprint.links.clear();
                self.blueprint.selected = None;
                self.blueprint.selected_set.clear();
                self.blueprint.next_id = 0;
                self.status = "已清空蓝图".to_string();
            }
            ui.separator();
            // 操作提示：触摸模式与鼠标模式显示不同提示。
            // 触摸模式检测到 Event::Touch 后置 touch_mode=true（粘性），此时引脚放大、
            // 空白处单指拖拽平移画布、双指捏合缩放。
            if self.blueprint.touch_mode {
                ui.label(
                    egui::RichText::new("🖥 触摸模式：单指拖节点=移动 | 单指拖空白=平移画布 | 拖引脚=连线 | 双指捏合=缩放")
                        .size(12.0)
                        .color(egui::Color32::from_rgb(120, 200, 255)),
                );
            } else {
                ui.label(
                    egui::RichText::new("右键拖动=平移 | 左键拖动=移动 | 空白拖拽=框选多选 | 拖引脚=连线 | Del=删除")
                        .size(12.0)
                        .color(egui::Color32::from_rgb(150, 150, 160)),
                );
            }
        });
        ui.separator();

        // ---- 画布区域 ----
        let available = ui.available_size();
        let text_edit_h = 100.0;
        let canvas_h = (available.y - text_edit_h).max(100.0);
        let (canvas_rect, _) = ui.allocate_exact_size(
            egui::Vec2::new(available.x, canvas_h),
            egui::Sense::click_and_drag(),
        );
        let painter = ui.painter();
        let pan = self.blueprint.pan;
        let zoom = self.blueprint.zoom.max(0.2).min(3.0);

        // 背景
        painter.rect_filled(canvas_rect, 0.0, egui::Color32::from_rgb(18, 18, 26));

        // 网格点（随缩放调整间距，下限避免过密）
        let grid_size = (40.0 * zoom).max(8.0);
        let start_x = canvas_rect.left() + pan.x.rem_euclid(grid_size);
        let start_y = canvas_rect.top() + pan.y.rem_euclid(grid_size);
        let mut gx = start_x;
        while gx < canvas_rect.right() {
            let mut gy = start_y;
            while gy < canvas_rect.bottom() {
                painter.circle_filled(
                    egui::pos2(gx, gy),
                    1.0,
                    egui::Color32::from_rgb(45, 45, 58),
                );
                gy += grid_size;
            }
            gx += grid_size;
        }

        // ---- 读取输入 ----
        // 优先用 interact_pos()：触摸释放时 hover_pos() 会返回 None 或跳到 (0,0)，
        // 导致拖拽预览飘走、松手位置判定失效（节点无法放置）。
        // interact_pos() 在触摸拖拽/释放期间返回稳定的触点位置。
        let mouse_pos = ui
            .input(|i| i.pointer.interact_pos().or_else(|| i.pointer.hover_pos()))
            .unwrap_or(egui::Pos2::ZERO);
        let primary_pressed =
            ui.input(|i| i.pointer.button_pressed(egui::PointerButton::Primary));
        let primary_down =
            ui.input(|i| i.pointer.button_down(egui::PointerButton::Primary));
        let primary_released =
            ui.input(|i| i.pointer.button_released(egui::PointerButton::Primary));
        let secondary_pressed =
            ui.input(|i| i.pointer.button_pressed(egui::PointerButton::Secondary));
        let secondary_down =
            ui.input(|i| i.pointer.button_down(egui::PointerButton::Secondary));
        let secondary_released =
            ui.input(|i| i.pointer.button_released(egui::PointerButton::Secondary));
        let delta = ui.input(|i| i.pointer.delta());
        let in_canvas = canvas_rect.contains(mouse_pos);
        // 画布逻辑坐标 = (屏幕坐标 - 画布原点 - 平移偏移) / 缩放
        // 节点 pos/size 均为逻辑坐标，命中测试在此空间进行。
        // 注意：Pos2 - Pos2 = Vec2，需转回 Pos2 以匹配节点命中测试等接口。
        let canvas_pos = ((mouse_pos - canvas_rect.min - pan) / zoom).to_pos2();

        // ---- 平板触摸手势（Windows/Linux 平板双指）----
        // egui 0.21 把单指触摸映射为 pointer 事件（单指点击/拖拽等价鼠标，单指拖节点已可用）；
        // ≥2 指时聚合为 MultiTouchInfo。
        // 触摸模式检测：扫描 i.events 中的 Event::Touch（触摸屏会在 pointer 事件之外
        // 额外发送 Event::Touch）。检测到后置 touch_mode=true（粘性，本会话内保持）。
        let has_touch_event = ui.input(|i| {
            i.events
                .iter()
                .any(|e| matches!(e, egui::Event::Touch { .. }))
        });
        if has_touch_event {
            self.blueprint.touch_mode = true;
        }
        let touch_mode = self.blueprint.touch_mode;
        // 引脚半径：触摸模式下放大（渲染与命中测试共用），便于手指点按。
        let pin_r = if touch_mode { 9.0 } else { 5.0 };
        let pin_hit_r = if touch_mode { 16.0 } else { 8.0 };

        // 双指捏合只控制缩放（不再用 translation_delta 平移画布——
        // 平移改由单指拖空白处完成，与右栏空白处拖拽滚动一致）。
        // 双指手势进行时抑制单指节点拖拽，避免冲突。
        let multi_touch = ui.input(|i| i.multi_touch());
        let mut touch_active = false;
        if let Some(mt) = multi_touch {
            if in_canvas
                || canvas_rect
                    .intersects(egui::Rect::from_center_size(mt.start_pos, egui::Vec2::splat(1.0)))
            {
                touch_active = true;
                let bp = &mut self.blueprint;
                // 双指捏合缩放：以双指起始中点（手势起点）为锚点缩放，
                // 让双指之间的内容在缩放后仍大致保持在手指下方。
                let new_zoom = (bp.zoom * mt.zoom_delta).max(0.2).min(3.0);
                if (new_zoom - bp.zoom).abs() > 1e-4 {
                    let anchor_pos = mt.start_pos;
                    let anchor =
                        ((anchor_pos - canvas_rect.min - bp.pan) / bp.zoom).to_pos2();
                    bp.zoom = new_zoom;
                    bp.pan = anchor_pos - canvas_rect.min - anchor.to_vec2() * new_zoom;
                }
            }
        }
        // Ctrl+滚轮缩放（桌面端）：egui 的 zoom_delta() 在 Ctrl 按下滚动时返回缩放因子
        if !touch_active {
            let zd = ui.input(|i| i.zoom_delta());
            if (zd - 1.0).abs() > 1e-4 && in_canvas {
                let bp = &mut self.blueprint;
                let new_zoom = (bp.zoom * zd).max(0.2).min(3.0);
                if (new_zoom - bp.zoom).abs() > 1e-4 {
                    // 以鼠标位置为锚点缩放
                    let anchor = ((mouse_pos - canvas_rect.min - bp.pan) / bp.zoom).to_pos2();
                    bp.zoom = new_zoom;
                    bp.pan = mouse_pos - canvas_rect.min - anchor.to_vec2() * new_zoom;
                }
            }
        }

        // ---- 处理交互（可变借用 self.blueprint）----
        // 预读编辑面板矩形与框选标记到局部，避免在 bp 借用期间访问 self 其他字段。
        let panel_rect = self.bp_edit_panel_rect;
        let mut start_box_select = false;
        {
            let bp = &mut self.blueprint;

            // 左键按下：选择 / 拖动 / 开始连线 / 触摸模式空白处开始平移 / 非触摸空白开始框选
            // 双指手势进行时抑制，避免与触摸缩放冲突。
            if primary_pressed && in_canvas && bp.context_menu_pos.is_none() && !touch_active {
                // 优先检测输出引脚（开始连线）
                if let Some(from_id) = bp.output_pin_at(canvas_pos, pin_hit_r) {
                    bp.connecting_from = Some(from_id);
                    bp.connecting_pos = canvas_pos;
                } else if let Some(node_id) = bp.node_at(canvas_pos) {
                    // 选中并准备拖动
                    let node_pos = bp
                        .nodes
                        .iter()
                        .find(|n| n.id == node_id)
                        .map(|n| n.pos)
                        .unwrap_or_default();
                    bp.drag_node = Some(node_id);
                    bp.drag_offset = canvas_pos - node_pos;
                    bp.selected = Some(node_id);
                    // 多选：若点击的节点已在 selected_set 中，保留集合（可整体拖动）；
                    // 否则单选该节点，清空旧集合。
                    if !bp.selected_set.contains(&node_id) {
                        bp.selected_set.clear();
                        bp.selected_set.insert(node_id);
                    }
                } else {
                    // 点击空白：取消选中
                    bp.selected = None;
                    // 触摸模式下空白处按下开始平移画布（与右栏空白处拖拽滚动一致）。
                    // 非触摸模式下空白处按下开始框选多选。
                    if touch_mode {
                        bp.touch_panning = true;
                    } else {
                        start_box_select = true;
                    }
                }
            }

            // 左键持续按下：拖动节点 / 更新连线位置 / 触摸平移
            // 双指手势进行时抑制。框选进行中不拖动节点（drag_node 为 None）。
            if primary_down && !touch_active {
                if let Some(node_id) = bp.drag_node {
                    // 多选拖动：若拖动节点在 selected_set 中且集合不止一个，整体平移；
                    // 否则仅移动该节点。
                    if bp.selected_set.contains(&node_id) && bp.selected_set.len() > 1 {
                        if let Some(cur) =
                            bp.nodes.iter().find(|n| n.id == node_id).map(|n| n.pos)
                        {
                            let new_pos = canvas_pos - bp.drag_offset;
                            let delta_move = new_pos - cur;
                            for n in bp.nodes.iter_mut() {
                                if bp.selected_set.contains(&n.id) {
                                    n.pos += delta_move;
                                }
                            }
                        }
                    } else if let Some(node) = bp.nodes.iter_mut().find(|n| n.id == node_id) {
                        node.pos = canvas_pos - bp.drag_offset;
                    }
                }
                if bp.connecting_from.is_some() {
                    bp.connecting_pos = canvas_pos;
                }
                // 触摸模式空白处拖拽平移画布（drag_node/connecting_from 均为 None 时）。
                if bp.touch_panning {
                    bp.pan += delta;
                }
            }

            // 左键释放：完成连线 / 结束触摸平移
            if primary_released {
                if let Some(from_id) = bp.connecting_from {
                    if let Some(to_id) = bp.input_pin_at(canvas_pos, pin_hit_r) {
                        bp.add_link(from_id, to_id);
                    }
                    bp.connecting_from = None;
                }
                bp.drag_node = None;
                bp.touch_panning = false;
            }

            // 右键按下：记录起点
            if secondary_pressed && in_canvas {
                bp.right_press_pos = Some(mouse_pos);
                bp.right_moved = false;
            }
            // 右键持续按下：平移
            // 双指手势进行时抑制（触摸平移由 MultiTouchInfo 处理）。
            if secondary_down && !touch_active {
                if let Some(start) = bp.right_press_pos {
                    if (mouse_pos - start).length() > 4.0 {
                        bp.right_moved = true;
                    }
                    if bp.right_moved {
                        bp.pan += delta;
                        // 右键拖动平移时显示拖拽光标
                        ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
                    }
                }
            }
            // 右键释放：若未拖动则弹出右键菜单
            if secondary_released {
                if !bp.right_moved && in_canvas {
                    bp.context_menu_pos = Some(mouse_pos);
                }
                bp.right_press_pos = None;
                bp.right_moved = false;
            }

            // Delete 键：删除选中节点（优先多选集合，回退到单选）
            if ui.input(|i| i.key_pressed(egui::Key::Delete)) {
                if !bp.selected_set.is_empty() {
                    let to_remove: Vec<usize> = bp.selected_set.iter().copied().collect();
                    for id in to_remove {
                        bp.remove_node(id);
                    }
                    bp.selected_set.clear();
                } else if let Some(id) = bp.selected {
                    bp.remove_node(id);
                }
            }
            // Escape：关闭右键菜单 / 退出编辑
            if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                bp.context_menu_pos = None;
                bp.editing_node = None;
            }
            // 双击节点：进入就地编辑模式
            let double_clicked = ui.input(|i| i.pointer.button_double_clicked(egui::PointerButton::Primary));
            if double_clicked && in_canvas && bp.context_menu_pos.is_none() {
                if let Some(node_id) = bp.node_at(canvas_pos) {
                    bp.editing_node = Some(node_id);
                }
            }
            // 点击空白处退出编辑模式（点击编辑面板内部不退出）
            if primary_pressed && in_canvas && bp.editing_node.is_some() {
                if bp.node_at(canvas_pos).is_none() {
                    let in_panel = panel_rect
                        .map(|r| r.contains(mouse_pos))
                        .unwrap_or(false);
                    if !in_panel {
                        bp.editing_node = None;
                    }
                }
            }
        }

        // ---- 框选多选（非触摸模式）----
        // 起点已在上面交互块中通过 start_box_select 标记，此处写入 self 字段。
        if start_box_select {
            self.box_select_start = Some(canvas_pos);
            self.box_select_end = Some(canvas_pos);
            self.blueprint.selected_set.clear();
        }
        // 框选拖动：更新终点
        if primary_down && self.box_select_start.is_some() && !touch_active {
            self.box_select_end = Some(canvas_pos);
        }
        // 框选释放：把中心位于矩形内的节点加入 selected_set
        if primary_released && self.box_select_start.is_some() {
            if let (Some(start), Some(end)) = (self.box_select_start, self.box_select_end) {
                let rect = egui::Rect::from_two_pos(start, end);
                for n in &self.blueprint.nodes {
                    let size = BlueprintState::node_size(&n.text);
                    let center = egui::pos2(n.pos.x + size.x / 2.0, n.pos.y + size.y / 2.0);
                    if rect.contains(center) {
                        self.blueprint.selected_set.insert(n.id);
                    }
                }
            }
            self.box_select_start = None;
            self.box_select_end = None;
        }

        // ---- 拖拽放置（从积木面板拖出模板，在画布上释放即添加节点）----
        if self.drag_template.is_some() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
            if let Some(idx) = self.drag_template {
                let (label, desc, template) = NODE_TEMPLATES[idx];
                let kind = BlueprintState::detect_kind(template);
                let color = BlueprintState::kind_color(kind);
                // 浮动预览跟随光标：所见即所得——直接渲染积木卡片本身（与面板里完全一致），
                // 而非仅显示标签文字。半透明以区别于已放置的节点。
                egui::Area::new(egui::Id::new("bp_drag_preview"))
                    .order(egui::Order::Foreground)
                    .fixed_pos(mouse_pos + egui::vec2(12.0, 12.0))
                    .interactable(false)
                    .show(ui.ctx(), |ui| {
                        block_card_frame(ui, color, 0.0)
                            .fill(color.linear_multiply(0.25))
                            .show(ui, |ui| {
                                paint_block_card(ui, label, desc, template, color);
                            });
                    });
                // 释放：在画布内添加节点，否则取消
                if primary_released {
                    if in_canvas {
                        // 选择模板特殊处理：生成 Choice + 2 个 Option + 连线
                        if kind == NodeKind::Choice {
                            let choice_id = self.blueprint.add_node(
                                NodeKind::Choice,
                                canvas_pos,
                                template.to_string(),
                            );
                            // 两个选项节点放在 Choice 下方
                            let size = BlueprintState::node_size(template);
                            let opt1_pos = egui::pos2(canvas_pos.x, canvas_pos.y + size.y + 24.0);
                            let opt2_pos = egui::pos2(canvas_pos.x, opt1_pos.y + size.y + 24.0);
                            let opt1_id = self.blueprint.add_node(
                                NodeKind::ChoiceOption,
                                opt1_pos,
                                "| 选项A".to_string(),
                            );
                            let opt2_id = self.blueprint.add_node(
                                NodeKind::ChoiceOption,
                                opt2_pos,
                                "| 选项B".to_string(),
                            );
                            // Choice → 选项A → 选项B（顺序连线）
                            self.blueprint.links.push(BlueprintLink { from: choice_id, to: opt1_id });
                            self.blueprint.links.push(BlueprintLink { from: opt1_id, to: opt2_id });
                            self.status = "已添加选择块（含2个选项）".to_string();
                        } else {
                            self.blueprint.add_node(kind, canvas_pos, template.to_string());
                            self.status = format!("已添加节点：{}", label);
                        }
                    }
                    self.drag_template = None;
                }
            }
        }

        // ---- 拖拽放置（从预览面板拖出生成的语法，在画布上释放即添加节点）----
        if self.drag_syntax.is_some() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
            if let Some(syntax) = &self.drag_syntax {
                let kind = BlueprintState::detect_kind(syntax);
                let color = BlueprintState::kind_color(kind);
                let display = BlueprintState::node_display_text(syntax, kind);
                let label = BlueprintState::node_label(syntax, kind);
                // 浮动预览跟随光标：半透明积木卡片。
                egui::Area::new(egui::Id::new("bp_drag_syntax_preview"))
                    .order(egui::Order::Foreground)
                    .fixed_pos(mouse_pos + egui::vec2(12.0, 12.0))
                    .interactable(false)
                    .show(ui.ctx(), |ui| {
                        block_card_frame(ui, color, 0.0)
                            .fill(color.linear_multiply(0.25))
                            .show(ui, |ui| {
                                ui.horizontal(|ui| {
                                    let (rect, _) = ui.allocate_exact_size(
                                        egui::Vec2::new(10.0, 10.0),
                                        egui::Sense::hover(),
                                    );
                                    ui.painter().rect_filled(rect, 2.0, color);
                                    ui.label(egui::RichText::new(&label).strong().color(color));
                                });
                                ui.label(
                                    egui::RichText::new(&display)
                                        .color(egui::Color32::from_rgb(200, 210, 220)),
                                );
                            });
                    });
                // 释放：在画布内添加节点，否则取消
                if primary_released {
                    if in_canvas {
                        let pos = self.blueprint.next_placement_pos();
                        self.blueprint.add_node(kind, canvas_pos, syntax.clone());
                        let _ = pos; // 用 canvas_pos 而非 next_placement_pos
                        self.status = format!("已添加节点：{}", label);
                    }
                    self.drag_syntax = None;
                }
            }
        }

        // ---- 绘制连线 ----（不可变借用 self.blueprint + painter）
        // 折叠注释时把经过注释的连线桥接为非注释节点之间的直连，避免视觉断开。
        let bp = &self.blueprint;
        let clipped = painter.with_clip_rect(canvas_rect);
        let collapse = bp.collapse_comments;
        // 桥接后的去重连线集合。
        let mut drawn: std::collections::HashSet<(usize, usize)> = std::collections::HashSet::new();
        for link in &bp.links {
            let (from_id, to_id) = if collapse {
                match (bp.resolve_from(link.from), bp.resolve_to(link.to)) {
                    (Some(f), Some(t)) => (f, t),
                    _ => continue,
                }
            } else {
                (link.from, link.to)
            };
            if from_id == to_id {
                continue;
            }
            if !drawn.insert((from_id, to_id)) {
                continue;
            }
            let from_node = bp.nodes.iter().find(|n| n.id == from_id);
            let to_node = bp.nodes.iter().find(|n| n.id == to_id);
            if let (Some(from), Some(to)) = (from_node, to_node) {
                let from_size_logic = BlueprintState::node_size(&from.text);
                let to_size_logic = BlueprintState::node_size(&to.text);
                // 逻辑坐标下的引脚位置（源节点底部中心、目标节点顶部中心）
                let from_pin_logic = egui::pos2(
                    from.pos.x + from_size_logic.x / 2.0,
                    from.pos.y + from_size_logic.y,
                );
                let to_pin_logic =
                    egui::pos2(to.pos.x + to_size_logic.x / 2.0, to.pos.y);
                // 逻辑坐标转屏幕坐标
                let to_screen = |p: egui::Pos2| {
                    egui::pos2(
                        p.x * zoom + pan.x + canvas_rect.min.x,
                        p.y * zoom + pan.y + canvas_rect.min.y,
                    )
                };
                // 跨章节连线用贝塞尔曲线（绕开所有节点），章节内连线用正交折线 route_link
                if bp.is_cross_section_link(from_id, to_id) {
                    // 跨章节：贝塞尔曲线，笔触稍粗、色调略浅以区分章节内连线
                    let [p0, p1, p2, p3] =
                        bp.route_bezier_around(from_pin_logic, to_pin_logic, &[from_id, to_id]);
                    clipped.add(egui::epaint::CubicBezierShape {
                        points: [to_screen(p0), to_screen(p1), to_screen(p2), to_screen(p3)],
                        closed: false,
                        fill: egui::Color32::TRANSPARENT,
                        stroke: egui::Stroke::new(
                            2.5 * zoom,
                            egui::Color32::from_rgb(140, 180, 235),
                        )
                        .into(),
                    });
                } else {
                    // 章节内：正交折线 route_link（绕开中间节点，排除源/目标节点本身）
                    let path = bp.route_link(from_pin_logic, to_pin_logic, &[from_id, to_id]);
                    let screen_path: Vec<egui::Pos2> =
                        path.iter().map(|p| to_screen(*p)).collect();
                    clipped.add(egui::epaint::PathShape {
                        points: screen_path,
                        closed: false,
                        fill: egui::Color32::TRANSPARENT,
                        stroke: egui::Stroke::new(
                            2.0 * zoom,
                            egui::Color32::from_rgb(120, 160, 220),
                        )
                        .into(),
                    });
                }
            }
        }

        // 绘制正在拉出的连线
        if let Some(from_id) = bp.connecting_from {
            if let Some(from) = bp.nodes.iter().find(|n| n.id == from_id) {
                let from_size_logic = BlueprintState::node_size(&from.text);
                // 逻辑坐标下的源引脚（节点底部中心）与目标点（鼠标位置）
                let from_pin_logic = egui::pos2(
                    from.pos.x + from_size_logic.x / 2.0,
                    from.pos.y + from_size_logic.y,
                );
                let to_pin_logic = bp.connecting_pos;
                // 正交路由：绕开中间节点（排除源节点本身）
                let path = bp.route_link(from_pin_logic, to_pin_logic, &[from_id]);
                // 逻辑坐标转屏幕坐标
                let screen_path: Vec<egui::Pos2> = path
                    .iter()
                    .map(|p| {
                        egui::pos2(
                            p.x * zoom + pan.x + canvas_rect.min.x,
                            p.y * zoom + pan.y + canvas_rect.min.y,
                        )
                    })
                    .collect();
                clipped.add(egui::epaint::PathShape {
                    points: screen_path,
                    closed: false,
                    fill: egui::Color32::TRANSPARENT,
                    stroke: egui::Stroke::new(
                        2.0 * zoom,
                        egui::Color32::from_rgb(255, 200, 80),
                    )
                    .into(),
                });
            }
        }

        // ---- 绘制节点 ----
        for node in &bp.nodes {
            // 折叠注释时不作为独立节点绘制（已隐藏到下一个非注释积木）。
            if collapse && node.kind == NodeKind::Comment {
                continue;
            }
            let size = BlueprintState::node_size(&node.text) * zoom;
            let screen_pos = egui::pos2(
                node.pos.x * zoom + pan.x + canvas_rect.min.x,
                node.pos.y * zoom + pan.y + canvas_rect.min.y,
            );
            let rect = egui::Rect::from_min_size(screen_pos, size);
            let color = BlueprintState::kind_color(node.kind);
            let is_selected = bp.selected == Some(node.id) || bp.selected_set.contains(&node.id);

            // 节点背景：半透明色填充（与积木卡片一致），圆角。
            clipped.rect_filled(rect, 4.0 * zoom, color.linear_multiply(0.15));
            // 彩色边框（节点本身的「方框」）。
            clipped.rect_stroke(
                rect,
                4.0 * zoom,
                egui::Stroke::new(1.5 * zoom, color),
            );
            // 选中时叠加黄色加粗边框。
            if is_selected {
                clipped.rect_stroke(
                    rect,
                    4.0 * zoom,
                    egui::Stroke::new(2.5 * zoom, egui::Color32::from_rgb(255, 220, 80)),
                );
            }
            // 左上色块标识类型（与积木卡片同款）。
            let header_h = 22.0 * zoom;
            let swatch_size = 10.0 * zoom;
            let pad = 8.0 * zoom;
            let swatch_rect = egui::Rect::from_min_size(
                egui::pos2(screen_pos.x + pad, screen_pos.y + (header_h - swatch_size) * 0.5),
                egui::Vec2::new(swatch_size, swatch_size),
            );
            clipped.rect_filled(swatch_rect, 2.0 * zoom, color);
            // 标题文字：强调色，左对齐于色块右侧。
            let label = BlueprintState::node_label(&node.text, node.kind);
            clipped.text(
                egui::pos2(swatch_rect.right() + 6.0 * zoom, screen_pos.y + header_h * 0.5),
                egui::Align2::LEFT_CENTER,
                &label,
                egui::FontId::proportional(13.0 * zoom),
                color,
            );
            // 折叠注释时：若该积木吸附了注释，在标题栏右侧绘制注释标记，并支持悬停查看。
            if collapse {
                let comments = bp.comments_for_node(node.id);
                if !comments.is_empty() {
                    let badge_text = format!("注{}", comments.len());
                    let badge_pos = egui::pos2(rect.right() - pad, screen_pos.y + header_h * 0.5);
                    clipped.text(
                        badge_pos,
                        egui::Align2::RIGHT_CENTER,
                        &badge_text,
                        egui::FontId::proportional(10.0 * zoom),
                        egui::Color32::from_rgb(255, 230, 120),
                    );
                    // 悬停显示注释全文（用不可见交互区域承载 tooltip）。
                    let badge_rect = egui::Rect::from_min_size(
                        egui::pos2(rect.right() - 40.0 * zoom, screen_pos.y),
                        egui::Vec2::new(40.0 * zoom, header_h),
                    );
                    let tooltip = comments.join("\n");
                    ui.interact(
                        badge_rect,
                        egui::Id::new(("bp_comment_badge", node.id)),
                        egui::Sense::hover(),
                    )
                    .on_hover_text(&tooltip);
                }
            }
            // 正文：若该节点正在编辑则跳过（稍后用 TextEdit 覆盖）。
            // 否则用 galley 自动换行布局，配合 clip_rect 裁剪超出固定高度的部分。
            // 显示友好文本（去掉脚本前缀符号），不暴露代码语法。
            if bp.editing_node != Some(node.id) {
                let body_w = (size.x - 2.0 * pad).max(10.0);
                let body_h = (size.y - header_h - 8.0 * zoom).max(10.0);
                let body_pos = egui::pos2(screen_pos.x + pad, screen_pos.y + header_h + 4.0 * zoom);
                let body_rect = egui::Rect::from_min_size(body_pos, egui::Vec2::new(body_w, body_h));
                let display = BlueprintState::node_display_text(&node.text, node.kind);
                let font = egui::FontId::proportional(12.0 * zoom);
                let galley = ui.fonts(|f| {
                    f.layout(display, font, egui::Color32::from_rgb(200, 210, 220), body_w)
                });
                let body_painter = clipped.with_clip_rect(body_rect);
                body_painter.galley(body_pos, galley, egui::Color32::from_rgb(200, 210, 220));
            }
            // 输入引脚（顶部中心）——保留上下引脚用于连线。
            // 触摸模式下引脚半径放大（pin_r），便于手指点按。
            let in_pin = egui::pos2(rect.center().x, rect.top());
            clipped.circle_filled(in_pin, pin_r * zoom, egui::Color32::from_rgb(100, 180, 255));
            clipped.circle_stroke(
                in_pin,
                pin_r * zoom,
                egui::Stroke::new(1.5 * zoom, egui::Color32::from_rgb(60, 60, 80)),
            );
            // 输出引脚（底部中心）——结局/结局声明模块无输出引脚（流程终止）。
            if node.kind != NodeKind::StoryEnd && node.kind != NodeKind::Ending {
                let out_pin = egui::pos2(rect.center().x, rect.bottom());
                clipped.circle_filled(out_pin, pin_r * zoom, egui::Color32::from_rgb(255, 180, 100));
                clipped.circle_stroke(
                    out_pin,
                    pin_r * zoom,
                    egui::Stroke::new(1.5 * zoom, egui::Color32::from_rgb(60, 60, 80)),
                );
            }
        }

        // 空画布提示
        if bp.nodes.is_empty() {
            clipped.text(
                canvas_rect.center(),
                egui::Align2::CENTER_CENTER,
                "从左侧拖拽积木到画布，或右键空白处添加节点，或「从脚本导入」",
                egui::FontId::proportional(14.0),
                egui::Color32::from_rgb(120, 130, 150),
            );
        }

        // ---- 绘制框选矩形（非触摸模式下拖拽空白处时）----
        if let (Some(start), Some(end)) = (self.box_select_start, self.box_select_end) {
            // 逻辑坐标转屏幕坐标
            let s = egui::pos2(
                start.x * zoom + pan.x + canvas_rect.min.x,
                start.y * zoom + pan.y + canvas_rect.min.y,
            );
            let e = egui::pos2(
                end.x * zoom + pan.x + canvas_rect.min.x,
                end.y * zoom + pan.y + canvas_rect.min.y,
            );
            let sel_rect = egui::Rect::from_two_pos(s, e);
            clipped.rect_filled(
                sel_rect,
                0.0,
                egui::Color32::from_rgba_unmultiplied(100, 150, 255, 40),
            );
            clipped.rect_stroke(
                sel_rect,
                0.0,
                egui::Stroke::new(1.0, egui::Color32::from_rgb(100, 150, 255)),
            );
        }

        // ---- 就地编辑选中节点（双击进入，按节点类型显示填空式编辑器）----
        // 面板在节点附近就地显示：默认在节点右侧，空间不足时放左侧；
        // 宽度按当前节点类型的最宽 ComboBox 选项自适应；底部超出画布时上移避让。
        // 点击面板内部不会取消编辑。
        if let Some(edit_id) = self.blueprint.editing_node {
            // 切换编辑节点时（双击新节点 / 重新进入），从节点文本重新解析填充编辑状态
            if self.bp_edit_id != Some(edit_id) {
                if let Some(node) = self.blueprint.nodes.iter().find(|n| n.id == edit_id) {
                    self.bp_edit.init_from(&node.text, node.kind);
                    self.bp_edit_id = Some(edit_id);
                }
            }
            // 获取节点位置与文本（拷贝/克隆，尽早结束对 self.blueprint 的借用，
            // 以便后续在面板闭包内调用 &mut self 方法 bp_writeback 等）
            let node_data = self
                .blueprint
                .nodes
                .iter()
                .find(|n| n.id == edit_id)
                .map(|n| (n.pos, n.text.clone()));
            if let Some((node_pos, node_text)) = node_data {
                // 节点屏幕位置与尺寸（与节点绘制公式一致）
                let node_size_screen = BlueprintState::node_size(&node_text) * zoom;
                let node_screen_pos = egui::pos2(
                    node_pos.x * zoom + pan.x + canvas_rect.min.x,
                    node_pos.y * zoom + pan.y + canvas_rect.min.y,
                );
                // 准备资源列表（克隆避免借用冲突）
                let sprites = self.sprite_preview.available.clone();
                let bgs = self.bg_preview.available.clone();
                let musics = self.music_preview.available.clone();
                let sections = Self::section_names_from_script(&self.editor_content);
                let transitions = [
                    "fade", "fade_black", "fade_white", "dissolve",
                    "slide_left", "slide_right", "slide_up", "slide_down",
                    "wipe_left", "wipe_right", "blur", "instant",
                ];
                // 立绘下场角色名推断：从上游节点提取（克隆为 owned 避免借用冲突）
                // - upstream_names：沿入线回溯找到的立绘上场角色名
                // - has_direct：是否有上游连接（用于不可用位置警告）
                // - all_sprite_names_canvas：全画布所有立绘上场角色名（回退/标注在用）
                let (upstream_names, has_direct) =
                    self.blueprint.sprite_names_from_upstream(edit_id);
                let all_sprite_names_canvas = self.blueprint.all_sprite_names();
                // 面板宽度自适应：按当前节点类型会展示的 ComboBox 选项最宽项估算。
                // 用 painter.layout_no_wrap 测量文本像素宽，加上下拉按钮与内边距余量，
                // 限制在 [220, 360] 避免过窄或过宽。块作用域让 measure/借用提前结束。
                let panel_width = {
                    let font_id = egui::FontId::proportional(14.0);
                    let measure = |txt: &str| {
                        ui.painter()
                            .layout_no_wrap(txt.to_string(), font_id.clone(), egui::Color32::WHITE)
                            .size()
                            .x
                    };
                    // 选出当前编辑分支会用到的候选项（与下方 match 中 ComboBox 选项一致）
                    let candidates: Vec<String> = match self.bp_edit.kind {
                        NodeKind::Direction => {
                            let is_enter = self.bp_edit.text.trim_start().starts_with('+');
                            if is_enter {
                                sprites.clone()
                            } else if !upstream_names.is_empty() {
                                upstream_names.clone()
                            } else {
                                let mut v = all_sprite_names_canvas.clone();
                                for s in &sprites {
                                    if !v.contains(s) {
                                        v.push(s.clone());
                                    }
                                }
                                v
                            }
                        }
                        NodeKind::Command => {
                            let cmd: String = self
                                .bp_edit
                                .text
                                .trim_start()
                                .strip_prefix('@')
                                .unwrap_or("")
                                .trim_start()
                                .chars()
                                .take_while(|c| !c.is_whitespace())
                                .collect();
                            match cmd.as_str() {
                                "bg" => {
                                    let mut v = bgs.clone();
                                    for t in transitions {
                                        v.push(t.to_string());
                                    }
                                    v
                                }
                                "music" => musics.clone(),
                                _ => Vec::new(),
                            }
                        }
                        NodeKind::Flow | NodeKind::Visit => sections.clone(),
                        _ => Vec::new(),
                    };
                    let mut content_w = 0.0f32;
                    for c in &candidates {
                        let w = measure(c);
                        if w > content_w {
                            content_w = w;
                        }
                    }
                    (content_w + 56.0).clamp(220.0, 360.0)
                };
                // 面板内可用正文宽度（用于 ComboBox 宽度与 TextEdit desired_width）
                let inner_w = panel_width - 16.0;
                // 兜底编辑器用的 body_rect（仅用到宽度）
                let body_rect = egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::Vec2::new(inner_w, 80.0),
                );
                // 面板位置：默认在节点右侧；右侧空间不足则放左侧
                let panel_height_est = 120.0; // 估算高度，用于边界避让
                let right_space = canvas_rect.right() - (node_screen_pos.x + node_size_screen.x);
                let panel_x = if right_space >= panel_width + 10.0 {
                    node_screen_pos.x + node_size_screen.x + 10.0
                } else {
                    node_screen_pos.x - panel_width - 10.0
                };
                // 避让画布上下边界：面板底部超出则上移；上移后仍超出顶部则贴顶
                let mut panel_y = node_screen_pos.y;
                if panel_y + panel_height_est > canvas_rect.bottom() {
                    panel_y = canvas_rect.bottom() - panel_height_est - 10.0;
                }
                if panel_y < canvas_rect.top() {
                    panel_y = canvas_rect.top() + 10.0;
                }
                // 左侧不要溢出画布
                let panel_x = panel_x.max(canvas_rect.left() + 10.0);
                let panel_pos = egui::pos2(panel_x, panel_y);
                let panel_response = egui::Area::new(egui::Id::new("bp_edit_panel"))
                    .order(egui::Order::Foreground)
                    .fixed_pos(panel_pos)
                    .show(ui.ctx(), |ui| {
                        egui::Frame::popup(ui.style())
                            .show(ui, |ui| {
                                ui.set_min_width(panel_width);
                                ui.set_max_width(panel_width);
                                ui.vertical(|ui| {
                                    // 去掉重复的"编辑节点"小标题（节点标题栏已有）
                                    match self.bp_edit.kind {
                                NodeKind::Direction => {
                                    let is_enter = self.bp_edit.text.trim_start().starts_with('+');
                                    if is_enter {
                                        // 立绘上场：立绘名 + x/y/size（无小标题，节点标题栏已有）
                                        let before = self.bp_edit.sprite_name.clone();
                                        egui::ComboBox::from_id_salt("bp_edit_sprite_enter")
                                            .width(inner_w)
                                            .selected_text(if self.bp_edit.sprite_name.is_empty() {
                                                "（选择立绘）"
                                            } else {
                                                self.bp_edit.sprite_name.as_str()
                                            })
                                            .show_ui(ui, |ui| {
                                                for s in &sprites {
                                                    // 标注已在画布上使用的立绘
                                                    let label = if all_sprite_names_canvas.contains(s) {
                                                        format!("{}（在用）", s)
                                                    } else {
                                                        s.clone()
                                                    };
                                                    ui.selectable_value(
                                                        &mut self.bp_edit.sprite_name,
                                                        s.clone(),
                                                        label,
                                                    );
                                                }
                                            });
                                        ui.horizontal(|ui| {
                                            ui.label("x");
                                            let r1 = ui.add(
                                                egui::DragValue::new(&mut self.bp_edit.x_percent)
                                                    .range(0.0..=1.0)
                                                    .speed(0.01)
                                                    .fixed_decimals(2),
                                            );
                                            ui.label("y");
                                            let r2 = ui.add(
                                                egui::DragValue::new(&mut self.bp_edit.y_percent)
                                                    .range(0.0..=1.0)
                                                    .speed(0.01)
                                                    .fixed_decimals(2),
                                            );
                                            ui.label("size");
                                            let r3 = ui.add(
                                                egui::DragValue::new(&mut self.bp_edit.scale)
                                                    .range(0.1..=5.0)
                                                    .speed(0.01)
                                                    .fixed_decimals(2),
                                            );
                                            if self.bp_edit.sprite_name != before
                                                || r1.changed()
                                                || r2.changed()
                                                || r3.changed()
                                            {
                                                self.bp_writeback(edit_id);
                                            }
                                        });
                                    } else {
                                        // 立绘下场：角色名从上游推断（无小标题）
                                        // 上游找到立绘上场→用上游角色名；
                                        // 否则回退全画布角色名 + 资源文件列表
                                        let candidates: Vec<String> = if !upstream_names.is_empty() {
                                            upstream_names.clone()
                                        } else {
                                            let mut v = all_sprite_names_canvas.clone();
                                            for s in &sprites {
                                                if !v.contains(s) {
                                                    v.push(s.clone());
                                                }
                                            }
                                            v
                                        };
                                        // 上游有连接但未找到立绘上场：不可用位置警告 + 断开按钮
                                        if has_direct && upstream_names.is_empty() {
                                            ui.label(
                                                egui::RichText::new("警告：上游节点未找到立绘上场，请检查连线")
                                                    .color(egui::Color32::from_rgb(255, 150, 100)),
                                            );
                                            if ui.button("断开入线连接").clicked() {
                                                self.blueprint.links.retain(|l| l.to != edit_id);
                                            }
                                        }
                                        let before = self.bp_edit.sprite_name.clone();
                                        egui::ComboBox::from_id_salt("bp_edit_sprite_exit")
                                            .width(inner_w)
                                            .selected_text(if self.bp_edit.sprite_name.is_empty() {
                                                "（选择角色）"
                                            } else {
                                                self.bp_edit.sprite_name.as_str()
                                            })
                                            .show_ui(ui, |ui| {
                                                for name in &candidates {
                                                    ui.selectable_value(
                                                        &mut self.bp_edit.sprite_name,
                                                        name.clone(),
                                                        name,
                                                    );
                                                }
                                            });
                                        if self.bp_edit.sprite_name != before {
                                            self.bp_writeback(edit_id);
                                        }
                                    }
                                }
                                NodeKind::Command => {
                                    // 按 @ 子命令分发：bg / music / stop_music / 其他
                                    let cmd: String = self
                                        .bp_edit
                                        .text
                                        .trim_start()
                                        .strip_prefix('@')
                                        .unwrap_or("")
                                        .trim_start()
                                        .chars()
                                        .take_while(|c| !c.is_whitespace())
                                        .collect();
                                    match cmd.as_str() {
                                        "bg" => {
                                            // 背景：背景资源 + 过渡效果（无小标题）
                                            let before_bg = self.bp_edit.bg_name.clone();
                                            egui::ComboBox::from_id_salt("bp_edit_bg")
                                                .width(inner_w)
                                                .selected_text(if self.bp_edit.bg_name.is_empty() {
                                                    "（选择背景）"
                                                } else {
                                                    self.bp_edit.bg_name.as_str()
                                                })
                                                .show_ui(ui, |ui| {
                                                    for s in &bgs {
                                                        ui.selectable_value(
                                                            &mut self.bp_edit.bg_name,
                                                            s.clone(),
                                                            s,
                                                        );
                                                    }
                                                });
                                            let before_tr = self.bp_edit.transition.clone();
                                            egui::ComboBox::from_id_salt("bp_edit_bg_transition")
                                                .width(inner_w)
                                                .selected_text(self.bp_edit.transition.as_str())
                                                .show_ui(ui, |ui| {
                                                    for t in transitions {
                                                        ui.selectable_value(
                                                            &mut self.bp_edit.transition,
                                                            t.to_string(),
                                                            t,
                                                        );
                                                    }
                                                });
                                            if self.bp_edit.bg_name != before_bg
                                                || self.bp_edit.transition != before_tr
                                            {
                                                self.bp_writeback(edit_id);
                                            }
                                        }
                                        "music" => {
                                            // 音乐：音乐资源（无小标题）
                                            let before = self.bp_edit.music_name.clone();
                                            egui::ComboBox::from_id_salt("bp_edit_music")
                                                .width(inner_w)
                                                .selected_text(if self.bp_edit.music_name.is_empty() {
                                                    "（选择音乐）"
                                                } else {
                                                    self.bp_edit.music_name.as_str()
                                                })
                                                .show_ui(ui, |ui| {
                                                    for s in &musics {
                                                        ui.selectable_value(
                                                            &mut self.bp_edit.music_name,
                                                            s.clone(),
                                                            s,
                                                        );
                                                    }
                                                });
                                            if self.bp_edit.music_name != before {
                                                self.bp_writeback(edit_id);
                                            }
                                        }
                                        "stop_music" => {
                                            // 停止音乐：无需编辑
                                            ui.label("（停止音乐，无需编辑）");
                                        }
                                        _ => {
                                            // 其他命令：通用文本兜底
                                            self.bp_edit_textedit_fallback(
                                                ui,
                                                edit_id,
                                                body_rect,
                                                1.0,
                                            );
                                        }
                                    }
                                }
                                NodeKind::Flow | NodeKind::Visit => {
                                    // 跳转/访问：章节选择（无小标题）
                                    let before = self.bp_edit.section_name.clone();
                                    egui::ComboBox::from_id_salt("bp_edit_section")
                                        .width(inner_w)
                                        .selected_text(if self.bp_edit.section_name.is_empty() {
                                            "（选择章节）"
                                        } else {
                                            self.bp_edit.section_name.as_str()
                                        })
                                        .show_ui(ui, |ui| {
                                            for s in &sections {
                                                ui.selectable_value(
                                                    &mut self.bp_edit.section_name,
                                                    s.clone(),
                                                    s,
                                                );
                                            }
                                        });
                                    if self.bp_edit.section_name != before {
                                        self.bp_writeback(edit_id);
                                    }
                                }
                                NodeKind::Return => {
                                    // 返回：无需编辑
                                    ui.label("（返回，无需编辑）");
                                }
                                NodeKind::Dialogue => {
                                    // 对话：角色名 + ":" + 内容（无小标题）
                                    ui.horizontal(|ui| {
                                        let rsp = ui.add(
                                            egui::TextEdit::singleline(&mut self.bp_edit.speaker)
                                                .desired_width(inner_w * 0.3)
                                                .font(egui::FontId::proportional(13.0)),
                                        );
                                        ui.label(":");
                                        let rct = ui.add(
                                            egui::TextEdit::singleline(&mut self.bp_edit.content)
                                                .desired_width(inner_w * 0.6)
                                                .font(egui::FontId::proportional(13.0)),
                                        );
                                        if rsp.changed() || rct.changed() {
                                            self.bp_writeback(edit_id);
                                        }
                                    });
                                }
                                NodeKind::Narration => {
                                    // 旁白：内容（无小标题）
                                    let r = ui.add(
                                        egui::TextEdit::multiline(&mut self.bp_edit.content)
                                            .desired_width(inner_w)
                                            .desired_rows(2)
                                            .font(egui::FontId::proportional(13.0)),
                                    );
                                    if r.changed() {
                                        self.bp_writeback(edit_id);
                                    }
                                }
                                _ => {
                                    // 其他类型（章节/选择/选项/等待/结局/变量/注释等）：通用文本兜底
                                    self.bp_edit_textedit_fallback(ui, edit_id, body_rect, 1.0);
                                }
                            }
                                });
                            });
                    });
                // 记录面板矩形，用于"点击面板内不退出编辑"判定（下一帧使用）
                self.bp_edit_panel_rect = Some(panel_response.response.rect);
            } else {
                // 节点不存在了，退出编辑
                self.blueprint.editing_node = None;
                self.bp_edit_id = None;
                self.bp_edit_panel_rect = None;
            }
        } else if self.bp_edit_id.is_some() {
            // 未在编辑：清理 stale 编辑状态
            self.bp_edit_id = None;
            self.bp_edit_panel_rect = None;
        }

        // ---- 右键菜单 ----
        if let Some(menu_pos) = self.blueprint.context_menu_pos {
            let mut close_menu = false;
            let pan_copy = self.blueprint.pan;
            let canvas_min = canvas_rect.min;
            // 按内容自适应宽度：用 painter 测量每行真实文本宽度，取最长行 + 少量余量。
            // 不再凭字符数估算、不加固定 56px 留白，避免菜单比最长行还宽。
            let btn_pad = ui.style().spacing.button_padding.x;
            let item_sp = ui.style().spacing.item_spacing.x;
            let font_id = egui::FontId::proportional(14.0);
            let measure = |ui: &egui::Ui, txt: &str| {
                ui.painter()
                    .layout_no_wrap(txt.to_string(), font_id.clone(), egui::Color32::WHITE)
                    .size()
                    .x
            };
            let mut content_width = measure(ui, "添加节点");
            for (label, desc, _) in NODE_TEMPLATES {
                // 行宽 = 标签按钮(文本 + 左右内边距) + 行内间距 + 描述文本
                let row_w = measure(ui, label) + 2.0 * btn_pad + item_sp + measure(ui, desc);
                if row_w > content_width {
                    content_width = row_w;
                }
            }
            for txt in ["整理布局", "从脚本导入", "折叠注释", "展开注释", "删除选中节点"] {
                let bw = measure(ui, txt) + 2.0 * btn_pad;
                if bw > content_width {
                    content_width = bw;
                }
            }
            content_width += 4.0; // 少量安全余量，避免字宽估算误差导致右侧贴边
            // 屏幕边界检测用的总宽度（含 popup 内边距）
            let menu_width = content_width + 16.0;
            // 防止菜单超出屏幕底部：若菜单高度可能超出，把弹出位置往上挪。
            // 估算菜单高度：每个条目约 22px，加标题、分隔符和操作按钮。
            let estimated_height = (NODE_TEMPLATES.len() as f32 + 7.0) * 22.0;
            let screen_rect = ui.ctx().screen_rect();
            let adjusted_y = if menu_pos.y + estimated_height > screen_rect.bottom() {
                (screen_rect.bottom() - estimated_height - 8.0).max(screen_rect.top() + 8.0)
            } else {
                menu_pos.y
            };
            let adjusted_x = if menu_pos.x + menu_width > screen_rect.right() {
                (screen_rect.right() - menu_width - 8.0).max(screen_rect.left() + 8.0)
            } else {
                menu_pos.x
            };
            let menu_pos_copy = egui::pos2(adjusted_x, adjusted_y);
            let response = egui::Area::new(egui::Id::new("bp_context_menu"))
                .order(egui::Order::Foreground)
                .fixed_pos(menu_pos_copy)
                .show(ui.ctx(), |ui| {
                    egui::Frame::popup(ui.style()).show(ui, |ui| {
                        // 取消按钮默认最小交互宽度，让短按钮按文本收缩；统一行宽到最长行
                        ui.spacing_mut().interact_size.x = 0.0;
                        ui.set_min_width(content_width);
                        ui.label(egui::RichText::new("添加节点").strong());
                        ui.separator();
                        // 节点模板列表：每行按钮(标签) + 灰色描述文字，字号统一。
                        for (label, desc, template) in NODE_TEMPLATES {
                            ui.horizontal(|ui| {
                                if ui.button(*label).clicked() {
                                    let cpos = (menu_pos_copy - canvas_min - pan_copy).to_pos2();
                                    let text = template.to_string();
                                    let kind = BlueprintState::detect_kind(&text);
                                    // 选择模板特殊处理：生成 Choice + 2 个 Option + 连线
                                    if kind == NodeKind::Choice {
                                        let choice_id = self.blueprint.add_node(NodeKind::Choice, cpos, text);
                                        let size = BlueprintState::node_size(template);
                                        let opt1_pos = egui::pos2(cpos.x, cpos.y + size.y + 24.0);
                                        let opt2_pos = egui::pos2(cpos.x, opt1_pos.y + size.y + 24.0);
                                        let opt1_id = self.blueprint.add_node(NodeKind::ChoiceOption, opt1_pos, "| 选项A".to_string());
                                        let opt2_id = self.blueprint.add_node(NodeKind::ChoiceOption, opt2_pos, "| 选项B".to_string());
                                        self.blueprint.links.push(BlueprintLink { from: choice_id, to: opt1_id });
                                        self.blueprint.links.push(BlueprintLink { from: opt1_id, to: opt2_id });
                                    } else {
                                        self.blueprint.add_node(kind, cpos, text);
                                    }
                                    close_menu = true;
                                }
                                ui.label(
                                    egui::RichText::new(*desc)
                                        .color(egui::Color32::from_rgb(150, 155, 170)),
                                );
                            });
                        }
                        ui.separator();
                        if ui.button("整理布局").clicked() {
                            self.blueprint.auto_layout();
                            self.status = "已整理布局".to_string();
                            close_menu = true;
                        }
                        if ui.button("从脚本导入").clicked() {
                            self.blueprint.from_script(&self.editor_content);
                            self.status = "已从脚本导入蓝图".to_string();
                            close_menu = true;
                        }
                        // 折叠/展开注释
                        let comment_label = if self.blueprint.collapse_comments {
                            "展开注释"
                        } else {
                            "折叠注释"
                        };
                        if ui.button(comment_label).clicked() {
                            self.blueprint.collapse_comments = !self.blueprint.collapse_comments;
                            self.blueprint.auto_layout();
                            close_menu = true;
                        }
                        if self.blueprint.selected.is_some() {
                            ui.separator();
                            if ui.button("删除选中节点").clicked() {
                                if let Some(id) = self.blueprint.selected {
                                    self.blueprint.remove_node(id);
                                }
                                close_menu = true;
                            }
                        }
                    });
                });
            // 点击菜单外部关闭
            let menu_rect = response.response.rect;
            if (primary_pressed || secondary_pressed) && !menu_rect.contains(mouse_pos) {
                close_menu = true;
            }
            if close_menu {
                self.blueprint.context_menu_pos = None;
            }
        }

        // ---- 右下角缩放控制（靠近画布右下角且鼠标在画布内才显示，类似游戏浮动按钮）----
        // 加 in_canvas 条件：避免鼠标在积木面板等画布外区域时误触发缩放控制。
        let br = canvas_rect.right_bottom();
        let near = in_canvas && mouse_pos.distance(br) < 150.0;
        if near {
            let show_pos = egui::pos2(br.x - 168.0, br.y - 38.0);
            egui::Area::new(egui::Id::new("bp_zoom_ctrl"))
                .order(egui::Order::Foreground)
                .fixed_pos(show_pos)
                .show(ui.ctx(), |ui| {
                    egui::Frame::group(ui.style())
                        .fill(egui::Color32::from_rgba_unmultiplied(30, 30, 40, 200))
                        .stroke(egui::Stroke::new(1.0, egui::Color32::from_rgb(70, 70, 90)))
                        .inner_margin(egui::Margin::symmetric(6.0, 3.0))
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                if ui.button("−").clicked() {
                                    self.blueprint.zoom =
                                        ((self.blueprint.zoom - 0.1).max(0.2) * 10.0).round() / 10.0;
                                }
                                // 输入框未聚焦时，用当前缩放同步显示文本
                                let input_id = egui::Id::new("bp_zoom_input");
                                if !ui.ctx().memory(|m| m.has_focus(input_id)) {
                                    self.zoom_input = format!(
                                        "{}",
                                        (self.blueprint.zoom * 100.0).round() as i32
                                    );
                                }
                                let resp = ui.add(
                                    egui::TextEdit::singleline(&mut self.zoom_input)
                                        .desired_width(36.0)
                                        .frame(false)
                                        .id(input_id)
                                        .hint_text("100"),
                                );
                                ui.label("%");
                                if resp.lost_focus() {
                                    let parsed = self
                                        .zoom_input
                                        .trim()
                                        .trim_end_matches('%')
                                        .parse::<f32>();
                                    if let Ok(v) = parsed {
                                        self.blueprint.zoom =
                                            (v / 100.0).clamp(0.2, 3.0);
                                    }
                                    self.zoom_input = format!(
                                        "{}",
                                        (self.blueprint.zoom * 100.0).round() as i32
                                    );
                                }
                                if ui.button("+").clicked() {
                                    self.blueprint.zoom =
                                        ((self.blueprint.zoom + 0.1).min(3.0) * 10.0).round()
                                            / 10.0;
                                }
                            });
                        });
                });
        }

        // ---- 底部操作提示栏 ----
        ui.separator();
        ui.label(
            egui::RichText::new("双击节点编辑 ｜ 拖拽积木添加 ｜ 左键拖动移动 ｜ 空白处拖拽框选 ｜ 右键拖动平移 ｜ 右键空白添加节点 ｜ Delete 删除")
                .size(13.0)
                .color(egui::Color32::from_rgb(120, 120, 140)),
        );
    }

    /// 鼠标悬停在 `=>`/`<=` 标记上时显示配对信息 tooltip。
    ///
    /// - 悬停 `=>`：「跳转到目标：<目标章节名>」+「返回点在第 X 行」（X 为配对 `<=` 行号，1 基）
    /// - 悬停 `<=`：「返回到上一个拜访点」
    fn show_flow_hover_tooltip(
        &self,
        ctx: &egui::Context,
        output: &egui::text_edit::TextEditOutput,
        marks: &[FlowMark],
        pairs: &[Option<usize>],
    ) {
        let hover_pos = match output.response.hover_pos() {
            Some(p) => p,
            None => return,
        };
        let local = hover_pos - output.galley_pos;
        if local.x < 0.0 || local.y < 0.0 {
            return;
        }
        let cursor = output.galley.cursor_from_pos(local);
        let ci = cursor.ccursor.index;
        let idx = match mark_at_cursor(marks, ci) {
            Some(i) => i,
            None => return,
        };
        let m = &marks[idx];
        let tip = match m.kind {
            FlowKind::Visit => {
                let target = m.target.as_deref().unwrap_or("(未指定)");
                match pairs[idx] {
                    Some(p) => format!(
                        "跳转到目标：{}\n返回点在第 {} 行",
                        target,
                        marks[p].line + 1
                    ),
                    None => format!("跳转到目标：{}\n返回点未找到", target),
                }
            }
            FlowKind::Return => "返回到上一个拜访点".to_string(),
        };
        egui::show_tooltip_at(
            ctx,
            egui::LayerId::background(),
            egui::Id::new("flow_pair_tooltip"),
            hover_pos,
            |ui| {
                ui.label(egui::RichText::new(tip).monospace());
            },
        );
    }

    /// 大纲视图：按缩进展示章节（`#`）、分支选项（`?`/`|`）与 `=>`/`<=` 配对结构。
    ///
    /// 每个 `=>` 打开一个「拜访块」，其与匹配 `<=` 之间的内容缩进一级；
    /// 嵌套 `=>` 进一步缩进。配对基于文本顺序栈匹配（[`compute_flow_pairs`]），
    /// 不做块级语义分析。仅展示上述结构性行，对话/旁白/指令等不显示。
    /// 渲染蓝图积木面板：显示可拖拽的节点模板列表，拖到画布释放即添加节点。
    /// 仅在蓝图模式下显示。面板可滚动以容纳所有模板。
    fn show_blocks_panel(&mut self, ui: &mut egui::Ui) {
        ui.label("拖拽积木（方形拖拽点）到画布添加节点：");
        ui.add_space(4.0);
        // 关闭「拖拽即滚动」：默认 drag_to_scroll=true 会吞掉垂直拖拽手势用于滚动列表，
        // 导致积木上的 Sense::drag() 拿不到拖拽（拖动变成上下滚动，和滚轮效果一样）。
        // 关闭后仍可用滚轮滚动，积木可正常拖出。
        // auto_shrink(false)：让 ScrollArea 在水平方向撑满父容器宽度，而非按内容收缩。
        // 否则内容（积木卡片）比面板窄时，ScrollArea 自身宽度也跟着收缩，其 solid
        // 滚动条会画在收缩后内容区的右边缘——即面板中间，而非面板右边缘。
        // 撑满后滚动条贴右；卡片本身仍按自然宽度渲染，不会被撑大。
        let scroll_output = egui::ScrollArea::vertical()
            .drag_to_scroll(false)
            .auto_shrink([false; 2])
            .show(ui, |ui| {
            for (idx, (label, desc, template)) in NODE_TEMPLATES.iter().enumerate() {
                let kind = BlueprintState::detect_kind(template);
                let color = BlueprintState::kind_color(kind);
                let frame = block_card_frame(ui, color, 3.0);
                let resp = frame.show(ui, |ui| {
                    paint_block_card(ui, label, desc, template, color);
                });
                // 拖拽添加：用 Sense::drag 检测拖拽开始，记录模板索引，画布上释放即添加。
                let drag_resp = resp.response.interact(egui::Sense::drag());
                if drag_resp.drag_started() {
                    self.drag_template = Some(idx);
                }
                // 拖拽中显示拖动光标
                if drag_resp.dragged() {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
                }
                // 拖拽中或悬停时提示用法（on_hover_text 消费所有权，放最后）
                if drag_resp.hovered() || drag_resp.dragged() {
                    drag_resp.on_hover_text("按住拖拽到画布释放");
                }
            }
        });
        // 手动处理空白处触摸/鼠标拖拽滚动：
        // drag_to_scroll(false) 保证积木卡片拖拽不被吞，但空白处（卡片之间间隙、
        // 列表底部余白）也无法拖拽滚动。这里补充：若无积木正在被拖拽
        // （drag_template 为 None，说明拖拽不在积木上），且指针在滚动区域内
        // 按下并移动，则手动调整滚动偏移。触摸屏单指上下拖空白处即可滚动。
        // 用 interact_pos() 而非 hover_pos()，保证触摸释放前位置稳定可用。
        if self.drag_template.is_none() {
            let inner_rect = scroll_output.inner_rect;
            let scroll_id = scroll_output.id;
            let content_size = scroll_output.content_size;
            let mut state = scroll_output.state;
            let interact_pos = ui.input(|i| i.pointer.interact_pos());
            let primary_down = ui.input(|i| i.pointer.primary_down());
            let delta = ui.input(|i| i.pointer.delta());
            if primary_down && delta.y.abs() > 0.5 {
                if let Some(pos) = interact_pos {
                    if inner_rect.contains(pos) {
                        let max_offset =
                            (content_size.y - inner_rect.height()).max(0.0);
                        state.offset.y = (state.offset.y - delta.y).clamp(0.0, max_offset);
                        state.store(ui.ctx(), scroll_id);
                    }
                }
            }
        }
    }

    fn show_outline(&self, ui: &mut egui::Ui) {
        let content = &self.editor_content;
        if content.trim().is_empty() {
            ui.label(
                egui::RichText::new("（暂无内容）")
                    .italics()
                    .color(egui::Color32::from_gray(130)),
            );
            return;
        }

        let marks = scan_flow_marks(content);
        let pairs = compute_flow_pairs(&marks);
        let lines: Vec<&str> = content.split('\n').collect();
        let indent_w = 16.0;
        ui.spacing_mut().item_spacing.y = 2.0;

        let mut mark_iter = 0usize;
        let mut depth: i32 = 0;
        let mut shown = 0u32;

        for (line_no, raw) in lines.iter().enumerate() {
            // 推进 mark 游标到当前行（marks 行号单调递增）。
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

            // 计算该行缩进级别、显示内容、颜色与类型描述。
            // 逻辑（depth/配对/裸 `?` 跳过/非结构行跳过）保持不变，仅改渲染样式。
            let (indent_level, text_opt, color, kind_desc) = if let Some(mi) = mark_here {
                let m = &marks[mi];
                match m.kind {
                    FlowKind::Visit => {
                        let t = m.target.as_deref().unwrap_or("");
                        (depth, Some(format!("=> {}", t)), COLOR_FLOW, "访问子章节")
                    }
                    FlowKind::Return => {
                        // 配对 <= 对齐其 =>（depth-1）；孤立 <= 留在当前 depth。
                        let lvl = if pairs[mi].is_some() { depth - 1 } else { depth };
                        (lvl, Some("<=".to_string()), COLOR_FLOW, "返回")
                    }
                }
            } else if let Some(rest) = trimmed.strip_prefix('#') {
                (depth, Some(format!("# {}", rest.trim())), COLOR_SECTION, "章节")
            } else if let Some(rest) = trimmed.strip_prefix('?') {
                let r = rest.trim();
                if r.is_empty() {
                    continue; // 裸 `?` 终止符不显示
                }
                (depth, Some(format!("? {}", r)), COLOR_CHOICE, "选项提示")
            } else if let Some(rest) = trimmed.strip_prefix('|') {
                (depth, Some(format!("| {}", rest.trim())), COLOR_CHOICE, "选项")
            } else {
                continue; // 对话/旁白/指令/变量等不显示
            };

            // 更新 depth：=> 入栈，配对 <= 出栈。
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

            if let Some(text) = text_opt {
                let indent_level = indent_level.max(0) as usize;
                // 采用与积木面板一致的卡片风格：填色边框 + 左侧色块 + 强调标题 + 灰色类型描述。
                // 用 outer_margin.left 实现按 depth 缩进，卡片右侧仍占满可用宽度。
                let frame = egui::Frame::group(ui.style())
                    .fill(color.linear_multiply(0.15))
                    .stroke(egui::Stroke::new(1.0, color))
                    .inner_margin(egui::Margin::same(6.0))
                    .outer_margin(egui::Margin {
                        left: indent_level as f32 * indent_w,
                        right: 0.0,
                        top: 2.0,
                        bottom: 2.0,
                    });
                frame.show(ui, |ui| {
                    ui.horizontal(|ui| {
                        // 色块标识类型
                        let (rect, _) = ui.allocate_exact_size(
                            egui::Vec2::new(10.0, 10.0),
                            egui::Sense::hover(),
                        );
                        ui.painter().rect_filled(rect, 2.0, color);
                        // 主文本（强调色）
                        ui.label(egui::RichText::new(&text).strong().color(color));
                        // 右侧类型描述（小号灰字）
                        ui.label(
                            egui::RichText::new(kind_desc)
                                .small()
                                .color(egui::Color32::from_rgb(140, 145, 160)),
                        );
                    });
                });
                shown += 1;
            }
        }

        if shown == 0 {
            ui.label(
                egui::RichText::new("（未发现章节 / 分支 / => 等结构）")
                    .italics()
                    .color(egui::Color32::from_gray(130)),
            );
        }
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

    /// 渲染左栏资源文件列表（立绘 / 背景 / 音乐通用）。
    ///
    /// `dir_name` 是 `assets/` 下的子目录名（`characters` / `bg` / `music`），
    /// `extensions` 是允许的扩展名列表（小写比较，不区分大小写匹配）。
    /// 每帧重新扫描目录以反映最新内容；底部提供「上传资源」按钮（暂以状态栏
    /// 提示引导用户手动复制文件，避免引入新的原生对话框依赖）。双击图片资源
    /// 在弹窗中预览，双击音乐资源调用系统文件管理器打开所在目录。
    fn show_resource_list(&mut self, ui: &mut egui::Ui, dir_name: &str, extensions: &[&str]) {
        // 扫描 assets/{dir_name}/ 目录，按扩展名过滤。
        let dir = self.project_root().join("assets").join(dir_name);
        let mut names: Vec<String> = Vec::new();
        if let Ok(entries) = std::fs::read_dir(&dir) {
            names = entries
                .filter_map(|e| e.ok())
                .filter_map(|e| {
                    let p = e.path();
                    if p
                        .extension()
                        .is_some_and(|ext| extensions.iter().any(|x| ext.eq_ignore_ascii_case(x)))
                    {
                        p.file_name().map(|n| n.to_string_lossy().into_owned())
                    } else {
                        None
                    }
                })
                .collect();
            names.sort();
        }

        // 把扫描结果回写到对应字段（便于外部读取，此处用 match 分派避免
        // `&mut self` 与字段 `&mut` 借用冲突）。
        match dir_name {
            "characters" => self.resource_sprites = names.clone(),
            "bg" => self.resource_backgrounds = names.clone(),
            "music" => self.resource_musics = names.clone(),
            _ => {}
        }

        ui.heading(dir_name);
        ui.label(format!("目录：{}", dir.display()));
        ui.separator();

        // 文件列表
        egui::ScrollArea::vertical().show(ui, |ui| {
            if names.is_empty() {
                ui.label("（无资源文件）");
            }
            for name in &names {
                let resp = ui.selectable_label(false, name.as_str());
                if resp.double_clicked() {
                    let full_path = dir.join(name);
                    if dir_name == "music" {
                        // 双击音乐：在系统文件管理器中打开所在目录（选中文件）。
                        #[cfg(target_os = "linux")]
                        {
                            let _ = std::process::Command::new("xdg-open")
                                .arg(&full_path)
                                .spawn();
                        }
                        #[cfg(target_os = "windows")]
                        {
                            let _ = std::process::Command::new("explorer")
                                .arg("/select,")
                                .arg(&full_path)
                                .spawn();
                        }
                        #[cfg(target_os = "macos")]
                        {
                            let _ = std::process::Command::new("open")
                                .arg("-R")
                                .arg(&full_path)
                                .spawn();
                        }
                        self.status = format!("在系统中打开：{}", name);
                    } else {
                        // 双击图片：弹出预览窗口（清空纹理缓存以触发重新解码）。
                        self.resource_preview_path = Some(full_path);
                        self.resource_preview_texture = None;
                    }
                }
            }
        });

        ui.separator();
        // 上传资源按钮：为避免引入 rfd 等新依赖，暂以状态栏提示引导用户
        // 手动复制文件到对应资源目录，下次刷新（重新进入标签页）即可看到。
        if ui.button("上传资源").clicked() {
            self.status = format!(
                "请手动复制文件到 {} 目录后重新切到此标签页以刷新列表",
                dir.display()
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
        let chars_dir = self.project_root().join("assets").join("characters");
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
        egui::ComboBox::from_id_salt("sprite_preview_select")
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
                    .project_root()
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
                // y：立绘中心点对齐到预览区 y_frac（统一公式，无 1.0 特殊判断，
                // 避免拖动到 1.0 时从底部跳到顶部的突变）。
                let y = rect.top() + preview_h * y_frac - draw_h / 2.0;
                let dest_rect = egui::Rect::from_min_size(
                    egui::pos2(x, y),
                    egui::Vec2::new(draw_w, draw_h),
                );
                // 用预览区域作为裁剪矩形，超出部分不绘制。
                let clipped = painter.with_clip_rect(rect);
                clipped.image(
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
        // 蓝图模式下显示积木卡片预览（可拖拽到画布），非蓝图模式显示原始语法文本。
        if self.blueprint_mode {
            let kind = BlueprintState::detect_kind(&syntax);
            let color = BlueprintState::kind_color(kind);
            let display = BlueprintState::node_display_text(&syntax, kind);
            let card_resp = block_card_frame(ui, color, 0.0).show(ui, |ui| {
                ui.horizontal(|ui| {
                    let (rect, _) = ui.allocate_exact_size(egui::Vec2::new(10.0, 10.0), egui::Sense::hover());
                    ui.painter().rect_filled(rect, 2.0, color);
                    ui.label(egui::RichText::new("立绘上场").strong().color(color));
                });
                ui.label(
                    egui::RichText::new(&display)
                        .color(egui::Color32::from_rgb(200, 210, 220)),
                );
            });
            // 拖拽积木卡片到画布添加节点
            let drag_resp = card_resp.response.interact(egui::Sense::drag());
            if drag_resp.drag_started() {
                self.drag_syntax = Some(syntax.clone());
            }
            if drag_resp.hovered() || drag_resp.dragged() {
                drag_resp.on_hover_text("按住拖拽到画布释放");
            }
        } else {
            ui.label(
                egui::RichText::new(&syntax)
                    .monospace()
                    .color(egui::Color32::from_rgb(200, 220, 255)),
            );
        }
        ui.horizontal(|ui| {
            if ui.button("复制到剪贴板").clicked() {
                ctx.output_mut(|o| o.copied_text = syntax.clone());
                self.status = "语法已复制到剪贴板".to_string();
            }
            // 蓝图模式下按钮变为"添加为节点"，把生成的语法作为节点加到画布。
            let append_label = if self.blueprint_mode { "添加为节点" } else { "追加到脚本" };
            if ui.button(append_label).clicked() {
                if self.blueprint_mode {
                    let kind = BlueprintState::detect_kind(&syntax);
                    let pos = self.blueprint.next_placement_pos();
                    self.blueprint.add_node(kind, pos, syntax.clone());
                    self.status = "已添加为蓝图节点".to_string();
                } else {
                    self.editor_content.push_str(&format!("{}\n", syntax));
                    self.status = "语法已追加到脚本末尾".to_string();
                }
            }
            // 隐藏立绘：蓝图模式下同样添加为节点
            let hide_label = if self.blueprint_mode { "添加下场节点" } else { "隐藏此立绘" };
            if ui.button(hide_label).clicked() {
                let hide_syntax = format!("- {}", name);
                if self.blueprint_mode {
                    let kind = BlueprintState::detect_kind(&hide_syntax);
                    let pos = self.blueprint.next_placement_pos();
                    self.blueprint.add_node(kind, pos, hide_syntax);
                    self.status = "已添加下场节点".to_string();
                } else {
                    self.editor_content.push_str(&format!("{}\n", hide_syntax));
                    self.status = "隐藏立绘语法已追加到脚本末尾".to_string();
                }
            }
            if ui.button("放大预览").clicked() {
                self.show_enlarged_preview = true;
            }
            // 替换插入：蓝图模式下改为"替换选中节点"
            let replace_label = if self.blueprint_mode { "替换选中节点" } else { "替换插入" };
            if ui.button(replace_label).clicked() {
                if self.blueprint_mode {
                    if self.blueprint.replace_selected_text(syntax.clone()) {
                        self.status = "已替换选中节点文本".to_string();
                    } else {
                        self.status = "请先选中一个节点再替换".to_string();
                    }
                } else {
                    self.find_replace_syntax = syntax.clone();
                    self.show_find_replace_dialog = true;
                }
            }
        });
    }

    /// 渲染背景预览面板：允许作者选择背景图片，预览效果，并生成对应的 `.akrs` 语法。
    fn show_bg_preview(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();

        // 扫描 assets/bg/ 目录（检测变更后重新扫描）
        let bg_dir = self.project_root().join("assets").join("bg");
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
        egui::ComboBox::from_id_salt("bg_preview_select")
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
        egui::ComboBox::from_id_salt("bg_transition_select")
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
                let bg_dir = self.project_root().join("assets").join("bg");
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
        // 蓝图模式下显示积木卡片预览（可拖拽到画布），非蓝图模式显示原始语法文本。
        if self.blueprint_mode && !self.bg_preview.selected.is_empty() {
            let kind = BlueprintState::detect_kind(&syntax);
            let color = BlueprintState::kind_color(kind);
            let display = BlueprintState::node_display_text(&syntax, kind);
            let card_resp = block_card_frame(ui, color, 0.0).show(ui, |ui| {
                ui.horizontal(|ui| {
                    let (rect, _) = ui.allocate_exact_size(egui::Vec2::new(10.0, 10.0), egui::Sense::hover());
                    ui.painter().rect_filled(rect, 2.0, color);
                    ui.label(egui::RichText::new("背景").strong().color(color));
                });
                ui.label(
                    egui::RichText::new(&display)
                        .color(egui::Color32::from_rgb(200, 210, 220)),
                );
            });
            let drag_resp = card_resp.response.interact(egui::Sense::drag());
            if drag_resp.drag_started() {
                self.drag_syntax = Some(syntax.clone());
            }
            if drag_resp.hovered() || drag_resp.dragged() {
                drag_resp.on_hover_text("按住拖拽到画布释放");
            }
        } else {
            ui.label(
                egui::RichText::new(&syntax)
                    .monospace()
                    .color(egui::Color32::from_rgb(200, 220, 255)),
            );
        }
        ui.horizontal(|ui| {
            if ui.button("复制到剪贴板").clicked() {
                ctx.output_mut(|o| o.copied_text = syntax.clone());
                self.status = "语法已复制到剪贴板".to_string();
            }
            let append_label = if self.blueprint_mode { "添加为节点" } else { "追加到脚本" };
            if ui.button(append_label).clicked() {
                if self.blueprint_mode {
                    if self.bg_preview.selected.is_empty() {
                        self.status = "请先选择背景".to_string();
                    } else {
                        let kind = BlueprintState::detect_kind(&syntax);
                        let pos = self.blueprint.next_placement_pos();
                        self.blueprint.add_node(kind, pos, syntax.clone());
                        self.status = "已添加为蓝图节点".to_string();
                    }
                } else {
                    self.editor_content.push_str(&format!("{}\n", syntax));
                    self.status = "语法已追加到脚本末尾".to_string();
                }
            }
            if ui.button("放大预览").clicked() {
                self.show_enlarged_preview = true;
            }
            let replace_label = if self.blueprint_mode { "替换选中节点" } else { "替换插入" };
            if ui.button(replace_label).clicked() {
                if self.blueprint_mode {
                    if self.blueprint.replace_selected_text(syntax.clone()) {
                        self.status = "已替换选中节点文本".to_string();
                    } else {
                        self.status = "请先选中一个节点再替换".to_string();
                    }
                } else {
                    self.find_replace_syntax = syntax.clone();
                    self.show_find_replace_dialog = true;
                }
            }
        });
    }

    /// 渲染音乐预览面板：允许作者选择音乐文件，并生成对应的 `.akrs` 语法。
    fn show_music_preview(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();

        // 扫描 assets/music/ 目录（检测变更后重新扫描）
        let music_dir = self.project_root().join("assets").join("music");
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
        egui::ComboBox::from_id_salt("music_preview_select")
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
        // 蓝图模式下显示积木卡片预览（可拖拽到画布），非蓝图模式显示原始语法文本。
        if self.blueprint_mode && !self.music_preview.selected.is_empty() {
            let play_kind = BlueprintState::detect_kind(&play_syntax);
            let play_color = BlueprintState::kind_color(play_kind);
            let play_display = BlueprintState::node_display_text(&play_syntax, play_kind);
            let play_resp = block_card_frame(ui, play_color, 0.0).show(ui, |ui| {
                ui.horizontal(|ui| {
                    let (rect, _) = ui.allocate_exact_size(egui::Vec2::new(10.0, 10.0), egui::Sense::hover());
                    ui.painter().rect_filled(rect, 2.0, play_color);
                    ui.label(egui::RichText::new("音乐").strong().color(play_color));
                });
                ui.label(
                    egui::RichText::new(&play_display)
                        .color(egui::Color32::from_rgb(200, 210, 220)),
                );
            });
            let play_drag = play_resp.response.interact(egui::Sense::drag());
            if play_drag.drag_started() {
                self.drag_syntax = Some(play_syntax.clone());
            }
            if play_drag.hovered() || play_drag.dragged() {
                play_drag.on_hover_text("按住拖拽到画布释放");
            }
            let stop_kind = BlueprintState::detect_kind(stop_syntax);
            let stop_color = BlueprintState::kind_color(stop_kind);
            let stop_display = BlueprintState::node_display_text(stop_syntax, stop_kind);
            let stop_resp = block_card_frame(ui, stop_color, 0.0).show(ui, |ui| {
                ui.horizontal(|ui| {
                    let (rect, _) = ui.allocate_exact_size(egui::Vec2::new(10.0, 10.0), egui::Sense::hover());
                    ui.painter().rect_filled(rect, 2.0, stop_color);
                    ui.label(egui::RichText::new("停止音乐").strong().color(stop_color));
                });
                ui.label(
                    egui::RichText::new(&stop_display)
                        .color(egui::Color32::from_rgb(200, 210, 220)),
                );
            });
            let stop_drag = stop_resp.response.interact(egui::Sense::drag());
            if stop_drag.drag_started() {
                self.drag_syntax = Some(stop_syntax.to_string());
            }
            if stop_drag.hovered() || stop_drag.dragged() {
                stop_drag.on_hover_text("按住拖拽到画布释放");
            }
        } else {
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
        }

        ui.add_space(8.0);
        let play_label = if self.blueprint_mode { "添加播放节点" } else { "追加播放语法" };
        let play_replace_label = if self.blueprint_mode { "替换选中节点" } else { "替换插入播放语法" };
        ui.horizontal(|ui| {
            if ui.button("复制播放语法").clicked() {
                ctx.output_mut(|o| o.copied_text = play_syntax.clone());
                self.status = "播放语法已复制到剪贴板".to_string();
            }
            if ui.button(play_label).clicked() {
                if self.blueprint_mode {
                    let kind = BlueprintState::detect_kind(&play_syntax);
                    let pos = self.blueprint.next_placement_pos();
                    self.blueprint.add_node(kind, pos, play_syntax.clone());
                    self.status = "已添加为蓝图节点".to_string();
                } else {
                    self.editor_content.push_str(&format!("{}\n", play_syntax));
                    self.status = "播放语法已追加到脚本末尾".to_string();
                }
            }
            if ui.button(play_replace_label).clicked() {
                if self.blueprint_mode {
                    if self.blueprint.replace_selected_text(play_syntax.clone()) {
                        self.status = "已替换选中节点文本".to_string();
                    } else {
                        self.status = "请先选中一个节点再替换".to_string();
                    }
                } else {
                    self.find_replace_syntax = play_syntax.clone();
                    self.show_find_replace_dialog = true;
                }
            }
        });
        let stop_label = if self.blueprint_mode { "添加关闭节点" } else { "追加关闭语法" };
        let stop_replace_label = if self.blueprint_mode { "替换选中节点" } else { "替换插入关闭语法" };
        ui.horizontal(|ui| {
            if ui.button("复制关闭语法").clicked() {
                ctx.output_mut(|o| o.copied_text = stop_syntax.to_string());
                self.status = "关闭语法已复制到剪贴板".to_string();
            }
            if ui.button(stop_label).clicked() {
                if self.blueprint_mode {
                    let kind = BlueprintState::detect_kind(stop_syntax);
                    let pos = self.blueprint.next_placement_pos();
                    self.blueprint.add_node(kind, pos, stop_syntax.to_string());
                    self.status = "已添加为蓝图节点".to_string();
                } else {
                    self.editor_content.push_str(&format!("{}\n", stop_syntax));
                    self.status = "关闭语法已追加到脚本末尾".to_string();
                }
            }
            if ui.button(stop_replace_label).clicked() {
                if self.blueprint_mode {
                    if self.blueprint.replace_selected_text(stop_syntax.to_string()) {
                        self.status = "已替换选中节点文本".to_string();
                    } else {
                        self.status = "请先选中一个节点再替换".to_string();
                    }
                } else {
                    self.find_replace_syntax = stop_syntax.to_string();
                    self.show_find_replace_dialog = true;
                }
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
                akrs_lines.push(format!("// {}", trimmed.trim_start_matches('#').trim()));
                continue;
            }

            // 未识别的行保留为注释
            if !trimmed.is_empty() {
                akrs_lines.push(format!("// 未转换: {}", trimmed));
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

        // 拦截窗口关闭：若有未保存修改则中止关闭并弹出确认对话框。
        // eframe 0.29 移除了 on_close_event 回调，改用 viewport close_requested 事件。
        if ctx.input(|i| i.viewport().close_requested()) {
            if !self.force_close && self.is_dirty() {
                if self.editor_settings.auto_save_on_exit {
                    // 自动保存模式：尝试保存后放行关闭；保存失败则回退到确认对话框，
                    // 避免静默丢失用户修改。
                    self.save_file();
                    if self.is_dirty() {
                        self.pending_exit = true;
                        ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                    }
                } else {
                    self.pending_exit = true;
                    ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                }
            }
        }

        // 引擎动画的帧增量（打字机 / 过渡）。
        let now: f64 = ctx.input(|i| i.time);
        let dt = if self.last_time > 0.0 {
            ((now - self.last_time) as f32).clamp(0.0, 0.1)
        } else {
            0.0
        };
        self.last_time = now;

        // 键盘快捷键：直接遍历事件流，用事件自带的 modifiers（Event::Key 携带的
        // 按键瞬间修饰键状态，比 i.modifiers 帧级状态更可靠——后者是「帧开始时」
        // 的快照，在跨帧的按键序列中可能滞后），并把动作执行推迟到闭包外，
        // 避免在 input 读锁内修改 self。
        #[derive(Default)]
        struct ShortcutFlags {
            new_file: bool,
            open: bool,
            save: bool,
            run: bool,
            help: bool,
            ins1: bool,
            ins2: bool,
            ins3: bool,
            ins4: bool,
            toggle_blueprint: bool,
            toggle_translation: bool,
            undo: bool,
            redo: bool,
        }
        let sc = ctx.input(|i| {
            let mut f = ShortcutFlags::default();
            for event in &i.events {
                if let egui::Event::Key { key, pressed: true, modifiers, .. } = event {
                    // 撤销/重做单独处理：Ctrl+Z 撤销，Ctrl+Y 或 Ctrl+Shift+Z 重做。
                    if modifiers.ctrl && !modifiers.shift && *key == egui::Key::Z {
                        f.undo = true;
                        continue;
                    }
                    if modifiers.ctrl
                        && ((*key == egui::Key::Y && !modifiers.shift)
                            || (*key == egui::Key::Z && modifiers.shift))
                    {
                        f.redo = true;
                        continue;
                    }
                    // 其余快捷键要求 Ctrl 按下且无 Shift。
                    if !modifiers.ctrl || modifiers.shift {
                        continue;
                    }
                    match key {
                        egui::Key::N => f.new_file = true,
                        egui::Key::O => f.open = true,
                        egui::Key::S => f.save = true,
                        egui::Key::R => f.run = true,
                        egui::Key::H => f.help = true,
                        egui::Key::B => f.toggle_blueprint = true,
                        egui::Key::T => f.toggle_translation = true,
                        egui::Key::Num1 => f.ins1 = true,
                        egui::Key::Num2 => f.ins2 = true,
                        egui::Key::Num3 => f.ins3 = true,
                        egui::Key::Num4 => f.ins4 = true,
                        _ => {}
                    }
                }
            }
            f
        });
        if sc.undo {
            self.undo();
        }
        if sc.redo {
            self.redo();
        }
        if sc.new_file {
            self.new_file();
        }
        if sc.open {
            if self.show_welcome {
                self.show_welcome = false;
                self.status = "请从左侧文件列表选择文件".to_string();
            } else {
                self.open_current_name();
            }
        }
        if sc.save && !self.editor_content.trim().is_empty() {
            self.save_file();
        }
        if sc.run && !self.editor_content.trim().is_empty() {
            self.run_script();
        }
        // Ctrl+1/2/3/4：快速插入语法（无剧本内容时跳过，与「插入语法」按钮守卫一致）。
        let can_ins = !self.editor_content.trim().is_empty() || self.blueprint_mode;
        if sc.ins1 && can_ins {
            self.editor_content.push_str("+ 角色\n");
            self.status = "已插入：+ 角色（立绘上场）".to_string();
        }
        if sc.ins2 && can_ins {
            self.editor_content.push_str("- 角色\n");
            self.status = "已插入：- 角色（立绘下场）".to_string();
        }
        if sc.ins3 && can_ins {
            self.editor_content.push_str("# 章节\n");
            self.status = "已插入：# 章节（章节标题）".to_string();
        }
        if sc.ins4 && can_ins {
            self.editor_content.push_str("@bg 背景\n");
            self.status = "已插入：@bg 背景（背景指令）".to_string();
        }
        // Ctrl+H：显示帮助窗口
        if sc.help {
            self.show_shortcuts = true;
        }
        // Ctrl+B：切换蓝图模式
        if sc.toggle_blueprint {
            // 翻译模式下不允许直接进蓝图（避免互相打架），先退出翻译。
            if self.translation_mode {
                self.toggle_translation_mode();
            }
            self.toggle_blueprint_mode();
        }
        // Ctrl+T：切换对照翻译模式
        // 无剧本内容时：仅允许从翻译模式退出，不允许进入（与按钮守卫一致）。
        if sc.toggle_translation {
            if self.translation_mode {
                // 已在翻译模式，直接退出。
                self.toggle_translation_mode();
            } else if !self.editor_content.trim().is_empty() {
                // 有内容才允许进入；蓝图模式下先退出蓝图。
                if self.blueprint_mode {
                    self.toggle_blueprint_mode();
                }
                self.toggle_translation_mode();
            }
        }

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
            ui.horizontal(|ui| {
                // 蓝图模式切换按钮（最前面，独立按钮）。
                // 未打开剧本时禁用（避免空画布无意义操作）。
                let can_blueprint = !self.editor_content.trim().is_empty() || self.blueprint_mode;
                if self.blueprint_mode {
                    if ui.button("退出蓝图模式").on_hover_text("快捷键：Ctrl+B").clicked() {
                        self.toggle_blueprint_mode();
                    }
                } else {
                    let btn = egui::Button::new("蓝图模式");
                    let resp = if can_blueprint {
                        ui.add(btn).on_hover_text("快捷键：Ctrl+B")
                    } else {
                        ui.add_enabled(false, btn).on_hover_text("请先打开或新建剧本")
                    };
                    if resp.clicked() {
                        self.toggle_blueprint_mode();
                    }
                }

                // 对照翻译按钮（第二位，独立按钮）。
                // 无剧本内容时禁用（无可翻译内容）。
                // 已进入对照翻译模式时「退出」按钮始终可用，避免被困在模式里。
                if self.translation_mode {
                    if ui.button("退出对照翻译").on_hover_text("快捷键：Ctrl+T").clicked() {
                        self.toggle_translation_mode();
                    }
                } else {
                    if ui.add_enabled(!self.editor_content.trim().is_empty(), egui::Button::new("对照翻译"))
                        .on_hover_text("快捷键：Ctrl+T")
                        .clicked()
                    {
                        self.toggle_translation_mode();
                    }
                }

                ui.separator();

                // 菜单下拉按钮：集中收纳项目/文件/编译/其他操作，避免工具栏臃肿。
                ui.menu_button("菜单", |ui| {
                    // ---- 项目相关 ----
                    ui.label(egui::RichText::new("项目").strong());
                    if ui.button("打开项目").clicked() {
                        self.open_dir_picker();
                        ui.close_menu();
                    }
                    if ui.button("项目设置").clicked() {
                        self.show_project_settings = true;
                        ui.close_menu();
                    }
                    ui.separator();

                    // ---- 文件操作 ----
                    ui.label(egui::RichText::new("文件").strong());
                    if ui.button("新建").on_hover_text("快捷键：Ctrl+N").clicked() {
                        self.new_file();
                        ui.close_menu();
                    }
                    if ui.button("打开").on_hover_text("快捷键：Ctrl+O").clicked() {
                        self.open_file_picker(FilePickerMode::Open);
                        ui.close_menu();
                    }
                    // 保存：未打开/无内容剧本时禁用（无内容可保存）。
                    if ui.add_enabled(!self.editor_content.trim().is_empty(), egui::Button::new("保存"))
                        .on_hover_text("快捷键：Ctrl+S")
                        .clicked()
                    {
                        self.save_file();
                        ui.close_menu();
                    }
                    if ui.button("导入rpy").clicked() {
                        self.show_rpy_import = true;
                        self.rpy_import_warnings.clear();
                        self.rpy_import_source = None;
                        // 默认保存到项目的 scripts 目录，或工作目录
                        if self.project_loaded {
                            self.rpy_import_target = self.project_root().join("scripts").join("imported.akrs");
                        } else {
                            self.rpy_import_target = self.work_dir.join("imported.akrs");
                        }
                        ui.close_menu();
                    }
                    ui.separator();

                    // ---- 编译相关 ----
                    ui.label(egui::RichText::new("编译").strong());
                    if ui.button("预览游戏").clicked() {
                        self.start_game_preview();
                        ui.close_menu();
                    }
                    if ui.button("打包").clicked() {
                        self.build.show = true;
                        ui.close_menu();
                    }
                    // 运行：无剧本内容时禁用（无内容可运行）。
                    if ui.add_enabled(!self.editor_content.trim().is_empty(), egui::Button::new("运行"))
                        .on_hover_text("快捷键：Ctrl+R")
                        .clicked()
                    {
                        self.run_script();
                        ui.close_menu();
                    }
                    // 停止：仅在有运行中的预览引擎时显示。
                    if self.engine.is_some() {
                        if ui.button("停止").clicked() {
                            self.engine = None;
                            self.status = "预览已停止".to_string();
                            ui.close_menu();
                        }
                    }
                    ui.separator();

                    // ---- 其他 ----
                    if ui.button("首页").clicked() {
                        self.show_welcome = true;
                        self.engine = None;
                        self.status = "就绪".to_string();
                        ui.close_menu();
                    }
                    if ui.button("帮助").clicked() {
                        self.show_shortcuts = true;
                        ui.close_menu();
                    }
                    if ui.button("关于").clicked() {
                        self.show_about = true;
                        ui.close_menu();
                    }
                    ui.separator();
                    if ui.button("编辑器设置").clicked() {
                        self.show_editor_settings = true;
                        ui.close_menu();
                    }
                });

                ui.separator();

                // 快速插入语法按钮（收进下拉菜单，节省工具栏空间）
                // 蓝图模式下点击直接添加节点到画布；文本模式下追加到脚本末尾。
                // 无剧本内容时禁用（与蓝图模式按钮守卫一致）。
                let insert_label = if self.blueprint_mode { "插入节点" } else { "插入语法" };
                let can_edit = !self.editor_content.trim().is_empty() || self.blueprint_mode;
                ui.add_enabled_ui(can_edit, |ui| {
                    ui.menu_button(insert_label, |ui| {
                        let insert_buttons = [
                            ("立绘上场", "+ 角色", "立绘上场（0.5秒淡入）(Ctrl+1)"),
                            ("立绘下场", "- 角色", "立绘下场（0.5秒淡出）(Ctrl+2)"),
                            ("差分更换立绘", "+ 角色 (新pose) swap", "差分更换立绘（仅换pose，无过渡，位置/大小不变）"),
                            ("章节标题", "# 章节", "章节标题 (Ctrl+3)"),
                            ("背景指令", "@bg 背景", "背景指令 (Ctrl+4)"),
                            ("选择分支", "? 选项", "选择分支"),
                            ("变量操作", "$变量", "变量操作"),
                            ("结局声明", "ending \"true_end\" epilogue \"scripts/epilogue.akrs\" button \"尾声之后\"", "隐藏结局声明（彩蛋：定义尾声剧本与按钮文本）"),
                            ("解锁结局", "unlock \"true_end\"", "解锁隐藏结局标记（执行后主页显示尾声按钮）"),
                        ];
                        for (label, syntax, tooltip) in &insert_buttons {
                            if ui.button(*label).on_hover_text(*tooltip).clicked() {
                                if self.blueprint_mode {
                                    let kind = BlueprintState::detect_kind(syntax);
                                    let pos = self.blueprint.next_placement_pos();
                                    self.blueprint.add_node(kind, pos, syntax.to_string());
                                    self.status = format!("已添加节点：{}", syntax);
                                } else {
                                    self.editor_content.push_str(&format!("{}\n", syntax));
                                    self.status = format!("已插入：{}", syntax);
                                }
                                ui.close_menu();
                            }
                        }
                    });
                });

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

        // ---- 左栏：资源文件系统分类标签页 ---------------------------------
        egui::SidePanel::left("files")
            .resizable(true)
            .default_width(210.0)
            .show(ctx, |ui| {
                // 顶部水平标签页切换：脚本 / 立绘 / 背景 / 音乐
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut self.left_panel_tab, LeftPanelTab::Script, "脚本");
                    ui.selectable_value(&mut self.left_panel_tab, LeftPanelTab::Sprite, "立绘");
                    ui.selectable_value(&mut self.left_panel_tab, LeftPanelTab::Background, "背景");
                    ui.selectable_value(&mut self.left_panel_tab, LeftPanelTab::Music, "音乐");
                });
                ui.separator();

                match self.left_panel_tab {
                    LeftPanelTab::Script => {
                        // 保留原有的脚本文件列表逻辑
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
                    }
                    LeftPanelTab::Sprite => {
                        self.show_resource_list(ui, "characters", &["png"]);
                    }
                    LeftPanelTab::Background => {
                        self.show_resource_list(ui, "bg", &["png", "jpg", "jpeg"]);
                    }
                    LeftPanelTab::Music => {
                        self.show_resource_list(ui, "music", &["ogg", "mp3", "wav"]);
                    }
                }
            });

        // ---- 右栏：预览 --------------------------------------------------
        // 翻译模式时隐藏右侧边栏（防呆设计）
        if !self.translation_mode {
            // 从蓝图模式切换回普通模式时，若当前停在 Blocks 标签则自动切回大纲。
            if !self.blueprint_mode && self.preview_tab == PreviewTab::Blocks {
                self.preview_tab = PreviewTab::Outline;
            }
            egui::SidePanel::right("preview")
                .resizable(true)
                .default_width(330.0)
                .show(ctx, |ui| {
                    ui.heading("编辑器工具");
                    ui.horizontal(|ui| {
                        ui.selectable_value(&mut self.preview_tab, PreviewTab::Script, "剧本");
                        ui.selectable_value(&mut self.preview_tab, PreviewTab::Sprite, "立绘");
                        ui.selectable_value(&mut self.preview_tab, PreviewTab::Background, "背景");
                        ui.selectable_value(&mut self.preview_tab, PreviewTab::Music, "音乐");
                        ui.selectable_value(&mut self.preview_tab, PreviewTab::Outline, "大纲");
                        // 蓝图积木标签只在蓝图模式显示
                        if self.blueprint_mode {
                            ui.selectable_value(&mut self.preview_tab, PreviewTab::Blocks, "积木");
                        }
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
                        PreviewTab::Outline => {
                            self.show_outline(ui);
                        }
                        PreviewTab::Blocks => {
                            self.show_blocks_panel(ui);
                        }
                    }
                });
        }

        // ---- 中央面板：编辑器 / 蓝图 / 翻译 / 欢迎页 ---------------------
        egui::CentralPanel::default().show(ctx, |ui| {
            if self.show_welcome {
                self.show_welcome_panel(ui);
            } else if self.translation_mode {
                self.show_translation_view(ui);
            } else if self.blueprint_mode {
                self.show_blueprint(ui);
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
                .resizable(true)
                .collapsible(false)
                .default_width(450.0)
                .default_height(420.0)
                .show(ctx, |ui| {
                    // 内容较多时竖向滚动，避免窗口过高被主窗口截断。
                    egui::ScrollArea::vertical()
                        .max_height(ctx.screen_rect().height() * 0.8)
                        .show(ui, |ui| {
                            ui.heading("键盘快捷键");
                            ui.add_space(12.0);
                            ui.label(egui::RichText::new("文件操作").strong());
                            ui.separator();
                            let file_shortcuts = [
                                ("Ctrl+N", "新建剧本"),
                                ("Ctrl+O", "打开文件"),
                                ("Ctrl+S", "保存文件"),
                                ("Ctrl+R", "运行剧本"),
                                ("Ctrl+Z", "撤销编辑"),
                                ("Ctrl+Y / Ctrl+Shift+Z", "重做编辑"),
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
                                ("Ctrl+1", "+ 角色", "立绘上场（0.5秒淡入）"),
                                ("Ctrl+2", "- 角色", "立绘下场（0.5秒淡出）"),
                                ("按钮 ~", "+ 角色 (pose) swap", "差分更换（仅换pose，无过渡）"),
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
                            // 简化：直接使用项目根目录
                            self.rpy_import_target = self.project_root().join("scripts").join("imported.akrs");
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
                                    format!("▸  {}", entry.name)
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
                                let label = format!("▸  {}", entry.name);
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
                .resizable(true)
                .default_width(480.0)
                .default_height(560.0)
                .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
                .show(ctx, |ui| {
                    // 表单项很多，整体包竖向滚动，避免窗口高度超出主窗口被截断。
                    egui::ScrollArea::vertical()
                        .max_height(ctx.screen_rect().height() * 0.85)
                        .show(ui, |ui| {
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

                    ui.heading("开屏页（标题页）");
                    ui.add_space(8.0);

                    ui.horizontal(|ui| {
                        ui.label("背景图片：");
                        ui.add_sized(
                            [ui.available_width(), 28.0],
                            egui::TextEdit::singleline(&mut self.project_config.title_background)
                                .hint_text("相对 assets/ 的路径，如 title.png；留空用默认 title.png"),
                        );
                    });
                    ui.add_space(8.0);

                    ui.horizontal(|ui| {
                        ui.label("背景音乐：");
                        ui.add_sized(
                            [ui.available_width(), 28.0],
                            egui::TextEdit::singleline(&mut self.project_config.title_music)
                                .hint_text("assets/music/ 下的文件名，如 title_bgm.mp3；留空静音"),
                        );
                    });

                    ui.add_space(16.0);
                    ui.separator();
                    ui.add_space(12.0);

                    ui.heading("主题配色");
                    ui.add_space(8.0);
                    ui.label(
                        "自定义游戏内 3 种主题色（面板背景 / 按钮背景 / 对话框渐变）。\
                         点击色块可打开调色板，也可直接在输入框填写十六进制 #RRGGBB[AA]。\
                         默认值即引擎内置配色。\n\
                         文字颜色由游戏运行时按「已读 / 未读」自动着色，\
                         玩家可在游戏内设置页自定义，故不在此处配置。",
                    );
                    ui.add_space(6.0);

                    let edit_color = |ui: &mut egui::Ui, label: &str, color: &mut [u8; 4]| {
                        ui.horizontal(|ui| {
                            ui.label(label);
                            let mut c = egui::Color32::from_rgba_unmultiplied(color[0], color[1], color[2], color[3]);
                            egui::color_picker::color_edit_button_srgba(ui, &mut c, egui::color_picker::Alpha::BlendOrAdditive);
                            *color = c.to_array();
                        });
                    };
                    edit_color(ui, "主题色1（面板背景）：", &mut self.project_config.theme.primary);
                    ui.add_space(4.0);
                    edit_color(ui, "主题色2（按钮背景）：", &mut self.project_config.theme.secondary);
                    ui.add_space(4.0);
                    edit_color(ui, "对话框色（文本框渐变）：", &mut self.project_config.theme.dialogue);

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
                    ui.add_space(12.0);

                    ui.heading("关于页配置");
                    ui.add_space(8.0);
                    ui.label("自定义游戏内关于页的图片、文本和颜色。图片放在 assets/about/ 目录下。");

                    ui.add_space(8.0);
                    ui.checkbox(&mut self.project_config.about.sync_logo, "自动同步 logo（用 kokona.png 作为关于页图片）");

                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        ui.label("图片路径：");
                        ui.add_enabled(
                            !self.project_config.about.sync_logo,
                            egui::TextEdit::singleline(&mut self.project_config.about.image)
                                .hint_text("相对 assets/ 的路径，如 about/logo.png"),
                        );
                    });

                    ui.add_space(8.0);
                    ui.label("文本行（每行一条，第1行粗体显示，留空则回退默认）：");
                    let mut lines_clone = self.project_config.about.lines.clone();
                    for (i, line) in lines_clone.iter_mut().enumerate() {
                        ui.horizontal(|ui| {
                            ui.label(format!("第{}行：", i + 1));
                            ui.add_sized(
                                [ui.available_width(), 28.0],
                                egui::TextEdit::singleline(line),
                            );
                        });
                    }
                    self.project_config.about.lines = lines_clone;

                    ui.horizontal(|ui| {
                        if ui.button("添加一行").clicked() {
                            self.project_config.about.lines.push(String::new());
                        }
                        if ui.button("删除最后一行").clicked() && self.project_config.about.lines.len() > 1 {
                            self.project_config.about.lines.pop();
                        }
                    });

                    ui.add_space(8.0);
                    ui.label("颜色配置：");
                    let edit_color = |ui: &mut egui::Ui, label: &str, color: &mut [u8; 4]| {
                        ui.horizontal(|ui| {
                            ui.label(label);
                            let mut c = egui::Color32::from_rgba_unmultiplied(color[0], color[1], color[2], color[3]);
                            egui::color_picker::color_edit_button_srgba(ui, &mut c, egui::color_picker::Alpha::BlendOrAdditive);
                            *color = c.to_array();
                        });
                    };
                    edit_color(ui, "粗体行颜色（第1行）：", &mut self.project_config.about.bold_color);
                    edit_color(ui, "正常行颜色：", &mut self.project_config.about.normal_color);
                    edit_color(ui, "强调行颜色（后两行）：", &mut self.project_config.about.accent_color);

                    ui.add_space(16.0);
                    ui.separator();
                    ui.add_space(12.0);

                    ui.heading("项目警告");
                    ui.add_space(8.0);
                    ui.label(
                        "用编辑器打开本项目时弹出的作者提示文字（彩蛋/版权声明等）。\
                         留空则不弹出。玩家可在弹窗里选「不再显示」（仅本地生效）。",
                    );
                    ui.add_space(4.0);
                    ui.add_sized(
                        [ui.available_width(), 80.0],
                        egui::TextEdit::multiline(&mut self.project_config.warning)
                            .hint_text("例如：本作为心夏同人作品，角色立绘及背景等版权属于原作者"),
                    );

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
                        }); // 关闭 ScrollArea
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
                        egui::RichText::new("⚠ 警告")
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
                    self.project_root().join(&self.build.output_dir)
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
                    // 内容偏长，包竖向滚动避免小屏幕下被截断。
                    egui::ScrollArea::vertical()
                        .max_height(ctx.screen_rect().height() * 0.8)
                        .show(ui, |ui| {
                            ui.add_space(8.0);

                            ui.label(
                                egui::RichText::new("⚠ 系统未检测到 Rust/Cargo")
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
                        }); // 关闭 ScrollArea
                });

            if close {
                self.show_cargo_guide = false;
            }
        }

        // ---- 项目警告弹窗（作者留的提示，打开项目时显示）-------------------
        if self.show_project_warning {
            let mut close = false;
            let mut dismiss = false;

            egui::Window::new("项目提示")
                .collapsible(false)
                .resizable(false)
                .default_width(440.0)
                .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
                .show(ctx, |ui| {
                    ui.add_space(8.0);
                    ui.label(
                        egui::RichText::new("⚠ 作者提示")
                            .size(18.0)
                            .color(egui::Color32::from_rgb(255, 200, 100)),
                    );
                    ui.add_space(8.0);
                    // 警告正文：可能较长，包竖向滚动避免被窗口截断。
                    egui::ScrollArea::vertical()
                        .max_height(ctx.screen_rect().height() * 0.6)
                        .show(ui, |ui| {
                            ui.label(&self.project_config.warning);
                        });
                    ui.add_space(12.0);
                    ui.separator();
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.button("确定").clicked() {
                            close = true;
                        }
                        if ui.button("不再显示").clicked() {
                            dismiss = true;
                            close = true;
                        }
                    });
                });

            if dismiss {
                // 用 project_dir 作为忽略键，而非 work_dir——后者会被 open_file_path
                // 覆盖为 scripts/ 子目录，导致下次打开项目时 key 不匹配、警告重复弹出。
                let key = self.project_dir.as_deref().unwrap_or(&self.work_dir);
                self.dismissed_warnings.dismiss(key);
            }
            if close {
                self.show_project_warning = false;
            }
        }

        // ---- 退出确认对话框（点叉退出且有未保存修改时弹出）------------------
        if self.pending_exit {
            let mut choice: Option<ExitChoice> = None;
            egui::Window::new("确认退出")
                .collapsible(false)
                .resizable(false)
                .default_width(420.0)
                .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
                .show(ctx, |ui| {
                    ui.add_space(8.0);
                    ui.label(
                        egui::RichText::new("当前剧本有未保存的修改，退出前是否保存？")
                            .size(15.0),
                    );
                    ui.add_space(12.0);
                    ui.horizontal(|ui| {
                        if ui.button("保存").clicked() {
                            choice = Some(ExitChoice::Save);
                        }
                        if ui.button("不保存").clicked() {
                            choice = Some(ExitChoice::Discard);
                        }
                        if ui.button("取消").clicked() {
                            choice = Some(ExitChoice::Cancel);
                        }
                    });
                });
            match choice {
                Some(ExitChoice::Save) => {
                    // 保存成功才退出；保存失败则保留对话框并提示
                    self.save_file();
                    if !self.is_dirty() {
                        self.pending_exit = false;
                        self.force_close = true;
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                }
                Some(ExitChoice::Discard) => {
                    self.pending_exit = false;
                    self.force_close = true;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
                Some(ExitChoice::Cancel) => {
                    self.pending_exit = false;
                    self.force_close = false;
                }
                None => {}
            }
        }

        // ---- 「请先保存再预览」提示弹窗 --------------------------------------
        if self.show_save_reminder {
            egui::Window::new("无法预览")
                .collapsible(false)
                .resizable(false)
                .default_width(380.0)
                .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
                .show(ctx, |ui| {
                    ui.add_space(8.0);
                    ui.label(
                        egui::RichText::new("当前文件尚未保存到磁盘，无法预览。")
                            .size(16.0),
                    );
                    ui.add_space(6.0);
                    ui.label("请先按 Ctrl+S 保存文件（或用左栏文件名输入框命名后保存），再点击预览。");
                    ui.add_space(12.0);
                    ui.horizontal(|ui| {
                        if ui.button("知道了").clicked() {
                            self.show_save_reminder = false;
                        }
                    });
                });
        }

        // ---- 孤立注释无法迁移警告（保存/离开蓝图时若存在未连线注释则弹出）------
        if self.show_comment_warning {
            egui::Window::new("注释无法迁移")
                .collapsible(false)
                .resizable(false)
                .default_width(440.0)
                .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
                .show(ctx, |ui| {
                    ui.add_space(8.0);
                    ui.label(
                        egui::RichText::new("⚠ 以下注释节点未连线，无法迁移到脚本")
                            .size(16.0)
                            .color(egui::Color32::from_rgb(255, 200, 100)),
                    );
                    ui.add_space(6.0);
                    ui.label("蓝图中有注释节点既没有上方连线也没有下方连线，导出脚本时这些注释会被丢弃。请在蓝图中为它们连线（连上方则附在前驱之后，连下方则附在后继之前），否则注释不会出现在代码中。");
                    ui.add_space(8.0);
                    ui.label(
                        egui::RichText::new("被丢弃的注释：")
                            .color(egui::Color32::from_gray(180)),
                    );
                    egui::ScrollArea::vertical()
                        .max_height(180.0)
                        .show(ui, |ui| {
                            for c in &self.comment_warning_list {
                                ui.label(
                                    egui::RichText::new(c)
                                        .color(egui::Color32::from_gray(200))
                                        .monospace(),
                                );
                            }
                        });
                    ui.add_space(12.0);
                    ui.horizontal(|ui| {
                        if ui.button("知道了").clicked() {
                            self.show_comment_warning = false;
                            self.comment_warning_list.clear();
                        }
                    });
                });
        }

        // ---- 编辑器设置弹窗 --------------------------------------------------
        if self.show_editor_settings {
            let mut open = true;
            egui::Window::new("编辑器设置")
                .open(&mut open)
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
                .show(ctx, |ui| {
                    ui.add_space(6.0);
                    ui.checkbox(&mut self.editor_settings.open_last_file_on_startup, "进入编辑器默认打开上一次文件");
                    ui.checkbox(&mut self.editor_settings.auto_save_on_exit, "退出时自动保存文件不询问");
                    ui.checkbox(&mut self.editor_settings.auto_blueprint_on_startup, "进入编辑器自动启动蓝图模式");
                    ui.separator();
                    ui.horizontal(|ui| {
                        if ui.button("保存").clicked() {
                            self.editor_settings.save();
                            self.show_editor_settings = false;
                        }
                        if ui.button("取消").clicked() {
                            self.show_editor_settings = false;
                        }
                    });
                });
            if !open {
                self.show_editor_settings = false;
            }
        }

        // ---- 资源预览弹窗（双击左栏图片资源触发）-------------------------
        if let Some(path) = self.resource_preview_path.clone() {
            let mut open = true;
            egui::Window::new("资源预览")
                .open(&mut open)
                .collapsible(false)
                .resizable(true)
                .default_width(640.0)
                .default_height(480.0)
                .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
                .show(ctx, |ui| {
                    if let Some(name) = path.file_name() {
                        ui.label(format!("文件：{}", name.to_string_lossy()));
                    }
                    ui.separator();
                    // 纹理未缓存时尝试解码；成功后缓存避免每帧重新解码，
                    // 失败则显示错误（下一帧仍会重试，便于外部修复文件后即时看到）。
                    if self.resource_preview_texture.is_none() {
                        match image::open(&path) {
                            Ok(img) => {
                                let rgba = img.to_rgba8();
                                let (w, h) = (rgba.width() as usize, rgba.height() as usize);
                                let color_image =
                                    ColorImage::from_rgba_unmultiplied([w, h], rgba.as_raw());
                                let handle = ui.ctx().load_texture(
                                    "resource_preview",
                                    color_image,
                                    Default::default(),
                                );
                                self.resource_preview_texture = Some(handle);
                            }
                            Err(e) => {
                                ui.label(format!("无法加载图片：{}", e));
                            }
                        }
                    }
                    if let Some(handle) = &self.resource_preview_texture {
                        let tex_size = handle.size();
                        let tex_w = tex_size[0] as f32;
                        let tex_h = tex_size[1] as f32;
                        if tex_w > 0.0 && tex_h > 0.0 {
                            // 等比缩放到预览区可容纳尺寸（不超过 600×420），不放大。
                            let max_w = 600.0;
                            let max_h = 420.0;
                            let scale = (max_w / tex_w).min(max_h / tex_h).min(1.0);
                            let display_size = egui::Vec2::new(tex_w * scale, tex_h * scale);
                            ui.add(
                                egui::Image::from_texture(
                                    egui::load::SizedTexture::new(handle.id(), display_size),
                                ),
                            );
                        }
                    }
                });
            if !open {
                self.resource_preview_path = None;
                self.resource_preview_texture = None;
            }
        }
    }

}

/// 退出确认对话框的用户选择。
enum ExitChoice {
    Save,
    Discard,
    Cancel,
}

// ---------------------------------------------------------------------------
// 入口点
// ---------------------------------------------------------------------------

/// 启动 GUI 编辑器应用。
pub fn run_editor() -> Result<(), eframe::Error> {
    let icon = load_icon();
    let mut viewport = egui::ViewportBuilder::default()
        .with_inner_size(egui::Vec2::new(1280.0, 820.0));
    if let Some(icon) = icon {
        viewport = viewport.with_icon(icon);
    }
    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };
    eframe::run_native(
        "Akizuki*Rustgal 剧本编辑器",
        options,
        Box::new(|cc| {
            install_cjk_fonts(&cc.egui_ctx);
            Ok(Box::new(EditorApp::default()))
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
fn load_icon() -> Option<egui::IconData> {
    let data = include_bytes!("../../../assets/icon_kokona_64.bin");
    const W: u32 = 64;
    const H: u32 = 64;
    if data.len() == (W * H * 4) as usize {
        Some(egui::IconData {
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
/// 字符索引按 `char` 计数（与 egui `CCursor` 一致），非字节偏移。
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

/// 返回字符索引 `char_idx` 所在的 FlowMark 索引。
///
/// 光标位置为字符间隙；`char_idx` 落在 `[char_start, char_end]`（含端点）即视为停在该标记上。
fn mark_at_cursor(marks: &[FlowMark], char_idx: usize) -> Option<usize> {
    marks.iter().position(|m| char_idx >= m.char_start && char_idx <= m.char_end)
}

// ---------------------------------------------------------------------------
// 立绘指令行解析（Ctrl+点击跳转预览用）
// ---------------------------------------------------------------------------

/// `parse_sprite_line` 的解析结果。
struct ParsedSpriteLine {
    character_name: String,
    selected: String,
    x: f32,
    y: f32,
    scale: f32,
}

/// 解析剧本中的 `+ 角色 ...` 立绘指令行，提取预览参数。
///
/// 支持的语法（与 parser.rs 的 `parse_direction` 一致）：
/// - `+ 角色`
/// - `+ 角色 (pose)`
/// - `+ 角色 at 0.45,0.56`
/// - `+ 角色 at 0.45,0.56 size 1.10`
/// - `+ 角色 居中`（中文位置词）
/// - `+ 角色 (pose) swap`（差分更换）
///
/// 无 `at`/位置词时 x 默认 0.5，y 默认 1.0；无 `size` 时 scale 默认 1.0。
/// 有 pose 时 `selected` = pose，否则 `selected` = 角色名。
///
/// 非 `+` 开头的行返回 `None`。
fn parse_sprite_line(line: &str) -> Option<ParsedSpriteLine> {
    let trimmed = line.trim_start();
    if !trimmed.starts_with('+') {
        return None;
    }
    // 跳过 `+` 和后续空白。
    let rest = trimmed[1..].trim_start();
    if rest.is_empty() {
        return None;
    }

    // 读取角色名：到空格 / `(` / 行尾为止。
    let name_end = rest
        .find(|c: char| c.is_whitespace() || c == '(')
        .unwrap_or(rest.len());
    let character_name = rest[..name_end].to_string();
    if character_name.is_empty() {
        return None;
    }
    let mut remaining = rest[name_end..].trim_start();

    // 可选 pose：`(pose)`。
    let mut pose: Option<String> = None;
    if remaining.starts_with('(') {
        let close = remaining.find(')')?;
        let p = remaining[1..close].trim();
        if !p.is_empty() {
            pose = Some(p.to_string());
        }
        remaining = remaining[close + 1..].trim_start();
    }

    // 扫描剩余 token，提取 at / size / 位置词。
    let mut x = 0.5_f32;
    let mut y = 1.0_f32;
    let mut scale = 1.0_f32;

    let mut tokens = remaining.split_whitespace();
    while let Some(tok) = tokens.next() {
        match tok {
            "at" => {
                // `at x` 或 `at x,y`（百分比位置）。
                let val = tokens.next()?;
                let (xv, yv) = if let Some(comma) = val.find(',') {
                    let xv: f32 = val[..comma].trim().parse().ok()?;
                    let yv: f32 = val[comma + 1..].trim().parse().ok()?;
                    (xv, Some(yv))
                } else {
                    let xv: f32 = val.parse().ok()?;
                    (xv, None)
                };
                x = xv;
                if let Some(yv) = yv {
                    y = yv;
                }
            }
            "size" => {
                let val = tokens.next()?;
                let s: f32 = val.parse().ok()?;
                if s > 0.0 {
                    scale = s;
                }
            }
            // 位置词（与 Position::from_name + x_fraction 一致）。
            "left" | "居左" | "左" => x = 0.25,
            "center" | "centre" | "居中" | "中" => x = 0.5,
            "right" | "居右" | "右" => x = 0.75,
            // 其他词（enters/exits/swap/with transition/from 等）跳过。
            _ => {}
        }
    }

    let selected = pose.unwrap_or_else(|| character_name.clone());

    Some(ParsedSpriteLine {
        character_name,
        selected,
        x,
        y,
        scale,
    })
}

// ---------------------------------------------------------------------------
// 命令行解析（Ctrl+点击 @bg/@music 跳转预览用）
// ---------------------------------------------------------------------------

/// `parse_command_line` 的解析结果。
struct ParsedCommandLine {
    kind: String,   // "bg" / "music" 等
    target: String, // 资源名
}

/// 解析剧本中的 `@cmd target ...` 命令行，提取命令类型与目标资源名。
///
/// 支持：`@bg 背景名`、`@music 音乐名`。
/// 非 `@` 开头的行返回 `None`。
fn parse_command_line(line: &str) -> Option<ParsedCommandLine> {
    let trimmed = line.trim_start();
    if !trimmed.starts_with('@') {
        return None;
    }
    let rest = trimmed[1..].trim_start();
    // 读取命令名（到空格为止）。
    let cmd_end = rest.find(char::is_whitespace).unwrap_or(rest.len());
    let kind = rest[..cmd_end].to_string();
    let args = rest[cmd_end..].trim_start();
    // 取第一个参数作为目标资源名（可能是 ident 或 string）。
    let target = if args.starts_with('"') {
        // 字符串参数：取引号内内容。
        let close = args[1..].find('"')?;
        args[1..1 + close].to_string()
    } else {
        // ident 参数：到空格为止。
        let end = args.find(char::is_whitespace).unwrap_or(args.len());
        args[..end].to_string()
    };
    if target.is_empty() {
        return None;
    }
    Some(ParsedCommandLine { kind, target })
}

// ---------------------------------------------------------------------------
// 语法高亮
// ---------------------------------------------------------------------------

/// 为整个缓冲区构建带逐 token 着色的 `LayoutJob`。
///
/// `highlight_ranges` 为需要配对高亮的字符区间列表（全文 `char` 索引，含起止），
/// 通常包含光标所在 `=>`/`<=` 及其配对指令的字符范围。空列表表示不高亮。
fn highlight_code(text: &str, highlight_ranges: &[(usize, usize)]) -> egui::text::LayoutJob {
    let mut job = egui::text::LayoutJob::default();
    let lines: Vec<&str> = text.split('\n').collect();
    // 行首字符偏移与 `scan_flow_marks` 采用同一计数方案（每行 chars + 1 个换行符）。
    let mut char_offset = 0usize;
    for (idx, line) in lines.iter().enumerate() {
        highlight_line(&mut job, line, char_offset, highlight_ranges);
        char_offset += line.chars().count() + 1;
        if idx + 1 < lines.len() {
            // 重新插入被 `split` 消耗的换行符。
            job.append("\n", 0.0, text_format(COLOR_DEFAULT));
        }
    }
    job
}

/// 根据行首非空白 token 选择基础颜色。
fn line_base_color(trimmed: &str) -> egui::Color32 {
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

/// 将单行的着色片段追加到 `job`。
///
/// 在一行内，`"..."` 字符串字面量和行尾 `//` 注释始终使用各自的专用颜色；
/// 其余内容使用行的基础颜色。基于 `char` 操作（通过 `char_indices`）以保留多字节 UTF-8 内容。
///
/// `line_char_offset` 为该行行首在全文中的字符索引（与 `scan_flow_marks` 同口径），
/// 用于判断行首 `=>`/`<=` token 是否落在配对高亮区间内；命中时该 token 单独使用
/// [`highlight_format`] 着色，剩余部分仍按流程色处理。
fn highlight_line(
    job: &mut egui::text::LayoutJob,
    line: &str,
    line_char_offset: usize,
    highlight_ranges: &[(usize, usize)],
) {
    let trimmed = line.trim_start();
    let leading_ws = &line[..line.len() - trimmed.len()];
    if !leading_ws.is_empty() {
        job.append(leading_ws, 0.0, text_format(COLOR_DEFAULT));
    }

    // 流程行（=> / <=）的配对高亮：若行首 token 落在任一高亮区间内，
    // 用高亮格式单独着色 token，剩余部分按流程色继续着色。
    let flow_token = if trimmed.starts_with("=>") {
        Some("=>")
    } else if trimmed.starts_with("<=") {
        Some("<=")
    } else {
        None
    };
    if let Some(tok) = flow_token {
        let tok_char_start = line_char_offset + leading_ws.chars().count();
        let tok_char_end = tok_char_start + tok.chars().count();
        let highlighted = highlight_ranges
            .iter()
            .any(|(s, e)| *s <= tok_char_start && tok_char_end <= *e);
        if highlighted {
            job.append(tok, 0.0, highlight_format());
            highlight_remainder(job, &trimmed[tok.len()..], COLOR_FLOW);
            return;
        }
    }

    highlight_remainder(job, trimmed, line_base_color(trimmed));
}

/// 将一段文本按基础色着色追加到 `job`，处理 `"..."` 字符串字面量与行尾 `//` 注释。
fn highlight_remainder(job: &mut egui::text::LayoutJob, text: &str, base: egui::Color32) {
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let n = chars.len();

    let mut buf_start: Option<usize> = None;
    let mut buf_end: usize = 0;
    let mut i = 0;

    while i < n {
        let (bofs, c) = chars[i];

        if c == '"' {
            // 刷新待处理的基础着色文本，然后消费字符串字面量。
            if let Some(s) = buf_start {
                job.append(&text[s..buf_end], 0.0, text_format(base));
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
            job.append(&text[start..end_byte], 0.0, text_format(COLOR_STRING));
            continue;
        }

        if c == '/' && i + 1 < n && chars[i + 1].1 == '/' {
            // `//` 注释延续到行尾。
            if let Some(s) = buf_start {
                job.append(&text[s..buf_end], 0.0, text_format(base));
            }
            job.append(&text[bofs..], 0.0, text_format(COLOR_COMMENT));
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
        job.append(&text[s..buf_end], 0.0, text_format(base));
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

/// 构建配对高亮专用 `TextFormat`：流程色前景 + 亮黄背景，用于 `=>`/`<=` 配对标记。
fn highlight_format() -> egui::text::TextFormat {
    egui::text::TextFormat {
        font_id: egui::FontId::monospace(FONT_SIZE),
        color: COLOR_FLOW,
        background: COLOR_PAIR_HIGHLIGHT,
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
        // 光标在 "=>" 上（0..=2）
        assert_eq!(mark_at_cursor(&marks, 0), Some(0));
        assert_eq!(mark_at_cursor(&marks, 1), Some(0));
        assert_eq!(mark_at_cursor(&marks, 2), Some(0));
        // 光标在 " Sub" 中（3..6）不在标记上
        assert_eq!(mark_at_cursor(&marks, 3), None);
        // "=>" 占 0..2，'\n'=7，"<=" 在 8..10
        assert_eq!(mark_at_cursor(&marks, 8), Some(1));
        assert_eq!(mark_at_cursor(&marks, 9), Some(1));
    }

    #[test]
    fn highlight_preserves_text() {
        // LayoutJob 的 `text` 必须等于输入，以使光标位置对 TextEdit 有效。
        for src in [
            "",
            "hello",
            "# Title\nAki: \"Hi\"\n// comment\n$x = 1\n",
            "多行\n中文 \"字\" 符\n",
        ] {
            let job = highlight_code(src, &[]);
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
