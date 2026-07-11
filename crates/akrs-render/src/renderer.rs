//! Main renderer: game loop, drawing, and input handling.

use crate::assets::{AssetKind, AssetManager};
use akrs_core::ProjectConfig;
use akrs_runtime::{
    Engine, EngineEvent, EnginePhase, SceneState, BackgroundState, Settings, SettingsTab, SkipMode,
    TransitionPhase,
    format_play_time, format_timestamp,
    SaveMetadata, SaveSlot,
    crash,
};
use macroquad::audio::{play_sound, set_sound_volume, stop_sound, PlaySoundParams, Sound};
use macroquad::prelude::*;
use std::path::PathBuf;

/// 运行时主题配色（macroquad `Color` 形式）。
///
/// 由 `run()` 从 `ProjectConfig.theme` 构造后写入 thread_local 全局，
/// 供各绘制函数读取，避免给每个 `draw_*` 函数都加主题参数。
/// macroquad 主循环单线程，thread_local 访问安全且零争用。
#[derive(Clone, Copy)]
struct GameTheme {
    /// 主题色1：模态对话框/面板背景。
    primary: Color,
    /// 主题色2：按钮背景基色（悬停/按下态由 `shade` 派生）。
    secondary: Color,
    /// 文本色：按钮/HUD 文字。
    text: Color,
    /// 对话框色：游戏进行中文本框渐变基色（仅取 RGB）。
    dialogue: Color,
    /// 已读文字色：玩家看过的对话/旁白文字（默认浅紫）。
    read_text: Color,
    /// 未读文字色：玩家尚未看过的对话/旁白文字（默认白）。
    unread_text: Color,
}

impl Default for GameTheme {
    fn default() -> Self {
        Self {
            primary: Color::new(0.08, 0.06, 0.15, 0.97),
            secondary: Color::new(0.30, 0.55, 0.85, 0.90),
            text: Color::new(1.0, 1.0, 1.0, 1.0),
            dialogue: Color::new(0.55, 0.78, 0.95, 1.0),
            read_text: Color::new(0.784, 0.667, 0.902, 1.0),
            unread_text: Color::new(1.0, 1.0, 1.0, 1.0),
        }
    }
}

thread_local! {
    static GAME_THEME: std::cell::Cell<GameTheme> = std::cell::Cell::new(GameTheme::default());
}

/// 读取当前主题色。
fn theme() -> GameTheme {
    GAME_THEME.with(|t| t.get())
}

/// 写入当前主题色（`run()` 开头调用一次）。
fn set_theme(t: GameTheme) {
    GAME_THEME.with(|c| c.set(t));
}

/// 把 `[u8;4]` RGBA 转为 macroquad `Color`。
fn color_from_u8(v: [u8; 4]) -> Color {
    Color::new(v[0] as f32 / 255.0, v[1] as f32 / 255.0, v[2] as f32 / 255.0, v[3] as f32 / 255.0)
}

/// 计算并写入「有效主题色」：以项目主题（`ProjectConfig.theme`）为基础，
/// 叠加玩家设置中的主题色覆盖与已读/未读文字色。每帧调用一次，确保玩家改色即时生效。
fn apply_effective_theme(engine: &Engine, project_config: &ProjectConfig) {
    let p = &project_config.theme;
    let s = engine.settings();
    set_theme(GameTheme {
        primary: color_from_u8(s.theme_primary.unwrap_or(p.primary)),
        secondary: color_from_u8(s.theme_secondary.unwrap_or(p.secondary)),
        text: color_from_u8(p.text),
        dialogue: color_from_u8(s.theme_dialogue.unwrap_or(p.dialogue)),
        read_text: color_from_u8(s.read_text_color),
        unread_text: color_from_u8(s.unread_text_color),
    });
}

/// 配色标签页中可编辑的颜色字段。
#[derive(Debug, Clone, Copy, PartialEq)]
enum ColorField {
    /// 主题色1（面板背景）。
    ThemePrimary,
    /// 主题色2（按钮背景）。
    ThemeSecondary,
    /// 对话框色（文本框渐变）。
    ThemeDialogue,
    /// 已读文字色。
    ReadText,
    /// 未读文字色。
    UnreadText,
}

impl ColorField {
    /// 在配色标签页中的行索引（0..5）。
    fn index(self) -> usize {
        match self {
            Self::ThemePrimary => 0,
            Self::ThemeSecondary => 1,
            Self::ThemeDialogue => 2,
            Self::ReadText => 3,
            Self::UnreadText => 4,
        }
    }

    /// 是否为主题色字段（带「项目默认 / 自定义」开关）。
    fn is_theme(self) -> bool {
        matches!(self, Self::ThemePrimary | Self::ThemeSecondary | Self::ThemeDialogue)
    }

    /// 该字段当前的有效颜色（主题字段为 None 时回退项目默认）。
    fn value(self, settings: &Settings, project: &ProjectConfig) -> [u8; 4] {
        match self {
            Self::ThemePrimary => settings.theme_primary.unwrap_or(project.theme.primary),
            Self::ThemeSecondary => settings.theme_secondary.unwrap_or(project.theme.secondary),
            Self::ThemeDialogue => settings.theme_dialogue.unwrap_or(project.theme.dialogue),
            Self::ReadText => settings.read_text_color,
            Self::UnreadText => settings.unread_text_color,
        }
    }

    /// 主题字段是否处于「自定义」状态（Some）；已读/未读恒为 true。
    fn is_custom(self, settings: &Settings) -> bool {
        match self {
            Self::ThemePrimary => settings.theme_primary.is_some(),
            Self::ThemeSecondary => settings.theme_secondary.is_some(),
            Self::ThemeDialogue => settings.theme_dialogue.is_some(),
            _ => true,
        }
    }

    /// 写入颜色值。主题字段写入 Some；已读/未读直接写入。
    fn set(self, settings: &mut Settings, c: [u8; 4]) {
        match self {
            Self::ThemePrimary => settings.theme_primary = Some(c),
            Self::ThemeSecondary => settings.theme_secondary = Some(c),
            Self::ThemeDialogue => settings.theme_dialogue = Some(c),
            Self::ReadText => settings.read_text_color = c,
            Self::UnreadText => settings.unread_text_color = c,
        }
    }

    /// 主题字段：切换回「项目默认」（None）。已读/未读无操作。
    fn clear(self, settings: &mut Settings) {
        match self {
            Self::ThemePrimary => settings.theme_primary = None,
            Self::ThemeSecondary => settings.theme_secondary = None,
            Self::ThemeDialogue => settings.theme_dialogue = None,
            _ => {}
        }
    }
}

/// 配色标签页调色板预设（12 色，覆盖常见视觉小说用色）。
const COLOR_PALETTE: [[u8; 4]; 12] = [
    [255, 255, 255, 255], // 白
    [200, 200, 200, 255], // 浅灰
    [120, 120, 120, 255], // 灰
    [  0,   0,   0, 255], // 黑
    [255,  80,  80, 255], // 红
    [255, 160,  60, 255], // 橙
    [255, 220,  80, 255], // 黄
    [ 80, 220, 100, 255], // 绿
    [ 80, 200, 255, 255], // 青
    [ 80, 120, 255, 255], // 蓝
    [200, 170, 230, 255], // 浅紫（已读默认）
    [255, 180, 220, 255], // 粉
];

/// 把 `[u8;4]` 格式化为十六进制字符串。alpha 为 255 时输出 `#RRGGBB`，
/// 否则输出 `#RRGGBBAA`。
fn hex_string_from_color(c: [u8; 4]) -> String {
    if c[3] == 255 {
        format!("#{:02X}{:02X}{:02X}", c[0], c[1], c[2])
    } else {
        format!("#{:02X}{:02X}{:02X}{:02X}", c[0], c[1], c[2], c[3])
    }
}

/// 解析十六进制颜色字符串，支持 `#RRGGBB` 与 `#RRGGBBAA`（`#` 可选）。
/// 非法输入返回 None。
fn parse_hex_color(s: &str) -> Option<[u8; 4]> {
    let s = s.trim().trim_start_matches('#');
    let b = s.as_bytes();
    let h = |c: u8| -> Option<u8> {
        match c {
            b'0'..=b'9' => Some(c - b'0'),
            b'a'..=b'f' => Some(c - b'a' + 10),
            b'A'..=b'F' => Some(c - b'A' + 10),
            _ => None,
        }
    };
    let pair = |i: usize| -> Option<u8> { Some((h(b[i])? << 4) | h(b[i + 1])?) };
    match b.len() {
        6 => Some([pair(0)?, pair(2)?, pair(4)?, 255]),
        8 => Some([pair(0)?, pair(2)?, pair(4)?, pair(6)?]),
        _ => None,
    }
}

/// 基于基色提亮（`factor > 0`）或加深（`factor < 0`），alpha 保持不变。
/// 用于从 `secondary` 派生按钮的悬停/按下/默认三态颜色。
fn shade(c: Color, factor: f32) -> Color {
    let f = |v: f32| (v + factor).clamp(0.0, 1.0);
    Color::new(f(c.r), f(c.g), f(c.b), c.a)
}

/// Candidate system font paths, searched in order when the bundled font is
/// missing or unreadable.  The first existing file is loaded.
fn system_font_candidates() -> Vec<(&'static str, PathBuf)> {
    let mut cands: Vec<(&'static str, PathBuf)> = Vec::new();

    #[cfg(target_os = "windows")]
    {
        let win_fonts = std::env::var("WINDIR")
            .unwrap_or_else(|_| "C:\\Windows".to_string());
        let fonts_dir = PathBuf::from(&win_fonts).join("Fonts");
        cands.push(("微软雅黑", fonts_dir.join("msyh.ttc")));
        cands.push(("微软雅黑", fonts_dir.join("msyh.ttf")));
        cands.push(("微软雅黑粗体", fonts_dir.join("msyhbd.ttc")));
        cands.push(("SimSun 宋体", fonts_dir.join("simsun.ttc")));
        cands.push(("SimHei 黑体", fonts_dir.join("simhei.ttf")));
    }

    #[cfg(target_os = "macos")]
    {
        cands.push(("苹方", PathBuf::from("/System/Library/Fonts/PingFang.ttc")));
        cands.push(("苹方", PathBuf::from("/Library/Fonts/PingFang.ttc")));
        cands.push(("华文黑体", PathBuf::from("/System/Library/Fonts/STHeiti Light.ttc")));
        cands.push(("华文黑体", PathBuf::from("/System/Library/Fonts/Hiragino Sans GB.ttc")));
    }

    #[cfg(target_os = "linux")]
    {
        cands.push(("Noto Sans CJK SC", PathBuf::from("/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc")));
        cands.push(("Noto Sans CJK SC", PathBuf::from("/usr/share/fonts/opentype/noto-cjk-otf/NotoSansCJKsc-Regular.otf")));
        cands.push(("Noto Sans CJK SC", PathBuf::from("/usr/share/fonts/truetype/noto/NotoSansCJK-Regular.ttc")));
        cands.push(("文泉驿微米黑", PathBuf::from("/usr/share/fonts/truetype/wqy/wqy-microhei.ttc")));
        cands.push(("文泉驿微米黑", PathBuf::from("/usr/share/fonts/wenquanyi/wqy-microhei/wqy-microhei.ttc")));
        cands.push(("文泉驿正黑", PathBuf::from("/usr/share/fonts/wenquanyi/wqy-zenhei/wqy-zenhei.ttc")));
    }

    // Also check user-level font directories on every platform.
    if let Some(home) = dirs_or_home() {
        #[cfg(target_os = "windows")]
        cands.push(("用户字体", home.join("AppData/Local/Microsoft/Windows/Fonts/msyh.ttc")));
        #[cfg(target_os = "linux")]
        {
            cands.push(("用户字体", home.join(".fonts/NotoSansCJK-Regular.ttc")));
            cands.push(("用户字体", home.join(".local/share/fonts/NotoSansCJKsc-Regular.otf")));
        }
        #[cfg(target_os = "macos")]
        cands.push(("用户字体", home.join("Library/Fonts/PingFang.ttc")));
    }

    cands
}

/// Best-effort `HOME` / user-profile resolution without external crates.
fn dirs_or_home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

// 线程本地回退字体，用于 CJK 字形替换。
thread_local! {
    static FALLBACK_FONT: std::cell::RefCell<Option<Font>> = std::cell::RefCell::new(None);
}

fn set_fallback_font(font: Option<Font>) {
    FALLBACK_FONT.with(|f| *f.borrow_mut() = font);
}

fn get_fallback_font() -> Option<Font> {
    FALLBACK_FONT.with(|f| f.borrow().clone())
}

/// Check whether every character in `text` can be rendered by `font`.
fn can_render(text: &str, font: Option<Font>, size: u16) -> bool {
    if let Some(f) = font {
        text.chars().all(|c| measure_text(&c.to_string(), Some(f), size, 1.0).width > 0.0)
    } else {
        false
    }
}

/// Load the Chinese font, falling back through a priority chain.
///
/// Returns `(primary_font, fallback_font)`.  The primary font is the first
/// candidate that passes verification, tried in this order:
///   1. Runtime full-CJK font file (`assets/fonts/NotoSansCJK-Regular.ttc`).
///   2. Embedded subset font (compiled into the binary).
///   3. Platform system fonts.
/// The fallback font is the first *system* font that passes verification, so
/// that glyphs missing from the primary font can be drawn from the fallback.
fn load_font_with_fallback() -> (Option<Font>, Option<Font>) {
    /// Verify a loaded font by measuring a CJK character.
    fn verify_font(font: Font) -> Option<Font> {
        let dims = measure_text("世", Some(font), 32, 1.0);
        if dims.width > 0.0 {
            Some(font)
        } else {
            eprintln!("[akrs-render] Font loaded but CJK char width is 0, treating as unusable");
            None
        }
    }

    let mut primary: Option<Font> = None;
    let mut fallback: Option<Font> = None;

    // 1. 运行时 OTF 字体文件（最高优先级，覆盖所有 CJK 字形）。
    let runtime_font_path = "assets/fonts/SourceHanSansSC-Regular-2.otf";
    if let Ok(bytes) = std::fs::read(runtime_font_path) {
        match load_ttf_font_from_bytes(&bytes) {
            Ok(f) => {
                if let Some(f) = verify_font(f) {
                    eprintln!("[akrs-render] 中文字体已加载（运行时 OTF）");
                    primary = Some(f);
                }
            }
            Err(e) => {
                eprintln!("[akrs-render] 运行时 OTF 字体加载失败: {:?}", e);
            }
        }
    }

    // 2. 系统字体——填充 primary（如果仍为 None）和/或 fallback。
    for (name, path) in system_font_candidates() {
        if !path.exists() {
            continue;
        }
        match std::fs::read(&path) {
            Ok(bytes) => match load_ttf_font_from_bytes(&bytes) {
                Ok(f) => {
                    if let Some(f) = verify_font(f) {
                        if primary.is_none() {
                            eprintln!("[akrs-render] 无自定义字体，使用系统字体作为主字体: {}", name);
                            primary = Some(f);
                        } else if fallback.is_none() {
                            eprintln!("[akrs-render] 回退字体已加载: {}", name);
                            fallback = Some(f);
                        }
                        if primary.is_some() && fallback.is_some() {
                            break;
                        }
                    }
                }
                Err(e) => {
                    eprintln!("[akrs-render] 系统字体 {} 加载失败: {:?}，尝试下一个...", name, e);
                }
            },
            Err(e) => {
                eprintln!("[akrs-render] 无法读取系统字体 {}: {:?}", name, e);
            }
        }
    }

    if primary.is_none() && fallback.is_none() {
        eprintln!("[akrs-render] 无自定义字体且所有系统回退均失败，使用默认字体");
    }
    (primary, fallback)
}

/// Draw text with the loaded custom font, falling back to default.
///
/// If the primary font cannot render every character in the string, the
/// fallback font (set via `set_fallback_font`) is used instead.  If neither
/// font can render a character it is replaced with a white-square placeholder
/// (U+25A1) so the player sees a visible box rather than an empty gap.
fn draw_text_f(text: &str, x: f32, y: f32, font_size: f32, color: Color, font: &Option<Font>) {
    let size = font_size as u16;
    let fb = get_fallback_font();

    let primary_ok = can_render(text, *font, size);
    let fallback_ok = can_render(text, fb, size);

    let target = if primary_ok {
        font
    } else if fallback_ok {
        &fb
    } else {
        font
    };

    // If neither font can render the whole string, replace unrenderable
    // characters with a visible placeholder.
    let display_text: String = if !primary_ok && !fallback_ok {
        text.chars()
            .map(|c| {
                if can_render(&c.to_string(), *font, size) || can_render(&c.to_string(), fb, size) {
                    c
                } else {
                    '\u{25A1}'
                }
            })
            .collect()
    } else {
        text.to_string()
    };

    if let &Some(fnt) = target {
        draw_text_ex(&display_text, x, y, TextParams {
            font: fnt,
            font_size: size,
            font_scale: 1.0,
            color,
            ..Default::default()
        });
    } else {
        draw_text(&display_text, x, y, font_size, color);
    }
}

fn measure_text_f(text: &str, font: &Option<Font>, font_size: u16, font_scale: f32) -> TextDimensions {
    let fb = get_fallback_font();
    let primary_ok = can_render(text, *font, font_size);
    let fallback_ok = can_render(text, fb, font_size);
    let target = if primary_ok { *font } else if fallback_ok { fb } else { *font };
    measure_text(text, target, font_size, font_scale)
}

/// 设计基准分辨率，用于计算 UI 缩放因子。
const BASE_WIDTH: f32 = 1920.0;
const BASE_HEIGHT: f32 = 1080.0;

/// UI 缩放因子：基于窗口的**逻辑**像素尺寸相对于 1920×1080 设计基准的
/// 最小轴比例。
///
/// 为什么用逻辑像素而不是物理像素：macroquad 启用 `high_dpi` 后，绘制坐标
/// 系是**逻辑像素**（`screen_width()`/`screen_height()` 返回逻辑值），而
/// 帧缓冲区是物理像素。同一个逻辑字号在高 DPI 屏幕上会以更多物理像素
/// 渲染，物理上已经更大、更清晰——因此缩放因子只需适配逻辑画布大小即可。
///
/// 旧实现误用物理像素（`sw * dpi`）计算缩放，导致 2560×1600@150% 全屏时
/// 逻辑画布只有 1707×1067，却算出 1.33 的缩放，按 1.33× 放大的控件必然
/// 溢出逻辑画布，出现「控件大到拧麻花挤在一起」的现象。改用逻辑像素后，
/// 同一场景算出 0.89 的缩放，控件恰好填满画布，高 DPI 的物理放大由渲染
/// 管线负责，不再重复放大。
///
/// 全宽元素（对话框、设置面板）直接用 `sw`/`sh` 自适应，不会因 scale
/// 增大而溢出；固定尺寸元素（按钮、字号）相对屏幕边缘定位，同样安全。
fn ui_scale(sw: f32, sh: f32, _dpi: f32) -> f32 {
    let scale = (sw / BASE_WIDTH).min(sh / BASE_HEIGHT);
    // 钳制到合理范围：过小会导致 UI 不可读，过大在极端分辨率下可能堆叠溢出。
    scale.clamp(0.5, 2.5)
}

/// 计算合适的窗口尺寸（逻辑像素）：约占屏幕面积的 1/2，且不超过屏幕。
///
/// `screen_w`/`screen_h` 为屏幕**物理**像素；`dpi` 为显示器 DPI 倍率
/// （窗口创建前未知时传 1.0，保守估算）。返回逻辑像素尺寸，供
/// `request_new_screen_size` 使用。
fn calculate_window_size(screen_w: i32, screen_h: i32, dpi: f32) -> (i32, i32) {
    // 窗口物理尺寸约占屏幕 70%（面积过半），再换算为逻辑像素。
    let factor = 0.70;
    let phys_w = (screen_w as f32 * factor) as i32;
    let phys_h = (screen_h as f32 * factor) as i32;
    let log_w = (phys_w as f32 / dpi) as i32;
    let log_h = (phys_h as f32 / dpi) as i32;
    (log_w.max(640), log_h.max(360))
}

/// 获取屏幕物理像素尺寸（真实检测，不再瞎写）。
///
/// 委托给 `platform` 模块：Windows 用 `GetSystemMetrics`，Linux 解析
/// `xrandr`，其余平台回退 1920×1080。
fn get_screen_size() -> (i32, i32) {
    crate::platform::get_screen_size_physical()
}

/// 把用户期望的窗口分辨率（逻辑像素）夹取到屏幕能容纳的范围内。
///
/// 返回最终应使用的逻辑像素尺寸。规则：
/// - 若 `desired` 在屏幕物理尺寸的 95% 以内（按 DPI 换算后），原样使用；
/// - 否则按比例缩到 95% 以内，确保窗口边框和任务栏都有空间。
/// - 这同时保证 150% 缩放下选 2560×1600 也不会超屏。
fn clip_resolution_to_screen(desired: (u32, u32), screen_phys: (i32, i32), dpi: f32) -> (i32, i32) {
    let (dw, dh) = desired;
    if dw == 0 || dh == 0 {
        return calculate_window_size(screen_phys.0, screen_phys.1, dpi);
    }
    let dw_phys = dw as f32 * dpi;
    let dh_phys = dh as f32 * dpi;
    let max_w_phys = screen_phys.0 as f32 * 0.95;
    let max_h_phys = screen_phys.1 as f32 * 0.95;
    let scale_w = if dw_phys > max_w_phys { max_w_phys / dw_phys } else { 1.0 };
    let scale_h = if dh_phys > max_h_phys { max_h_phys / dh_phys } else { 1.0 };
    let s = scale_w.min(scale_h);
    ((dw as f32 * s) as i32, (dh as f32 * s) as i32)
}

/// UI state for menus and overlays.
#[derive(Debug, Clone, Copy, PartialEq)]
enum UiMode {
    Normal,
    SaveMenu,
    LoadMenu,
    SettingsMenu,
    /// Startup prompt shown when a crash-recovery autosave is detected.
    AutoSavePrompt,
    /// 确认对话框（用于返回标题、未应用设置退出等）。
    ConfirmDialog,
    /// 备注编辑弹窗（在存档页编辑某个槽位的玩家备注）。
    NoteEditDialog,
    /// 蓝屏错误界面（仿 Windows BSOD）：剧本编译/运行时错误等不可忽略
    /// 的错误发生时全屏弹出，显示报错模块/错误代码/原因分析/警告与
    /// 三个操作按钮（导出日志 / 尝试继续运行 / 退出引擎）。
    CrashScreen,
    /// 日志导出目录选择器：从蓝屏界面点击"导出日志"后进入，让玩家
    /// 选择导出目录。借鉴编辑器的应用内文件浏览器实现（无系统原生弹窗）。
    DirPicker,
}

/// Actions deferred to the swap point of a UI transition.
#[derive(Clone, Copy, Debug)]
enum PendingUiAction {
    None,
    StartGame,
    /// 从"继续游戏"存档槽位加载。
    ContinueGame,
    SaveSlot(usize),
    LoadSlot(usize),
    ContinueAutosave,
    DiscardAutosave,
    #[allow(dead_code)]
    BackToTitle,
    Quit,
    /// 应用设置更改并返回。
    ApplySettings,
    /// 放弃设置更改并返回。
    DiscardSettings,
    /// 故事结束后自动返回标题（淡入淡出，无按钮，不保留继续存档）。
    StoryEndToTitle,
    /// 进入隐藏结局的尾声剧本（索引到 engine.unlocked_endings_with_decl()）。
    /// 在 swap 点加载 epilogue .akrs 文件并开始播放。
    PlayEpilogue(usize),
}

/// 确认对话框的类型，用于显示不同的提示文本。
#[derive(Clone, Copy, Debug, PartialEq)]
enum ConfirmType {
    /// 返回标题界面的确认。
    BackToTitle,
    /// 未应用设置时退出设置菜单的确认。
    UnappliedSettings,
    /// 剧本文件错误警告（仅标题页降级模式下触发）。
    ScriptError,
}

/// Phase of a UI transition animation.
#[derive(Clone, Copy, Debug, PartialEq)]
enum UiTransPhase {
    Out, // First half: fading toward the swap point.
    In,  // Second half: fading back from the swap point.
}

/// UI transition state machine for smooth page switches.
/// Uses a cross-fade animation: a single 0.5s sweep where the overlay alpha
/// follows a bell curve (transparent → dim → transparent). The mode swap
/// happens at the midpoint (progress = 0.5), so the old and new screens
/// cross-fade without ever going to a pure black screen.
struct UiTransition {
    active: bool,
    phase: UiTransPhase,
    progress: f32, // 0.0 to 1.0 across the whole 0.5s transition
    target_mode: UiMode,
    pending: PendingUiAction,
}

impl UiTransition {
    fn new() -> Self {
        Self { active: false, phase: UiTransPhase::Out, progress: 0.0, target_mode: UiMode::Normal, pending: PendingUiAction::None }
    }

    /// Start a transition. If one is already active, the old one is
    /// fast-forwarded to completion (swap applied) before starting the new
    /// one from the Out phase, making transitions interruptible.
    fn start(&mut self, target: UiMode, pending: PendingUiAction) {
        self.active = true;
        self.phase = UiTransPhase::Out;
        self.progress = 0.0;
        self.target_mode = target;
        self.pending = pending;
    }

    /// Advance the transition by `dt` seconds.
    /// Returns `Some((target_mode, pending_action))` when the midpoint swap
    /// occurs (progress crosses 0.5), so the caller can apply the mode change
    /// and engine action. The transition completes (active = false) when
    /// progress reaches 1.0.
    fn update(&mut self, dt: f32) -> Option<(UiMode, PendingUiAction)> {
        if !self.active { return None; }
        // Total transition duration: 0.5s.
        let dur = 0.5;
        self.progress += dt / dur;
        // Swap at the midpoint (progress >= 0.5): flip phase and return the
        // pending action so the caller can swap the underlying screen.
        if self.phase == UiTransPhase::Out && self.progress >= 0.5 {
            self.phase = UiTransPhase::In;
            return Some((self.target_mode, std::mem::replace(&mut self.pending, PendingUiAction::None)));
        }
        if self.progress >= 1.0 {
            self.progress = 0.0;
            self.phase = UiTransPhase::Out;
            self.active = false;
        }
        None
    }

    /// Alpha (0.0–1.0) for the overlay drawn on top of the scene.
    /// 经典淡入淡出：Out 阶段 0→1（渐暗到全黑），In 阶段 1→0（从全黑渐亮）。
    /// 屏幕切换发生在 Out→In 的交界点（alpha=1.0），此时画面完全被黑色覆盖，
    /// 切换不可见，因此不会有"闪一下"的感觉。
    fn overlay_alpha(&self) -> f32 {
        if !self.active { return 0.0; }
        match self.phase {
            UiTransPhase::Out => self.progress * 2.0,     // 0.0 → 1.0
            UiTransPhase::In  => (1.0 - self.progress) * 2.0, // 1.0 → 0.0
        }
        .min(1.0)
        .max(0.0)
    }
}

/// 章节切换动画阶段。
#[derive(Clone, Copy, Debug, PartialEq)]
enum ChapterPhase {
    /// 全屏淡入淡出（0.5s）：先渐暗到全黑，再渐亮，遮盖章节切换的瞬时内容变化。
    Fade,
    /// 顶部通知从屏幕上方滑入（0.3s）。
    ToastIn,
    /// 顶部通知停留显示（1.0s）。
    ToastHold,
    /// 顶部通知向上滑出（0.3s）。
    ToastOut,
}

/// 章节切换动画状态机。
///
/// 由 `->` 章节跳转触发（引擎通过 `take_chapter_notify()` 通知）。
/// 依次播放：全屏淡入淡出 → 顶部通知滑入 → 停留 1s → 滑出。
/// 通知为白底（约 30% 透明，alpha≈0.7），深色文字，章节名与标题两行居中。
/// 文本过长时自动缩小字号以适配通知宽度。
struct ChapterAnimation {
    active: bool,
    phase: ChapterPhase,
    /// 当前阶段的进度 0.0..1.0。
    progress: f32,
    /// 章节名（`#` 后首个标识符）。
    name: String,
    /// 章节显示标题（name 之后的同行文本）。
    title: Option<String>,
}

/// 章节淡入淡出总时长（秒）。
const CHAPTER_FADE_DUR: f32 = 0.5;
/// 通知滑入时长（秒）。
const CHAPTER_TOAST_IN_DUR: f32 = 0.3;
/// 通知停留时长（秒）。
const CHAPTER_TOAST_HOLD_DUR: f32 = 1.0;
/// 通知滑出时长（秒）。
const CHAPTER_TOAST_OUT_DUR: f32 = 0.3;

impl ChapterAnimation {
    fn new() -> Self {
        Self { active: false, phase: ChapterPhase::Fade, progress: 0.0, name: String::new(), title: None }
    }

    /// 启动一次章节切换动画。若已有动画在进行，则替换为新章节内容并从头开始。
    fn start(&mut self, name: String, title: Option<String>) {
        self.active = true;
        self.phase = ChapterPhase::Fade;
        self.progress = 0.0;
        self.name = name;
        self.title = title;
    }

    /// 推进动画。返回值无意义（保留以便未来扩展）。
    fn update(&mut self, dt: f32) {
        if !self.active { return; }
        let dur = match self.phase {
            ChapterPhase::Fade => CHAPTER_FADE_DUR,
            ChapterPhase::ToastIn => CHAPTER_TOAST_IN_DUR,
            ChapterPhase::ToastHold => CHAPTER_TOAST_HOLD_DUR,
            ChapterPhase::ToastOut => CHAPTER_TOAST_OUT_DUR,
        };
        self.progress += dt / dur;
        if self.progress >= 1.0 {
            self.progress = 0.0;
            self.phase = match self.phase {
                ChapterPhase::Fade => ChapterPhase::ToastIn,
                ChapterPhase::ToastIn => ChapterPhase::ToastHold,
                ChapterPhase::ToastHold => ChapterPhase::ToastOut,
                ChapterPhase::ToastOut => { self.active = false; return; }
            };
        }
    }

    /// 全屏淡入淡出遮罩的 alpha（0.0..1.0）。仅 Fade 阶段非零。
    /// 前 0.25s 0→1（渐暗），后 0.25s 1→0（渐亮）。
    fn fade_alpha(&self) -> f32 {
        if !self.active || self.phase != ChapterPhase::Fade { return 0.0; }
        let p = self.progress;
        if p < 0.5 { p * 2.0 } else { (1.0 - p) * 2.0 }.clamp(0.0, 1.0)
    }

    /// 顶部通知的垂直偏移（像素，负值表示在屏幕上方之外）。
    /// ToastIn：从 -height 滑到 0；ToastHold：0；ToastOut：0 滑到 -height。
    /// `height` 为通知总高度（含内边距），由绘制函数传入。
    fn toast_offset(&self, height: f32) -> f32 {
        if !self.active { return 0.0; }
        match self.phase {
            ChapterPhase::ToastIn => -height * (1.0 - self.progress),
            ChapterPhase::ToastHold => 0.0,
            ChapterPhase::ToastOut => -height * self.progress,
            ChapterPhase::Fade => -height,
        }
    }

    /// 是否正在显示顶部通知（ToastIn/Hold/Out 阶段）。
    fn toast_visible(&self) -> bool {
        self.active && matches!(self.phase, ChapterPhase::ToastIn | ChapterPhase::ToastHold | ChapterPhase::ToastOut)
    }
}

/// Button layout for clickable regions.
struct ButtonRect {
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    #[allow(dead_code)]
    label: String,
    action: ButtonAction,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum ButtonAction {
    StartGame,
    /// 从"继续游戏"存档槽位加载（从游戏返回标题后保存的进度）。
    ContinueGame,
    LoadGame,
    Settings,
    Quit,
    SaveSlot(usize),
    LoadSlot(usize),
    BackToTitle,
    #[allow(dead_code)]
    BackToGame,
    CloseMenu,
    /// Load the crash-recovery autosave and resume the game.
    ContinueAutosave,
    /// Discard the crash-recovery autosave and stay on the title screen.
    DiscardAutosave,
    // ── In-game HUD quick actions ───
    /// Quick-save to the dedicated quick-save slot (`quicksave.json`).
    /// 独立于编号槽位，不覆盖存档页中的手动存档。
    QuickSave,
    /// Quick-load from the dedicated quick-save slot (`quicksave.json`).
    QuickLoad,
    /// Open the save menu from the HUD.
    OpenSaveMenu,
    /// Open the load menu from the HUD.
    OpenLoadMenu,
    /// Open the settings menu from the HUD.
    OpenSettings,
    /// Toggle hiding the dialogue box and HUD.
    ToggleHide,
    /// Toggle fast-forward mode (skip through dialogue quickly).
    FastForward,
    /// Toggle auto-play mode (advance dialogue automatically after a delay).
    AutoPlay,
    // ── Save/Load menu paging ───
    /// Go to the previous page of save/load slots.
    PrevPage,
    /// Go to the next page of save/load slots.
    NextPage,
    /// Extend the visible slot count by one page (used on the last page when
    /// more slots are still available beyond the currently displayed range).
    AddPage,
    // ── Confirm dialog actions ───
    /// 确认执行（返回标题/放弃设置等）。
    ConfirmYes,
    /// 取消确认对话框。
    ConfirmNo,
    // ── Note edit dialog actions ───
    /// 打开备注编辑弹窗（针对某个存档槽位）。
    EditNote(usize),
    /// 确认保存备注。
    NoteConfirm,
    /// 取消备注编辑。
    NoteCancel,
    // ── 蓝屏错误界面按钮 ───
    /// 导出最近日志到玩家选择的目录。
    CrashExportLog,
    /// 尝试继续运行（降级到标题页或返回标题）。
    CrashContinue,
    /// 退出引擎。
    CrashExit,
    // ── 日志导出目录选择器按钮 ───
    /// 进入上级目录。
    DirUp,
    /// 确认导出到当前目录。
    DirConfirm,
    /// 取消导出，返回蓝屏界面。
    DirCancel,
    /// 进入某个子目录（索引到 dir_picker_entries）。
    DirEntry(usize),
    // ── 隐藏结局 epilogue ───
    /// 从标题页进入隐藏结局的尾声剧本。
    /// 索引指向 `engine.unlocked_endings_with_decl()` 返回的列表。
    PlayEpilogue(usize),
}

/// 窗口配置 for macroquad。
/// 启用高 DPI 渲染以避免"假高清"模糊问题。
/// 窗口尺寸约为屏幕面积的 1/2，系统自动居中。
pub fn window_conf() -> macroquad::miniquad::conf::Conf {
    let (screen_w, screen_h) = get_screen_size();
    // 窗口创建前 DPI 未知，按 1.0 保守估算；启动后 run() 内会根据真实
    // DPI 重新调整尺寸并居中。
    let (win_w, win_h) = calculate_window_size(screen_w, screen_h, 1.0);

    macroquad::miniquad::conf::Conf {
        window_title: "Akizuki*Rustgal".to_string(),
        window_width: win_w,
        window_height: win_h,
        fullscreen: false,
        // 启用高 DPI 支持，确保渲染分辨率与显示分辨率匹配
        high_dpi: true,
        icon: Some(load_kokona_icon_or_fallback()),
        ..Default::default()
    }
}

/// Try to load the kokona.png icon; fall back to a programmatically-generated
/// crescent-moon icon if the PNG raw-RGBA data is missing or mismatched.
fn load_kokona_icon_or_fallback() -> macroquad::miniquad::conf::Icon {
    let small: [u8; 16 * 16 * 4] = match icon_bytes_from_raw(include_bytes!("../../../assets/icon_kokona_16.bin")) {
        Ok(b) => b,
        Err(_) => make_icon_16(),
    };
    let medium: [u8; 32 * 32 * 4] = match icon_bytes_from_raw(include_bytes!("../../../assets/icon_kokona_32.bin")) {
        Ok(b) => b,
        Err(_) => make_icon_32(),
    };
    let big: [u8; 64 * 64 * 4] = match icon_bytes_from_raw(include_bytes!("../../../assets/icon_kokona_64.bin")) {
        Ok(b) => b,
        Err(_) => make_icon_64(),
    };
    macroquad::miniquad::conf::Icon { small, medium, big }
}

/// Convert a raw byte slice to a fixed-size RGBA array.
fn icon_bytes_from_raw<const N: usize>(data: &[u8]) -> Result<[u8; N], ()> {
    if data.len() != N {
        return Err(());
    }
    let mut arr = [0u8; N];
    arr.copy_from_slice(data);
    Ok(arr)
}

/// Programmatic fallback icon: a crescent moon on an indigo background.
fn make_icon_16() -> [u8; 16 * 16 * 4] {
    let mut buf = [0u8; 16 * 16 * 4];
    for y in 0..16u32 { for x in 0..16u32 { let i = ((y*16+x)*4) as usize; let (r,g,b) = pixel_color(x as f32/16.0, y as f32/16.0); buf[i]=r; buf[i+1]=g; buf[i+2]=b; buf[i+3]=255; }}
    buf
}

fn make_icon_32() -> [u8; 32 * 32 * 4] {
    let mut buf = [0u8; 32 * 32 * 4];
    for y in 0..32u32 { for x in 0..32u32 { let i = ((y*32+x)*4) as usize; let (r,g,b) = pixel_color(x as f32/32.0, y as f32/32.0); buf[i]=r; buf[i+1]=g; buf[i+2]=b; buf[i+3]=255; }}
    buf
}

fn make_icon_64() -> [u8; 64 * 64 * 4] {
    let mut buf = [0u8; 64 * 64 * 4];
    for y in 0..64u32 { for x in 0..64u32 { let i = ((y*64+x)*4) as usize; let (r,g,b) = pixel_color(x as f32/64.0, y as f32/64.0); buf[i]=r; buf[i+1]=g; buf[i+2]=b; buf[i+3]=255; }}
    buf
}

fn pixel_color(fx: f32, fy: f32) -> (u8, u8, u8) {
    let (mut r, mut g, mut b) = (40u8, 30u8, 80u8);
    // Crescent moon
    let dist = ((fx - 0.40).powi(2) + (fy - 0.40).powi(2)).sqrt();
    let sdx = fx - 0.52;
    let sdist = (sdx.powi(2) + (fy - 0.35).powi(2)).sqrt();
    if dist <= 0.32 && !(sdist <= 0.27 && sdx > 0.0) {
        r = 220; g = 225; b = 245;
    }
    // Star
    if ((fx - 0.78).powi(2) + (fy - 0.18).powi(2)).sqrt() <= 0.03 {
        r = 200; g = 210; b = 255;
    }
    // Dewdrop
    if ((fx - 0.68).powi(2) + (fy - 0.72).powi(2)).sqrt() <= 0.05 {
        r = 100; g = 210; b = 225;
    }
    (r, g, b)
}

/// Number of save/load slots shown per page (2 rows × 4 columns).
const SLOTS_PER_PAGE: usize = 8;

// ─── HUD 自动隐藏/上浮下沉状态 ───
//
// 控制按钮组默认隐藏（下沉到屏幕底部之外），当鼠标移入触发区域时
// 平滑上浮显示，移出后平滑下沉恢复隐藏。reveal_progress 是 0..1 的
// 归一化进度：0 = 完全隐藏，1 = 完全显示。用指数平滑插值更新，
// 每帧只做一次浮点运算，不阻塞主循环。

/// HUD 按钮组的显隐动画状态。
struct HudVisibility {
    /// 0.0 = 完全隐藏（下沉），1.0 = 完全显示（上浮）。
    progress: f32,
}

impl HudVisibility {
    /// 下沉距离（按钮自身高度 + 一点边距），单位：像素（设计基准）。
    const SINK_PX: f32 = 74.0;
    /// 平滑系数：每帧进度向目标靠近的比例。值越大越快。
    /// 取 0.18 ≈ 约 8 帧（@60fps）走完 90% 距离，体感顺滑不拖沓。
    const SMOOTH: f32 = 0.18;
    /// 触发区域：在按钮组正上方额外延伸的高度，方便鼠标移入。
    const HOVER_BAND_PX: f32 = 36.0;

    fn new() -> Self {
        // 进入游戏时默认完全隐藏，无动画。
        Self { progress: 0.0 }
    }

    /// 每帧更新进度。`hovered` 表示鼠标是否在触发区域内。
    fn update(&mut self, hovered: bool, dt: f32) {
        let target = if hovered { 1.0 } else { 0.0 };
        // 帧率无关的指数平滑：alpha ∈ (0,1]，dt 越大 alpha 越接近 1。
        let alpha = 1.0 - (1.0 - Self::SMOOTH).powf(dt * 60.0);
        self.progress += (target - self.progress) * alpha;
        // 钳制，避免浮点漂移
        if self.progress < 0.001 { self.progress = 0.0; }
        if self.progress > 0.999 { self.progress = 1.0; }
    }

    /// 当前下沉偏移（像素，设计基准）。0 = 完全显示，SINK_PX = 完全下沉。
    fn sink_offset(&self) -> f32 {
        (1.0 - self.progress) * Self::SINK_PX
    }

    /// 当前整体透明度（0..1）。隐藏时趋近 0，显示时为 1。
    fn alpha(&self) -> f32 {
        self.progress
    }

    /// 按钮是否实质可见（用于点击命中判定）。低于 0.5 视为不可交互，
    /// 避免在半隐藏状态下误触。
    fn is_interactable(&self) -> bool {
        self.progress > 0.5
    }
}

/// Entry point: launch the game with a macroquad window.
///
/// This is an async function that must be called from a `#[macroquad::main]` async main:
///
/// ```ignore
/// #[macroquad::main(akrs_render::window_conf())]
/// async fn main() {
///     let engine = Engine::new(SCRIPT).unwrap();
///     akrs_render::run(engine).await;
/// }
/// ```
pub async fn run(mut engine: Engine, project_config: &ProjectConfig) {
    // 启动时先用白色填充，避免"先黑一帧再渲染"的视觉瑕疵。
    clear_background(WHITE);
    next_frame().await;

    // 应用项目配置的初始窗口大小和全屏状态。
    // 窗口标题受限于 miniquad 0.3 无运行时 API，暂无法动态修改，
    // 保留在 ProjectConfig.window_title 字段中，待后续升级启用。
    if project_config.start_fullscreen {
        set_fullscreen(true);
    } else {
        // 用真实 DPI 重新计算窗口尺寸并居中。
        // window_conf() 创建窗口时 DPI 未知，按 1.0 估算，
        // 此处拿到真实 DPI 后修正，确保 150% 缩放下不会超屏。
        let dpi = macroquad::window::dpi_scale();
        let screen_phys = get_screen_size();
        let (final_w, final_h) = clip_resolution_to_screen(
            project_config.default_resolution,
            screen_phys,
            dpi,
        );
        request_new_screen_size(final_w as f32, final_h as f32);
        // 物理像素尺寸用于平台层居中调用（Windows 用 SetWindowPos）。
        crate::platform::center_window_on_screen(
            (final_w as f32 * dpi) as i32,
            (final_h as f32 * dpi) as i32,
        );
    }

    let mut assets = AssetManager::new();
    // 预加载关于页头像（kokona.png，位于项目根目录），加载失败时为 None，关于页画占位框。
    // 路径 ../kokona.png 相对 assets/ 基目录解析为项目根目录的 kokona.png。
    let about_icon_texture = assets.get_texture(AssetKind::Title, "../kokona.png").await;
    // Load Chinese font for proper CJK text rendering, with system-font fallback.
    let (font, fallback_font) = load_font_with_fallback();
    set_fallback_font(fallback_font);
    // Intercept the window-close (X) button so we can autosave before exiting.
    prevent_quit();
    // Load persistent settings (text speed, volume, etc.) before starting so
    // the player's preferences from the previous session are honored.
    engine.load_settings();
    // load_settings() 从磁盘覆盖了 settings.language / settings.ui_language，
    // 而 main.rs 中 load_language() 设置的 translations_dir 仍保留。
    // 这里按刷新后的设置重新加载翻译器，使「设置态」与「已加载翻译表」一致。
    // 若 settings.language 为空，effective_language() 会走系统语言自动检测
    // （Windows 上现在用 GetUserDefaultLocaleName，不再永远返回 en-US）。
    engine.reload_language();
    engine.reload_ui_language();
    // 如果设置中开启了「显示终端调试输出」，为当前进程分配控制台窗口。
    // Windows 上默认 windows_subsystem = "windows" 无控制台；开启后调用
    // AllocConsole 重新分配，使 println!/eprintln! 输出可见。
    if engine.settings().debug_terminal {
        let _ = crate::platform::try_alloc_console();
    }
    // 启动时一次性升级所有旧格式存档（无 scene 字段）：
    // 旧存档通过 rebuild_scene_to 从入口重放重建场景持久态并回写。
    // 这样存档页缩略图能正常渲染背景与立绘，读档也不再黑屏。
    // 仅扫描常规槽位；autosave/continue/quicksave 在各自读档路径按需升级。
    engine.upgrade_all_legacy_saves();
    // If a crash-recovery autosave exists from a previous run, prompt the
    // player to resume before showing the title screen.
    let mut ui_mode = if engine.has_autosave() {
        UiMode::AutoSavePrompt
    } else {
        UiMode::Normal
    };
    // 若启动时已带崩溃信息（如剧本编译失败），蓝屏界面优先于一切。
    if engine.crash_info().is_some() {
        ui_mode = UiMode::CrashScreen;
    }
    let mut buttons: Vec<ButtonRect> = Vec::new();
    // 当前正在循环播放的 BGM 句柄（None 表示无 BGM 在播放）。
    // 切换 BGM 时先 stop 旧的，再 play 新的。
    let mut current_bgm: Option<Sound> = None;
    // 当前正在播放的语音句柄（None 表示无语音在播放）。
    // 新对话/旁白出现时先 stop 旧的，再 play 新的（一次性，不循环）。
    let mut current_voice: Option<Sound> = None;
    // 上一帧应用的 BGM 音量，用于检测设置变更并实时同步。
    let mut prev_bgm_volume: f32 = engine.settings().bgm_volume;
    // 上一帧应用的语音音量，用于检测设置变更并实时同步。
    let mut prev_voice_volume: f32 = engine.settings().voice_volume;
    // 标题音乐是否已开始播放。
    let mut title_music_played = false;
    // Whether the in-game dialogue box and HUD button group are hidden via
    // the "隐藏" button. The scene (background + characters) is still drawn.
    let mut hud_hidden = false;
    // HUD 控制按钮组的自动显隐状态：默认隐藏（下沉），鼠标悬停触发区域时上浮。
    let mut hud_visibility = HudVisibility::new();
    // Which slider (if any) is currently being dragged in the settings menu.
    // The value is (tab, index).
    let mut dragging_slider: Option<(SettingsTab, usize)> = None;
    // Whether the resolution dropdown in the settings menu is expanded.
    let mut dropdown_open: bool = false;
    // Whether the skip mode dropdown in the settings menu is expanded.
    let mut skip_dropdown_open: bool = false;
    // Whether the UI language dropdown in the settings menu is expanded.
    let mut ui_lang_dropdown_open: bool = false;
    // Whether the script language dropdown in the settings menu is expanded.
    let mut lang_dropdown_open: bool = false;
    // 当前激活的设置标签页。
    let mut settings_active_tab: SettingsTab = SettingsTab::Text;
    // Current page index for the save / load menus (grid paging).
    let mut save_page: usize = 0;
    let mut load_page: usize = 0;
    // Number of slots currently surfaced to the player in each menu. Starts at
    // 24 (3 pages) and grows by one page per "+" click up to the manager's
    // max_slots, so the save grid is effectively infinite.
    let mut save_displayed_slots: usize = 24;
    let mut load_displayed_slots: usize = 24;
    // UI transition state machine for smooth page switches.
    let mut ui_transition = UiTransition::new();
    // 章节切换动画状态机（由 `->` 章节跳转触发）。
    let mut chapter_anim = ChapterAnimation::new();
    // 上一次实际应用到窗口的全屏状态。
    // window_conf() 中 fullscreen 初始化为 false，故此处同步为 false。
    // 每帧检测 settings.fullscreen 是否与此值不一致，若不一致则切换窗口全屏状态，
    // 这样既能响应设置菜单中的切换，也能在启动时应用上次保存的偏好。
    let mut last_fullscreen_applied: bool = false;
    // 已应用的全屏设置值。只有点击"应用"按钮时才会更新此值。
    // 启动时用 engine.settings().fullscreen 初始化（应用上次保存的偏好）。
    let mut applied_fullscreen: bool = engine.settings().fullscreen;
    // 待应用的分辨率变更标志。仅在用户点击"应用"按钮时置 true，
    // 主循环消费后置 false。主循环读取 engine.settings().resolution 并夹取到
    // 屏幕能容纳的范围内，再调整窗口尺寸并居中。
    let mut pending_resolution_apply: bool = false;
    // 设置菜单进入时保存的设置快照，用于检测是否有未应用的更改。
    let mut settings_snapshot: Option<Settings> = None;
    // 确认对话框的类型（返回标题/未应用设置退出）。
    let mut confirm_type: Option<ConfirmType> = None;
    // 确认对话框返回后应切换到的 UI 模式。
    let mut confirm_return_mode: UiMode = UiMode::Normal;
    // 是否有"继续游戏"存档。
    let mut has_continue_save: bool = engine.has_continue_save();
    // 当前是否正在播放隐藏结局的尾声剧本（epilogue）。
    // 为 true 时，故事结束/返回标题会从 main_source 重建主引擎而非 epilogue 源码。
    let mut epilogue_active: bool = false;
    // 主剧本源码快照。进入 epilogue 前保存主剧本源码，epilogue 结束返回标题时
    // 据此重建主引擎（epilogue 引擎的 source() 是尾声剧本，不能用来重建标题页）。
    let mut main_source: String = engine.source().to_string();
    // 进入设置菜单前的 UI 模式（用于返回时恢复）。
    let mut settings_prev_mode: UiMode = UiMode::Normal;
    // 备注编辑弹窗状态：正在编辑的槽位号 + 当前输入缓冲区。
    // 进入弹窗时从存档读取已有备注作为初始值，确认时写回。
    let mut note_edit_slot: Option<usize> = None;
    let mut note_edit_buffer: String = String::new();
    // 配色标签页：正在编辑十六进制的字段（None 表示无字段处于编辑态）。
    let mut color_edit_active: Option<ColorField> = None;
    // 配色标签页：hex 输入缓冲区（仅在 color_edit_active 为 Some 时有意义）。
    let mut color_hex_buffer: String = String::new();
    // 备注编辑弹窗返回后应切换到的 UI 模式（SaveMenu 或 LoadMenu）。
    let mut note_return_mode: UiMode = UiMode::SaveMenu;
    // 模态弹窗（确认对话框 / 备注编辑弹窗）的淡入进度（0.0 → 1.0）。
    // 弹窗打开时在约 0.2 秒内由 0 升至 1，使弹窗内容平滑淡入，
    // 避免瞬间弹出带来的突兀感。弹窗关闭时直接归零（关闭后切回的页面
    // 若发生切换则由 ui_transition 负责，若回到原页则瞬间消失亦可接受）。
    let mut dialog_fade: f32 = 0.0;

    // ── 蓝屏错误界面 / 日志导出目录选择器状态 ───
    // 蓝屏界面与目录选择器的淡入进度（0.0 → 1.0，约 0.3 秒）。
    let mut crash_fade: f32 = 0.0;
    // 目录选择器：当前所在目录。
    let mut dir_picker_current: PathBuf =
        std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    // 目录选择器：当前目录下的子目录条目（仅目录，文件不列出）。
    let mut dir_picker_entries: Vec<String> = Vec::new();
    // 目录选择器：滚动偏移（以"行"为单位，支持鼠标滚轮）。
    let mut dir_picker_scroll: f32 = 0.0;
    // 目录选择器：状态提示（导出成功/失败信息），空表示无提示。
    let mut dir_picker_status: String = String::new();

    // 开屏页（标题页）资源：优先使用 project.json 中配置的 title_music /
    // title_background，留空则回退到默认的 title_bgm.mp3 / title.png。
    // demo 不含音频资源，因此默认情况下标题页静音；功能完整支持自定义。
    let title_music_name = if project_config.title_music.is_empty() {
        "title_bgm.mp3".to_string()
    } else {
        project_config.title_music.clone()
    };
    let title_bg_name = if project_config.title_background.is_empty() {
        "./title.png".to_string()
    } else {
        project_config.title_background.clone()
    };
    if !assets.check_music(&title_music_name) {
        eprintln!("[Warning] 开屏页音乐 {} 未找到 — 标题页将静音", title_music_name);
    }

    // 主题配色：从 project.json 的 theme 字段解析，叠加玩家设置中的已读/未读文字色，
    // 写入 thread_local 供绘制函数读取。每帧重新计算，使玩家改色即时生效。
    apply_effective_theme(&engine, project_config);

    loop {
        let dt = get_frame_time();
        let (sw, sh) = (screen_width(), screen_height());
        // 当前显示器真实 DPI 倍率（如 150% 缩放返回 1.5）。
        let dpi = macroquad::window::dpi_scale();
        // UI scale factor relative to the 1920×1080 design baseline.
        // 基于物理像素计算，避免高 DPI 下 UI 过小。
        let scale = ui_scale(sw, sh, dpi);

        // 每帧重算有效主题色：玩家在设置页改色（含已读/未读文字色）后立即生效，
        // 也覆盖应用/放弃设置、读档、返回标题等引擎重建路径。
        apply_effective_theme(&engine, project_config);

        // 全屏状态同步：使用 applied_fullscreen（已应用的值）而非正在编辑的值。
        // 这样设置菜单中切换全屏开关不会立即生效，只有点击"应用"后才生效。
        // 从全屏恢复为窗口时，恢复为屏幕面积的 ~70% 大小并居中。
        {
            let want_fullscreen = applied_fullscreen;
            if want_fullscreen != last_fullscreen_applied {
                set_fullscreen(want_fullscreen);
                // 如果从全屏恢复为窗口模式，重新计算窗口大小并居中
                if !want_fullscreen && last_fullscreen_applied {
                    let (screen_w, screen_h) = get_screen_size();
                    let (win_w, win_h) = calculate_window_size(screen_w, screen_h, dpi);
                    request_new_screen_size(win_w as f32, win_h as f32);
                    // 物理像素尺寸 = 逻辑 × DPI，用于平台层居中调用
                    crate::platform::center_window_on_screen(
                        (win_w as f32 * dpi) as i32,
                        (win_h as f32 * dpi) as i32,
                    );
                }
                last_fullscreen_applied = want_fullscreen;
            }
        }

        // 分辨率同步：仅当不在全屏模式、且用户在设置中改了分辨率并点击应用后生效。
        // 关键：用 clip_resolution_to_screen 把期望分辨率夹取到屏幕能容纳的范围内，
        // 这样在 150% 系统缩放下选 2560×1600 也不会超屏。
        if pending_resolution_apply && !applied_fullscreen {
            let screen_phys = get_screen_size();
            let (win_w, win_h) = clip_resolution_to_screen(
                engine.settings().resolution,
                screen_phys,
                dpi,
            );
            request_new_screen_size(win_w as f32, win_h as f32);
            crate::platform::center_window_on_screen(
                (win_w as f32 * dpi) as i32,
                (win_h as f32 * dpi) as i32,
            );
            pending_resolution_apply = false;
        }

        // The player clicked the window's close button. Autosave the current
        // progress (unless we're on the title screen or the story has ended)
        // and then exit immediately.
        if is_quit_requested() {
            if engine.phase() != EnginePhase::Title {
                let _ = engine.save_autosave();
            }
            // Persist settings so they are restored on the next launch.
            let _ = engine.save_settings();
            std::process::exit(0);
        }

        // Update engine
        let events = engine.update(dt);

        // Process events
        for event in &events {
            match event {
                EngineEvent::MusicChanged { name } => {
                    // 先停止当前 BGM（无论 name 是否为空）
                    if let Some(bgm) = current_bgm.take() {
                        stop_sound(bgm);
                    }
                    if !name.is_empty() {
                        // 加载并循环播放新 BGM
                        if let Some(sound) = assets.get_sound(AssetKind::Music, name).await {
                            let vol = engine.settings().bgm_volume;
                            play_sound(
                                sound,
                                PlaySoundParams { looped: true, volume: vol },
                            );
                            current_bgm = Some(sound);
                        }
                    }
                }
                EngineEvent::SoundPlayed { name } => {
                    // 一次性播放音效
                    if let Some(sound) = assets.get_sound(AssetKind::Sound, name).await {
                        let vol = engine.settings().sfx_volume;
                        play_sound(
                            sound,
                            PlaySoundParams { looped: false, volume: vol },
                        );
                    }
                }
                EngineEvent::VoicePlayed { name } => {
                    // 先停止当前语音（无论 name 是否为空）
                    if let Some(voice) = current_voice.take() {
                        stop_sound(voice);
                    }
                    if !name.is_empty() {
                        // 加载并播放新语音（一次性，不循环）
                        if let Some(sound) = assets.get_sound(AssetKind::Voice, name).await {
                            let vol = engine.settings().voice_volume;
                            play_sound(
                                sound,
                                PlaySoundParams { looped: false, volume: vol },
                            );
                            current_voice = Some(sound);
                        }
                    }
                }
                EngineEvent::GameStarted => {
                    // 玩家从标题进入游戏，停止标题音乐与残留语音
                    if let Some(bgm) = current_bgm.take() {
                        stop_sound(bgm);
                    }
                    if let Some(voice) = current_voice.take() {
                        stop_sound(voice);
                    }
                }
                EngineEvent::StoryEnded => {
                    // 故事结束，停止所有 BGM 与语音
                    if let Some(bgm) = current_bgm.take() {
                        stop_sound(bgm);
                    }
                    if let Some(voice) = current_voice.take() {
                        stop_sound(voice);
                    }
                }
                EngineEvent::Warning { message } => {
                    eprintln!("[Engine Warning] {}", message);
                    crash::push_log(format!("[Engine Warning] {}", message));
                }
                EngineEvent::Error { message } => {
                    eprintln!("[Engine Error] {}", message);
                    crash::push_log(format!("[Engine Error] {}", message));
                    // 游戏进行中（非标题/菜单）的运行时错误触发蓝屏界面。
                    // 标题/菜单阶段（如读档失败）的 Error 不触发蓝屏——
                    // 那些是可恢复的局部错误，已在各自 UI 中处理。
                    let in_game = matches!(
                        engine.phase(),
                        EnginePhase::Running | EnginePhase::Transitioning
                            | EnginePhase::Waiting | EnginePhase::ChoicePending
                    );
                    if in_game
                        && ui_mode != UiMode::CrashScreen
                        && ui_mode != UiMode::DirPicker
                        && engine.crash_info().is_none()
                    {
                        engine.set_crash_info(Some(crash::CrashInfo {
                            module_key: "error.module.runtime",
                            code: crash::error_code::SCRIPT_RUNTIME,
                            can_continue: true,
                        }));
                        ui_mode = UiMode::CrashScreen;
                        crash_fade = 0.0;
                    }
                }
                _ => {}
            }
        }

        // 取走待处理的章节切换通知（由 `->` 章节跳转设置），启动章节动画。
        // 仅在游戏中（非标题/菜单）播放，避免标题页或读档瞬间触发。
        if let Some(notify) = engine.take_chapter_notify() {
            if engine.phase() != EnginePhase::Title {
                chapter_anim.start(notify.name, notify.title);
            }
        }
        // 推进章节动画。
        chapter_anim.update(dt);

        // Handle title music：进入标题画面时循环播放开屏页音乐（若存在）。
        // 文件名取自 project.json 的 title_music（留空回退 title_bgm.mp3）。
        // 离开标题时重置 title_music_played，使玩家从游戏返回标题时能重新播放。
        if engine.phase() == EnginePhase::Title {
            if !title_music_played {
                title_music_played = true;
                if current_bgm.is_none() {
                    if let Some(sound) = assets.get_sound(AssetKind::Music, &title_music_name).await {
                        let vol = engine.settings().bgm_volume;
                        play_sound(
                            sound,
                            PlaySoundParams { looped: true, volume: vol },
                        );
                        current_bgm = Some(sound);
                    }
                }
            }
        } else {
            // 离开标题阶段，重置标志，下次回到标题时会重新检测并播放
            title_music_played = false;
        }

        // 实时同步 BGM 音量：当设置页调整 BGM 音量时立即生效
        let cur_bgm_vol = engine.settings().bgm_volume;
        if cur_bgm_vol != prev_bgm_volume {
            if let Some(bgm) = current_bgm {
                set_sound_volume(bgm, cur_bgm_vol);
            }
            prev_bgm_volume = cur_bgm_vol;
        }
        // 实时同步语音音量：当设置页调整语音音量时立即生效
        let cur_voice_vol = engine.settings().voice_volume;
        if cur_voice_vol != prev_voice_volume {
            if let Some(voice) = current_voice {
                set_sound_volume(voice, cur_voice_vol);
            }
            prev_voice_volume = cur_voice_vol;
        }

        // Clear buttons for this frame
        buttons.clear();

        // 模态弹窗淡入进度更新：弹窗激活时 0.2 秒内升至 1，否则归零。
        if ui_mode == UiMode::ConfirmDialog || ui_mode == UiMode::NoteEditDialog {
            dialog_fade = (dialog_fade + dt / 0.2).min(1.0);
        } else {
            dialog_fade = 0.0;
        }
        // 蓝屏界面 / 目录选择器淡入进度更新：0.3 秒内升至 1，否则归零。
        if ui_mode == UiMode::CrashScreen || ui_mode == UiMode::DirPicker {
            crash_fade = (crash_fade + dt / 0.3).min(1.0);
        } else {
            crash_fade = 0.0;
        }

        // 故事结束时自动淡入淡出返回标题（无需任何按钮或提示）。
        // 检测刚进入 StoryEnded 阶段且尚未开始过渡的情况。
        if engine.phase() == EnginePhase::StoryEnded
            && ui_mode == UiMode::Normal
            && !ui_transition.active
        {
            ui_transition.start(UiMode::Normal, PendingUiAction::StoryEndToTitle);
        }

        // Draw based on phase and UI mode
        clear_background(BLACK);

        // Update UI transition. Returns Some((mode, action)) at the swap point.
        if let Some((target_mode, pending)) = ui_transition.update(dt) {
            ui_mode = target_mode;
            match pending {
                PendingUiAction::None => {}
                PendingUiAction::StartGame => {
                    engine.start_game();
                    hud_hidden = false;
                    // 开始新游戏时清除"继续游戏"存档
                    let _ = engine.delete_continue();
                    has_continue_save = false;
                }
                PendingUiAction::ContinueGame => {
                    let _ = engine.load_continue();
                    hud_hidden = false;
                }
                PendingUiAction::SaveSlot(slot) => {
                    engine.save(slot);
                }
                PendingUiAction::LoadSlot(slot) => {
                    if engine.saves().has_save(slot) {
                        engine.load(slot);
                        hud_hidden = false;
                    }
                }
                PendingUiAction::ContinueAutosave => {
                    let _ = engine.load_autosave();
                    let _ = engine.delete_autosave();
                    hud_hidden = false;
                }
                PendingUiAction::DiscardAutosave => {
                    let _ = engine.delete_autosave();
                }
                PendingUiAction::BackToTitle => {
                    // epilogue 模式下：从尾声剧本返回标题，从 main_source 重建主引擎，
                    //   不保存继续存档（尾声不应成为主游戏的继续点），清除 epilogue 标记。
                    // 普通模式下：保存"继续游戏"存档后重建引擎。
                    // 重建前先把本会话新增的已读历史落盘（Engine::new 会重新加载）。
                    engine.save_read_history();
                    if epilogue_active {
                        epilogue_active = false;
                        let source = main_source.clone();
                        let saved_settings = engine.settings().clone();
                        let saved_translations_dir = engine.translations_dir().map(|p| p.to_path_buf());
                        let saved_title = engine.scene().title.clone();
                        let saved_subtitle = engine.scene().subtitle.clone();
                        if let Ok(mut new_engine) = Engine::new(&source) {
                            *new_engine.settings_mut() = saved_settings;
                            if let Some(dir) = saved_translations_dir {
                                new_engine.restore_translations(dir);
                            }
                            new_engine.set_title(saved_title, saved_subtitle);
                            engine = new_engine;
                            title_music_played = false;
                        }
                    } else {
                        // 保存"继续游戏"存档
                        let _ = engine.save_continue();
                        has_continue_save = true;
                        let source = engine.source().to_string();
                        let saved_settings = engine.settings().clone();
                        // 保存翻译目录，重建引擎后恢复（Engine::new 不会继承 translations_dir，
                        // 若不恢复则返回标题后 UI 翻译重置为英文且无法切换语言）。
                        let saved_translations_dir = engine.translations_dir().map(|p| p.to_path_buf());
                        if let Ok(mut new_engine) = Engine::new(&source) {
                            *new_engine.settings_mut() = saved_settings;
                            if let Some(dir) = saved_translations_dir {
                                new_engine.restore_translations(dir);
                            }
                            engine = new_engine;
                            title_music_played = false;
                        }
                    }
                    hud_hidden = false;
                    // 清除设置快照
                    settings_snapshot = None;
                }
                PendingUiAction::Quit => {
                    let _ = engine.delete_autosave();
                    let _ = engine.save_settings();
                    engine.save_read_history();
                    std::process::exit(0);
                }
                PendingUiAction::ApplySettings => {
                    let _ = engine.save_settings();
                    // 同步已应用的全屏值，使下一帧的全屏同步逻辑生效。
                    applied_fullscreen = engine.settings().fullscreen;
                    // 标记分辨率待应用，主循环会读取 engine.settings().resolution
                    // 并夹取到屏幕能容纳的范围内，然后调整窗口尺寸并居中。
                    pending_resolution_apply = true;
                    settings_snapshot = None;
                    // 应用设置后返回之前的模式
                    ui_mode = settings_prev_mode;
                }
                PendingUiAction::DiscardSettings => {
                    // 恢复进入设置菜单前的设置快照
                    if let Some(snapshot) = settings_snapshot.clone() {
                        *engine.settings_mut() = snapshot;
                        engine.reload_language();
                        // 注意：UI 语言独立于剧本语言，放弃设置时也需重新加载
                        // UI 翻译文件，否则用户在设置页切换 UI 语言后放弃会被
                        // 保留为切换后的语言而非快照值。
                        engine.reload_ui_language();
                    }
                    settings_snapshot = None;
                    // 放弃设置后返回之前的模式
                    ui_mode = settings_prev_mode;
                }
                PendingUiAction::StoryEndToTitle => {
                    // 故事结束自动返回标题。
                    // epilogue 模式下：尾声剧本结束，从主剧本源码重建主引擎，
                    //   不删除继续存档（尾声不应影响主游戏进度），清除 epilogue 标记。
                    // 普通模式下：重置引擎，删除继续存档，不显示继续游戏按钮。
                    // 重建前先把本会话新增的已读历史落盘（Engine::new 会重新加载）。
                    engine.save_read_history();
                    if epilogue_active {
                        epilogue_active = false;
                        let source = main_source.clone();
                        let saved_settings = engine.settings().clone();
                        let saved_translations_dir = engine.translations_dir().map(|p| p.to_path_buf());
                        let saved_title = engine.scene().title.clone();
                        let saved_subtitle = engine.scene().subtitle.clone();
                        if let Ok(mut new_engine) = Engine::new(&source) {
                            *new_engine.settings_mut() = saved_settings;
                            if let Some(dir) = saved_translations_dir {
                                new_engine.restore_translations(dir);
                            }
                            new_engine.set_title(saved_title, saved_subtitle);
                            engine = new_engine;
                            title_music_played = false;
                        }
                    } else {
                        let _ = engine.delete_continue();
                        has_continue_save = false;
                        let source = engine.source().to_string();
                        let saved_settings = engine.settings().clone();
                        // 保存翻译目录，重建引擎后恢复（同 BackToTitle）。
                        let saved_translations_dir = engine.translations_dir().map(|p| p.to_path_buf());
                        if let Ok(mut new_engine) = Engine::new(&source) {
                            *new_engine.settings_mut() = saved_settings;
                            if let Some(dir) = saved_translations_dir {
                                new_engine.restore_translations(dir);
                            }
                            engine = new_engine;
                            title_music_played = false;
                        }
                    }
                    hud_hidden = false;
                    settings_snapshot = None;
                    ui_mode = UiMode::Normal;
                }
                PendingUiAction::PlayEpilogue(idx) => {
                    // 进入隐藏结局的尾声剧本。
                    // 从 engine.unlocked_endings_with_decl() 取对应声明，
                    // 读取 epilogue .akrs 文件，创建新引擎并开始播放。
                    // 重建前先把本会话新增的已读历史落盘（Engine::new 会重新加载）。
                    engine.save_read_history();
                    let endings = engine.unlocked_endings_with_decl();
                    if let Some(decl) = endings.get(idx) {
                        let epilogue_path = &decl.epilogue;
                        match std::fs::read_to_string(epilogue_path) {
                            Ok(epilogue_source) => {
                                // 保存主剧本源码（若尚未进入 epilogue）。
                                if !epilogue_active {
                                    main_source = engine.source().to_string();
                                }
                                let saved_settings = engine.settings().clone();
                                let saved_translations_dir = engine.translations_dir().map(|p| p.to_path_buf());
                                let saved_title = engine.scene().title.clone();
                                let saved_subtitle = engine.scene().subtitle.clone();
                                match Engine::new(&epilogue_source) {
                                    Ok(mut new_engine) => {
                                        *new_engine.settings_mut() = saved_settings;
                                        if let Some(dir) = saved_translations_dir {
                                            new_engine.restore_translations(dir);
                                        }
                                        new_engine.set_title(saved_title, saved_subtitle);
                                        new_engine.start_game();
                                        engine = new_engine;
                                        epilogue_active = true;
                                        hud_hidden = false;
                                        title_music_played = false;
                                    }
                                    Err(errors) => {
                                        // epilogue 剧本编译失败：触发蓝屏崩溃界面。
                                        crash::push_log(
                                            format!("[epilogue] 尾声剧本「{}」编译失败：", epilogue_path),
                                        );
                                        for e in &errors {
                                            let line = format!("  - {:?}", e);
                                            crash::push_log(line);
                                        }
                                        engine.set_crash_info(Some(crash::CrashInfo {
                                            module_key: "error.module.script_compiler",
                                            code: crash::error_code::SCRIPT_COMPILE,
                                            can_continue: true,
                                        }));
                                        ui_mode = UiMode::CrashScreen;
                                    }
                                }
                            }
                            Err(e) => {
                                // epilogue 文件读取失败：触发蓝屏崩溃界面。
                                crash::push_log(format!(
                                    "[epilogue] 无法读取尾声剧本「{}」：{}", epilogue_path, e
                                ));
                                engine.set_crash_info(Some(crash::CrashInfo {
                                    module_key: "error.module.script_runtime",
                                    code: crash::error_code::SCRIPT_RUNTIME,
                                    can_continue: true,
                                }));
                                ui_mode = UiMode::CrashScreen;
                            }
                        }
                    }
                    settings_snapshot = None;
                }
            }
        }

        // Settings layout is computed once per frame and shared by both the
        // draw call and the interaction handler below.
        let settings_layout = compute_settings_layout(sw, sh, scale);

        if ui_mode == UiMode::CrashScreen {
            // 蓝屏错误界面：全屏蓝底白字，优先级最高，覆盖一切。
            draw_crash_screen(&engine, &mut buttons, sw, sh, &font, scale, crash_fade);
        } else if ui_mode == UiMode::DirPicker {
            // 日志导出目录选择器：蓝底面板 + 子目录列表 + 操作按钮。
            draw_dir_picker(
                &engine, &mut buttons, sw, sh, &font, scale,
                &dir_picker_current, &dir_picker_entries, dir_picker_scroll,
                &dir_picker_status, crash_fade,
            );
        } else if ui_mode == UiMode::AutoSavePrompt {
            // 异常中断恢复提示：以 title.png 为背景 + 10% 黑布遮罩 + 居中对话框
            draw_title_background(sw, sh, &mut assets, &title_bg_name).await;
            draw_rectangle(0.0, 0.0, sw, sh, Color::new(0.0, 0.0, 0.0, 0.10));
            draw_autosave_prompt(&engine, &mut buttons, sw, sh, &font, scale);
        } else if ui_mode == UiMode::SettingsMenu {
            // 进入设置菜单时保存当前设置的快照（仅一次），并重置标签页
            if settings_snapshot.is_none() {
                settings_snapshot = Some(engine.settings().clone());
                settings_active_tab = SettingsTab::Text;
            }
            // 设置菜单内部绘制背景和所有控件
            draw_settings_menu(&mut engine, &settings_layout, &font, dropdown_open, skip_dropdown_open, ui_lang_dropdown_open, lang_dropdown_open, settings_active_tab, scale, &about_icon_texture, project_config, color_edit_active, &color_hex_buffer);
        } else if ui_mode == UiMode::ConfirmDialog {
            // 确认对话框：先绘制底层界面（保持上下文可见），再叠加 8% 黑色 + 对话框。
            // confirm_return_mode 记录了确认对话框返回后应恢复的模式，据此绘制底层。
            match confirm_return_mode {
                UiMode::SettingsMenu => {
                    if settings_snapshot.is_none() {
                        settings_snapshot = Some(engine.settings().clone());
                    }
                    draw_settings_menu(&mut engine, &settings_layout, &font, dropdown_open, skip_dropdown_open, ui_lang_dropdown_open, lang_dropdown_open, settings_active_tab, scale, &about_icon_texture, project_config, color_edit_active, &color_hex_buffer);
                }
                UiMode::Normal => {
                    if engine.phase() == EnginePhase::Title {
                        draw_title_screen(&engine, &mut buttons, sw, sh, &mut assets, &font, scale, has_continue_save, &title_bg_name).await;
                    } else if engine.phase() == EnginePhase::StoryEnded {
                        draw_scene(&engine, &mut assets, sw, sh, true, &font, scale).await;
                    } else {
                        // 游戏中：只绘制场景（不绘制可交互 HUD，因为确认对话框期间不需要 HUD 交互）。
                        // 脚本驱动的 `- hide` 同样隐藏对话框。
                        draw_scene(&engine, &mut assets, sw, sh, !(hud_hidden || engine.scene().hide_textbox), &font, scale).await;
                    }
                }
                _ => {
                    // 其他模式（存档/读档菜单等）：绘制不透明背景作为兜底。
                    draw_rectangle(0.0, 0.0, sw, sh, Color::new(0.05, 0.05, 0.1, 1.0));
                }
            }
            // 10% 黑色叠层（透明度 0.10），让底层界面仍可见但变暗。
            draw_rectangle(0.0, 0.0, sw, sh, Color::new(0.0, 0.0, 0.0, 0.10));
            // 居中确认对话框
            draw_confirm_dialog(&engine, &mut buttons, sw, sh, &font, scale, confirm_type, dialog_fade);
        } else if ui_mode == UiMode::NoteEditDialog {
            // 备注编辑弹窗：先绘制底层存档页（保持上下文可见），再叠加
            // 40% 黑色遮罩 + 居中输入框 + 确认/取消按钮。
            draw_rectangle(0.0, 0.0, sw, sh, Color::new(0.05, 0.05, 0.1, 1.0));
            match note_return_mode {
                UiMode::SaveMenu => draw_save_menu(&engine, &mut buttons, sw, sh, &font, scale, save_page, save_displayed_slots, &mut assets).await,
                UiMode::LoadMenu => draw_load_menu(&engine, &mut buttons, sw, sh, &font, scale, load_page, load_displayed_slots, &mut assets).await,
                _ => {}
            }
            // 40% 黑色遮罩（比确认对话框更深，突出输入框）。
            draw_rectangle(0.0, 0.0, sw, sh, Color::new(0.0, 0.0, 0.0, 0.40));
            // 居中备注编辑弹窗。buttons 在上面已注册了存档页的按钮，
            // 弹窗按钮在此函数内追加到 buttons 末尾。
            draw_note_edit_dialog(&engine, &mut buttons, sw, sh, &font, scale, &note_edit_buffer, note_edit_slot, dialog_fade);
        } else if ui_mode != UiMode::Normal {
            // Save/Load menus: full-screen opaque background + full-screen grid.
            draw_rectangle(0.0, 0.0, sw, sh, Color::new(0.05, 0.05, 0.1, 1.0));
            match ui_mode {
                UiMode::SaveMenu => draw_save_menu(&engine, &mut buttons, sw, sh, &font, scale, save_page, save_displayed_slots, &mut assets).await,
                UiMode::LoadMenu => draw_load_menu(&engine, &mut buttons, sw, sh, &font, scale, load_page, load_displayed_slots, &mut assets).await,
                _ => {}
            }
        } else if engine.phase() == EnginePhase::Title {
            draw_title_screen(&engine, &mut buttons, sw, sh, &mut assets, &font, scale, has_continue_save, &title_bg_name).await;
        } else if engine.phase() == EnginePhase::StoryEnded {
            draw_scene(&engine, &mut assets, sw, sh, true, &font, scale).await;
        } else {
            // In-game: draw the scene. When the HUD is hidden, only the
            // background and characters are drawn (no dialogue box, choices,
            // or HUD), letting the player admire the scene unobstructed.
            // 脚本驱动的 `- hide` 与手动隐藏行为一致：隐藏对话框与 HUD 按钮。
            let script_hide = engine.scene().hide_textbox;
            let eff_hide = hud_hidden || script_hide;
            draw_scene(&engine, &mut assets, sw, sh, !eff_hide, &font, scale).await;
            if !eff_hide {
                // 检测鼠标是否在 HUD 触发区域内，更新显隐进度。
                let (tx, ty, tw, th) = hud_trigger_rect(sw, sh, scale);
                let (mx, my) = mouse_position();
                let hovered = mx >= tx && mx <= tx + tw && my >= ty && my <= ty + th;
                hud_visibility.update(hovered, dt);
                // 完全隐藏时不绘制也不注册按钮，避免误触；
                // 半显示/全显示时绘制并注册（下沉过程可见，但低于 0.5 不可点击）。
                if hud_visibility.is_interactable() {
                    draw_hud_buttons(&engine, &mut buttons, sw, sh, &font, scale, &hud_visibility, engine.is_skip_active(), engine.settings().auto_play);
                } else {
                    // 仍以最终位置注册按钮，使上浮过程中已可见即可点击；
                    // 但完全隐藏时不注册（is_interactable 已过滤）。
                    // 这里不绘制，保证下沉到底时画面干净。
                }
            } else {
                // hud_hidden 期间强制重置为隐藏，避免恢复时残留高亮态。
                hud_visibility = HudVisibility::new();
            }
        }

        // Handle mouse input.
        // All input is blocked while a UI transition is in progress.
        // The settings menu handles its own interaction (it needs per-frame
        // drag updates, not just click events), so it runs every frame; all
        // other modes use the generic button/click handler on press only.
        if !ui_transition.active {
            if ui_mode == UiMode::CrashScreen {
                // 蓝屏界面按钮处理：直接遍历 buttons 匹配 Crash* 动作，
                // 不走 handle_click / ui_transition 机制（蓝屏不属于页面切换）。
                if is_mouse_button_pressed(MouseButton::Left) {
                    let (mx, my) = mouse_position();
                    let mut hit: Option<ButtonAction> = None;
                    for btn in buttons.iter() {
                        if mx >= btn.x && mx <= btn.x + btn.w
                            && my >= btn.y && my <= btn.y + btn.h
                        {
                            match btn.action {
                                ButtonAction::CrashExportLog
                                | ButtonAction::CrashContinue
                                | ButtonAction::CrashExit => {
                                    hit = Some(btn.action);
                                    break;
                                }
                                _ => {}
                            }
                        }
                    }
                    if let Some(action) = hit {
                        match action {
                            ButtonAction::CrashExportLog => {
                                // 进入目录选择器，初始化为当前工作目录。
                                dir_picker_current =
                                    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
                                dir_picker_entries = read_subdirs(&dir_picker_current);
                                dir_picker_scroll = 0.0;
                                dir_picker_status.clear();
                                ui_mode = UiMode::DirPicker;
                                crash_fade = 0.0;
                            }
                            ButtonAction::CrashContinue => {
                                let info = engine.take_crash_info();
                                let can_continue = info.map(|i| i.can_continue).unwrap_or(false);
                                if can_continue {
                                    // 剧本编译失败（script_error）→ 引擎已是降级标题页，
                                    // 直接进入标题；运行时错误 → 重建引擎回到标题。
                                    if engine.script_error().is_some() {
                                        ui_mode = UiMode::Normal;
                                    } else {
                                        // 重建引擎回到标题（沿用 BackToTitle 的重建逻辑）。
                                        // 重建前先把本会话新增的已读历史落盘
                                        // （Engine::new 会重新加载）。
                                        engine.save_read_history();
                                        let source = engine.source().to_string();
                                        let saved_settings = engine.settings().clone();
                                        let saved_translations_dir =
                                            engine.translations_dir().map(|p| p.to_path_buf());
                                        if let Ok(mut new_engine) = Engine::new(&source) {
                                            *new_engine.settings_mut() = saved_settings;
                                            if let Some(dir) = saved_translations_dir {
                                                new_engine.restore_translations(dir);
                                            }
                                            engine = new_engine;
                                            title_music_played = false;
                                        }
                                        hud_hidden = false;
                                        settings_snapshot = None;
                                        ui_mode = UiMode::Normal;
                                    }
                                } else {
                                    // 不可继续：仅清除崩溃信息，仍留在蓝屏界面
                                    // （理论上不会走到，按钮在不可继续时禁用）。
                                }
                            }
                            ButtonAction::CrashExit => {
                                let _ = engine.delete_autosave();
                                let _ = engine.save_settings();
                                engine.save_read_history();
                                std::process::exit(0);
                            }
                            _ => {}
                        }
                    }
                }
            } else if ui_mode == UiMode::DirPicker {
                // 目录选择器：处理滚轮、目录进入、上级、确认、取消。
                let (_, wheel_y) = mouse_wheel();
                if wheel_y != 0.0 {
                    dir_picker_scroll = (dir_picker_scroll - wheel_y * 0.5).max(0.0);
                }
                if is_mouse_button_pressed(MouseButton::Left) {
                    let (mx, my) = mouse_position();
                    let mut hit: Option<ButtonAction> = None;
                    for btn in buttons.iter() {
                        if mx >= btn.x && mx <= btn.x + btn.w
                            && my >= btn.y && my <= btn.y + btn.h
                        {
                            match btn.action {
                                ButtonAction::DirUp
                                | ButtonAction::DirConfirm
                                | ButtonAction::DirCancel
                                | ButtonAction::DirEntry(_) => {
                                    hit = Some(btn.action);
                                    break;
                                }
                                _ => {}
                            }
                        }
                    }
                    if let Some(action) = hit {
                        match action {
                            ButtonAction::DirUp => {
                                if let Some(parent) = dir_picker_current.parent() {
                                    dir_picker_current = parent.to_path_buf();
                                    dir_picker_entries = read_subdirs(&dir_picker_current);
                                    dir_picker_scroll = 0.0;
                                    dir_picker_status.clear();
                                }
                            }
                            ButtonAction::DirEntry(idx) => {
                                if let Some(name) = dir_picker_entries.get(idx).cloned() {
                                    let next = dir_picker_current.join(&name);
                                    dir_picker_current = next;
                                    dir_picker_entries = read_subdirs(&dir_picker_current);
                                    dir_picker_scroll = 0.0;
                                    dir_picker_status.clear();
                                }
                            }
                            ButtonAction::DirConfirm => {
                                match crash::export_logs(&dir_picker_current) {
                                    Ok(path) => {
                                        dir_picker_status = format!(
                                            "{}: {}",
                                            engine.t_ui("crash.export_success"),
                                            path.display()
                                        );
                                    }
                                    Err(e) => {
                                        dir_picker_status = format!(
                                            "{}: {}",
                                            engine.t_ui("crash.export_failed"),
                                            e
                                        );
                                    }
                                }
                            }
                            ButtonAction::DirCancel => {
                                // 返回蓝屏界面（保留崩溃信息，玩家可再次操作）。
                                ui_mode = UiMode::CrashScreen;
                                crash_fade = 0.0;
                            }
                            _ => {}
                        }
                    }
                }
            } else if ui_mode == UiMode::SettingsMenu {
                if let Some((target, pending)) = handle_settings_interaction(
                    &mut engine, &settings_layout, &mut dragging_slider, &mut dropdown_open,
                    &mut skip_dropdown_open, &mut ui_lang_dropdown_open, &mut lang_dropdown_open, &mut settings_active_tab, scale,
                    &settings_snapshot, &mut settings_prev_mode,
                    project_config, &mut color_edit_active, &mut color_hex_buffer,
                ) {
                    // 如果返回确认对话框，设置确认类型
                    if target == UiMode::ConfirmDialog {
                        confirm_type = Some(ConfirmType::UnappliedSettings);
                        confirm_return_mode = UiMode::SettingsMenu;
                        ui_mode = UiMode::ConfirmDialog;
                    } else {
                        ui_transition.start(target, pending);
                    }
                }
            } else if ui_mode == UiMode::ConfirmDialog {
                // 确认对话框的按钮处理
                if is_mouse_button_pressed(MouseButton::Left) {
                    let (mx, my) = mouse_position();
                    if let Some(action) = handle_click(
                        mx, my, &buttons, &mut engine, &ui_mode, &mut hud_hidden,
                        sw, sh, scale,
                    ) {
                        match action {
                            ButtonAction::ConfirmYes => {
                                let prev_mode = confirm_return_mode;
                                let ctype = confirm_type.take();
                                confirm_type = None;
                                match ctype {
                                    Some(ConfirmType::BackToTitle) => {
                                        ui_mode = prev_mode;
                                        ui_transition.start(UiMode::Normal, PendingUiAction::BackToTitle);
                                    }
                                    Some(ConfirmType::UnappliedSettings) => {
                                        ui_transition.start(settings_prev_mode, PendingUiAction::DiscardSettings);
                                    }
                                    Some(ConfirmType::ScriptError) => {
                                        // 剧本错误警告确认后返回标题页。
                                        ui_mode = prev_mode;
                                    }
                                    _ => {
                                        ui_mode = prev_mode;
                                    }
                                }
                            }
                            ButtonAction::ConfirmNo => {
                                // 取消确认，返回之前的模式
                                ui_mode = confirm_return_mode;
                                confirm_type = None;
                            }
                            _ => {}
                        }
                    }
                }
            } else if ui_mode == UiMode::NoteEditDialog {
                // 备注编辑弹窗：仅响应 NoteConfirm/NoteCancel 两个按钮。
                // 注意：底层存档页的按钮（SaveSlot/LoadSlot/EditNote 等）已被
                // draw_save_menu/draw_load_menu 注册到 buttons 列表前面，且其
                // 命中区域可能与弹窗按钮重叠。若调用 handle_click 会按注册顺序
                // 命中底层按钮并返回非弹窗动作，导致弹窗按钮点击被忽略（_ => {}
                // 分支吞掉）。因此这里直接遍历 buttons，只匹配弹窗按钮动作。
                if is_mouse_button_pressed(MouseButton::Left) {
                    let (mx, my) = mouse_position();
                    // 仅查找 NoteConfirm/NoteCancel 命中，跳过其它按钮。
                    let mut hit: Option<ButtonAction> = None;
                    for btn in buttons.iter() {
                        if mx >= btn.x && mx <= btn.x + btn.w
                            && my >= btn.y && my <= btn.y + btn.h
                        {
                            match btn.action {
                                ButtonAction::NoteConfirm | ButtonAction::NoteCancel => {
                                    hit = Some(btn.action);
                                    break;
                                }
                                _ => {} // 忽略底层存档页按钮
                            }
                        }
                    }
                    if let Some(action) = hit {
                        match action {
                            ButtonAction::NoteConfirm => {
                                if let Some(slot) = note_edit_slot.take() {
                                    if let Err(e) = engine.saves().set_note(slot, &note_edit_buffer) {
                                        eprintln!("[Error] 保存备注失败: {}", e);
                                    }
                                }
                                note_edit_buffer.clear();
                                ui_mode = note_return_mode;
                            }
                            ButtonAction::NoteCancel => {
                                note_edit_slot = None;
                                note_edit_buffer.clear();
                                ui_mode = note_return_mode;
                            }
                            _ => {}
                        }
                    }
                }
            } else if is_mouse_button_pressed(MouseButton::Left) {
                let (mx, my) = mouse_position();
                if let Some(action) = handle_click(
                    mx, my, &buttons, &mut engine, &ui_mode, &mut hud_hidden,
                    sw, sh, scale,
                ) {
                    // Handle non-transition actions immediately.
                    match action {
                        ButtonAction::QuickSave => {
                            // 快速存档写入独立的 quicksave.json，不占用编号槽位，
                            // 也不会覆盖存档页中的任何手动存档。
                            engine.save_quicksave();
                        }
                        ButtonAction::QuickLoad => {
                            if engine.has_quicksave() {
                                engine.load_quicksave();
                                hud_hidden = false;
                            }
                        }
                        ButtonAction::ToggleHide => {
                            hud_hidden = !hud_hidden;
                        }
                        ButtonAction::FastForward => {
                            engine.toggle_skip();
                            // 切换快进时保存已读历史
                            if !engine.is_skip_active() {
                                engine.save_read_history();
                            }
                        }
                        ButtonAction::AutoPlay => {
                            // 切换自动播放开关
                            engine.toggle_auto_play();
                        }
                        ButtonAction::PrevPage => {
                            let page = if ui_mode == UiMode::SaveMenu { &mut save_page } else { &mut load_page };
                            if *page > 0 {
                                *page -= 1;
                            }
                        }
                        ButtonAction::NextPage => {
                            let displayed = if ui_mode == UiMode::SaveMenu { save_displayed_slots } else { load_displayed_slots };
                            let total_pages = ((displayed + SLOTS_PER_PAGE - 1) / SLOTS_PER_PAGE).max(1);
                            let page = if ui_mode == UiMode::SaveMenu { &mut save_page } else { &mut load_page };
                            if *page + 1 < total_pages {
                                *page += 1;
                            }
                        }
                        ButtonAction::AddPage => {
                            let max_slots = engine.saves().max_slots();
                            let (displayed, page) = if ui_mode == UiMode::SaveMenu {
                                (&mut save_displayed_slots, &mut save_page)
                            } else {
                                (&mut load_displayed_slots, &mut load_page)
                            };
                            *displayed = (*displayed + SLOTS_PER_PAGE).min(max_slots);
                            // Jump to the new last page so the freshly revealed
                            // slots are immediately visible.
                            let total_pages = ((*displayed + SLOTS_PER_PAGE - 1) / SLOTS_PER_PAGE).max(1);
                            *page = total_pages - 1;
                        }
                        ButtonAction::BackToTitle => {
                            // 返回标题需要确认
                            confirm_type = Some(ConfirmType::BackToTitle);
                            confirm_return_mode = ui_mode;
                            ui_mode = UiMode::ConfirmDialog;
                        }
                        ButtonAction::EditNote(slot) => {
                            // 打开备注编辑弹窗：读取已有备注作为初始值。
                            note_edit_slot = Some(slot);
                            note_edit_buffer = engine.saves()
                                .load_slot_full(slot)
                                .and_then(|s| s.metadata.note)
                                .unwrap_or_default();
                            note_return_mode = ui_mode;
                            ui_mode = UiMode::NoteEditDialog;
                        }
                        ButtonAction::NoteConfirm => {
                            // 确认保存备注：写回存档文件，返回存档页。
                            if let Some(slot) = note_edit_slot.take() {
                                if let Err(e) = engine.saves().set_note(slot, &note_edit_buffer) {
                                    eprintln!("[Error] 保存备注失败: {}", e);
                                }
                            }
                            note_edit_buffer.clear();
                            ui_mode = note_return_mode;
                        }
                        ButtonAction::NoteCancel => {
                            // 取消编辑：丢弃缓冲区，返回存档页。
                            note_edit_slot = None;
                            note_edit_buffer.clear();
                            ui_mode = note_return_mode;
                        }
                        ButtonAction::StartGame => {
                            if engine.script_error().is_some() {
                                confirm_type = Some(ConfirmType::ScriptError);
                                confirm_return_mode = ui_mode;
                                ui_mode = UiMode::ConfirmDialog;
                            } else {
                                ui_transition.start(UiMode::Normal, PendingUiAction::StartGame);
                            }
                        }
                        ButtonAction::ContinueGame => {
                            if engine.script_error().is_some() {
                                confirm_type = Some(ConfirmType::ScriptError);
                                confirm_return_mode = ui_mode;
                                ui_mode = UiMode::ConfirmDialog;
                            } else {
                                ui_transition.start(UiMode::Normal, PendingUiAction::ContinueGame);
                            }
                        }
                        ButtonAction::OpenSettings => {
                            // 进入设置时记录当前模式
                            settings_prev_mode = ui_mode;
                            ui_transition.start(UiMode::SettingsMenu, PendingUiAction::None);
                        }
                        ButtonAction::Settings => {
                            // 从标题进入设置时记录当前模式
                            settings_prev_mode = UiMode::Normal;
                            ui_transition.start(UiMode::SettingsMenu, PendingUiAction::None);
                        }
                        _ => {
                            // 剧本错误降级模式：拦截"读档"按钮，弹出警告。
                            if engine.script_error().is_some()
                                && matches!(action, ButtonAction::LoadGame)
                            {
                                confirm_type = Some(ConfirmType::ScriptError);
                                confirm_return_mode = ui_mode;
                                ui_mode = UiMode::ConfirmDialog;
                            } else if let Some((target, pending)) = handle_button_action(action) {
                                ui_transition.start(target, pending);
                            }
                        }
                    }
                }
            }
        }

        // Handle keyboard input
        if is_key_pressed(KeyCode::Escape) {
            if ui_transition.active {
                // Ignore Esc during a UI transition.
            } else if ui_mode == UiMode::AutoSavePrompt {
                // The recovery prompt requires an explicit choice; ignore Esc.
            } else if ui_mode == UiMode::ConfirmDialog {
                // 确认对话框按 Esc 取消，返回之前的模式
                ui_mode = confirm_return_mode;
                confirm_type = None;
            } else if ui_mode == UiMode::NoteEditDialog {
                // 备注编辑弹窗按 Esc 取消，丢弃缓冲区，返回存档页。
                note_edit_slot = None;
                note_edit_buffer.clear();
                ui_mode = note_return_mode;
            } else if ui_mode == UiMode::SettingsMenu {
                // 设置菜单按 Esc：若正在编辑颜色十六进制，先结束编辑而不退出菜单
                if color_edit_active.is_some() {
                    color_edit_active = None;
                } else {
                // 设置菜单按 Esc：检测是否有未应用的更改
                dragging_slider = None;
                let has_changes = if let Some(snapshot) = settings_snapshot.clone() {
                    let current = engine.settings();
                    current.text_speed != snapshot.text_speed
                        || current.bgm_volume != snapshot.bgm_volume
                        || current.sfx_volume != snapshot.sfx_volume
                        || current.voice_volume != snapshot.voice_volume
                        || current.auto_recovery != snapshot.auto_recovery
                        || current.fullscreen != snapshot.fullscreen
                        || current.resolution != snapshot.resolution
                        || current.auto_play != snapshot.auto_play
                        || current.auto_play_delay_with_voice != snapshot.auto_play_delay_with_voice
                        || current.auto_play_delay_without_voice != snapshot.auto_play_delay_without_voice
                        || current.skip_unread != snapshot.skip_unread
                        || current.skip_mode != snapshot.skip_mode
                        || current.language != snapshot.language
                        || current.ui_language != snapshot.ui_language
                        || current.read_text_color != snapshot.read_text_color
                        || current.unread_text_color != snapshot.unread_text_color
                        || current.theme_primary != snapshot.theme_primary
                        || current.theme_secondary != snapshot.theme_secondary
                        || current.theme_dialogue != snapshot.theme_dialogue
                        || current.debug_terminal != snapshot.debug_terminal
                } else {
                    false
                };
                if has_changes {
                    // 有更改，显示确认对话框
                    confirm_type = Some(ConfirmType::UnappliedSettings);
                    confirm_return_mode = UiMode::SettingsMenu;
                    ui_mode = UiMode::ConfirmDialog;
                } else {
                    // 无更改，直接返回
                    ui_transition.start(settings_prev_mode, PendingUiAction::DiscardSettings);
                }
                }
            } else if ui_mode != UiMode::Normal {
                ui_transition.start(UiMode::Normal, PendingUiAction::None);
            } else if engine.phase() != EnginePhase::Title {
                // 游戏中按 Esc 进入设置菜单，记录当前模式
                settings_prev_mode = ui_mode;
                ui_transition.start(UiMode::SettingsMenu, PendingUiAction::None);
            }
        }

        // Handle advance (space or enter). Disabled while the HUD is hidden so
        // the player does not skip dialogue they cannot see, and blocked during
        // UI transitions. 脚本驱动的 `- hide` 期间同样禁用推进。
        if !ui_transition.active && ui_mode == UiMode::Normal && !(hud_hidden || engine.scene().hide_textbox) && engine.phase() != EnginePhase::Title {
            if is_key_pressed(KeyCode::Space) || is_key_pressed(KeyCode::Enter) {
                engine.advance();
            }
        }

        // 备注编辑弹窗的键盘输入：收集字符、Backspace 删除、Enter 确认。
        // Esc 已在上面处理。此处仅处理 NoteEditDialog 模式。
        if ui_mode == UiMode::NoteEditDialog && !ui_transition.active {
            // Backspace：删除最后一个字符。
            if is_key_pressed(KeyCode::Backspace) {
                note_edit_buffer.pop();
            }
            // Enter：确认保存（与点击「确认」按钮等价）。
            if is_key_pressed(KeyCode::Enter) {
                if let Some(slot) = note_edit_slot.take() {
                    if let Err(e) = engine.saves().set_note(slot, &note_edit_buffer) {
                        eprintln!("[Error] 保存备注失败: {}", e);
                    }
                }
                note_edit_buffer.clear();
                ui_mode = note_return_mode;
            }
            // 字符输入：收集本帧所有按键字符。限制备注最大 60 字符。
            while let Some(c) = get_char_pressed() {
                if c.is_control() {
                    continue;
                }
                let cur_len = note_edit_buffer.chars().count();
                if cur_len < 60 {
                    note_edit_buffer.push(c);
                }
            }
        }

        // 配色标签页十六进制输入：Backspace 删除、Enter 提交、字符收集 + 实时套用。
        // Esc 已在上面处理（仅结束编辑而不退出菜单）。仅在用户实际修改缓冲区时
        // 才尝试解析套用，避免激活主题色字段时把 None 误转为 Some。
        if ui_mode == UiMode::SettingsMenu && color_edit_active.is_some() && !ui_transition.active {
            let mut modified = false;
            if is_key_pressed(KeyCode::Backspace) {
                color_hex_buffer.pop();
                modified = true;
            }
            if is_key_pressed(KeyCode::Enter) {
                color_edit_active = None;
            }
            // 仅接受十六进制字符与 '#'，最长 9（#RRGGBBAA）
            while let Some(c) = get_char_pressed() {
                if c.is_control() {
                    continue;
                }
                if !(c.is_ascii_hexdigit() || c == '#') {
                    continue;
                }
                let cur = color_hex_buffer.chars().count();
                if cur < 9 {
                    color_hex_buffer.push(c);
                    modified = true;
                }
            }
            if modified {
                if let Some(field) = color_edit_active {
                    if let Some(c) = parse_hex_color(&color_hex_buffer) {
                        field.set(engine.settings_mut(), c);
                    }
                }
            }
        }

        // Draw UI transition overlay (black fade) on top of everything.
        if ui_transition.active {
            let alpha = ui_transition.overlay_alpha();
            draw_rectangle(0.0, 0.0, sw, sh, Color::new(0.0, 0.0, 0.0, alpha));
        }

        // Draw chapter-switch animation: full-screen fade + top toast notification.
        // 章节淡入淡出遮罩盖在所有内容之上（含文本框）。
        let fade_alpha = chapter_anim.fade_alpha();
        if fade_alpha > 0.0 {
            draw_rectangle(0.0, 0.0, sw, sh, Color::new(0.0, 0.0, 0.0, fade_alpha));
        }
        if chapter_anim.toast_visible() {
            draw_chapter_toast(&chapter_anim, sw, sh, &font, scale);
        }

        next_frame().await;
    }
}

// ─── Drawing functions ───

/// 绘制标题页背景图（cover 模式，16:9 裁切适应），回退为天蓝色渐变。
async fn draw_title_background(sw: f32, sh: f32, assets: &mut AssetManager, bg_name: &str) {
    let title_bg = assets.get_texture(AssetKind::Title, bg_name).await;
    if let Some(tex) = title_bg {
        let tex_w = tex.width();
        let tex_h = tex.height();
        let bg_scale = (sw / tex_w).max(sh / tex_h);
        let draw_w = tex_w * bg_scale;
        let draw_h = tex_h * bg_scale;
        let offset_x = (sw - draw_w) / 2.0;
        let offset_y = (sh - draw_h) / 2.0;
        draw_texture_ex(
            tex.clone(),
            offset_x,
            offset_y,
            WHITE,
            DrawTextureParams {
                dest_size: Some(Vec2::new(draw_w, draw_h)),
                ..Default::default()
            },
        );
    } else {
        let bg_segments = 64;
        let seg_h = sh / bg_segments as f32;
        for i in 0..bg_segments {
            let t = i as f32 / (bg_segments - 1) as f32;
            let r = 0.3 + 0.2 * t;
            let g = 0.6 + 0.2 * t;
            let b = 0.9;
            draw_rectangle(0.0, i as f32 * seg_h, sw, seg_h,
                Color::new(r, g, b, 1.0));
        }
    }
}

async fn draw_title_screen(engine: &Engine, buttons: &mut Vec<ButtonRect>, sw: f32, sh: f32, assets: &mut AssetManager, font: &Option<Font>, scale: f32, has_continue_save: bool, title_bg_name: &str) {
    draw_title_background(sw, sh, assets, title_bg_name).await;

    let scene = engine.scene();

    // 文本与控件离左边的距离（约 1% 屏幕宽度）
    let left_pad = sw * 0.01;

    // 主标题（左上角，居左对齐）
    let title = scene.title.as_str();
    let title_font_size = 56.0 * scale;
    let title_x = left_pad;
    let title_y = 40.0 * scale;
    draw_text_f(
        title,
        title_x,
        title_y + title_font_size,
        title_font_size,
        WHITE,
        font,
    );

    // 副标题（主标题下方，居左对齐）
    let subtitle = scene.subtitle.as_str();
    let sub_size = 28.0 * scale;
    let sub_x = title_x;
    let sub_y = title_y + title_font_size + 20.0 * scale;
    draw_text_f(
        subtitle,
        sub_x,
        sub_y + sub_size,
        sub_size,
        WHITE,
        font,
    );

    // 按钮（左下角，稍微放大）
    let btn_w = 260.0 * scale;
    let btn_h = 56.0 * scale;
    let btn_x = title_x;

    let mut labels: Vec<(String, ButtonAction)> = Vec::new();

    // 如果有"继续游戏"存档，在"开始游戏"前显示"继续游戏"按钮
    if has_continue_save {
        labels.push((engine.t_ui("title.continue").to_string(), ButtonAction::ContinueGame));
    }
    labels.push((engine.t_ui("title.start").to_string(), ButtonAction::StartGame));
    // 隐藏结局「尾声之后」按钮：每个已解锁且当前剧本有声明的结局各一个按钮。
    // 按钮文本优先取 ending 声明中的 button "..."，否则用翻译键 title.epilogue
    // （默认「尾声之后」）。作为彩蛋放在「开始游戏」与「读档」之间。
    for (i, decl) in engine.unlocked_endings_with_decl().iter().enumerate() {
        let text = decl.button_text.clone()
            .unwrap_or_else(|| engine.t_ui("title.epilogue").to_string());
        labels.push((text, ButtonAction::PlayEpilogue(i)));
    }
    labels.push((engine.t_ui("title.load").to_string(), ButtonAction::LoadGame));
    labels.push((engine.t_ui("title.settings").to_string(), ButtonAction::Settings));
    labels.push((engine.t_ui("title.exit").to_string(), ButtonAction::Quit));

    // 计算最宽的文本宽度（标题、副标题、按钮中取最大）
    let title_w = measure_text_f(title, font, title_font_size as u16, 1.0).width;
    let sub_w = measure_text_f(subtitle, font, sub_size as u16, 1.0).width;
    let btn_text_pad = 40.0 * scale; // 按钮内边距左右各 20
    let max_text_w = title_w.max(sub_w).max(btn_w - btn_text_pad);

    // 白色半透明资料层（从屏幕最左侧开始，顶上下左三边；右边刚好覆盖最长标题 + 内边距）
    // 透明度 70% = 不透明度 30%（alpha = 0.3）
    let panel_x = 0.0;
    let panel_top = 0.0;
    let panel_bottom = sh;
    let panel_pad_right = 40.0 * scale; // 右侧内边距
    let panel_w = (max_text_w + left_pad + panel_pad_right).max(btn_w + left_pad + panel_pad_right);
    let panel_h = panel_bottom - panel_top;
    draw_rectangle(panel_x, panel_top, panel_w, panel_h, Color::new(1.0, 1.0, 1.0, 0.3));

    // 分隔线
    let line_y = sub_y + sub_size + 30.0 * scale;
    let line_w = (max_text_w + 10.0 * scale).min(panel_w - left_pad - 20.0 * scale);
    draw_rectangle(title_x, line_y, line_w, 2.0 * scale,
        Color::new(1.0, 1.0, 1.0, 0.7));

    // 从下往上排列按钮
    let total_btn_h = labels.len() as f32 * btn_h + (labels.len() - 1) as f32 * 14.0 * scale;
    let bottom_pad = 60.0 * scale;
    let mut btn_y = panel_bottom - bottom_pad - total_btn_h;

    for (label, action) in &labels {
        draw_button(btn_x, btn_y, btn_w, btn_h, label.as_str(), buttons, *action, font, scale);
        btn_y += btn_h + 14.0 * scale;
    }
}

async fn draw_scene(engine: &Engine, assets: &mut AssetManager, sw: f32, sh: f32, show_ui: bool, font: &Option<Font>, scale: f32) {
    let scene = engine.scene();

    // Draw background
    draw_background(scene, assets, sw, sh).await;

    // Draw characters
    draw_characters(scene, assets, sw, sh, font, scale).await;

    // Draw transition overlay
    draw_transition(scene, sw, sh);

    // When the UI is hidden (the "隐藏" button), skip the dialogue box and
    // choices so only the scene itself is visible.
    if !show_ui {
        return;
    }

    // 选择菜单显示时，自动隐藏对话框文本，直到选择完成。
    // Draw choices
    if let Some(choices) = &scene.choices {
        draw_choices(choices, sw, sh, font, scale);
    } else {
        // Draw dialogue box（仅在无选项时显示）
        if let Some(dialogue) = &scene.dialogue {
            draw_dialogue(dialogue, sw, sh, font, scale);
        }
    }
}

/// 绘制章节切换的顶部通知（白底 30% 透明，章节名 + 标题两行居中）。
///
/// 由 `ChapterAnimation` 在 ToastIn/Hold/Out 阶段调用。通知从屏幕顶部滑入，
/// 停留 1s 后滑出。文本过长时自动缩小字号以适配最大宽度（屏幕宽度的 80%）。
fn draw_chapter_toast(anim: &ChapterAnimation, sw: f32, sh: f32, font: &Option<Font>, scale: f32) {
    let name = &anim.name;
    let title = anim.title.as_deref();

    // 基准字号。
    let mut name_size = 38.0 * scale;
    let mut title_size = 28.0 * scale;
    let pad_x = 40.0 * scale;
    let pad_y = 26.0 * scale;
    let gap = 12.0 * scale;
    let max_toast_w = sw * 0.8;
    let top_margin = sh * 0.07;
    let min_factor = 0.45; // 字号最低缩到 45%，避免过小不可读

    // 测量文本宽度，按需等比缩小字号以适配最大宽度。
    let measure = |text: &str, size: f32| -> f32 {
        if text.is_empty() { 0.0 } else { measure_text_f(text, font, size as u16, 1.0).width }
    };
    let mut factor = 1.0;
    loop {
        let name_w = measure(name, name_size * factor);
        let title_w = if let Some(t) = title { measure(t, title_size * factor) } else { 0.0 };
        let content_w = name_w.max(title_w);
        if content_w + 2.0 * pad_x <= max_toast_w || factor <= min_factor {
            break;
        }
        factor -= 0.05;
    }
    factor = factor.max(min_factor);
    name_size *= factor;
    title_size *= factor;

    let name_w = measure(name, name_size);
    let title_w = if let Some(t) = title { measure(t, title_size) } else { 0.0 };
    let content_w = name_w.max(title_w);

    let name_line_h = name_size * 1.2;
    let title_line_h = title_size * 1.2;
    let toast_w = (content_w + 2.0 * pad_x).min(max_toast_w).max(120.0 * scale);
    let toast_h = pad_y + name_line_h + (if title.is_some() { gap + title_line_h } else { 0.0 }) + pad_y;

    let toast_x = (sw - toast_w) / 2.0;
    let rest_y = top_margin;
    let offset_y = anim.toast_offset(toast_h + top_margin);
    let draw_y = rest_y + offset_y;

    // 白底（约 30% 透明 → alpha 0.7）。圆角效果用 glamera 不便，这里用矩形 + 细边框。
    draw_rectangle(toast_x, draw_y, toast_w, toast_h, Color::new(1.0, 1.0, 1.0, 0.7));
    // 细边框增强层次感。
    draw_rectangle_lines(toast_x, draw_y, toast_w, toast_h, 2.0 * scale, Color::new(0.0, 0.0, 0.0, 0.15));

    let text_color = Color::new(0.10, 0.10, 0.14, 1.0);

    // 第一行：章节名（居中）。
    let name_y = draw_y + pad_y + name_size; // baseline 在字号处
    let name_x = toast_x + (toast_w - name_w) / 2.0;
    draw_text_f(name, name_x, name_y, name_size, text_color, font);

    // 第二行：章节标题（居中）。
    if let Some(t) = title {
        let title_y = name_y + gap + title_size;
        let title_x = toast_x + (toast_w - title_w) / 2.0;
        draw_text_f(t, title_x, title_y, title_size, text_color, font);
    }
}

async fn draw_background(scene: &SceneState, assets: &mut AssetManager, sw: f32, sh: f32) {
    // 背景交叉淡入：当处于 bg_crossfade 过渡时，同时绘制旧背景（淡出）与新背景（淡入）。
    // 不画全屏遮罩，对话框等 UI 在背景之上正常绘制、不被遮挡。
    if let Some(overlay) = &scene.transition {
        if overlay.bg_crossfade {
            // 合并进度 t（0→1）：Out 阶段 0→0.5，In 阶段 0.5→1.0。
            // overlay.progress 已做 ease_in_out，跨阶段在 0.5 处连续。
            let t = match overlay.phase {
                TransitionPhase::Out => overlay.progress * 0.5,
                TransitionPhase::In => 0.5 + overlay.progress * 0.5,
            };
            // 先画旧背景（淡出）。
            if let Some(prev) = &scene.prev_background {
                draw_single_background(prev, assets, sw, sh, 1.0 - t).await;
            } else {
                // 无旧背景：用黑底淡出。
                draw_rectangle(0.0, 0.0, sw, sh, Color::new(0.0, 0.0, 0.0, 1.0 - t));
            }
            // 再画新背景（淡入）。
            if let Some(bg) = &scene.background {
                draw_single_background(bg, assets, sw, sh, t).await;
            } else {
                draw_rectangle(0.0, 0.0, sw, sh, Color::new(0.0, 0.0, 0.0, t));
            }
            return;
        }
    }

    // 普通模式：只画当前背景。
    if let Some(bg) = &scene.background {
        draw_single_background(bg, assets, sw, sh, bg.alpha).await;
    } else {
        // Default: black background
        draw_rectangle(0.0, 0.0, sw, sh, BLACK);
    }
}

/// 绘制单个背景图层（按 cover 模式缩放铺满屏幕），alpha 由调用方指定。
/// 交叉淡入时分别以互补 alpha 调用两次绘制新旧背景。
async fn draw_single_background(bg: &BackgroundState, assets: &mut AssetManager, sw: f32, sh: f32, alpha: f32) {
    if let Some(tex) = assets.get_texture(AssetKind::Bg, &bg.name).await {
        // Draw texture scaled to screen
        let tex_w = tex.width();
        let tex_h = tex.height();
        let scale = (sw / tex_w).max(sh / tex_h);
        let draw_w = tex_w * scale;
        let draw_h = tex_h * scale;
        let offset_x = (sw - draw_w) / 2.0 + bg.offset_x * sw;
        let offset_y = (sh - draw_h) / 2.0 + bg.offset_y * sh;
        draw_texture_ex(
            tex.clone(),
            offset_x,
            offset_y,
            Color::new(1.0, 1.0, 1.0, alpha),
            DrawTextureParams {
                dest_size: Some(Vec2::new(draw_w, draw_h)),
                ..Default::default()
            },
        );
    } else {
        // Placeholder: colored rectangle based on resource name hash
        let placeholder_color = name_to_color(&bg.name);
        draw_rectangle(0.0, 0.0, sw, sh, Color::new(
            placeholder_color.0,
            placeholder_color.1,
            placeholder_color.2,
            alpha,
        ));
    }
}

async fn draw_characters(scene: &SceneState, assets: &mut AssetManager, sw: f32, sh: f32, font: &Option<Font>, scale: f32) {
    for char_state in &scene.characters {
        // 优先使用精确百分比位置（custom_x/custom_y）；否则回退到 position 字段。
        let x_frac = char_state.custom_x.unwrap_or_else(|| char_state.position.x_fraction());
        // y 百分比：None 时默认底部站立（1.0）。
        // 注意：custom_y 的语义是立绘中心点的 y 百分比，便于作者控制纵向位置。
        let y_frac = char_state.custom_y.unwrap_or(1.0);
        let sprite_name = if let Some(pose) = &char_state.pose {
            pose.clone()
        } else {
            char_state.name.clone()
        };

        if let Some(tex) = assets.get_texture(AssetKind::Character, &sprite_name).await {
            let tex_w = tex.width();
            let tex_h = tex.height();
            // 默认立绘高度为屏幕高度的 80%，再乘以 char_state.scale。
            let scale_factor = (sh * 0.8) / tex_h;
            let draw_w = tex_w * scale_factor;
            let draw_h = tex_h * scale_factor;
            // x：按 x_frac 百分比水平居中。
            let x = sw * x_frac - draw_w / 2.0 + char_state.offset_x;
            // y：按 y_frac 百分比定位立绘中心点。
            // 当 y_frac=1.0（底部）时，立绘底部贴齐屏幕底部（留 50px*scale 边距），
            // 与原有行为一致；y_frac<1.0 时立绘中心点对齐到屏幕 y_frac 位置。
            let y = if (y_frac - 1.0).abs() < 0.001 {
                sh - draw_h - 50.0 * scale
            } else {
                sh * y_frac - draw_h / 2.0
            };
            draw_texture_ex(
                tex.clone(),
                x,
                y,
                Color::new(1.0, 1.0, 1.0, char_state.alpha),
                DrawTextureParams {
                    dest_size: Some(Vec2::new(draw_w * char_state.scale, draw_h * char_state.scale)),
                    ..Default::default()
                },
            );
        } else {
            // Placeholder: colored rectangle
            let placeholder_color = name_to_color(&char_state.name);
            let char_w = 200.0 * scale * char_state.scale;
            let char_h = 400.0 * scale * char_state.scale;
            let x = sw * x_frac - char_w / 2.0 + char_state.offset_x;
            let y = if (y_frac - 1.0).abs() < 0.001 {
                sh - char_h - 50.0 * scale
            } else {
                sh * y_frac - char_h / 2.0
            };
            draw_rectangle(
                x, y, char_w, char_h,
                Color::new(placeholder_color.0, placeholder_color.1, placeholder_color.2, char_state.alpha),
            );
            // Draw character name on placeholder
            draw_text_f(
                &char_state.name,
                x + 10.0 * scale,
                y + 30.0 * scale,
                24.0 * scale,
                WHITE,
                font,
            );
        }
    }
}

fn draw_dialogue(dialogue: &akrs_runtime::DialogueState, sw: f32, sh: f32, font: &Option<Font>, scale: f32) {
    let box_h = 350.0 * scale;
    let box_y = sh - box_h - 20.0 * scale;
    let box_x = 0.0;
    let box_w = sw;

    // 对话框背景：平滑渐变效果
    // 顶部：15%透明（alpha=0.85），底部：完全透明（alpha=0.0）
    // 渐变延伸到屏幕底部，使用 64 段绘制实现更平滑过渡。
    // 渐变基色取自主题对话框色（theme.dialogue 的 RGB），可经编辑器自定义。
    let t = theme();
    let (dr, dg, db) = (t.dialogue.r, t.dialogue.g, t.dialogue.b);
    // 已读/未读文字色：已读（玩家看过）默认浅紫，未读（首次出现）默认白。
    // 颜色由玩家在设置页「配色」标签页自定义。
    let text_color = if dialogue.is_read { t.read_text } else { t.unread_text };
    let gradient_segments = 64;
    // 渐变区域从对话框顶部一直延伸到屏幕底部
    let gradient_start_y = box_y;
    let gradient_end_y = sh; // 屏幕底部
    let total_gradient_h = gradient_end_y - gradient_start_y;
    let segment_h = total_gradient_h / gradient_segments as f32;
    for i in 0..gradient_segments {
        let seg_y = gradient_start_y + i as f32 * segment_h;
        let alpha_top = 0.85; // 顶部 15% 透明 = 85% 不透明
        let alpha_bottom = 0.0; // 底部完全透明
        let gt = i as f32 / (gradient_segments - 1) as f32;
        let alpha = alpha_top * (1.0 - gt) + alpha_bottom * gt;
        // 只在对话框区域内绘制，但渐变计算覆盖到屏幕底部
        if seg_y < box_y + box_h {
            let draw_h = segment_h.min(box_y + box_h - seg_y);
            draw_rectangle(box_x, seg_y, box_w, draw_h, Color::new(dr, dg, db, alpha));
        }
    }
    // 边框已移除（用户反馈边框线看着难受）

    let name_font_size = 42.0 * scale;
    let text_font_size = 39.0 * scale;
    let text_left_padding = 120.0 * scale;

    // 内容整体下移 5%、右移 2%
    let content_offset_y = box_h * 0.05;
    let content_offset_x = sw * 0.02;

    // 角色名（仅在非旁白时显示）—— 同样按已读/未读着色。
    if !dialogue.speaker.is_empty() {
        draw_text_f(
            &dialogue.speaker,
            box_x + 20.0 * scale + content_offset_x,
            box_y + 36.0 * scale + content_offset_y,
            name_font_size,
            text_color,
            font,
        );
    }

    // 对白文本（打字机效果）
    // 旁白与对话位置完全一致，唯一区别是旁白不显示角色名
    let displayed: String = dialogue.full_text.chars().take(dialogue.displayed_chars).collect();
    let text_y = box_y + 100.0 * scale + content_offset_y; // 统一的起始 y 位置
    draw_text_wrapped(
        &displayed,
        box_x + text_left_padding + content_offset_x,
        text_y,
        box_w - text_left_padding - 60.0 * scale - content_offset_x,
        text_font_size,
        text_color,
        font,
        scale,
    );

    // 点击继续指示器：小三角上下跳动动画
    if dialogue.complete {
        let t = get_time() as f32;
        let bounce_period = 0.8;
        let bounce_amplitude = 6.0 * scale;
        let bounce_offset = bounce_amplitude * (t * 2.0 * 3.14159 / bounce_period).sin();
        let indicator_size = 24.0 * scale;
        // 上移一些，避免被弹出的 HUD 按钮遮挡
        let base_y = box_y + box_h - 60.0 * scale;
        draw_text_f(
            "▼",
            box_x + box_w - 40.0 * scale,
            base_y + bounce_offset,
            indicator_size,
            Color::new(0.3, 0.5, 0.8, 0.85),
            font,
        );
    }
}

fn draw_choices(choices: &akrs_runtime::ChoicesState, sw: f32, sh: f32, font: &Option<Font>, scale: f32) {
    // Prompt
    if let Some(prompt) = &choices.prompt {
        let prompt_size = 32.0 * scale;
        let pw = measure_text_f(prompt, font, prompt_size as u16, 1.0).width;
        draw_text_f(
            prompt,
            (sw - pw) / 2.0,
            sh * 0.2,
            prompt_size,
            Color::new(0.8, 0.9, 1.0, 1.0),
            font,
        );
    }

    // Options
    let opt_w = 500.0 * scale;
    let opt_h = 60.0 * scale;
    let opt_x = (sw - opt_w) / 2.0;
    let _total_h = choices.options.len() as f32 * (opt_h + 15.0 * scale);
    let mut opt_y = sh * 0.3;

    for (i, opt) in choices.options.iter().enumerate() {
        let is_selected = i == choices.selected;
        let bg_color = if is_selected {
            Color::new(0.35, 0.6, 0.9, 0.95)
        } else {
            Color::new(0.2, 0.4, 0.7, 0.85)
        };
        draw_rectangle(opt_x, opt_y, opt_w, opt_h, bg_color);
        draw_rectangle_lines(opt_x, opt_y, opt_w, opt_h, 2.0 * scale,
            if is_selected { Color::new(0.6, 0.85, 1.0, 1.0) } else { Color::new(0.4, 0.6, 0.85, 0.6) });

        let opt_font = 24.0 * scale;
        let text_color = if opt.available { WHITE } else { Color::new(0.4, 0.4, 0.4, 0.8) };
        let tw = measure_text_f(&opt.text, font, opt_font as u16, 1.0).width;
        draw_text_f(
            &opt.text,
            opt_x + (opt_w - tw) / 2.0,
            opt_y + 38.0 * scale,
            opt_font,
            text_color,
            font,
        );

        opt_y += opt_h + 15.0 * scale;
    }
}

fn draw_transition(scene: &SceneState, sw: f32, sh: f32) {
    if let Some(overlay) = &scene.transition {
        use akrs_core::Transition;

        // 背景交叉淡入由 draw_background 处理（画两层背景互补 alpha），
        // 这里不画全屏遮罩，对话框等 UI 保持可见。
        if overlay.bg_crossfade {
            return;
        }

        // 计算基础透明度（Out 阶段增加，In 阶段减少）
        let base_alpha = match overlay.phase {
            TransitionPhase::Out => overlay.progress,
            TransitionPhase::In => 1.0 - overlay.progress,
        };

        // 根据过渡类型绘制不同的效果
        match overlay.kind {
            // 淡入淡出到黑色（默认）
            Transition::Fade | Transition::FadeBlack => {
                draw_rectangle(0.0, 0.0, sw, sh, Color::new(0.0, 0.0, 0.0, base_alpha));
            }
            // 淡入淡出到白色
            Transition::FadeWhite => {
                draw_rectangle(0.0, 0.0, sw, sh, Color::new(1.0, 1.0, 1.0, base_alpha));
            }
            // 滑动效果（简化为淡入淡出 + 方向性暗示）
            // 由于 macroquad 不支持多 pass 渲染，无法实现真正的场景滑动，
            // 这里用带有方向性偏移的遮罩模拟滑动感
            Transition::SlideLeft => {
                let offset = sw * base_alpha * 0.2;
                draw_rectangle(0.0 + offset, 0.0, sw - offset, sh, Color::new(0.0, 0.0, 0.0, base_alpha));
            }
            Transition::SlideRight => {
                let offset = sw * base_alpha * 0.2;
                draw_rectangle(0.0, 0.0, sw - offset, sh, Color::new(0.0, 0.0, 0.0, base_alpha));
            }
            Transition::SlideUp => {
                let offset = sh * base_alpha * 0.2;
                draw_rectangle(0.0, 0.0 + offset, sw, sh - offset, Color::new(0.0, 0.0, 0.0, base_alpha));
            }
            Transition::SlideDown => {
                let offset = sh * base_alpha * 0.2;
                draw_rectangle(0.0, 0.0, sw, sh - offset, Color::new(0.0, 0.0, 0.0, base_alpha));
            }
            // 溶解效果（简化为淡入淡出，因为无法实现真正的交叉淡入）
            Transition::Dissolve => {
                // Dissolve 交叉淡入需要同时渲染新旧场景，
                // macroquad 单 pass 架构无法实现，退化为 Fade
                draw_rectangle(0.0, 0.0, sw, sh, Color::new(0.0, 0.0, 0.0, base_alpha));
            }
            // 擦除效果（从左/右边缘擦除）
            Transition::WipeLeft => {
                // Out 阶段：黑色遮罩从右向左扩展
                // In 阶段：黑色遮罩从左向右收缩
                let wipe_x = match overlay.phase {
                    TransitionPhase::Out => sw * (1.0 - base_alpha),
                    TransitionPhase::In => 0.0,
                };
                let wipe_w = match overlay.phase {
                    TransitionPhase::Out => sw * base_alpha,
                    TransitionPhase::In => sw * base_alpha,
                };
                draw_rectangle(wipe_x, 0.0, wipe_w, sh, Color::new(0.0, 0.0, 0.0, 1.0));
            }
            Transition::WipeRight => {
                let wipe_x = match overlay.phase {
                    TransitionPhase::Out => 0.0,
                    TransitionPhase::In => sw * (1.0 - base_alpha),
                };
                let wipe_w = match overlay.phase {
                    TransitionPhase::Out => sw * base_alpha,
                    TransitionPhase::In => sw * base_alpha,
                };
                draw_rectangle(wipe_x, 0.0, wipe_w, sh, Color::new(0.0, 0.0, 0.0, 1.0));
            }
            // 模糊效果（简化为淡入淡出，因为 macroquad 不支持模糊 shader）
            Transition::Blur => {
                draw_rectangle(0.0, 0.0, sw, sh, Color::new(0.0, 0.0, 0.0, base_alpha));
            }
            // Instant 不需要绘制任何过渡效果
            Transition::Instant => {}
        }
    }
}

#[allow(dead_code)]
fn draw_dim_overlay(sw: f32, sh: f32, alpha: f32) {
    draw_rectangle(0.0, 0.0, sw, sh, Color::new(0.0, 0.0, 0.0, alpha));
}

/// Crash-recovery prompt shown at startup when an autosave is detected.
///
/// Draws a centered modal dialog with a semi-transparent backdrop and two
/// choices: resume the autosave, or discard it and start fresh.
fn draw_autosave_prompt(engine: &Engine, buttons: &mut Vec<ButtonRect>, sw: f32, sh: f32, font: &Option<Font>, scale: f32) {
    // 居中对话框面板
    let dialog_w = (720.0 * scale).min(sw - 80.0 * scale);
    let dialog_h = (360.0 * scale).min(sh - 80.0 * scale);
    let dialog_x = (sw - dialog_w) / 2.0;
    let dialog_y = (sh - dialog_h) / 2.0;

    // Panel background + border.
    let tp = theme().primary;
    draw_rectangle(
        dialog_x,
        dialog_y,
        dialog_w,
        dialog_h,
        Color::new(tp.r, tp.g, tp.b, 0.97),
    );
    draw_rectangle_lines(
        dialog_x,
        dialog_y,
        dialog_w,
        dialog_h,
        2.0 * scale,
        Color::new(0.29, 0.62, 1.0, 0.9),
    );
    // Subtle top accent line.
    draw_rectangle(
        dialog_x,
        dialog_y,
        dialog_w,
        4.0 * scale,
        Color::new(0.29, 0.62, 1.0, 0.8),
    );

    let center_x = sw / 2.0;
    let mut cursor_y = dialog_y + 56.0 * scale;

    // Title.
    let title = engine.t_ui("autosave.title");
    let title_size = 36.0 * scale;
    let tw = measure_text_f(title, font, title_size as u16, 1.0).width;
    draw_text_f(
        title,
        center_x - tw / 2.0,
        cursor_y,
        title_size,
        Color::new(0.8, 0.9, 1.0, 1.0),
        font,
    );
    cursor_y += 50.0 * scale;

    // Divider.
    draw_rectangle(
        dialog_x + 40.0 * scale,
        cursor_y,
        dialog_w - 80.0 * scale,
        1.0 * scale,
        Color::new(0.4, 0.35, 0.55, 0.6),
    );
    cursor_y += 36.0 * scale;

    // Message (two lines for readability).
    let line1 = engine.t_ui("autosave.line1");
    let line2 = engine.t_ui("autosave.line2");
    let msg_size = 24.0 * scale;
    let l1w = measure_text_f(line1, font, msg_size as u16, 1.0).width;
    let l2w = measure_text_f(line2, font, msg_size as u16, 1.0).width;
    draw_text_f(line1, center_x - l1w / 2.0, cursor_y, msg_size, WHITE, font);
    cursor_y += 36.0 * scale;
    draw_text_f(line2, center_x - l2w / 2.0, cursor_y, msg_size, WHITE, font);
    cursor_y += 40.0 * scale;

    // Autosave summary (section + play time), if readable.
    if let Some(save) = engine.saves().load_autosave().ok() {
        let summary = format!(
            "进度：{}    游戏时间：{}",
            save.metadata.section_name,
            format_play_time(save.metadata.play_time_secs),
        );
        let summary_size = 22.0 * scale;
        let sw2 = measure_text_f(&summary, font, summary_size as u16, 1.0).width;
        draw_text_f(
            &summary,
            center_x - sw2 / 2.0,
            cursor_y,
            summary_size,
            Color::new(0.75, 0.75, 0.85, 1.0),
            font,
        );
    }

    // Action buttons.
    let btn_w = 240.0 * scale;
    let btn_h = 56.0 * scale;
    let gap = 40.0 * scale;
    let total_w = btn_w * 2.0 + gap;
    let btn1_x = center_x - total_w / 2.0;
    let btn2_x = btn1_x + btn_w + gap;
    let btn_y = dialog_y + dialog_h - btn_h - 36.0 * scale;

    draw_button(btn1_x, btn_y, btn_w, btn_h, engine.t_ui("autosave.continue"), buttons, ButtonAction::ContinueAutosave, font, scale);
    draw_button(btn2_x, btn_y, btn_w, btn_h, engine.t_ui("autosave.restart"), buttons, ButtonAction::DiscardAutosave, font, scale);
}

/// 绘制备注编辑弹窗：居中对话框 + 单行输入框（含光标）+ 确认/取消按钮。
///
/// 字符输入由主循环在 `ui_mode == NoteEditDialog` 时收集到 `buffer`，
/// 此函数只负责绘制当前 buffer 内容。40% 黑色遮罩已由调用方绘制。
fn draw_note_edit_dialog(
    engine: &Engine,
    buttons: &mut Vec<ButtonRect>,
    sw: f32,
    sh: f32,
    font: &Option<Font>,
    scale: f32,
    buffer: &str,
    slot: Option<usize>,
    fade: f32,
) {
    let dialog_w = (560.0 * scale).min(sw - 80.0 * scale);
    let dialog_h = (240.0 * scale).min(sh - 80.0 * scale);
    let dialog_x = (sw - dialog_w) / 2.0;
    let dialog_y = (sh - dialog_h) / 2.0;

    // 面板背景 + 边框。
    let tp = theme().primary;
    draw_rectangle(dialog_x, dialog_y, dialog_w, dialog_h, Color::new(tp.r, tp.g, tp.b, 0.97));
    draw_rectangle_lines(dialog_x, dialog_y, dialog_w, dialog_h, 2.0 * scale, Color::new(0.29, 0.62, 1.0, 0.9));

    let pad = 28.0 * scale;
    // 标题。带槽位时用 note.edit_title_slot（含 {slot} 占位符），否则用 note.edit_title。
    let title = match slot {
        Some(s) => engine.t_ui("note.edit_title_slot").replace("{slot}", &format!("{}", s + 1)),
        None => engine.t_ui("note.edit_title").to_string(),
    };
    let title_size = 24.0 * scale;
    draw_text_f(&title, dialog_x + pad, dialog_y + pad + title_size, title_size, WHITE, font);
    // 提示。
    let hint_size = 14.0 * scale;
    draw_text_f(
        engine.t_ui("note.hint"),
        dialog_x + pad,
        dialog_y + pad + title_size + 22.0 * scale,
        hint_size,
        Color::new(0.6, 0.65, 0.75, 1.0),
        font,
    );

    // 输入框。
    let input_x = dialog_x + pad;
    let input_y = dialog_y + pad + title_size + 40.0 * scale;
    let input_w = dialog_w - 2.0 * pad;
    let input_h = 48.0 * scale;
    draw_rectangle(input_x, input_y, input_w, input_h, Color::new(0.12, 0.14, 0.22, 1.0));
    draw_rectangle_lines(input_x, input_y, input_w, input_h, 1.5 * scale, Color::new(0.4, 0.55, 0.8, 0.8));

    // 输入框文本 + 光标。光标以闪烁的竖线表示，周期约 1s。
    let text_size = 20.0 * scale;
    let text_pad = 10.0 * scale;
    // 限制显示宽度，超出部分尾部截断（简单处理，不做滚动）。
    let max_text_w = input_w - 2.0 * text_pad;
    let display_text = fit_text(buffer, font, text_size, max_text_w - 8.0 * scale);
    draw_text_f(
        &display_text,
        input_x + text_pad,
        input_y + input_h / 2.0 + text_size / 2.5,
        text_size,
        Color::new(0.9, 0.92, 0.98, 1.0),
        font,
    );
    // 光标：在显示文本末尾画一条竖线，每秒闪烁。
    let cursor_blink = (get_time() * 2.0).floor() as i64 % 2 == 0;
    if cursor_blink {
        let cursor_x = input_x + text_pad
            + measure_text_f(&display_text, font, text_size as u16, 1.0).width
            + 2.0 * scale;
        draw_rectangle(cursor_x, input_y + 10.0 * scale, 2.0 * scale, input_h - 20.0 * scale, WHITE);
    }

    // 确认/取消按钮。
    let btn_w = 160.0 * scale;
    let btn_h = 48.0 * scale;
    let gap = 32.0 * scale;
    let total_w = btn_w * 2.0 + gap;
    let btn1_x = dialog_x + (dialog_w - total_w) / 2.0;
    let btn2_x = btn1_x + btn_w + gap;
    let btn_y = dialog_y + dialog_h - btn_h - 24.0 * scale;
    draw_button(btn1_x, btn_y, btn_w, btn_h, engine.t_ui("confirm.ok"), buttons, ButtonAction::NoteConfirm, font, scale);
    draw_button(btn2_x, btn_y, btn_w, btn_h, engine.t_ui("confirm.cancel"), buttons, ButtonAction::NoteCancel, font, scale);

    // 弹窗淡入遮罩：以面板底色覆盖整个面板区域，alpha = 1 - fade。
    // fade=0 时面板区域被同色矩形完全遮盖（看不见内容），fade=1 时无遮罩，
    // 中间过程内容平滑淡入，避免弹窗瞬间弹出。
    if fade < 1.0 {
        draw_rectangle(dialog_x, dialog_y, dialog_w, dialog_h, {
            let tp = theme().primary;
            Color::new(tp.r, tp.g, tp.b, 1.0 - fade)
        });
    }
}

/// Draw a confirmation dialog for returning to title or discarding settings.
/// 注意：全屏 10% 黑色叠层已由调用方绘制，此函数只绘制居中的对话框面板。
fn draw_confirm_dialog(engine: &Engine, buttons: &mut Vec<ButtonRect>, sw: f32, sh: f32, font: &Option<Font>, scale: f32, confirm_type: Option<ConfirmType>, fade: f32) {
    // Centered dialog panel.
    let dialog_w = (600.0 * scale).min(sw - 80.0 * scale);
    let dialog_h = (280.0 * scale).min(sh - 80.0 * scale);
    let dialog_x = (sw - dialog_w) / 2.0;
    let dialog_y = (sh - dialog_h) / 2.0;

    // Panel background + border.
    let tp = theme().primary;
    draw_rectangle(
        dialog_x,
        dialog_y,
        dialog_w,
        dialog_h,
        Color::new(tp.r, tp.g, tp.b, 0.97),
    );
    draw_rectangle_lines(
        dialog_x,
        dialog_y,
        dialog_w,
        dialog_h,
        2.0 * scale,
        Color::new(0.29, 0.62, 1.0, 0.9),
    );
    // Top accent line.
    draw_rectangle(
        dialog_x,
        dialog_y,
        dialog_w,
        4.0 * scale,
        Color::new(0.29, 0.62, 1.0, 0.8),
    );

    let center_x = sw / 2.0;
    let mut cursor_y = dialog_y + 48.0 * scale;

    // Title and message based on confirm type.
    let (title, message): (&str, &str) = match confirm_type {
        Some(ConfirmType::BackToTitle) => (engine.t_ui("confirm.return_title"), engine.t_ui("confirm.return_message")),
        Some(ConfirmType::UnappliedSettings) => (engine.t_ui("confirm.unapplied"), engine.t_ui("confirm.unapplied_message")),
        Some(ConfirmType::ScriptError) => (engine.t_ui("script_error.title"), engine.t_ui("script_error.message")),
        None => (engine.t_ui("confirm.default_title"), engine.t_ui("confirm.default_message")),
    };

    let title_size = 32.0 * scale;
    let tw = measure_text_f(title, font, title_size as u16, 1.0).width;
    draw_text_f(
        title,
        center_x - tw / 2.0,
        cursor_y,
        title_size,
        Color::new(0.8, 0.9, 1.0, 1.0),
        font,
    );
    cursor_y += 50.0 * scale;

    // Divider.
    draw_rectangle(
        dialog_x + 40.0 * scale,
        cursor_y,
        dialog_w - 80.0 * scale,
        1.0 * scale,
        Color::new(0.4, 0.35, 0.55, 0.6),
    );
    cursor_y += 30.0 * scale;

    // Message (split into lines).
    let msg_size = 22.0 * scale;
    for line in message.lines() {
        let lw = measure_text_f(line, font, msg_size as u16, 1.0).width;
        draw_text_f(line, center_x - lw / 2.0, cursor_y, msg_size, WHITE, font);
        cursor_y += 32.0 * scale;
    }

    // Action buttons.
    // 剧本错误警告只有"确定"按钮（居中），其他类型有"确定"+"取消"两个按钮。
    let btn_w = 180.0 * scale;
    let btn_h = 50.0 * scale;
    let btn_y = dialog_y + dialog_h - btn_h - 30.0 * scale;

    if confirm_type == Some(ConfirmType::ScriptError) {
        // 单按钮居中
        let btn_x = center_x - btn_w / 2.0;
        draw_button(btn_x, btn_y, btn_w, btn_h, engine.t_ui("script_error.ok"), buttons, ButtonAction::ConfirmYes, font, scale);
    } else {
        let gap = 30.0 * scale;
        let total_w = btn_w * 2.0 + gap;
        let btn1_x = center_x - total_w / 2.0;
        let btn2_x = btn1_x + btn_w + gap;
        draw_button(btn1_x, btn_y, btn_w, btn_h, engine.t_ui("confirm.ok"), buttons, ButtonAction::ConfirmYes, font, scale);
        draw_button(btn2_x, btn_y, btn_w, btn_h, engine.t_ui("confirm.cancel"), buttons, ButtonAction::ConfirmNo, font, scale);
    }

    // 弹窗淡入遮罩：以面板底色覆盖整个面板区域，alpha = 1 - fade。
    // fade=0 时面板区域被同色矩形完全遮盖（看不见内容），fade=1 时无遮罩，
    // 中间过程内容平滑淡入，避免弹窗瞬间弹出。
    if fade < 1.0 {
        draw_rectangle(dialog_x, dialog_y, dialog_w, dialog_h, {
            let tp = theme().primary;
            Color::new(tp.r, tp.g, tp.b, 1.0 - fade)
        });
    }
}

// ─── Menu drawing ───

/// 读取目录下的子目录名（仅目录，按名称排序）。供日志导出目录选择器使用。
/// 借鉴编辑器 `read_picker_entries` 的实现思路（无系统原生文件对话框）。
fn read_subdirs(dir: &std::path::Path) -> Vec<String> {
    let mut v = Vec::new();
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                if let Some(name) = p.file_name().and_then(|n| n.to_str()) {
                    v.push(name.to_string());
                }
            }
        }
    }
    v.sort_by(|a, b| a.to_lowercase().cmp(&b.to_lowercase()));
    v
}

/// 仿 Windows 蓝屏的错误界面。
///
/// 全屏蓝底白字，从上到下：表情符号 `:(`、粗体标题、报错模块、错误代码
/// （16 进制）、原因分析、固定警告行；右下角三个按钮：导出日志 / 尝试继续
/// 运行 / 退出引擎。`can_continue` 为 false 时"尝试继续运行"按钮置灰且不可点击。
fn draw_crash_screen(
    engine: &Engine,
    buttons: &mut Vec<ButtonRect>,
    sw: f32,
    sh: f32,
    font: &Option<Font>,
    scale: f32,
    fade: f32,
) {
    // Windows BSOD 蓝（#0078D7）。
    let bsod_blue = Color::new(0.0, 120.0 / 255.0, 215.0 / 255.0, 1.0);
    draw_rectangle(0.0, 0.0, sw, sh, bsod_blue);

    // 提取崩溃信息（module_key 为 &'static str，不持有 engine 借用）。
    let (module_key, code, can_continue) = match engine.crash_info() {
        Some(i) => (i.module_key, i.code, i.can_continue),
        None => ("error.module.unknown", crash::error_code::FATAL, false),
    };
    let module = engine.t_ui(module_key);
    let code_str = crash::format_code(code);
    let reason = engine.t_ui(crash::reason_key(code));

    let margin = 90.0 * scale;
    let mut y = 130.0 * scale;

    // 表情符号 :( （致敬 Windows 蓝屏）。
    let emo_size = 72.0 * scale;
    draw_text_f(":(", margin, y, emo_size, WHITE, font);
    y += 90.0 * scale;

    // 粗体标题：通过 1px 偏移重绘两次模拟加粗（macroquad 无粗体字重参数）。
    let title = engine.t_ui("crash.title");
    let title_size = 48.0 * scale;
    let draw_bold = |text: &str, x: f32, ly: f32, size: f32| {
        draw_text_f(text, x + 1.0, ly, size, WHITE, font);
        draw_text_f(text, x, ly + 1.0, size, WHITE, font);
        draw_text_f(text, x, ly, size, WHITE, font);
    };
    draw_bold(title, margin, y, title_size);
    y += 90.0 * scale;

    // 报错模块 / 错误代码 / 原因分析。
    let line_size = 28.0 * scale;
    let line_gap = 46.0 * scale;
    let label_module = engine.t_ui("crash.module_label");
    let label_code = engine.t_ui("crash.code_label");
    let label_reason = engine.t_ui("crash.reason_label");

    draw_text_f(
        &format!("{}{}", label_module, module),
        margin, y, line_size, WHITE, font,
    );
    y += line_gap;
    draw_text_f(
        &format!("{}{}", label_code, code_str),
        margin, y, line_size, WHITE, font,
    );
    y += line_gap;
    // 原因分析可能较长，按宽度简单截断显示，避免溢出屏幕。
    let reason_line = format!("{}{}", label_reason, reason);
    let max_w = sw - 2.0 * margin;
    let reason_display = truncate_text_to_width(&reason_line, font, line_size as u16, max_w);
    draw_text_f(&reason_display, margin, y, line_size, WHITE, font);

    // 固定警告行（黄色，突出）。
    let warning = engine.t_ui("crash.warning");
    let warn_size = 26.0 * scale;
    let warn_y = sh - 150.0 * scale;
    let warn_display = truncate_text_to_width(warning, font, warn_size as u16, max_w);
    draw_text_f(&warn_display, margin, warn_y, warn_size, Color::new(1.0, 0.85, 0.2, 1.0), font);

    // 右下角三个按钮：导出日志 / 尝试继续运行 / 退出引擎。
    let bw = 210.0 * scale;
    let bh = 56.0 * scale;
    let gap = 20.0 * scale;
    let right_margin = 50.0 * scale;
    let bottom_margin = 60.0 * scale;
    let total_w = 3.0 * bw + 2.0 * gap;
    let mut bx = sw - right_margin - total_w;
    let by = sh - bottom_margin - bh;

    draw_button(bx, by, bw, bh, engine.t_ui("crash.export_log"), buttons, ButtonAction::CrashExportLog, font, scale);
    bx += bw + gap;
    if can_continue {
        draw_button(bx, by, bw, bh, engine.t_ui("crash.continue"), buttons, ButtonAction::CrashContinue, font, scale);
    } else {
        // 置灰且不注册（不可点击）。
        draw_rectangle(bx, by, bw, bh, Color::new(0.13, 0.4, 0.7, 1.0));
        draw_rectangle_lines(bx, by, bw, bh, 2.0 * scale, Color::new(0.5, 0.7, 0.9, 0.7));
        let fs = (bh * 0.4).min(28.0 * scale);
        let lbl = engine.t_ui("crash.continue");
        let tw = measure_text_f(lbl, font, fs as u16, 1.0).width;
        draw_text_f(lbl, bx + (bw - tw) / 2.0, by + bh / 2.0 + fs / 3.0, fs, Color::new(0.75, 0.85, 0.95, 0.8), font);
    }
    bx += bw + gap;
    draw_button(bx, by, bw, bh, engine.t_ui("crash.exit"), buttons, ButtonAction::CrashExit, font, scale);

    // 淡入遮罩：以蓝屏底色覆盖全屏，alpha = 1 - fade。
    if fade < 1.0 {
        draw_rectangle(0.0, 0.0, sw, sh, Color::new(bsod_blue.r, bsod_blue.g, bsod_blue.b, 1.0 - fade));
    }
}

/// 按目标宽度截断文本（保留开头部分，末尾加省略号）。
fn truncate_text_to_width(text: &str, font: &Option<Font>, font_size: u16, max_w: f32) -> String {
    if measure_text_f(text, font, font_size, 1.0).width <= max_w {
        return text.to_string();
    }
    let ellipsis = "…";
    let ell_w = measure_text_f(ellipsis, font, font_size, 1.0).width;
    let mut s = String::new();
    for c in text.chars() {
        let candidate = format!("{}{}", s, c);
        if measure_text_f(&candidate, font, font_size, 1.0).width + ell_w > max_w {
            s.push_str(ellipsis);
            return s;
        }
        s = candidate;
    }
    s
}

/// 日志导出目录选择器：蓝底面板，列出当前目录的子目录，支持上级 / 进入 /
/// 滚轮滚动 / 确认导出 / 取消。借鉴编辑器应用内文件浏览器实现。
fn draw_dir_picker(
    engine: &Engine,
    buttons: &mut Vec<ButtonRect>,
    sw: f32,
    sh: f32,
    font: &Option<Font>,
    scale: f32,
    current_dir: &std::path::Path,
    entries: &[String],
    scroll: f32,
    status: &str,
    fade: f32,
) {
    let bsod_blue = Color::new(0.0, 120.0 / 255.0, 215.0 / 255.0, 1.0);
    draw_rectangle(0.0, 0.0, sw, sh, bsod_blue);

    let margin = 80.0 * scale;
    let mut y = 70.0 * scale;

    // 标题。
    let title = engine.t_ui("crash.dir_picker_title");
    let title_size = 40.0 * scale;
    draw_text_f(title, margin, y, title_size, WHITE, font);
    y += 60.0 * scale;

    // 当前路径。
    let path_str = current_dir.display().to_string();
    let path_display = truncate_text_to_width(&path_str, font, 22, sw - 2.0 * margin);
    draw_text_f(&path_display, margin, y, 22.0 * scale, Color::new(0.85, 0.95, 1.0, 1.0), font);
    y += 40.0 * scale;

    // 上级目录按钮。
    let up_w = 180.0 * scale;
    let up_h = 44.0 * scale;
    draw_button(margin, y, up_w, up_h, engine.t_ui("crash.dir_up"), buttons, ButtonAction::DirUp, font, scale);
    y += up_h + 20.0 * scale;

    // 列表区域。
    let list_x = margin;
    let list_w = sw - 2.0 * margin;
    let list_bottom = sh - 180.0 * scale;
    let list_h = (list_bottom - y).max(100.0 * scale);
    // 列表背景。
    draw_rectangle(list_x, y, list_w, list_h, Color::new(0.0, 0.45, 0.8, 0.5));
    draw_rectangle_lines(list_x, y, list_w, list_h, 2.0 * scale, Color::new(1.0, 1.0, 1.0, 0.6));

    let row_h = 42.0 * scale;
    let visible_count = ((list_h / row_h) as usize).max(1);
    let total = entries.len();
    let start_idx = (scroll.floor() as usize).min(total.saturating_sub(visible_count));

    // 绘制可见条目（裁剪到列表区域）。
    // macroquad 无内置裁剪，这里靠只绘制可见行 + 坐标落在列表内来实现。
    for i in 0..visible_count {
        let idx = start_idx + i;
        if idx >= total {
            break;
        }
        let row_y = y + i as f32 * row_h;
        let name = &entries[idx];
        // 条目按钮（占列表行宽，留小内边距）。
        let entry_x = list_x + 6.0 * scale;
        let entry_w = list_w - 12.0 * scale;
        draw_button(entry_x, row_y + 3.0, entry_w, row_h - 6.0, name, buttons, ButtonAction::DirEntry(idx), font, scale);
    }

    // 空目录提示。
    if total == 0 {
        let hint = engine.t_ui("crash.dir_empty");
        let hw = measure_text_f(hint, font, 22, 1.0).width;
        draw_text_f(hint, list_x + (list_w - hw) / 2.0, y + list_h / 2.0, 22.0 * scale, Color::new(0.9, 0.9, 0.9, 0.9), font);
    }

    // 状态提示（导出成功/失败）。
    if !status.is_empty() {
        let status_y = sh - 130.0 * scale;
        let status_display = truncate_text_to_width(status, font, 22, sw - 2.0 * margin);
        draw_text_f(&status_display, margin, status_y, 22.0 * scale, Color::new(1.0, 0.92, 0.4, 1.0), font);
    }

    // 底部按钮：确认导出到此目录 / 取消。
    let bw = 240.0 * scale;
    let bh = 52.0 * scale;
    let gap = 24.0 * scale;
    let right_margin = 60.0 * scale;
    let bottom_margin = 50.0 * scale;
    let total_bw = 2.0 * bw + gap;
    let mut bx = sw - right_margin - total_bw;
    let by = sh - bottom_margin - bh;
    draw_button(bx, by, bw, bh, engine.t_ui("crash.dir_confirm"), buttons, ButtonAction::DirConfirm, font, scale);
    bx += bw + gap;
    draw_button(bx, by, bw, bh, engine.t_ui("crash.dir_cancel"), buttons, ButtonAction::DirCancel, font, scale);

    // 淡入遮罩。
    if fade < 1.0 {
        draw_rectangle(0.0, 0.0, sw, sh, Color::new(bsod_blue.r, bsod_blue.g, bsod_blue.b, 1.0 - fade));
    }
}

/// Draw the full-screen panel background + title for the save/load menus.
fn draw_panel(sw: f32, sh: f32, title: &str, font: &Option<Font>, scale: f32) {
    // 天蓝色全屏背景。
    draw_rectangle(0.0, 0.0, sw, sh, Color::new(0.1, 0.2, 0.4, 1.0));
    // 天蓝色边框。
    draw_rectangle_lines(0.0, 0.0, sw, sh, 2.0 * scale, Color::new(0.45, 0.7, 0.95, 0.8));

    let title_size = 62.0 * scale;
    let tw = measure_text_f(title, font, title_size as u16, 1.0).width;
    draw_text_f(title, (sw - tw) / 2.0, sh * 0.09, title_size, WHITE, font);
}

async fn draw_save_menu(engine: &Engine, buttons: &mut Vec<ButtonRect>, sw: f32, sh: f32, font: &Option<Font>, scale: f32, page: usize, displayed_slots: usize, assets: &mut AssetManager) {
    draw_panel(sw, sh, engine.t_ui("save.save_title"), font, scale);

    let saves = engine.saves();
    let max_slots = saves.max_slots();
    let all_saves = saves.list_saves();

    draw_slot_grid(engine, sw, sh, font, scale, page, displayed_slots, max_slots, &all_saves, buttons, true, assets).await;

    // Back button (bottom-left).
    let back_w = 240.0 * scale;
    let back_h = 62.0 * scale;
    draw_button(
        48.0 * scale,
        sh - back_h - 36.0 * scale,
        back_w,
        back_h,
        engine.t_ui("save.back"),
        buttons,
        ButtonAction::CloseMenu,
        font,
        scale,
    );
}

async fn draw_load_menu(engine: &Engine, buttons: &mut Vec<ButtonRect>, sw: f32, sh: f32, font: &Option<Font>, scale: f32, page: usize, displayed_slots: usize, assets: &mut AssetManager) {
    draw_panel(sw, sh, engine.t_ui("save.load_title"), font, scale);

    let saves = engine.saves();
    let max_slots = saves.max_slots();
    let all_saves = saves.list_saves();

    draw_slot_grid(engine, sw, sh, font, scale, page, displayed_slots, max_slots, &all_saves, buttons, false, assets).await;

    // Back button (bottom-left).
    let back_w = 240.0 * scale;
    let back_h = 62.0 * scale;
    draw_button(
        48.0 * scale,
        sh - back_h - 36.0 * scale,
        back_w,
        back_h,
        engine.t_ui("save.back"),
        buttons,
        ButtonAction::CloseMenu,
        font,
        scale,
    );
}

/// Draw the 2×4 grid of save/load slots plus the page navigation control.
///
/// `is_save` selects the click action attached to each cell
/// (`SaveSlot` for the save menu, `LoadSlot` for the load menu). The grid
/// iterates over `displayed_slots` (the number of slots currently surfaced to
/// the player) rather than the hard `max_slots` cap, so the player can grow
/// the visible range one page at a time via the "+" button.
async fn draw_slot_grid(
    engine: &Engine,
    sw: f32,
    sh: f32,
    font: &Option<Font>,
    scale: f32,
    page: usize,
    displayed_slots: usize,
    max_slots: usize,
    all_saves: &[Option<SaveMetadata>],
    buttons: &mut Vec<ButtonRect>,
    is_save: bool,
    assets: &mut AssetManager,
) {
    let cols = 4; // 2 rows × 4 columns = SLOTS_PER_PAGE
    // 存档页放大倍率：在 dpi 适配系数 `scale` 之上再叠加 1.2x 放大，
    // 让存档格子更饱满。cell_w 按不溢出屏幕宽度反算（充分利用横向空间），
    // cell_h 与内部元素按 1.2x 放大，保留多 dpi 适配。
    const SAVE_ZOOM: f32 = 1.2;
    let eff = scale * SAVE_ZOOM; // 有效倍率 = dpi × 存档页放大
    let gap_x = 29.0 * eff;
    let gap_y = 29.0 * eff;
    let cell_h = 240.0 * eff;
    // cell_w：按 4 列不溢出 sw 反算，左右各留 24×scale 边距。
    // 同时不超过基准 408×eff（避免超宽屏上格子过宽破坏比例）。
    let usable_w = sw - 2.0 * 24.0 * scale;
    let cell_w_max = 408.0 * eff;
    let cell_w = ((usable_w - (cols - 1) as f32 * gap_x) / cols as f32).min(cell_w_max);

    let grid_w = cols as f32 * cell_w + (cols - 1) as f32 * gap_x;
    let grid_x = (sw - grid_w) / 2.0;
    let grid_y = sh * 0.22;

    // At most one cell can be hovered per frame; remember its tooltip text so
    // it can be rendered last (above the page nav and neighbouring cells).
    let mut hovered_tooltip: Option<String> = None;

    for i in 0..SLOTS_PER_PAGE {
        let slot = page * SLOTS_PER_PAGE + i;
        if slot >= displayed_slots {
            break;
        }
        let col = i % cols;
        let row = i / cols;
        let x = grid_x + col as f32 * (cell_w + gap_x);
        let y = grid_y + row as f32 * (cell_h + gap_y);

        // Cloning the metadata here is cheap and keeps the borrow simple.
        let meta_opt = all_saves.get(slot).and_then(|o| o.as_ref());
        let meta_clone = meta_opt.cloned();
        let action = if is_save {
            ButtonAction::SaveSlot(slot)
        } else {
            ButtonAction::LoadSlot(slot)
        };
        if let Some(t) = draw_slot_cell(engine, x, y, cell_w, cell_h, slot, meta_clone.as_ref(), buttons, font, eff, action, assets).await {
            hovered_tooltip = Some(t);
        }
    }

    // Page navigation (bottom-right): [←] [page/total] [→], plus an extra
    // "+" button on the last page when more slots can be revealed.
    let total_pages = ((displayed_slots + SLOTS_PER_PAGE - 1) / SLOTS_PER_PAGE).max(1);
    let can_add_page = displayed_slots < max_slots;
    draw_page_nav(buttons, sw, sh, font, eff, page, total_pages, can_add_page);

    // Draw the hover tooltip last so it floats above the grid cells and the
    // page navigation control.
    if let Some(text) = hovered_tooltip {
        let (mx, my) = mouse_position();
        draw_tooltip(&text, mx, my, sw, sh, font, scale);
    }
}

/// Draw a single save/load slot cell with its metadata summary.  Empty slots
/// show "空" with a semi-transparent overlay and are still registered as
/// clickable (the save menu writes to them; the load menu no-ops on them).
///
/// 单元格布局：左侧缩略图（按场景快照重绘）+ 右侧文字栏（章节名/描述/备注）。
/// 缩略图通过读取存档的 `SceneSnapshot`，按比例缩小重绘背景与立绘，无需
/// macroquad 截图 API，跨平台稳定。备注以斜体灰色显示在底部，1 行省略。
///
/// Returns `Some(text)` containing the full save description when the mouse
/// hovers over a populated cell, so the caller can render a tooltip with the
/// untruncated text on top of every other element.
async fn draw_slot_cell(
    engine: &Engine,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    slot: usize,
    meta: Option<&SaveMetadata>,
    buttons: &mut Vec<ButtonRect>,
    font: &Option<Font>,
    scale: f32,
    action: ButtonAction,
    assets: &mut AssetManager,
) -> Option<String> {
    let (mx, my) = mouse_position();
    let hover = mx >= x && mx <= x + w && my >= y && my <= y + h;

    // Cell background（天蓝色主题）。
    draw_rectangle(x, y, w, h, Color::new(0.15, 0.3, 0.55, 0.95));
    draw_rectangle_lines(x, y, w, h, 1.5 * scale, Color::new(0.45, 0.7, 0.95, 0.7));

    let pad = 14.0 * scale;
    let slot_label = format!("{} {}", engine.t_ui("save.slot"), slot + 1);

    // 布局尺寸（均基于 scale，多分辨率自适应）。
    // 左侧缩略图区：16:9 长方形（与游戏画面比例一致），避免背景图被裁切或超出边框。
    let thumb_w = 192.0 * scale;
    let thumb_h = 108.0 * scale;
    let thumb_x = x + pad;
    let thumb_y = y + 44.0 * scale;
    // 右侧文字栏起点。
    let text_x = thumb_x + thumb_w + 12.0 * scale;
    let text_w = x + w - pad - text_x;
    // 「编辑备注」小按钮：缩略图下方，仅在有存档时显示。
    let note_btn_w = thumb_w;
    let note_btn_h = 24.0 * scale;
    let note_btn_x = thumb_x;
    let note_btn_y = thumb_y + thumb_h + 6.0 * scale;

    // 顶部：槽位号 + 时间戳（横排）。
    draw_text_f(
        &slot_label,
        x + pad,
        y + 29.0 * scale,
        22.0 * scale,
        Color::new(0.8, 0.9, 1.0, 1.0),
        font,
    );

    let mut tooltip: Option<String> = None;

    if let Some(m) = meta {
        let ts = format_timestamp(m.timestamp);
        // 时间戳右对齐到单元格右边。
        let ts_w = measure_text_f(&ts, font, (16.0 * scale) as u16, 1.0).width;
        draw_text_f(
            &ts,
            x + w - pad - ts_w,
            y + 29.0 * scale,
            16.0 * scale,
            Color::new(0.7, 0.7, 0.8, 1.0),
            font,
        );

        // 左侧缩略图：读取存档的完整数据（含场景快照）重绘。
        let full_save: Option<SaveSlot> = engine.saves().load_slot_full(slot);
        draw_slot_thumbnail(full_save.as_ref(), assets, thumb_x, thumb_y, thumb_w, thumb_h, scale, font).await;

        // 右侧文字栏：章节名 + 描述 + 备注。
        let section_size = 18.0 * scale;
        let section_display = fit_text(&m.section_name, font, section_size, text_w);
        let section_truncated = section_display.ends_with('…') && m.section_name != section_display;
        draw_text_f(&section_display, text_x, thumb_y + section_size, section_size, WHITE, font);

        // 描述：按可用高度动态计算行数（放大后区域变大可显示更多行），
        // 字符换行 + 末行省略。可用高度 = 缩略图高度 - 章节名行 - 间距，
        // 行高 = desc_size + 行间距。
        let desc_size = 15.0 * scale;
        let desc_line_h = desc_size + 4.0 * scale;
        // 描述区域底部留出备注行高度（有备注时）或贴齐缩略图底部（无备注时）。
        let has_note = m.note.as_deref().filter(|s| !s.is_empty()).is_some();
        let desc_bottom = if has_note {
            thumb_y + thumb_h - 16.0 * scale // 留备注行空间
        } else {
            thumb_y + thumb_h - 4.0 * scale
        };
        let desc_top = thumb_y + section_size + 8.0 * scale;
        let desc_avail_h = (desc_bottom - desc_top).max(desc_line_h);
        let desc_max_lines = ((desc_avail_h / desc_line_h) as usize).max(1);
        let desc_lines = wrap_text_cn(&m.description, font, desc_size, text_w, desc_max_lines);
        // 判断描述是否被截断：wrap_text_cn 超过 max_lines 会省略末行。
        let desc_truncated = {
            // 重新算不限制行数时的总行数，若大于 desc_max_lines 则被截断。
            let full_lines = wrap_text_cn(&m.description, font, desc_size, text_w, usize::MAX);
            full_lines.len() > desc_lines.len() || desc_lines.last().map(|l| l.ends_with('…')).unwrap_or(false)
        };
        let mut desc_y = desc_top;
        for line in &desc_lines {
            draw_text_f(line, text_x, desc_y, desc_size, Color::new(0.75, 0.75, 0.85, 1.0), font);
            desc_y += desc_line_h;
        }

        // 备注：玩家自定义，斜体灰色，1 行省略。空视为无备注。
        let mut note_truncated = false;
        if let Some(note) = m.note.as_deref().filter(|s| !s.is_empty()) {
            let note_size = 14.0 * scale;
            let note_prefix = "📝 ";
            let note_full = format!("{}{}", note_prefix, note);
            let note_display = fit_text(&note_full, font, note_size, text_w);
            note_truncated = note_display.ends_with('…');
            draw_text_f(
                &note_display,
                text_x,
                thumb_y + thumb_h - 2.0 * scale,
                note_size,
                Color::new(0.6, 0.8, 0.6, 1.0),
                font,
            );
        }

        // hover 时若任一文字被截断，组合完整信息显示 tooltip：
        // 章节名 / 描述 / 备注，缺省项跳过。
        if hover && (section_truncated || desc_truncated || note_truncated) {
            let mut parts: Vec<String> = Vec::new();
            if section_truncated {
                parts.push(m.section_name.clone());
            }
            if desc_truncated {
                parts.push(m.description.clone());
            }
            if note_truncated {
                if let Some(note) = m.note.as_deref().filter(|s| !s.is_empty()) {
                    parts.push(format!("📝 {}", note));
                }
            }
            if !parts.is_empty() {
                tooltip = Some(parts.join("\n"));
            }
        }

        // 「编辑备注」小按钮：缩略图下方，点击弹出输入框。
        let note_label = if m.note.as_deref().filter(|s| !s.is_empty()).is_some() {
            "改备注"
        } else {
            "加备注"
        };
        let note_label_size = 13.0 * scale;
        let (nmx, nmy) = mouse_position();
        let note_hover = nmx >= note_btn_x && nmx <= note_btn_x + note_btn_w
            && nmy >= note_btn_y && nmy <= note_btn_y + note_btn_h;
        draw_rectangle(
            note_btn_x,
            note_btn_y,
            note_btn_w,
            note_btn_h,
            if note_hover { Color::new(0.25, 0.45, 0.7, 1.0) } else { Color::new(0.18, 0.32, 0.55, 1.0) },
        );
        draw_rectangle_lines(note_btn_x, note_btn_y, note_btn_w, note_btn_h, 1.0 * scale, Color::new(0.45, 0.7, 0.95, 0.6));
        let nlw = measure_text_f(note_label, font, note_label_size as u16, 1.0).width;
        draw_text_f(
            note_label,
            note_btn_x + (note_btn_w - nlw) / 2.0,
            note_btn_y + note_btn_h - 7.0 * scale,
            note_label_size,
            Color::new(0.85, 0.9, 1.0, 1.0),
            font,
        );
        buttons.push(ButtonRect {
            x: note_btn_x,
            y: note_btn_y,
            w: note_btn_w,
            h: note_btn_h,
            label: note_label.to_string(),
            action: ButtonAction::EditNote(slot),
        });
    } else {
        // Empty slot: centered "空" + a dimming overlay (visually disabled).
        let empty_size = 29.0 * scale;
        let label = engine.t_ui("save.empty");
        let lw = measure_text_f(label, font, empty_size as u16, 1.0).width;
        draw_text_f(
            label,
            x + (w - lw) / 2.0,
            y + h / 2.0 + 10.0 * scale,
            empty_size,
            Color::new(0.5, 0.5, 0.55, 1.0),
            font,
        );
        draw_rectangle(x, y, w, h, Color::new(0.0, 0.0, 0.0, 0.4));
    }

    buttons.push(ButtonRect {
        x,
        y,
        w,
        h,
        label: slot_label,
        action,
    });

    tooltip
}

/// 绘制存档缩略图：根据存档的场景快照，按比例缩小重绘背景与立绘。
///
/// 缩略图区域为 `thumb_w × thumb_h`（逻辑像素），采用 16:9 比例（与游戏
/// 画面一致）。背景按 contain 模式缩放（完整显示在区域内，不超出边框），
/// 立绘按原比例缩小到缩略图高度。无场景快照时：存档存在则用标题图（title.png）
/// 作为兜底画面，确保每个有存档的槽位都有预览图；无存档则绘制占位符文字。
async fn draw_slot_thumbnail(
    save: Option<&SaveSlot>,
    assets: &mut AssetManager,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    _scale: f32,
    font: &Option<Font>,
) {
    // 缩略图背景框。
    draw_rectangle(x, y, w, h, Color::new(0.05, 0.05, 0.08, 1.0));
    draw_rectangle_lines(x, y, w, h, 1.0, Color::new(0.3, 0.4, 0.55, 0.6));

    let scene = match save.and_then(|s| s.scene.as_ref()) {
        Some(s) => s,
        None => {
            // 无场景快照：旧格式存档未保存场景数据。有存档的槽位必须出画面，
            // 因此用标题图（title.png，始终可用）作 contain 模式兜底绘制。
            // 仅当 save 为 None（理论上不会发生，调用方仅在有存档时调用本函数）
            // 才回退到占位符文字。
            if save.is_some() {
                if let Some(tex) = assets.get_texture(AssetKind::Title, "./title.png").await {
                    let tex_w = tex.width();
                    let tex_h = tex.height();
                    if tex_w > 0.0 && tex_h > 0.0 {
                        let s = (w / tex_w).min(h / tex_h);
                        let dw = tex_w * s;
                        let dh = tex_h * s;
                        let dx = x + (w - dw) / 2.0;
                        let dy = y + (h - dh) / 2.0;
                        draw_texture_ex(
                            tex.clone(),
                            dx,
                            dy,
                            WHITE,
                            DrawTextureParams {
                                dest_size: Some(Vec2::new(dw, dh)),
                                ..Default::default()
                            },
                        );
                        return;
                    }
                }
                // 标题图加载失败：用深色底占位，避免空白。
                draw_rectangle(x, y, w, h, Color::new(0.12, 0.15, 0.22, 1.0));
                return;
            }
            let label = "无预览";
            let size = 16.0;
            let tw = measure_text_f(label, font, size as u16, 1.0).width;
            draw_text_f(
                label,
                x + (w - tw) / 2.0,
                y + h / 2.0 + size / 2.5,
                size,
                Color::new(0.4, 0.45, 0.55, 0.8),
                font,
            );
            return;
        }
    };

    // 绘制背景（cover 模式：填满缩略图区域，超出部分裁剪）。
    // 与主场景 draw_background 一致，让缩略图"和用户看到的一样"。
    // 用 draw_texture_clipped 把绘制限定在缩略图矩形 (x, y, w, h) 内。
    if let Some(bg) = &scene.background {
        if let Some(tex) = assets.get_texture(AssetKind::Bg, &bg.name).await {
            let tex_w = tex.width();
            let tex_h = tex.height();
            if tex_w > 0.0 && tex_h > 0.0 {
                // cover：取较大的缩放比，填满缩略图区域。
                let s = (w / tex_w).max(h / tex_h);
                let dw = tex_w * s;
                let dh = tex_h * s;
                let dx = x + (w - dw) / 2.0 + bg.offset_x * w;
                let dy = y + (h - dh) / 2.0 + bg.offset_y * h;
                draw_texture_clipped(
                    &tex, dx, dy, dw, dh, tex_w, tex_h,
                    Color::new(1.0, 1.0, 1.0, bg.alpha),
                    x, y, w, h,
                );
            }
        } else {
            // 背景纹理缺失：用资源名哈希色占位（已限定在缩略图区域内）。
            let c = name_to_color(&bg.name);
            draw_rectangle(x, y, w, h, Color::new(c.0, c.1, c.2, bg.alpha));
        }
    }

    // 绘制立绘（按缩略图高度等比缩小，超出部分裁剪到缩略图区域内）。
    // 与主场景 draw_characters 逻辑一致，但所有坐标基于缩略图尺寸，
    // 并用 draw_texture_clipped 裁掉超出 16:9 区域的部分，避免立绘
    // 突出到缩略图边框外。
    for char_state in &scene.characters {
        let x_frac = char_state.custom_x.unwrap_or_else(|| char_state.position.x_fraction());
        let y_frac = char_state.custom_y.unwrap_or(1.0);
        let sprite_name = char_state.pose.clone().unwrap_or_else(|| char_state.name.clone());
        if let Some(tex) = assets.get_texture(AssetKind::Character, &sprite_name).await {
            let tex_w = tex.width();
            let tex_h = tex.height();
            if tex_w > 0.0 && tex_h > 0.0 {
                // 立绘高度为缩略图高度的 80%，再乘以 char_state.scale。
                let scale_factor = (h * 0.8) / tex_h * char_state.scale;
                let dw = tex_w * scale_factor;
                let dh = tex_h * scale_factor;
                let dx = w * x_frac - dw / 2.0 + char_state.offset_x * (w / 1920.0);
                // y_frac=1.0（底部）时贴齐缩略图底部；否则中心点对齐。
                let dy = if (y_frac - 1.0).abs() < 0.001 {
                    h - dh - 50.0 * (h / 1080.0)
                } else {
                    h * y_frac - dh / 2.0
                };
                draw_texture_clipped(
                    &tex, x + dx, y + dy, dw, dh, tex_w, tex_h,
                    Color::new(1.0, 1.0, 1.0, char_state.alpha),
                    x, y, w, h,
                );
            }
        }
    }
}

/// 绘制纹理并裁剪到指定矩形区域内（超出部分不显示）。
///
/// 用于存档缩略图：背景用 cover 模式会超出 16:9 缩略图区域，立绘按位置
/// 定位也可能超出，必须裁剪避免突出到缩略图边框外污染相邻格子。
///
/// # 实现
///
/// macroquad 0.3 的 `DrawTextureParams.source` 接受源纹理像素坐标的 `Rect`，
/// 表示只绘制源纹理的这一部分。本函数计算目标矩形 `(dx, dy, dw, dh)` 与
/// 裁剪矩形 `(clip_x, clip_y, clip_w, clip_h)` 的交集，把交集映射回源纹理
/// 坐标，用 `source` 只绘制可见部分，`dest_size` 设为交集大小。
///
/// 参数：
/// - `tex`：纹理。
/// - `dx, dy, dw, dh`：目标绘制矩形（屏幕坐标）。
/// - `tex_w, tex_h`：源纹理原始尺寸（像素）。
/// - `tint`：着色（含 alpha）。
/// - `clip_x, clip_y, clip_w, clip_h`：裁剪矩形（屏幕坐标），只绘制此区域内的部分。
fn draw_texture_clipped(
    tex: &Texture2D,
    dx: f32, dy: f32, dw: f32, dh: f32,
    tex_w: f32, tex_h: f32,
    tint: Color,
    clip_x: f32, clip_y: f32, clip_w: f32, clip_h: f32,
) {
    // 目标矩形与裁剪矩形的交集（屏幕坐标）。
    let ix0 = dx.max(clip_x);
    let iy0 = dy.max(clip_y);
    let ix1 = (dx + dw).min(clip_x + clip_w);
    let iy1 = (dy + dh).min(clip_y + clip_h);
    let iw = ix1 - ix0;
    let ih = iy1 - iy0;
    if iw <= 0.0 || ih <= 0.0 {
        return; // 无交集，不绘制。
    }
    // 交集在目标矩形内的偏移比例，映射回源纹理坐标。
    let u0 = (ix0 - dx) / dw;
    let v0 = (iy0 - dy) / dh;
    let u1 = (ix1 - dx) / dw;
    let v1 = (iy1 - dy) / dh;
    // 源纹理像素坐标（macroquad 的 source 用像素坐标）。
    let src_x = u0 * tex_w;
    let src_y = v0 * tex_h;
    let src_w = (u1 - u0) * tex_w;
    let src_h = (v1 - v0) * tex_h;
    draw_texture_ex(
        tex.clone(),
        ix0, iy0,
        tint,
        DrawTextureParams {
            dest_size: Some(Vec2::new(iw, ih)),
            source: Some(Rect {
                x: src_x,
                y: src_y,
                w: src_w,
                h: src_h,
            }),
            ..Default::default()
        },
    );
}

/// Draw the page navigation control anchored to the bottom-right corner:
/// a "←" (previous) button, a "page/total" indicator, and a "→" (next) button.
/// When `can_add_page` is true and the current page is the last one, an extra
/// "+" button is drawn to the left of the "←" button, letting the player grow
/// the visible slot range by one page.
fn draw_page_nav(
    buttons: &mut Vec<ButtonRect>,
    sw: f32,
    sh: f32,
    font: &Option<Font>,
    scale: f32,
    page: usize,
    total_pages: usize,
    can_add_page: bool,
) {
    let btn_w = 72.0 * scale;
    let btn_h = 53.0 * scale;
    let gap = 19.0 * scale;

    let label = format!("{}/{}", page + 1, total_pages);
    let label_size = 24.0 * scale;
    let lw = measure_text_f(&label, font, label_size as u16, 1.0).width;
    let label_w = lw + 29.0 * scale;

    // The "+" button sits to the left of "←" and only appears on the last page
    // when more slots can still be revealed.
    let is_last_page = page + 1 >= total_pages;
    let show_add = is_last_page && can_add_page;
    let add_w = if show_add { btn_w + gap } else { 0.0 };

    let total_w = add_w + btn_w * 2.0 + gap * 2.0 + label_w;
    let x0 = sw - total_w - 48.0 * scale;
    let y = sh - btn_h - 36.0 * scale;

    if show_add {
        draw_button(x0, y, btn_w, btn_h, "+", buttons, ButtonAction::AddPage, font, scale);
    }

    draw_button(x0 + add_w, y, btn_w, btn_h, "←", buttons, ButtonAction::PrevPage, font, scale);

    draw_text_f(
        &label,
        x0 + add_w + btn_w + gap + (label_w - lw) / 2.0,
        y + btn_h / 2.0 + label_size / 3.0,
        label_size,
        WHITE,
        font,
    );

    draw_button(
        x0 + add_w + btn_w + gap + label_w,
        y,
        btn_w,
        btn_h,
        "→",
        buttons,
        ButtonAction::NextPage,
        font,
        scale,
    );
}

/// Truncate `text` (appending an ellipsis) so it fits within `max_w` at the
/// given font size.  Used to keep slot summaries inside their grid cells.
fn fit_text(text: &str, font: &Option<Font>, font_size: f32, max_w: f32) -> String {
    if max_w <= 0.0 {
        return String::new();
    }
    if measure_text_f(text, font, font_size as u16, 1.0).width <= max_w {
        return text.to_string();
    }
    let mut chars: Vec<char> = text.chars().collect();
    while !chars.is_empty() {
        chars.pop();
        let mut s: String = chars.iter().collect();
        s.push('…');
        if measure_text_f(&s, font, font_size as u16, 1.0).width <= max_w {
            return s;
        }
    }
    String::new()
}

/// Wrap `text` into at most `max_lines` lines that each fit within `max_w` at
/// the given font size.  Wrapping is done character-by-character (CJK text has
/// no whitespace word boundaries), breaking whenever the next character would
/// overflow the line width.  If the text needs more than `max_lines` lines,
/// the final line is truncated and an ellipsis "…" is appended.
fn wrap_text_cn(text: &str, font: &Option<Font>, font_size: f32, max_w: f32, max_lines: usize) -> Vec<String> {
    if max_lines == 0 || max_w <= 0.0 {
        return Vec::new();
    }

    // First pass: wrap into as many lines as needed.
    let mut all_lines: Vec<String> = Vec::new();
    let mut current = String::new();
    for c in text.chars() {
        let test = format!("{}{}", current, c);
        let w = measure_text_f(&test, font, font_size as u16, 1.0).width;
        if w > max_w && !current.is_empty() {
            all_lines.push(std::mem::take(&mut current));
            current.push(c);
        } else {
            current = test;
        }
    }
    if !current.is_empty() {
        all_lines.push(current);
    }

    // Within budget: return as-is.
    if all_lines.len() <= max_lines {
        return all_lines;
    }

    // Over budget: keep only `max_lines` lines and ellipsize the last one.
    let mut result: Vec<String> = all_lines.into_iter().take(max_lines).collect();
    let last = result.last_mut().expect("max_lines >= 1");
    // Keep trimming the last line until "last + …" fits within max_w.
    loop {
        let mut probe = last.clone();
        probe.push('…');
        if measure_text_f(&probe, font, font_size as u16, 1.0).width <= max_w || last.is_empty() {
            *last = probe;
            break;
        }
        last.pop();
    }
    result
}

/// Draw a tooltip showing the full `text` near the mouse cursor.  The tooltip
/// is a semi-transparent dark box with a border, positioned just below the
/// cursor; if it would run off the bottom of the screen it flips above the
/// cursor instead.  Wrapping uses `wrap_text_cn` so long descriptions stay
/// readable.
fn draw_tooltip(text: &str, mouse_x: f32, mouse_y: f32, sw: f32, sh: f32, font: &Option<Font>, scale: f32) {
    if text.is_empty() {
        return;
    }
    let font_size = 16.0 * scale;
    let pad = 8.0 * scale;
    let max_w = 360.0 * scale;
    let lines = wrap_text_cn(text, font, font_size, max_w, 8);
    if lines.is_empty() {
        return;
    }

    let line_h = font_size + 4.0 * scale;
    let mut text_w = 0.0_f32;
    for line in &lines {
        let w = measure_text_f(line, font, font_size as u16, 1.0).width;
        if w > text_w {
            text_w = w;
        }
    }
    let box_w = (text_w + 2.0 * pad).min(sw);
    let box_h = lines.len() as f32 * line_h + 2.0 * pad;

    // Default position: below and slightly right of the cursor.
    let mut box_x = mouse_x + 12.0 * scale;
    let mut box_y = mouse_y + 18.0 * scale;
    // Flip horizontally if it would overflow the right edge.
    if box_x + box_w > sw - 4.0 {
        box_x = (mouse_x - box_w - 12.0 * scale).max(4.0);
    }
    if box_x < 4.0 {
        box_x = 4.0;
    }
    // Flip vertically if it would overflow the bottom edge.
    if box_y + box_h > sh - 4.0 {
        box_y = (mouse_y - box_h - 12.0 * scale).max(4.0);
    }

    draw_rectangle(box_x, box_y, box_w, box_h, Color::new(0.05, 0.05, 0.1, 0.92));
    draw_rectangle_lines(box_x, box_y, box_w, box_h, 1.5 * scale, Color::new(0.5, 0.75, 1.0, 0.9));

    let mut ty = box_y + pad + font_size;
    for line in &lines {
        draw_text_f(line, box_x + pad, ty, font_size, WHITE, font);
        ty += line_h;
    }
}

/// A simple axis-aligned rectangle used for settings control layout.
#[derive(Clone, Copy, Default)]
struct Rect4 {
    x: f32,
    y: f32,
    w: f32,
    h: f32,
}

/// Pre-computed geometry for every control in the settings menu.
///
/// Computing this once per frame lets both `draw_settings_menu` (rendering)
/// and `handle_settings_interaction` (input) share the exact same hit regions
/// without duplicating the layout math.  All stored coordinates are already
/// scaled by `ui_scale`.
struct SettingsLayout {
    panel_x: f32,
    panel_y: f32,
    panel_w: f32,
    panel_h: f32,
    /// 标签页按钮位置（文本、音频、画面、快进、配色、开发者、帮助、关于）。
    tab_rects: [Rect4; 8],
    /// 内容区顶部 y（标签页下方）。
    #[allow(dead_code)]
    content_top: f32,
    /// X position of the left-aligned labels.
    label_x: f32,
    /// X position of the value text shown to the right of each slider.
    value_x: f32,
    /// 控件起始 x（开关 / 下拉 / 颜色编辑器等左侧）。
    #[allow(dead_code)]
    control_x: f32,
    /// 文本标签页：第 0 行为文本速度滑块。
    text_row_mid: f32,
    text_slider_track: Rect4,
    text_slider_hit: Rect4,
    /// 文本标签页：自动播放相关控件。
    /// 第 1 行为自动播放开关，第 2 行为有语音间隔滑块，第 3 行为无语音间隔滑块。
    auto_play_row_mids: [f32; 3],
    auto_play_toggle: Rect4,
    auto_play_slider_tracks: [Rect4; 2],
    auto_play_slider_hits: [Rect4; 2],
    /// 音频标签页：第 0 行 BGM，第 1 行 SFX，第 2 行 Voice。
    audio_row_mids: [f32; 3],
    audio_slider_tracks: [Rect4; 3],
    audio_slider_hits: [Rect4; 3],
    /// 画面标签页：第 0 行 auto_recovery，第 1 行 fullscreen，第 2 行 resolution，
    /// 第 3 行 ui_language，第 4 行 script_language。
    /// （显示终端开关已移至「开发者」标签页。）
    display_row_mids: [f32; 5],
    display_toggles: [Rect4; 2],
    display_dropdown: Rect4,
    /// UI 语言下拉（影响界面文本）。
    ui_language_dropdown: Rect4,
    /// 剧本语言下拉（影响对话/旁白/选项/角色名/语音）。
    language_dropdown: Rect4,
    /// 快进标签页：第 0 行 skip_unread，第 1 行 skip_mode 下拉。
    skip_row_mids: [f32; 2],
    skip_toggle: Rect4,
    skip_dropdown: Rect4,
    /// 配色标签页：5 行颜色编辑器（主题色1/2/对话框、已读、未读）的行中线 y。
    color_row_mids: [f32; 5],
    /// 配色标签页：每行的大色块（点击可激活十六进制编辑）。
    color_swatch_rects: [Rect4; 5],
    /// 配色标签页：每行的十六进制输入框（点击激活文本输入）。
    color_hex_rects: [Rect4; 5],
    /// 配色标签页：前 3 行（主题色）的「项目默认 / 自定义」切换按钮。
    color_default_btn_rects: [Rect4; 3],
    /// 配色标签页：5 行 × 12 色调色板预设小色块（共 60 个）。
    /// 索引 = row * 12 + palette_index。
    color_palette_rects: [Rect4; 60],
    /// 开发者标签页：红字警告文本的顶部 y（标题下方）。
    dev_warning_y: f32,
    /// 开发者标签页：显示终端调试输出开关。
    dev_debug_toggle: Rect4,
    /// 开发者标签页：「将全部文本设为未读」按钮。
    dev_clear_read_btn: Rect4,
    /// "应用" button hit rect.
    apply_btn: Rect4,
    /// "取消" button hit rect.
    cancel_btn: Rect4,
}

/// Compute the full-screen settings menu layout from the current screen size
/// and UI scale.  The panel covers the entire window.
fn compute_settings_layout(sw: f32, sh: f32, scale: f32) -> SettingsLayout {
    // Full-screen panel.
    let panel_x = 0.0;
    let panel_y = 0.0;
    let panel_w = sw;
    let panel_h = sh;

    // 标题（顶部居中）
    let title_size = 53.0 * scale;
    let title_top = 36.0 * scale;

    // 标签页（浏览器风格，位于标题下方）
    let tab_labels = ["文本", "音频", "画面", "快进", "配色", "开发者", "帮助", "关于"];
    let tab_h = 62.0 * scale;
    let tab_top = title_top + title_size + 24.0 * scale;
    // 8 个标签需收窄宽度和起始 x，以在 1280 宽度下不溢出（8×140 + 7×5 + 80×2 = 1235）。
    let tab_start_x = 80.0 * scale;
    let tab_w = 140.0 * scale;
    let tab_gap = 5.0 * scale;
    let mut tab_rects = [Rect4::default(); 8];
    for (i, _) in tab_labels.iter().enumerate() {
        tab_rects[i] = Rect4 {
            x: tab_start_x + i as f32 * (tab_w + tab_gap),
            y: tab_top,
            w: tab_w,
            h: tab_h,
        };
    }

    // 内容区（标签页下方）
    let content_top = tab_top + tab_h + 36.0 * scale;
    let _content_bottom = sh - 144.0 * scale;

    let label_x = panel_x + 96.0 * scale;
    let control_x = panel_x + 384.0 * scale;
    let track_w = (panel_w - 384.0 * scale - 288.0 * scale).max(192.0 * scale);
    let value_x = control_x + track_w + 29.0 * scale;

    // 通用行高
    let row_h = 106.0 * scale;
    let track_h = 19.0 * scale;
    let toggle_w = 120.0 * scale;
    let toggle_h = 48.0 * scale;

    // 文本标签页（4 行：文本速度、自动播放开关、有语音间隔、无语音间隔）
    let text_row_mid = content_top + row_h * 0.5 + row_h;
    let text_slider_track = Rect4 {
        x: control_x, y: text_row_mid - track_h / 2.0,
        w: track_w, h: track_h,
    };
    let text_slider_hit = Rect4 {
        x: control_x - 10.0 * scale, y: text_row_mid - 31.0 * scale,
        w: track_w + 20.0 * scale, h: 62.0 * scale,
    };
    // 自动播放：第 1 行开关，第 2、3 行滑块
    let mut auto_play_row_mids = [0.0; 3];
    let mut auto_play_slider_tracks = [Rect4::default(); 2];
    let mut auto_play_slider_hits = [Rect4::default(); 2];
    for i in 0..3 {
        auto_play_row_mids[i] = content_top + row_h * 0.5 + (i as f32 + 2.0) * row_h;
    }
    let auto_play_toggle = Rect4 {
        x: control_x, y: auto_play_row_mids[0] - toggle_h / 2.0,
        w: toggle_w, h: toggle_h,
    };
    for i in 0..2 {
        let mid = auto_play_row_mids[i + 1];
        auto_play_slider_tracks[i] = Rect4 {
            x: control_x, y: mid - track_h / 2.0,
            w: track_w, h: track_h,
        };
        auto_play_slider_hits[i] = Rect4 {
            x: control_x - 10.0 * scale, y: mid - 31.0 * scale,
            w: track_w + 20.0 * scale, h: 62.0 * scale,
        };
    }

    // 音频标签页（3 行：BGM、SFX、Voice）
    let mut audio_row_mids = [0.0; 3];
    let mut audio_slider_tracks = [Rect4::default(); 3];
    let mut audio_slider_hits = [Rect4::default(); 3];
    for i in 0..3 {
        let mid = content_top + row_h * 0.5 + (i as f32 + 1.0) * row_h;
        audio_row_mids[i] = mid;
        audio_slider_tracks[i] = Rect4 {
            x: control_x, y: mid - track_h / 2.0,
            w: track_w, h: track_h,
        };
        audio_slider_hits[i] = Rect4 {
            x: control_x - 10.0 * scale, y: mid - 31.0 * scale,
            w: track_w + 20.0 * scale, h: 62.0 * scale,
        };
    }

    // 画面标签页（5 行：自动续播、全屏、分辨率、UI 语言、剧本语言）
    // 显示终端开关已移至「开发者」标签页。
    let mut display_row_mids = [0.0; 5];
    let mut display_toggles = [Rect4::default(); 2];
    for i in 0..5 {
        display_row_mids[i] = content_top + row_h * 0.5 + (i as f32 + 1.0) * row_h;
    }
    // 第 0 行 auto_recovery，第 1 行 fullscreen
    let toggle_indices = [0usize, 1];
    for (idx, row) in toggle_indices.iter().enumerate() {
        let mid = display_row_mids[*row];
        display_toggles[idx] = Rect4 {
            x: control_x, y: mid - toggle_h / 2.0,
            w: toggle_w, h: toggle_h,
        };
    }
    let display_dropdown = Rect4 {
        x: control_x, y: display_row_mids[2] - 26.0 * scale,
        w: 312.0 * scale, h: 53.0 * scale,
    };
    // UI 语言下拉（第 4 行，索引 3）
    let ui_language_dropdown = Rect4 {
        x: control_x, y: display_row_mids[3] - 26.0 * scale,
        w: 312.0 * scale, h: 53.0 * scale,
    };
    // 剧本语言下拉（第 5 行，索引 4）
    let language_dropdown = Rect4 {
        x: control_x, y: display_row_mids[4] - 26.0 * scale,
        w: 312.0 * scale, h: 53.0 * scale,
    };

    // 快进标签页（2 行：允许跳过未读、快进模式）
    let mut skip_row_mids = [0.0; 2];
    for i in 0..2 {
        skip_row_mids[i] = content_top + row_h * 0.5 + (i as f32 + 1.0) * row_h;
    }
    let skip_toggle = Rect4 {
        x: control_x, y: skip_row_mids[0] - toggle_h / 2.0,
        w: toggle_w, h: toggle_h,
    };
    let skip_dropdown = Rect4 {
        x: control_x, y: skip_row_mids[1] - 26.0 * scale,
        w: 312.0 * scale, h: 53.0 * scale,
    };

    // 配色标签页（5 行：主题色1、主题色2、对话框色、已读文字色、未读文字色）
    let mut color_row_mids = [0.0; 5];
    for i in 0..5 {
        color_row_mids[i] = content_top + row_h * 0.5 + (i as f32 + 1.0) * row_h;
    }
    // 配色行内几何：色块 / 十六进制框 / 调色板。
    // 主题色行（0,1,2）在色块后多一个「项目默认/自定义」按钮；已读/未读行（3,4）没有。
    let swatch_size = 44.0 * scale;
    let hex_box_w = 168.0 * scale;
    let hex_box_h = 40.0 * scale;
    let def_btn_w = 132.0 * scale;
    let palette_cell = 30.0 * scale;
    let palette_gap = 5.0 * scale;
    let mut color_swatch_rects = [Rect4::default(); 5];
    let mut color_hex_rects = [Rect4::default(); 5];
    let mut color_default_btn_rects = [Rect4::default(); 3];
    let mut color_palette_rects = [Rect4::default(); 60];
    for i in 0..5 {
        let mid = color_row_mids[i];
        // 色块（所有行均位于 control_x）
        let swatch = Rect4 {
            x: control_x, y: mid - swatch_size / 2.0,
            w: swatch_size, h: swatch_size,
        };
        color_swatch_rects[i] = swatch;
        // 主题色行：色块右侧加「项目默认/自定义」按钮
        let mut cursor_x = swatch.x + swatch.w + 8.0 * scale;
        if i < 3 {
            color_default_btn_rects[i] = Rect4 {
                x: cursor_x, y: mid - hex_box_h / 2.0,
                w: def_btn_w, h: hex_box_h,
            };
            cursor_x += def_btn_w + 8.0 * scale;
        }
        // 十六进制输入框
        let hex_rect = Rect4 {
            x: cursor_x, y: mid - hex_box_h / 2.0,
            w: hex_box_w, h: hex_box_h,
        };
        color_hex_rects[i] = hex_rect;
        // 调色板：12 个小色块
        let pal_start_x = hex_rect.x + hex_rect.w + 16.0 * scale;
        for j in 0..12 {
            color_palette_rects[i * 12 + j] = Rect4 {
                x: pal_start_x + j as f32 * (palette_cell + palette_gap),
                y: mid - palette_cell / 2.0,
                w: palette_cell, h: palette_cell,
            };
        }
    }

    // 开发者标签页：标题下方红字警告，然后显示终端开关，再「全部设为未读」按钮。
    // 警告文本紧贴内容区顶部；开关与按钮沿用通用行高/toggle 尺寸。
    let dev_warning_y = content_top + 8.0 * scale;
    let dev_debug_row_mid = content_top + row_h * 0.5 + row_h;
    let dev_debug_toggle = Rect4 {
        x: control_x, y: dev_debug_row_mid - toggle_h / 2.0,
        w: toggle_w, h: toggle_h,
    };
    let dev_clear_btn_w = 360.0 * scale;
    let dev_clear_btn_h = 60.0 * scale;
    let dev_clear_read_btn = Rect4 {
        x: control_x,
        y: dev_debug_row_mid + row_h - dev_clear_btn_h / 2.0,
        w: dev_clear_btn_w, h: dev_clear_btn_h,
    };

    // 两个按钮：应用和取消
    let btn_w = 240.0 * scale;
    let btn_h = 67.0 * scale;
    let btn_gap = 58.0 * scale;
    let total_btn_w = btn_w * 2.0 + btn_gap;
    let btn_start_x = (sw - total_btn_w) / 2.0;
    let btn_y = panel_y + panel_h - btn_h - 48.0 * scale;
    let apply_btn = Rect4 {
        x: btn_start_x, y: btn_y, w: btn_w, h: btn_h,
    };
    let cancel_btn = Rect4 {
        x: btn_start_x + btn_w + btn_gap, y: btn_y, w: btn_w, h: btn_h,
    };

    SettingsLayout {
        panel_x, panel_y, panel_w, panel_h,
        tab_rects, content_top,
        label_x, value_x, control_x,
        text_row_mid, text_slider_track, text_slider_hit,
        auto_play_row_mids, auto_play_toggle,
        auto_play_slider_tracks, auto_play_slider_hits,
        audio_row_mids, audio_slider_tracks, audio_slider_hits,
        display_row_mids, display_toggles, display_dropdown, ui_language_dropdown, language_dropdown,
        skip_row_mids, skip_toggle, skip_dropdown,
        color_row_mids,
        color_swatch_rects, color_hex_rects, color_default_btn_rects, color_palette_rects,
        dev_warning_y, dev_debug_toggle, dev_clear_read_btn,
        apply_btn, cancel_btn,
    }
}

/// 绘制「配色」选项卡：5 行颜色编辑器（主题色1/2/对话框、已读、未读）。
///
/// 每行结构（从左到右）：
/// - 标签
/// - 大色块（点击激活十六进制编辑）
/// - [仅主题色行]「项目默认 / 自定义」切换按钮
/// - 十六进制输入框（点击激活文本输入，支持 #RRGGBB / #RRGGBBAA）
/// - 12 色调色板预设（点击直接套用）
fn draw_color_tab(engine: &Engine, layout: &SettingsLayout, font: &Option<Font>, scale: f32, project_config: &ProjectConfig, color_edit_active: Option<ColorField>, color_hex_buffer: &str) {
    let settings = engine.settings();
    let label_size = 34.0 * scale;
    let hint_size = 24.0 * scale;
    let hex_text_size = 24.0 * scale;
    let btn_text_size = 22.0 * scale;

    // 顶部提示
    draw_text_f(engine.t_ui("settings.color.hint"), layout.label_x, layout.content_top + hint_size, hint_size,
        Color::new(0.6, 0.75, 0.95, 0.95), font);

    let fields = [ColorField::ThemePrimary, ColorField::ThemeSecondary, ColorField::ThemeDialogue, ColorField::ReadText, ColorField::UnreadText];
    let label_keys = [
        engine.t_ui("settings.color.theme_primary"),
        engine.t_ui("settings.color.theme_secondary"),
        engine.t_ui("settings.color.theme_dialogue"),
        engine.t_ui("settings.color.read_text"),
        engine.t_ui("settings.color.unread_text"),
    ];

    for (i, field) in fields.iter().enumerate() {
        let mid = layout.color_row_mids[i];
        let val = field.value(settings, project_config);

        // 标签
        draw_text_f(label_keys[i], layout.label_x, mid + 8.0 * scale, label_size, WHITE, font);

        // 大色块
        let swatch = layout.color_swatch_rects[i];
        draw_rectangle(swatch.x, swatch.y, swatch.w, swatch.h, color_from_u8(val));
        draw_rectangle_lines(swatch.x, swatch.y, swatch.w, swatch.h, 1.5 * scale,
            Color::new(0.6, 0.8, 1.0, 0.9));

        // 主题色行：「项目默认 / 自定义」按钮
        if field.is_theme() {
            let btn = layout.color_default_btn_rects[i];
            let custom = field.is_custom(settings);
            let (btn_label, btn_bg, btn_fg) = if custom {
                (engine.t_ui("settings.color.use_custom"), Color::new(0.30, 0.55, 0.85, 0.9), Color::new(0.95, 0.98, 1.0, 1.0))
            } else {
                (engine.t_ui("settings.color.use_default"), Color::new(0.18, 0.22, 0.35, 0.9), Color::new(0.7, 0.8, 0.95, 0.95))
            };
            draw_rectangle(btn.x, btn.y, btn.w, btn.h, btn_bg);
            draw_rectangle_lines(btn.x, btn.y, btn.w, btn.h, 1.2 * scale, btn_fg);
            let tw = measure_text_f(btn_label, font, btn_text_size as u16, 1.0).width;
            draw_text_f(btn_label, btn.x + (btn.w - tw) / 2.0, btn.y + btn.h / 2.0 + 7.0 * scale,
                btn_text_size, btn_fg, font);
        }

        // 十六进制输入框
        let hex = layout.color_hex_rects[i];
        let active = color_edit_active == Some(*field);
        let hex_bg = if active { Color::new(0.16, 0.24, 0.40, 1.0) } else { Color::new(0.12, 0.18, 0.32, 0.95) };
        let hex_border = if active { Color::new(0.7, 0.85, 1.0, 1.0) } else { Color::new(0.45, 0.7, 0.95, 0.8) };
        draw_rectangle(hex.x, hex.y, hex.w, hex.h, hex_bg);
        draw_rectangle_lines(hex.x, hex.y, hex.w, hex.h, 1.5 * scale, hex_border);
        // 框内文本：激活时显示输入缓冲区，否则显示当前色值
        let display = if active {
            color_hex_buffer.to_string()
        } else {
            hex_string_from_color(val)
        };
        draw_text_f(&display, hex.x + 10.0 * scale, hex.y + hex.h / 2.0 + 8.0 * scale,
            hex_text_size, WHITE, font);
        // 激活时画光标（闪烁竖线）
        if active {
            let tw = measure_text_f(&display, font, hex_text_size as u16, 1.0).width;
            let cx = hex.x + 10.0 * scale + tw + 2.0 * scale;
            let cy = hex.y + 6.0 * scale;
            let ch = hex.h - 12.0 * scale;
            // 用 floor(get_time()*2) 取整做 0.5Hz 闪烁
            if (get_time().floor() as i64) % 2 == 0 {
                draw_rectangle(cx, cy, 2.0 * scale, ch, WHITE);
            }
        }

        // 调色板预设
        for j in 0..12 {
            let pr = layout.color_palette_rects[i * 12 + j];
            let pc = COLOR_PALETTE[j];
            draw_rectangle(pr.x, pr.y, pr.w, pr.h, color_from_u8(pc));
            // 与当前色相同则加亮边框
            let selected = pc == val;
            draw_rectangle_lines(pr.x, pr.y, pr.w, pr.h,
                if selected { 2.5 * scale } else { 1.0 * scale },
                if selected { Color::new(1.0, 1.0, 0.4, 1.0) } else { Color::new(0.5, 0.7, 0.95, 0.6) });
        }
    }
}

/// 绘制「开发者」选项卡：红字警告 + 显示终端开关 + 全部设为未读按钮。
///
/// 本页集中放置高危/调试开关。标题下方以红字警告「请不要随便动本页的内容」。
/// - 显示终端开关：从「画面」标签页移入此处。
/// - 全部设为未读：清空已读历史（`engine.clear_read_history()`），使所有句子重新视为未读。
fn draw_developer_tab(engine: &Engine, layout: &SettingsLayout, font: &Option<Font>, scale: f32) {
    let settings = engine.settings();
    let label_size = 34.0 * scale;
    let warning_size = 32.0 * scale;
    let hint_size = 28.0 * scale;

    // 红字警告（标题下方）
    let warning = engine.t_ui("settings.dev.warning");
    draw_text_f(warning, layout.label_x, layout.dev_warning_y + warning_size,
        warning_size, Color::new(1.0, 0.27, 0.27, 1.0), font);

    // 显示终端调试输出开关
    let toggle_row_mid = layout.dev_debug_toggle.y + layout.dev_debug_toggle.h / 2.0;
    draw_text_f(engine.t_ui("settings.dev.debug_terminal"),
        layout.label_x, toggle_row_mid + 8.0 * scale, label_size, WHITE, font);
    draw_toggle(layout.dev_debug_toggle, settings.debug_terminal, font, scale);

    // 全部设为未读按钮
    let btn = layout.dev_clear_read_btn;
    draw_rectangle(btn.x, btn.y, btn.w, btn.h, Color::new(0.55, 0.18, 0.18, 1.0));
    draw_rectangle_lines(btn.x, btn.y, btn.w, btn.h, 1.5 * scale,
        Color::new(0.9, 0.5, 0.5, 1.0));
    let btn_label = engine.t_ui("settings.dev.clear_read_history");
    let bw = measure_text_f(btn_label, font, label_size as u16, 1.0).width;
    draw_text_f(btn_label, btn.x + (btn.w - bw) / 2.0,
        btn.y + btn.h / 2.0 + label_size / 3.0, label_size, WHITE, font);
    // 按钮下方提示
    let hint = engine.t_ui("settings.dev.clear_read_history_hint");
    draw_text_f(hint, btn.x, btn.y + btn.h + 28.0 * scale, hint_size,
        Color::new(0.7, 0.7, 0.7, 1.0), font);
}

/// 绘制「帮助」选项卡：键位功能对照表。
/// 表格分两栏（按键 | 功能），分两个区块（游戏内操作 / 备注编辑）。
/// 字号略大于普通设置项，多 DPI 自适应。
fn draw_help_tab(engine: &Engine, layout: &SettingsLayout, font: &Option<Font>, scale: f32) {
    let text_size = 36.0 * scale;
    let section_size = 40.0 * scale;
    let row_h = text_size * 1.8;
    let gap = 40.0 * scale;

    // 两列布局：按键列 + 功能列
    let col_key_x = layout.label_x;
    let col_key_w = 240.0 * scale;
    let col_func_x = col_key_x + col_key_w + gap;
    let col_func_w = 600.0 * scale;

    let mut y = layout.content_top + 20.0 * scale;

    // ── 区块 1：游戏内操作 ──
    draw_text_f(engine.t_ui("help.section.game"), col_key_x, y + section_size, section_size,
        Color::new(0.7, 0.85, 1.0, 1.0), font);
    y += section_size + 16.0 * scale;
    // 表头
    draw_text_f(engine.t_ui("help.key"), col_key_x, y + text_size, text_size,
        Color::new(0.5, 0.6, 0.75, 1.0), font);
    draw_text_f(engine.t_ui("help.function"), col_func_x, y + text_size, text_size,
        Color::new(0.5, 0.6, 0.75, 1.0), font);
    y += row_h;
    let game_rows: [(&str, &str); 3] = [
        ("Space / Enter", engine.t_ui("help.advance")),
        ("Esc", engine.t_ui("help.escape")),
        ("鼠标点击", engine.t_ui("help.click")),
    ];
    for (key, func) in &game_rows {
        draw_text_f(key, col_key_x, y + text_size, text_size, WHITE, font);
        draw_text_f(func, col_func_x, y + text_size, text_size, WHITE, font);
        y += row_h;
    }

    y += 30.0 * scale;

    // ── 区块 2：备注编辑 ──
    draw_text_f(engine.t_ui("help.section.note"), col_key_x, y + section_size, section_size,
        Color::new(0.7, 0.85, 1.0, 1.0), font);
    y += section_size + 16.0 * scale;
    draw_text_f(engine.t_ui("help.key"), col_key_x, y + text_size, text_size,
        Color::new(0.5, 0.6, 0.75, 1.0), font);
    draw_text_f(engine.t_ui("help.function"), col_func_x, y + text_size, text_size,
        Color::new(0.5, 0.6, 0.75, 1.0), font);
    y += row_h;
    let note_rows: [(&str, &str); 3] = [
        ("Enter", engine.t_ui("help.enter_note")),
        ("Esc", engine.t_ui("help.escape")),
        ("Backspace", engine.t_ui("help.backspace")),
    ];
    for (key, func) in &note_rows {
        draw_text_f(key, col_key_x, y + text_size, text_size, WHITE, font);
        draw_text_f(func, col_func_x, y + text_size, text_size, WHITE, font);
        y += row_h;
    }

    y += 20.0 * scale;
    draw_text_f(engine.t_ui("help.hint"), col_key_x, y + text_size * 0.85,
        text_size * 0.85, Color::new(0.55, 0.65, 0.8, 0.9), font);

    // 消除未使用变量警告
    let _ = col_func_w;
}

/// 绘制「关于」选项卡：图标 + 5 行文字，整体占屏幕 50% 居中。
///
/// 布局：
/// ```text
/// ┌──────────────────────────────────┐
/// │  ┌──────┐                        │
/// │  │      │  **Akizuki*Rustgal**   │  ← 第 1 行（粗体），与图标顶部对齐
/// │  │ 图标 │  简单好用的视觉小说引擎  │  ← 第 2 行
/// │  │      │  版本 1.0 Build 35     │  ← 第 3 行
/// │  └──────┘                        │
/// │           心夏麻麻可爱喵           │  ← 第 4 行（不再与图标对齐）
/// │           最喜欢心夏麻麻了喵       │  ← 第 5 行
/// └──────────────────────────────────┘
/// ```
/// 图标高度 ≈ 3 行文字高度。5 行文字全部左对齐。
fn draw_about_tab(engine: &Engine, layout: &SettingsLayout, font: &Option<Font>, scale: f32, icon_texture: &Option<Texture2D>) {
    let sw = layout.panel_w;
    let sh = layout.panel_h;

    // 文字尺寸（略大于普通设置项，多 DPI 自适应）
    let text_size = 40.0 * scale;
    let line_h = text_size * 1.5;

    // 关于块占屏幕 50% 宽度，居中
    let block_w = sw * 0.5;
    let block_x = (sw - block_w) / 2.0;

    // 图标高度 = 3 行文字高度
    let icon_h = line_h * 3.0;
    let icon_w = icon_h; // 正方形图标
    let icon_x = block_x;
    let gap = 40.0 * scale;
    let text_x = icon_x + icon_w + gap;

    // 前三行与图标顶部对齐；后两行在图标下方
    let line1_y = layout.content_top + 20.0 * scale + text_size; // baseline
    let line2_y = line1_y + line_h;
    let line3_y = line2_y + line_h;
    let line4_y = line3_y + line_h;
    let line5_y = line4_y + line_h;

    let icon_y = line1_y - text_size; // 图标顶部与第一行顶部对齐

    // ── 绘制图标 ──
    if let Some(tex) = icon_texture {
        let tex_w = tex.width();
        let tex_h = tex.height();
        if tex_w > 0.0 && tex_h > 0.0 {
            // 保持比例缩放到 icon_h 高度（contain 模式）
            let s = (icon_w / tex_w).min(icon_h / tex_h);
            let dw = tex_w * s;
            let dh = tex_h * s;
            let dx = icon_x + (icon_w - dw) / 2.0;
            let dy = icon_y + (icon_h - dh) / 2.0;
            draw_texture_ex(*tex, dx, dy, WHITE, DrawTextureParams {
                dest_size: Some(vec2(dw, dh)),
                ..Default::default()
            });
        }
    } else {
        // 图标加载失败时画占位框
        draw_rectangle_lines(icon_x, icon_y, icon_w, icon_h, 2.0 * scale,
            Color::new(0.5, 0.6, 0.75, 0.6));
    }

    // ── 第 1 行：Akizuki*Rustgal（粗体）──
    // macroquad 仅加载单一字体，通过多次微小偏移绘制模拟加粗效果。
    let name = engine.t_ui("about.name");
    let bold_color = Color::new(1.0, 0.95, 0.6, 1.0);
    let bold_off = 1.5 * scale;
    for (dx, dy) in [(0.0, 0.0), (bold_off, 0.0), (-bold_off, 0.0), (0.0, bold_off), (0.0, -bold_off)] {
        draw_text_f(name, text_x + dx, line1_y + dy, text_size, bold_color, font);
    }

    // ── 第 2 行：简单好用的视觉小说引擎 ──
    let subtitle = engine.t_ui("about.subtitle");
    draw_text_f(subtitle, text_x, line2_y, text_size,
        Color::new(0.8, 0.85, 0.95, 1.0), font);

    // ── 第 3 行：版本 1.0(Build 0040) ──
    // 版本号取自 Cargo.toml 的 CARGO_PKG_VERSION（SemVer 形如 "1.0.40"）：
    //   - 主次版本 = major.minor（如 "1.0"）
    //   - build 号 = patch 段数字，按四位数补零显示（如 40 → "0040"）
    //     该值与 build.rs 注入的 BUILD_NUMBER（commit 数 + 37）保持一致。
    // 显示格式：版本 1.0(Build 0040)。
    // 注：SemVer 不允许 patch 段有前导零，故 Cargo.toml 写 "1.0.40"，
    // 显示时再补零为 "0040"。
    let pkg_ver = env!("CARGO_PKG_VERSION");
    let (ver_main, build_num) = match pkg_ver.rsplit_once('.') {
        Some((head, patch)) => (head, patch.parse::<u64>().unwrap_or(0)),
        None => (pkg_ver, 0),
    };
    let build_str = format!("{} {}({} {:04})",
        engine.t_ui("about.version_label"),
        ver_main,
        engine.t_ui("about.build_label"),
        build_num);
    draw_text_f(&build_str, text_x, line3_y, text_size,
        Color::new(0.7, 0.8, 0.95, 1.0), font);

    // ── 第 4 行：心夏麻麻可爱喵（不再与图标对齐）──
    let line4 = engine.t_ui("about.line4");
    draw_text_f(line4, text_x, line4_y, text_size * 0.9,
        Color::new(0.9, 0.7, 0.8, 1.0), font);

    // ── 第 5 行：最喜欢心夏麻麻了喵 ──
    let line5 = engine.t_ui("about.line5");
    draw_text_f(line5, text_x, line5_y, text_size * 0.9,
        Color::new(0.9, 0.7, 0.8, 1.0), font);

    // 消除未使用变量警告
    let _ = sh;
}

/// Draw the interactive full-screen settings menu. Reads live values from the
/// engine so dragging a slider is reflected immediately.  The dropdown list
/// is drawn last (via `draw_dropdown_list`) so it floats above the back button.
fn draw_settings_menu(engine: &mut Engine, layout: &SettingsLayout, font: &Option<Font>, dropdown_open: bool, skip_dropdown_open: bool, ui_lang_dropdown_open: bool, lang_dropdown_open: bool, active_tab: SettingsTab, scale: f32, icon_texture: &Option<Texture2D>, project_config: &ProjectConfig, color_edit_active: Option<ColorField>, color_hex_buffer: &str) {
    // 天蓝色背景
    draw_rectangle(layout.panel_x, layout.panel_y, layout.panel_w, layout.panel_h,
        Color::new(0.1, 0.15, 0.3, 0.95));

    // Subtle full-screen frame around the settings page.
    draw_rectangle_lines(
        layout.panel_x,
        layout.panel_y,
        layout.panel_w,
        layout.panel_h,
        2.0 * scale,
        Color::new(0.45, 0.7, 0.95, 0.8),
    );

    // 标题（顶部居中）
    let title = engine.t_ui("settings.title");
    let title_size = 53.0 * scale;
    let tw = measure_text_f(title, font, title_size as u16, 1.0).width;
    let title_x = (layout.panel_w - tw) / 2.0;
    let title_y = 36.0 * scale;
    draw_text_f(title, title_x, title_y + title_size, title_size, WHITE, font);

    // 标签页（浏览器风格）
    let tab_labels = [
        engine.t_ui("settings.tab.text"),
        engine.t_ui("settings.tab.audio"),
        engine.t_ui("settings.tab.display"),
        engine.t_ui("settings.tab.skip"),
        engine.t_ui("settings.tab.color"),
        engine.t_ui("settings.tab.developer"),
        engine.t_ui("settings.tab.help"),
        engine.t_ui("settings.tab.about"),
    ];
    let tab_tabs = [SettingsTab::Text, SettingsTab::Audio, SettingsTab::Display, SettingsTab::Skip, SettingsTab::Color, SettingsTab::Developer, SettingsTab::Help, SettingsTab::About];
    let tab_label_size = 29.0 * scale;
    for (i, label) in tab_labels.iter().enumerate() {
        let r = layout.tab_rects[i];
        let active = active_tab == tab_tabs[i];
        // 标签背景（激活的更亮）
        let bg_color = if active {
            Color::new(0.3, 0.55, 0.85, 1.0)
        } else {
            Color::new(0.15, 0.25, 0.45, 0.8)
        };
        draw_rectangle(r.x, r.y, r.w, r.h, bg_color);
        // 标签边框（底部激活时不画，与内容区连为一体）
        draw_rectangle_lines(r.x, r.y, r.w, r.h, 1.5 * scale,
            if active { Color::new(0.5, 0.75, 1.0, 1.0) } else { Color::new(0.3, 0.5, 0.7, 0.6) });
        // 激活时底部用内容区背景色覆盖（连接效果）
        if active {
            draw_rectangle(r.x, r.y + r.h - 2.0 * scale, r.w, 2.0 * scale,
                Color::new(0.1, 0.15, 0.3, 0.95));
        }
        // 标签文字
        let tw = measure_text_f(label, font, tab_label_size as u16, 1.0).width;
        draw_text_f(label, r.x + (r.w - tw) / 2.0, r.y + r.h / 2.0 + tab_label_size / 3.0,
            tab_label_size, WHITE, font);
    }

    // 内容区分隔线（标签页下方一条横线）
    let line_y = layout.tab_rects[0].y + layout.tab_rects[0].h;
    draw_rectangle(layout.tab_rects[0].x, line_y,
        layout.tab_rects[7].x + layout.tab_rects[7].w - layout.tab_rects[0].x, 1.5 * scale,
        Color::new(0.45, 0.7, 0.95, 0.6));

    let settings = engine.settings();
    let label_size = 34.0 * scale;
    let value_size = 31.0 * scale;

    match active_tab {
        SettingsTab::Text => {
            // 文本速度
            draw_text_f(engine.t_ui("settings.text_speed"), layout.label_x, layout.text_row_mid + 8.0 * scale, label_size, WHITE, font);
            draw_slider_track(layout.text_slider_track, settings.text_speed / 999.0, scale);
            draw_text_f(
                &format!("{:.0} 字/秒", settings.text_speed),
                layout.value_x,
                layout.text_row_mid + 8.0 * scale,
                value_size,
                WHITE,
                font,
            );
            // 自动播放开关
            draw_text_f(engine.t_ui("settings.auto_play"), layout.label_x, layout.auto_play_row_mids[0] + 8.0 * scale, label_size, WHITE, font);
            draw_toggle(layout.auto_play_toggle, settings.auto_play, font, scale);
            // 有语音间隔（0.0 - 5.0 秒）
            draw_text_f(engine.t_ui("settings.auto_play_delay_with_voice"), layout.label_x, layout.auto_play_row_mids[1] + 8.0 * scale, label_size, WHITE, font);
            draw_slider_track(layout.auto_play_slider_tracks[0], settings.auto_play_delay_with_voice / 5.0, scale);
            draw_text_f(
                &format!("{:.1} 秒", settings.auto_play_delay_with_voice),
                layout.value_x,
                layout.auto_play_row_mids[1] + 8.0 * scale,
                value_size,
                WHITE,
                font,
            );
            // 无语音间隔（0.0 - 5.0 秒）
            draw_text_f(engine.t_ui("settings.auto_play_delay_without_voice"), layout.label_x, layout.auto_play_row_mids[2] + 8.0 * scale, label_size, WHITE, font);
            draw_slider_track(layout.auto_play_slider_tracks[1], settings.auto_play_delay_without_voice / 5.0, scale);
            draw_text_f(
                &format!("{:.1} 秒", settings.auto_play_delay_without_voice),
                layout.value_x,
                layout.auto_play_row_mids[2] + 8.0 * scale,
                value_size,
                WHITE,
                font,
            );
        }
        SettingsTab::Audio => {
            // BGM 音量
            draw_text_f(engine.t_ui("settings.bgm_volume"), layout.label_x, layout.audio_row_mids[0] + 8.0 * scale, label_size, WHITE, font);
            draw_slider_track(layout.audio_slider_tracks[0], settings.bgm_volume, scale);
            draw_text_f(
                &format!("{:.0}%", settings.bgm_volume * 100.0),
                layout.value_x,
                layout.audio_row_mids[0] + 8.0 * scale,
                value_size,
                WHITE,
                font,
            );
            // 音效音量
            draw_text_f(engine.t_ui("settings.sfx_volume"), layout.label_x, layout.audio_row_mids[1] + 8.0 * scale, label_size, WHITE, font);
            draw_slider_track(layout.audio_slider_tracks[1], settings.sfx_volume, scale);
            draw_text_f(
                &format!("{:.0}%", settings.sfx_volume * 100.0),
                layout.value_x,
                layout.audio_row_mids[1] + 8.0 * scale,
                value_size,
                WHITE,
                font,
            );
            // 语音音量
            draw_text_f(engine.t_ui("settings.voice_volume"), layout.label_x, layout.audio_row_mids[2] + 8.0 * scale, label_size, WHITE, font);
            draw_slider_track(layout.audio_slider_tracks[2], settings.voice_volume, scale);
            draw_text_f(
                &format!("{:.0}%", settings.voice_volume * 100.0),
                layout.value_x,
                layout.audio_row_mids[2] + 8.0 * scale,
                value_size,
                WHITE,
                font,
            );
        }
        SettingsTab::Display => {
            // 自动恢复
            draw_text_f(engine.t_ui("settings.auto_recovery"), layout.label_x, layout.display_row_mids[0] + 8.0 * scale, label_size, WHITE, font);
            draw_toggle(layout.display_toggles[0], settings.auto_recovery, font, scale);
            // 全屏模式
            draw_text_f(engine.t_ui("settings.fullscreen"), layout.label_x, layout.display_row_mids[1] + 8.0 * scale, label_size, WHITE, font);
            draw_toggle(layout.display_toggles[1], settings.fullscreen, font, scale);
            // 分辨率下拉
            draw_text_f(engine.t_ui("settings.resolution"), layout.label_x, layout.display_row_mids[2] + 8.0 * scale, label_size, WHITE, font);
            draw_dropdown_box_resolution(layout.display_dropdown, settings.resolution, font, dropdown_open, scale);
            // UI 语言下拉
            draw_text_f(engine.t_ui("settings.ui_language"), layout.label_x, layout.display_row_mids[3] + 8.0 * scale, label_size, WHITE, font);
            let ui_lang_label = if settings.ui_language.is_empty() {
                // 空表示「跟随剧本语言」
                format!("{} ({})", engine.t_ui("language.follow_script"), engine.settings().effective_ui_language())
            } else {
                let display = engine.available_languages()
                    .iter()
                    .find(|(c, _)| *c == settings.ui_language)
                    .map(|(_, n)| n.clone())
                    .unwrap_or_else(|| settings.ui_language.clone());
                format!("{} ({})", display, settings.ui_language)
            };
            draw_dropdown_box_language(layout.ui_language_dropdown, &settings.ui_language, &ui_lang_label, font, ui_lang_dropdown_open, scale);
            // 剧本语言下拉
            draw_text_f(engine.t_ui("settings.script_language"), layout.label_x, layout.display_row_mids[4] + 8.0 * scale, label_size, WHITE, font);
            let script_lang_label = if engine.translator().language().is_empty() {
                engine.t_ui("language.original").to_string()
            } else {
                format!("{} ({})", engine.translator().display_name(), engine.translator().language())
            };
            draw_dropdown_box_language(layout.language_dropdown, &settings.language, &script_lang_label, font, lang_dropdown_open, scale);
        }
        SettingsTab::Skip => {
            // 允许跳过未读文本
            draw_text_f(engine.t_ui("settings.skip_unread"), layout.label_x, layout.skip_row_mids[0] + 8.0 * scale, label_size, WHITE, font);
            draw_toggle(layout.skip_toggle, settings.skip_unread, font, scale);
            // 快进模式下拉
            draw_text_f(engine.t_ui("settings.skip_mode"), layout.label_x, layout.skip_row_mids[1] + 8.0 * scale, label_size, WHITE, font);
            draw_dropdown_box_skip_mode(engine, layout.skip_dropdown, settings.skip_mode, font, skip_dropdown_open, scale);
        }
        SettingsTab::Color => {
            draw_color_tab(engine, layout, font, scale, project_config, color_edit_active, color_hex_buffer);
        }
        SettingsTab::Developer => {
            draw_developer_tab(engine, layout, font, scale);
        }
        SettingsTab::Help => {
            draw_help_tab(engine, layout, font, scale);
        }
        SettingsTab::About => {
            draw_about_tab(engine, layout, font, scale, icon_texture);
        }
    }

    // 两个按钮：应用和取消（视觉绘制，点击由 handle_settings_interaction 处理）
    let mut btns = Vec::new();
    draw_button(
        layout.apply_btn.x,
        layout.apply_btn.y,
        layout.apply_btn.w,
        layout.apply_btn.h,
        engine.t_ui("settings.apply"),
        &mut btns,
        ButtonAction::ConfirmYes, // 暂用这个 action，实际处理在 handle_settings_interaction
        font,
        scale,
    );
    draw_button(
        layout.cancel_btn.x,
        layout.cancel_btn.y,
        layout.cancel_btn.w,
        layout.cancel_btn.h,
        engine.t_ui("settings.cancel"),
        &mut btns,
        ButtonAction::ConfirmNo, // 暂用这个 action，实际处理在 handle_settings_interaction
        font,
        scale,
    );

    // Draw the expanded dropdown lists LAST so they are rendered above every other
    // control (including the buttons) — fixes the z-order issue.
    if dropdown_open && active_tab == SettingsTab::Display {
        draw_dropdown_list_resolution(layout.display_dropdown, settings.resolution, font, scale);
    }
    if ui_lang_dropdown_open && active_tab == SettingsTab::Display {
        let available = engine.available_languages();
        draw_dropdown_list_language(layout.ui_language_dropdown, &available, &settings.ui_language, font, scale);
    }
    if lang_dropdown_open && active_tab == SettingsTab::Display {
        let available = engine.available_languages();
        draw_dropdown_list_language(layout.language_dropdown, &available, &settings.language, font, scale);
    }
    if skip_dropdown_open && active_tab == SettingsTab::Skip {
        draw_dropdown_list_skip_mode(engine, layout.skip_dropdown, settings.skip_mode, font, scale);
    }
}

/// Draw a horizontal slider track with a filled portion and a knob. `fraction`
/// is clamped to 0.0-1.0.  The knob radius is scaled by `scale`.
fn draw_slider_track(track: Rect4, fraction: f32, scale: f32) {
    let f = fraction.clamp(0.0, 1.0);
    // Track background.
    draw_rectangle(track.x, track.y, track.w, track.h, Color::new(0.2, 0.2, 0.3, 0.8));
    // Filled portion.
    draw_rectangle(track.x, track.y, track.w * f, track.h, Color::new(0.36, 0.61, 0.84, 0.9));
    // Knob.
    let knob_x = track.x + track.w * f;
    let knob_y = track.y + track.h / 2.0;
    draw_circle(knob_x, knob_y, 9.0 * scale, Color::new(0.8, 0.9, 1.0, 1.0));
}

/// Draw an on/off toggle switch.
fn draw_toggle(r: Rect4, on: bool, font: &Option<Font>, scale: f32) {
    let (bg, fg, label) = if on {
        (
            Color::new(0.29, 0.62, 1.0, 0.9),
            Color::new(0.85, 0.92, 1.0, 1.0),
            "ON",
        )
    } else {
        (
            Color::new(0.3, 0.2, 0.25, 0.9),
            Color::new(0.7, 0.6, 0.6, 1.0),
            "OFF",
        )
    };
    draw_rectangle(r.x, r.y, r.w, r.h, bg);
    draw_rectangle_lines(r.x, r.y, r.w, r.h, 1.5 * scale, fg);
    let label_size = 18.0 * scale;
    let tw = measure_text_f(label, font, label_size as u16, 1.0).width;
    draw_text_f(label, r.x + (r.w - tw) / 2.0, r.y + r.h / 2.0 + 6.0 * scale, label_size, fg, font);
}

/// Draw only the collapsed current-value box of the resolution dropdown.  The
/// expanded list is drawn separately by `draw_dropdown_list_resolution` so it can be
/// rendered on top of all other controls.
fn draw_dropdown_box_resolution(r: Rect4, resolution: (u32, u32), font: &Option<Font>, open: bool, scale: f32) {
    draw_rectangle(r.x, r.y, r.w, r.h, Color::new(0.15, 0.25, 0.45, 0.9));
    draw_rectangle_lines(r.x, r.y, r.w, r.h, 1.5 * scale, Color::new(0.45, 0.7, 0.95, 0.8));
    let label = format!("{}x{}", resolution.0, resolution.1);
    let label_size = 20.0 * scale;
    draw_text_f(&label, r.x + 12.0 * scale, r.y + r.h / 2.0 + 7.0 * scale, label_size, WHITE, font);
    // Dropdown arrow.
    let arrow = if open { "v" } else { ">" };
    let arrow_size = 18.0 * scale;
    draw_text_f(
        arrow,
        r.x + r.w - 24.0 * scale,
        r.y + r.h / 2.0 + 7.0 * scale,
        arrow_size,
        Color::new(0.5, 0.75, 1.0, 0.9),
        font,
    );
}

/// Draw the expanded options list of the resolution dropdown, rendered below
/// the current-value box.  Call this after every other control so the list
/// appears on top (correct z-order).
fn draw_dropdown_list_resolution(r: Rect4, resolution: (u32, u32), font: &Option<Font>, scale: f32) {
    let item_h = 32.0 * scale;
    let presets = Settings::resolution_presets();
    let list_h = item_h * presets.len() as f32;
    // List background.
    draw_rectangle(r.x, r.y + r.h, r.w, list_h, Color::new(0.12, 0.2, 0.35, 0.97));
    draw_rectangle_lines(r.x, r.y + r.h, r.w, list_h, 1.0 * scale, Color::new(0.45, 0.7, 0.95, 0.6));
    let item_size = 18.0 * scale;
    for (i, (w, h)) in presets.iter().enumerate() {
        let iy = r.y + r.h + i as f32 * item_h;
        let item_label = format!("{}x{}", w, h);
        let is_selected = (*w, *h) == resolution;
        let color = if is_selected {
            Color::new(0.5, 0.75, 1.0, 0.95)
        } else {
            WHITE
        };
        draw_text_f(&item_label, r.x + 12.0 * scale, iy + item_h / 2.0 + 6.0 * scale, item_size, color, font);
    }
}

/// 语言下拉菜单的折叠框。
/// `current_lang` 为空字符串表示「原文/跟随」。
/// `display_label` 是折叠框里显示的文本（已由调用方拼好）。
fn draw_dropdown_box_language(r: Rect4, current_lang: &str, display_label: &str, font: &Option<Font>, open: bool, scale: f32) {
    let _ = current_lang;
    draw_rectangle(r.x, r.y, r.w, r.h, Color::new(0.15, 0.25, 0.45, 0.9));
    draw_rectangle_lines(r.x, r.y, r.w, r.h, 1.5 * scale, Color::new(0.45, 0.7, 0.95, 0.8));
    let label_size = 20.0 * scale;
    draw_text_f(display_label, r.x + 12.0 * scale, r.y + r.h / 2.0 + 7.0 * scale, label_size, WHITE, font);
    let arrow = if open { "v" } else { ">" };
    let arrow_size = 18.0 * scale;
    draw_text_f(
        arrow,
        r.x + r.w - 24.0 * scale,
        r.y + r.h / 2.0 + 7.0 * scale,
        arrow_size,
        Color::new(0.5, 0.75, 1.0, 0.9),
        font,
    );
}

/// 语言下拉菜单的展开列表。
/// `available` 是可选语言列表 (code, display_name)。
/// `current_lang` 用于高亮当前选中项。
fn draw_dropdown_list_language(r: Rect4, available: &[(String, String)], current_lang: &str, font: &Option<Font>, scale: f32) {
    let item_h = 32.0 * scale;
    let mut options: Vec<(String, String)> = Vec::new();
    options.push(("".to_string(), "原文".to_string()));
    for (code, name) in available {
        options.push((code.clone(), format!("{} ({})", name, code)));
    }
    let list_h = item_h * options.len() as f32;
    draw_rectangle(r.x, r.y + r.h, r.w, list_h, Color::new(0.12, 0.2, 0.35, 0.97));
    draw_rectangle_lines(r.x, r.y + r.h, r.w, list_h, 1.0 * scale, Color::new(0.45, 0.7, 0.95, 0.6));
    let item_size = 18.0 * scale;
    for (i, (code, label)) in options.iter().enumerate() {
        let iy = r.y + r.h + i as f32 * item_h;
        let is_selected = *code == current_lang;
        let color = if is_selected {
            Color::new(0.5, 0.75, 1.0, 0.95)
        } else {
            WHITE
        };
        draw_text_f(label, r.x + 12.0 * scale, iy + item_h / 2.0 + 6.0 * scale, item_size, color, font);
    }
}

/// 快进模式下拉菜单的折叠框。
fn draw_dropdown_box_skip_mode(engine: &Engine, r: Rect4, mode: SkipMode, font: &Option<Font>, open: bool, scale: f32) {
    draw_rectangle(r.x, r.y, r.w, r.h, Color::new(0.15, 0.25, 0.45, 0.9));
    draw_rectangle_lines(r.x, r.y, r.w, r.h, 1.5 * scale, Color::new(0.45, 0.7, 0.95, 0.8));
    let label = match mode {
        SkipMode::TextOnly => engine.t_ui("skip_mode.text_only"),
        SkipMode::WithVoice => engine.t_ui("skip_mode.with_voice"),
    };
    let label_size = 20.0 * scale;
    draw_text_f(label, r.x + 12.0 * scale, r.y + r.h / 2.0 + 7.0 * scale, label_size, WHITE, font);
    let arrow = if open { "v" } else { ">" };
    let arrow_size = 18.0 * scale;
    draw_text_f(
        arrow,
        r.x + r.w - 24.0 * scale,
        r.y + r.h / 2.0 + 7.0 * scale,
        arrow_size,
        Color::new(0.5, 0.75, 1.0, 0.9),
        font,
    );
}

/// 快进模式下拉菜单的展开列表。
fn draw_dropdown_list_skip_mode(engine: &Engine, r: Rect4, mode: SkipMode, font: &Option<Font>, scale: f32) {
    let item_h = 32.0 * scale;
    let options = [SkipMode::TextOnly, SkipMode::WithVoice];
    let labels = [engine.t_ui("skip_mode.text_only"), engine.t_ui("skip_mode.with_voice")];
    let list_h = item_h * options.len() as f32;
    draw_rectangle(r.x, r.y + r.h, r.w, list_h, Color::new(0.12, 0.2, 0.35, 0.97));
    draw_rectangle_lines(r.x, r.y + r.h, r.w, list_h, 1.0 * scale, Color::new(0.45, 0.7, 0.95, 0.6));
    let item_size = 18.0 * scale;
    for (i, opt) in options.iter().enumerate() {
        let iy = r.y + r.h + i as f32 * item_h;
        let is_selected = *opt == mode;
        let color = if is_selected {
            Color::new(0.5, 0.75, 1.0, 0.95)
        } else {
            WHITE
        };
        draw_text_f(labels[i], r.x + 12.0 * scale, iy + item_h / 2.0 + 6.0 * scale, item_size, color, font);
    }
}

/// Handle all mouse interaction for the settings menu: slider dragging,
/// toggle clicking, dropdown cycling, tab switching, and the back button.
///
/// This runs every frame (not just on click) so that an in-progress slider
/// drag follows the mouse smoothly while the button is held. Returns a UI
/// transition request when the back button is clicked.
fn handle_settings_interaction(
    engine: &mut Engine,
    layout: &SettingsLayout,
    dragging_slider: &mut Option<(SettingsTab, usize)>,
    dropdown_open: &mut bool,
    skip_dropdown_open: &mut bool,
    ui_lang_dropdown_open: &mut bool,
    lang_dropdown_open: &mut bool,
    active_tab: &mut SettingsTab,
    scale: f32,
    settings_snapshot: &Option<Settings>,
    _settings_prev_mode: &mut UiMode,
    project_config: &ProjectConfig,
    color_edit_active: &mut Option<ColorField>,
    color_hex_buffer: &mut String,
) -> Option<(UiMode, PendingUiAction)> {
    let (mx, my) = mouse_position();
    let down = is_mouse_button_down(MouseButton::Left);
    let pressed = is_mouse_button_pressed(MouseButton::Left);
    let released = is_mouse_button_released(MouseButton::Left);

    // Continue dragging an already-grabbed slider while the button is held.
    if let Some((tab, i)) = *dragging_slider
        && down
    {
        let track_opt = match (tab, i) {
            (SettingsTab::Text, 0) => Some(layout.text_slider_track),
            (SettingsTab::Text, 1) => Some(layout.auto_play_slider_tracks[0]),
            (SettingsTab::Text, 2) => Some(layout.auto_play_slider_tracks[1]),
            (SettingsTab::Audio, 0) => Some(layout.audio_slider_tracks[0]),
            (SettingsTab::Audio, 1) => Some(layout.audio_slider_tracks[1]),
            (SettingsTab::Audio, 2) => Some(layout.audio_slider_tracks[2]),
            _ => None,
        };
        if let Some(track) = track_opt {
            update_slider_value(engine, tab, i, mx, track);
        }
    }

    // Release ends any drag.
    if released {
        *dragging_slider = None;
    }

    // A fresh press starts a new interaction.
    if pressed {
        // 若正在编辑颜色十六进制，且本次点击不在当前激活的 hex 输入框内，
        // 则先结束编辑（点击其它控件即视为提交当前缓冲区）。
        if let Some(active) = *color_edit_active {
            let active_hex = layout.color_hex_rects[active.index()];
            if !point_in_rect(mx, my, active_hex) {
                *color_edit_active = None;
            }
        }

        // 先检查标签页点击（8 个标签：文本/音频/画面/快进/配色/开发者/帮助/关于）
        let tab_tabs = [SettingsTab::Text, SettingsTab::Audio, SettingsTab::Display, SettingsTab::Skip, SettingsTab::Color, SettingsTab::Developer, SettingsTab::Help, SettingsTab::About];
        for (i, tab) in tab_tabs.iter().enumerate() {
            if point_in_rect(mx, my, layout.tab_rects[i]) {
                *active_tab = *tab;
                *dropdown_open = false;
                *skip_dropdown_open = false;
                *ui_lang_dropdown_open = false;
                *lang_dropdown_open = false;
                return None;
            }
        }

        // 根据当前标签页处理控件
        match *active_tab {
            SettingsTab::Text => {
                // 文本速度滑块
                if point_in_rect(mx, my, layout.text_slider_hit) {
                    *dragging_slider = Some((SettingsTab::Text, 0));
                    update_slider_value(engine, SettingsTab::Text, 0, mx, layout.text_slider_track);
                    return None;
                }
                // 自动播放开关
                if point_in_rect(mx, my, layout.auto_play_toggle) {
                    let settings = engine.settings_mut();
                    settings.auto_play = !settings.auto_play;
                    return None;
                }
                // 有语音间隔滑块
                if point_in_rect(mx, my, layout.auto_play_slider_hits[0]) {
                    *dragging_slider = Some((SettingsTab::Text, 1));
                    update_slider_value(engine, SettingsTab::Text, 1, mx, layout.auto_play_slider_tracks[0]);
                    return None;
                }
                // 无语音间隔滑块
                if point_in_rect(mx, my, layout.auto_play_slider_hits[1]) {
                    *dragging_slider = Some((SettingsTab::Text, 2));
                    update_slider_value(engine, SettingsTab::Text, 2, mx, layout.auto_play_slider_tracks[1]);
                    return None;
                }
            }
            SettingsTab::Audio => {
                // BGM、SFX、Voice 滑块
                for i in 0..3 {
                    if point_in_rect(mx, my, layout.audio_slider_hits[i]) {
                        *dragging_slider = Some((SettingsTab::Audio, i));
                        update_slider_value(engine, SettingsTab::Audio, i, mx, layout.audio_slider_tracks[i]);
                        return None;
                    }
                }
            }
            SettingsTab::Display => {
                // 开关（auto_recovery、fullscreen 共 2 个；debug_terminal 已移至开发者页）
                for i in 0..2 {
                    if point_in_rect(mx, my, layout.display_toggles[i]) {
                        let settings = engine.settings_mut();
                        match i {
                            0 => settings.auto_recovery = !settings.auto_recovery,
                            1 => settings.fullscreen = !settings.fullscreen,
                            _ => {}
                        }
                        return None;
                    }
                }
                // 分辨率下拉
                if point_in_rect(mx, my, layout.display_dropdown) {
                    *dropdown_open = !*dropdown_open;
                    *ui_lang_dropdown_open = false;
                    *lang_dropdown_open = false;
                    return None;
                }
                if *dropdown_open {
                    let item_h = 32.0 * scale;
                    let presets = Settings::resolution_presets();
                    let mut hit = false;
                    for (i, (pw, ph)) in presets.iter().enumerate() {
                        let item_y = layout.display_dropdown.y + layout.display_dropdown.h + i as f32 * item_h;
                        let item_rect = Rect4 {
                            x: layout.display_dropdown.x,
                            y: item_y,
                            w: layout.display_dropdown.w,
                            h: item_h,
                        };
                        if point_in_rect(mx, my, item_rect) {
                            engine.settings_mut().resolution = (*pw, *ph);
                            *dropdown_open = false;
                            hit = true;
                            break;
                        }
                    }
                    if hit {
                        return None;
                    }
                    // Clicked outside the list: close it.
                    *dropdown_open = false;
                    return None;
                }
                // UI 语言下拉
                if point_in_rect(mx, my, layout.ui_language_dropdown) {
                    *ui_lang_dropdown_open = !*ui_lang_dropdown_open;
                    *dropdown_open = false;
                    *lang_dropdown_open = false;
                    return None;
                }
                if *ui_lang_dropdown_open {
                    let item_h = 32.0 * scale;
                    let available = engine.available_languages();
                    let mut options: Vec<(String, String)> = Vec::new();
                    options.push(("".to_string(), "原文".to_string()));
                    for (code, name) in &available {
                        options.push((code.clone(), format!("{} ({})", name, code)));
                    }
                    let mut hit = false;
                    for (i, (code, _)) in options.iter().enumerate() {
                        let item_y = layout.ui_language_dropdown.y + layout.ui_language_dropdown.h + i as f32 * item_h;
                        let item_rect = Rect4 {
                            x: layout.ui_language_dropdown.x,
                            y: item_y,
                            w: layout.ui_language_dropdown.w,
                            h: item_h,
                        };
                        if point_in_rect(mx, my, item_rect) {
                            engine.settings_mut().ui_language = code.clone();
                            engine.reload_ui_language();
                            *ui_lang_dropdown_open = false;
                            hit = true;
                            break;
                        }
                    }
                    if hit {
                        return None;
                    }
                    // Clicked outside the list: close it.
                    *ui_lang_dropdown_open = false;
                    return None;
                }
                // 剧本语言下拉
                if point_in_rect(mx, my, layout.language_dropdown) {
                    *lang_dropdown_open = !*lang_dropdown_open;
                    *dropdown_open = false;
                    *ui_lang_dropdown_open = false;
                    return None;
                }
                if *lang_dropdown_open {
                    let item_h = 32.0 * scale;
                    let available = engine.available_languages();
                    let mut options: Vec<(String, String)> = Vec::new();
                    options.push(("".to_string(), "原文".to_string()));
                    for (code, name) in &available {
                        options.push((code.clone(), format!("{} ({})", name, code)));
                    }
                    let mut hit = false;
                    for (i, (code, _)) in options.iter().enumerate() {
                        let item_y = layout.language_dropdown.y + layout.language_dropdown.h + i as f32 * item_h;
                        let item_rect = Rect4 {
                            x: layout.language_dropdown.x,
                            y: item_y,
                            w: layout.language_dropdown.w,
                            h: item_h,
                        };
                        if point_in_rect(mx, my, item_rect) {
                            engine.settings_mut().language = code.clone();
                            engine.reload_language();
                            // 若 UI 语言设为「跟随剧本语言」，同步重载 UI 翻译
                            if engine.settings().ui_language.is_empty() {
                                engine.reload_ui_language();
                            }
                            *lang_dropdown_open = false;
                            hit = true;
                            break;
                        }
                    }
                    if hit {
                        return None;
                    }
                    // Clicked outside the list: close it.
                    *lang_dropdown_open = false;
                    return None;
                }
            }
            SettingsTab::Skip => {
                // 允许跳过未读开关
                if point_in_rect(mx, my, layout.skip_toggle) {
                    let settings = engine.settings_mut();
                    settings.skip_unread = !settings.skip_unread;
                    return None;
                }
                // 快进模式下拉
                if point_in_rect(mx, my, layout.skip_dropdown) {
                    *skip_dropdown_open = !*skip_dropdown_open;
                    return None;
                }
                if *skip_dropdown_open {
                    let item_h = 32.0 * scale;
                    let options = [SkipMode::TextOnly, SkipMode::WithVoice];
                    for (i, opt) in options.iter().enumerate() {
                        let item_y = layout.skip_dropdown.y + layout.skip_dropdown.h + i as f32 * item_h;
                        let item_rect = Rect4 {
                            x: layout.skip_dropdown.x,
                            y: item_y,
                            w: layout.skip_dropdown.w,
                            h: item_h,
                        };
                        if point_in_rect(mx, my, item_rect) {
                            engine.settings_mut().skip_mode = *opt;
                            *skip_dropdown_open = false;
                            return None;
                        }
                    }
                    // Clicked outside the list: close it.
                    *skip_dropdown_open = false;
                    return None;
                }
            }
            // 帮助和关于页为纯展示，无交互控件
            SettingsTab::Help | SettingsTab::About => {}
            SettingsTab::Color => {
                let fields = [ColorField::ThemePrimary, ColorField::ThemeSecondary, ColorField::ThemeDialogue, ColorField::ReadText, ColorField::UnreadText];
                for (i, field) in fields.iter().enumerate() {
                    // 大色块：点击激活该字段的 hex 编辑
                    if point_in_rect(mx, my, layout.color_swatch_rects[i]) {
                        *color_edit_active = Some(*field);
                        *color_hex_buffer = hex_string_from_color(field.value(engine.settings(), project_config));
                        return None;
                    }
                    // [主题色行]「项目默认 / 自定义」按钮
                    if field.is_theme() && point_in_rect(mx, my, layout.color_default_btn_rects[i]) {
                        let settings = engine.settings_mut();
                        if field.is_custom(settings) {
                            // 自定义 → 项目默认
                            field.clear(settings);
                        } else {
                            // 项目默认 → 自定义（用项目默认值初始化）
                            let v = field.value(settings, project_config);
                            field.set(settings, v);
                        }
                        // 切换后若该字段正处于编辑状态，结束编辑
                        if *color_edit_active == Some(*field) {
                            *color_edit_active = None;
                        }
                        return None;
                    }
                    // hex 输入框：点击激活编辑（点击当前已激活的框则保持）
                    if point_in_rect(mx, my, layout.color_hex_rects[i]) {
                        *color_edit_active = Some(*field);
                        *color_hex_buffer = hex_string_from_color(field.value(engine.settings(), project_config));
                        return None;
                    }
                    // 调色板预设：点击直接套用
                    for j in 0..12 {
                        if point_in_rect(mx, my, layout.color_palette_rects[i * 12 + j]) {
                            let c = COLOR_PALETTE[j];
                            field.set(engine.settings_mut(), c);
                            // 若正在编辑该字段，同步刷新缓冲区
                            if *color_edit_active == Some(*field) {
                                *color_hex_buffer = hex_string_from_color(c);
                            }
                            return None;
                        }
                    }
                }
            }
            SettingsTab::Developer => {
                // 显示终端调试输出开关
                if point_in_rect(mx, my, layout.dev_debug_toggle) {
                    let settings = engine.settings_mut();
                    settings.debug_terminal = !settings.debug_terminal;
                    return None;
                }
                // 全部设为未读：清空已读历史
                if point_in_rect(mx, my, layout.dev_clear_read_btn) {
                    engine.clear_read_history();
                    return None;
                }
            }
        }

        // 应用按钮：保存设置并返回
        if point_in_rect(mx, my, layout.apply_btn) {
            *dragging_slider = None;
            return Some((UiMode::Normal, PendingUiAction::ApplySettings));
        }
        // 取消按钮：检测是否有更改，若有则显示确认对话框
        if point_in_rect(mx, my, layout.cancel_btn) {
            *dragging_slider = None;
            // 检测设置是否有更改
            let has_changes = if let Some(snapshot) = settings_snapshot {
                let current = engine.settings();
                current.text_speed != snapshot.text_speed
                    || current.bgm_volume != snapshot.bgm_volume
                    || current.sfx_volume != snapshot.sfx_volume
                    || current.voice_volume != snapshot.voice_volume
                    || current.auto_recovery != snapshot.auto_recovery
                    || current.fullscreen != snapshot.fullscreen
                    || current.resolution != snapshot.resolution
                    || current.skip_unread != snapshot.skip_unread
                    || current.skip_mode != snapshot.skip_mode
                    || current.auto_play != snapshot.auto_play
                    || current.auto_play_delay_with_voice != snapshot.auto_play_delay_with_voice
                    || current.auto_play_delay_without_voice != snapshot.auto_play_delay_without_voice
                    || current.language != snapshot.language
                    || current.ui_language != snapshot.ui_language
                    || current.read_text_color != snapshot.read_text_color
                    || current.unread_text_color != snapshot.unread_text_color
                    || current.theme_primary != snapshot.theme_primary
                    || current.theme_secondary != snapshot.theme_secondary
                    || current.theme_dialogue != snapshot.theme_dialogue
                    || current.debug_terminal != snapshot.debug_terminal
            } else {
                false
            };
            if has_changes {
                // 需要确认，返回确认对话框模式，pending 会在调用者处理
                return Some((UiMode::ConfirmDialog, PendingUiAction::None));
            } else {
                // 无更改，直接返回
                return Some((UiMode::Normal, PendingUiAction::DiscardSettings));
            }
        }
    }
    None
}

/// Update a slider's value from the mouse X position, clamped to the track.
fn update_slider_value(engine: &mut Engine, tab: SettingsTab, index: usize, mx: f32, track: Rect4) {
    let t = if track.w > 0.0 {
        ((mx - track.x) / track.w).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let settings = engine.settings_mut();
    match (tab, index) {
        (SettingsTab::Text, 0) => settings.text_speed = (t * 999.0).round(),
        // 自动播放间隔：0.0 - 5.0 秒，按 0.1 秒精度取整
        (SettingsTab::Text, 1) => settings.auto_play_delay_with_voice = (t * 5.0 * 10.0).round() / 10.0,
        (SettingsTab::Text, 2) => settings.auto_play_delay_without_voice = (t * 5.0 * 10.0).round() / 10.0,
        (SettingsTab::Audio, 0) => settings.bgm_volume = t,
        (SettingsTab::Audio, 1) => settings.sfx_volume = t,
        (SettingsTab::Audio, 2) => settings.voice_volume = t,
        _ => {}
    }
}

/// Cycle the resolution setting to the next preset in `Settings::resolution_presets()`.
#[allow(dead_code)]
fn cycle_resolution(engine: &mut Engine) {
    let presets = Settings::resolution_presets();
    let current = engine.settings().resolution;
    let next_idx = match presets.iter().position(|p| *p == current) {
        Some(i) => (i + 1) % presets.len(),
        None => 0,
    };
    engine.settings_mut().resolution = presets[next_idx];
}

/// True if the point (mx, my) lies inside the rectangle `r`.
fn point_in_rect(mx: f32, my: f32, r: Rect4) -> bool {
    mx >= r.x && mx <= r.x + r.w && my >= r.y && my <= r.y + r.h
}

// ─── Input handling ───

fn handle_click(
    mx: f32,
    my: f32,
    buttons: &[ButtonRect],
    engine: &mut Engine,
    ui_mode: &UiMode,
    hud_hidden: &mut bool,
    sw: f32,
    sh: f32,
    scale: f32,
) -> Option<ButtonAction> {
    // Check button clicks first
    for btn in buttons {
        if mx >= btn.x && mx <= btn.x + btn.w && my >= btn.y && my <= btn.y + btn.h {
            return Some(btn.action);
        }
    }

    // If the HUD is hidden, any click (outside the now-absent buttons) simply
    // restores the dialogue box and HUD without advancing the dialogue.
    if *hud_hidden {
        *hud_hidden = false;
        return None;
    }

    // If no button clicked and in normal mode, try advancing dialogue
    if *ui_mode == UiMode::Normal {
        if engine.phase() == EnginePhase::Title {
            // Clicking on title screen without hitting a button does nothing
        } else if engine.phase() == EnginePhase::ChoicePending {
            // Click on choice options
            handle_choice_click(mx, my, engine, sw, sh, scale);
        } else if engine.phase() == EnginePhase::StoryEnded {
            // Ignore
        } else {
            // Click anywhere to advance
            engine.advance();
        }
    }

    None
}

fn handle_choice_click(mx: f32, my: f32, engine: &mut Engine, sw: f32, sh: f32, scale: f32) {
    if let Some(choices) = &engine.scene().choices.clone() {
        let opt_w = 500.0 * scale;
        let opt_h = 60.0 * scale;
        let opt_x = (sw - opt_w) / 2.0;
        let mut opt_y = sh * 0.3;

        for (i, opt) in choices.options.iter().enumerate() {
            if mx >= opt_x && mx <= opt_x + opt_w && my >= opt_y && my <= opt_y + opt_h {
                if opt.available {
                    engine.choose(i);
                }
                return;
            }
            opt_y += opt_h + 15.0 * scale;
        }
    }
}

/// Map a button action to a UI transition request.
/// Returns `None` for actions that don't change UI mode (QuickSave, QuickLoad,
/// ToggleHide, paging) — those are handled by the caller.
fn handle_button_action(action: ButtonAction) -> Option<(UiMode, PendingUiAction)> {
    match action {
        ButtonAction::StartGame => Some((UiMode::Normal, PendingUiAction::StartGame)),
        ButtonAction::ContinueGame => Some((UiMode::Normal, PendingUiAction::ContinueGame)),
        ButtonAction::LoadGame => Some((UiMode::LoadMenu, PendingUiAction::None)),
        ButtonAction::Settings => Some((UiMode::SettingsMenu, PendingUiAction::None)),
        ButtonAction::Quit => Some((UiMode::Normal, PendingUiAction::Quit)),
        ButtonAction::SaveSlot(slot) => Some((UiMode::Normal, PendingUiAction::SaveSlot(slot))),
        ButtonAction::LoadSlot(slot) => Some((UiMode::Normal, PendingUiAction::LoadSlot(slot))),
        ButtonAction::BackToTitle => Some((UiMode::ConfirmDialog, PendingUiAction::None)), // 先显示确认对话框
        ButtonAction::BackToGame | ButtonAction::CloseMenu => Some((UiMode::Normal, PendingUiAction::None)),
        ButtonAction::ContinueAutosave => Some((UiMode::Normal, PendingUiAction::ContinueAutosave)),
        ButtonAction::DiscardAutosave => Some((UiMode::Normal, PendingUiAction::DiscardAutosave)),
        ButtonAction::OpenSaveMenu => Some((UiMode::SaveMenu, PendingUiAction::None)),
        ButtonAction::OpenLoadMenu => Some((UiMode::LoadMenu, PendingUiAction::None)),
        ButtonAction::OpenSettings => Some((UiMode::SettingsMenu, PendingUiAction::None)),
        ButtonAction::QuickSave | ButtonAction::QuickLoad
        | ButtonAction::ToggleHide | ButtonAction::FastForward
        | ButtonAction::AutoPlay
        | ButtonAction::PrevPage | ButtonAction::NextPage
        | ButtonAction::AddPage => None,
        ButtonAction::ConfirmYes => Some((UiMode::Normal, PendingUiAction::None)), // 由调用者处理具体确认逻辑
        ButtonAction::ConfirmNo => Some((UiMode::Normal, PendingUiAction::None)),  // 返回上一个模式
        // 备注编辑相关动作由主循环直接处理，不触发 UI transition。
        ButtonAction::EditNote(_) | ButtonAction::NoteConfirm | ButtonAction::NoteCancel => None,
        // 蓝屏界面 / 目录选择器动作由主循环直接处理，不触发 UI transition。
        ButtonAction::CrashExportLog | ButtonAction::CrashContinue | ButtonAction::CrashExit
        | ButtonAction::DirUp | ButtonAction::DirConfirm | ButtonAction::DirCancel
        | ButtonAction::DirEntry(_) => None,
        // 隐藏结局 epilogue：进入尾声剧本（淡入淡出后由 swap 点处理加载）。
        ButtonAction::PlayEpilogue(i) => Some((UiMode::Normal, PendingUiAction::PlayEpilogue(i))),
    }
}

// ─── Utility ───

fn draw_button(
    x: f32, y: f32, w: f32, h: f32,
    label: &str,
    buttons: &mut Vec<ButtonRect>,
    action: ButtonAction,
    font: &Option<Font>,
    scale: f32,
) {
    let (mx, my) = mouse_position();
    let hover = mx >= x && mx <= x + w && my >= y && my <= y + h;

    // 按钮配色：基于主题色2（secondary）派生悬停/默认两态。
    let t = theme();
    let bg_color = if hover { shade(t.secondary, 0.15) } else { t.secondary };
    draw_rectangle(x, y, w, h, bg_color);
    let border = if hover {
        Color::new(shade(t.secondary, 0.40).r, shade(t.secondary, 0.40).g, shade(t.secondary, 0.40).b, 1.0)
    } else {
        Color::new(shade(t.secondary, 0.20).r, shade(t.secondary, 0.20).g, shade(t.secondary, 0.20).b, 0.8)
    };
    draw_rectangle_lines(x, y, w, h, 2.0 * scale, border);

    // Font size scales with the button height, capped to keep labels legible.
    let font_size = (h * 0.4).min(28.0 * scale);
    let tw = measure_text_f(label, font, font_size as u16, 1.0).width;
    draw_text_f(
        label,
        x + (w - tw) / 2.0,
        y + h / 2.0 + font_size / 3.0,
        font_size,
        t.text,
        font,
    );

    buttons.push(ButtonRect {
        x, y, w, h,
        label: label.to_string(),
        action,
    });
}

// ─── HUD 按钮组布局常量（设计基准像素）───
// 按钮已放大 1.5 倍，尺寸适中便于点击
const HUD_BTN_W: f32 = 126.0;
const HUD_BTN_H: f32 = 54.0;
const HUD_BTN_GAP: f32 = 12.0;
const HUD_BTN_COUNT: usize = 9;
const HUD_RIGHT_MARGIN: f32 = 20.0;
const HUD_BOTTOM_MARGIN: f32 = 70.0;

/// 计算 HUD 按钮组的触发区域（含上方缓冲带），用于鼠标悬停检测。
/// 返回 (x, y, w, h)，已按 scale 缩放。
fn hud_trigger_rect(sw: f32, sh: f32, scale: f32) -> (f32, f32, f32, f32) {
    let btn_w = HUD_BTN_W * scale;
    let btn_h = HUD_BTN_H * scale;
    let gap = HUD_BTN_GAP * scale;
    let total_w = HUD_BTN_COUNT as f32 * btn_w + (HUD_BTN_COUNT - 1) as f32 * gap;
    let start_x = sw - total_w - HUD_RIGHT_MARGIN * scale;
    let start_y = sh - HUD_BOTTOM_MARGIN * scale;
    // 触发区域：按钮组本身 + 上方缓冲带，方便鼠标移入。
    let band = HudVisibility::HOVER_BAND_PX * scale;
    (
        start_x,
        start_y - band,
        total_w,
        btn_h + band,
    )
}

/// Draw the in-game HUD button group anchored to the bottom-right corner.
///
/// The group is only shown during normal gameplay (not on the title screen,
/// menus, or while the HUD is hidden). Each button registers itself in the
/// `buttons` vector so the generic click handler can dispatch its action.
///
/// `visibility` 控制整体的上浮/下沉偏移与透明度。
fn draw_hud_buttons(
    engine: &Engine,
    buttons: &mut Vec<ButtonRect>,
    sw: f32,
    sh: f32,
    font: &Option<Font>,
    scale: f32,
    visibility: &HudVisibility,
    skip_active: bool,
    auto_play_active: bool,
) {
    let btn_w = HUD_BTN_W * scale;
    let btn_h = HUD_BTN_H * scale;
    let gap = HUD_BTN_GAP * scale;
    let total_w = HUD_BTN_COUNT as f32 * btn_w + (HUD_BTN_COUNT - 1) as f32 * gap;
    let start_x = sw - total_w - HUD_RIGHT_MARGIN * scale;
    // 基准 y（完全显示时的位置）+ 下沉偏移
    let base_y = sh - HUD_BOTTOM_MARGIN * scale;
    let start_y = base_y + visibility.sink_offset() * scale;
    let alpha = visibility.alpha();

    // HUD 按钮标签走 UI 翻译器（engine.t_ui），找不到时回退到 key 本身。
    let hud_buttons: [(&str, ButtonAction); 9] = [
        (engine.t_ui("hud.skip"), ButtonAction::FastForward),
        (engine.t_ui("hud.auto"), ButtonAction::AutoPlay),
        (engine.t_ui("hud.quick_save"), ButtonAction::QuickSave),
        (engine.t_ui("hud.quick_load"), ButtonAction::QuickLoad),
        (engine.t_ui("hud.save"), ButtonAction::OpenSaveMenu),
        (engine.t_ui("hud.load"), ButtonAction::OpenLoadMenu),
        (engine.t_ui("hud.title"), ButtonAction::BackToTitle),
        (engine.t_ui("hud.settings"), ButtonAction::OpenSettings),
        (engine.t_ui("hud.hide"), ButtonAction::ToggleHide),
    ];

    let mut x = start_x;
    for (label, action) in &hud_buttons {
        let is_active = match action {
            ButtonAction::FastForward => skip_active,
            ButtonAction::AutoPlay => auto_play_active,
            _ => false,
        };
        // 快读按钮在无快存时禁用（灰色、无操作）。
        let is_disabled = matches!(action, ButtonAction::QuickLoad) && !engine.has_quicksave();
        draw_small_button(x, start_y, btn_w, btn_h, label, buttons, *action, font, scale, alpha, is_active, is_disabled);
        x += btn_w + gap;
    }
}

/// Draw a small semi-transparent button with an 18px font, used by the HUD
/// button group. Registers the button region for click handling.
///
/// 三态交互：
/// - 默认（未悬停未按下）：半透明低亮度，贴合隐藏氛围
/// - 悬停（鼠标在按钮内未按下）：高亮、轻微放大、边框提亮
/// - 按下（鼠标按下瞬间）：按压反馈（缩小、颜色加深、下移 1px）
///
/// `alpha` 为整体透明度（来自 HUD 显隐进度），各颜色通道按此缩放。
/// `disabled` 为 true 时绘制为灰色禁用态：不响应悬停/按下、不注册点击区域
/// （因此点击不会触发任何操作）。
fn draw_small_button(
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    label: &str,
    buttons: &mut Vec<ButtonRect>,
    action: ButtonAction,
    font: &Option<Font>,
    scale: f32,
    alpha: f32,
    active: bool,
    disabled: bool,
) {
    // 禁用态：灰色绘制，不注册点击区域（点击自然无操作）。
    if disabled {
        let t = theme();
        let bg = shade(t.secondary, -0.35);
        let border = shade(t.secondary, -0.20);
        let bg_color = Color::new(bg.r, bg.g, bg.b, 0.55 * alpha);
        draw_rectangle(x, y, w, h, bg_color);
        draw_rectangle_lines(x, y, w, h, 1.5 * scale, Color::new(border.r, border.g, border.b, 0.45 * alpha));
        let font_size = 20.0 * scale;
        let tw = measure_text_f(label, font, font_size as u16, 1.0).width;
        let text_color = Color::new(t.text.r, t.text.g, t.text.b, 0.45 * alpha);
        draw_text_f(
            label,
            x + (w - tw) / 2.0,
            y + h / 2.0 + font_size / 3.0,
            font_size,
            text_color,
            font,
        );
        return;
    }

    let (mx, my) = mouse_position();
    let hover = mx >= x && mx <= x + w && my >= y && my <= y + h;
    let pressed = hover && is_mouse_button_down(MouseButton::Left);

    // 三态颜色：基于主题色2（secondary）派生，激活态（如快进中）使用更亮的颜色。
    let t = theme();
    let sec = t.secondary;
    let (bg, border, scale_factor, dy) = if pressed {
        // 按下：颜色加深、按压下移 1px、轻微缩小
        (shade(sec, -0.10), shade(sec, 0.15), 0.96, 1.0 * scale)
    } else if hover || active {
        // 悬停或激活：高亮、放大
        (shade(sec, 0.05), shade(sec, 0.30), 1.06, 0.0)
    } else {
        // 默认：低亮度半透明
        (shade(sec, -0.15), sec, 1.00, 0.0)
    };
    let (bg_a_base, border_a_base) = if pressed {
        (0.95, 1.0)
    } else if hover || active {
        (0.92, 1.0)
    } else {
        (0.70, 0.55)
    };

    // 按缩放因子调整绘制尺寸（以中心为基准）
    let draw_w = w * scale_factor;
    let draw_h = h * scale_factor;
    let draw_x = x + (w - draw_w) / 2.0;
    let draw_y = y + (h - draw_h) / 2.0 + dy;

    let bg_color = Color::new(bg.r, bg.g, bg.b, bg_a_base * alpha);
    draw_rectangle(draw_x, draw_y, draw_w, draw_h, bg_color);
    draw_rectangle_lines(
        draw_x, draw_y, draw_w, draw_h,
        1.5 * scale,
        Color::new(border.r, border.g, border.b, border_a_base * alpha),
    );

    let font_size = 20.0 * scale;
    let tw = measure_text_f(label, font, font_size as u16, 1.0).width;
    // 文字透明度随 alpha 走；悬停/按下时略提亮
    let text_alpha = if pressed { 0.95 } else if hover { 1.0 } else { 0.9 } * alpha;
    let text_color = Color::new(t.text.r, t.text.g, t.text.b, text_alpha);
    draw_text_f(
        label,
        draw_x + (draw_w - tw) / 2.0,
        draw_y + draw_h / 2.0 + font_size / 3.0,
        font_size,
        text_color,
        font,
    );

    // 命中判定仍用原始矩形（不随缩放变化），保证点击稳定。
    buttons.push(ButtonRect {
        x, y, w, h,
        label: label.to_string(),
        action,
    });
}

fn draw_text_wrapped(text: &str, x: f32, y: f32, max_w: f32, font_size: f32, color: Color, font: &Option<Font>, scale: f32) {
    let mut current_y = y;
    let mut current_line = String::new();
    let line_gap = 8.0 * scale;

    for word in text.split_whitespace() {
        let test_line = if current_line.is_empty() {
            word.to_string()
        } else {
            format!("{} {}", current_line, word)
        };
        let w = measure_text_f(&test_line, font, font_size as u16, 1.0).width;

        if w > max_w && !current_line.is_empty() {
            draw_text_f(&current_line, x, current_y, font_size, color, font);
            current_y += font_size + line_gap;
            current_line = word.to_string();
        } else {
            current_line = test_line;
        }
    }

    if !current_line.is_empty() {
        draw_text_f(&current_line, x, current_y, font_size, color, font);
    }
}

/// Convert a resource name to a deterministic color for placeholder rendering.
fn name_to_color(name: &str) -> (f32, f32, f32) {
    let hash: u32 = name.chars().map(|c| c as u32).fold(0u32, |acc, c| {
        acc.wrapping_mul(31).wrapping_add(c)
    });
    let r = ((hash >> 16) & 0xFF) as f32 / 255.0;
    let g = ((hash >> 8) & 0xFF) as f32 / 255.0;
    let b = (hash & 0xFF) as f32 / 255.0;
    // Ensure minimum brightness
    (
        r * 0.5 + 0.2,
        g * 0.5 + 0.2,
        b * 0.5 + 0.2,
    )
}
