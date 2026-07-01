//! Main renderer: game loop, drawing, and input handling.

use crate::assets::{AssetKind, AssetManager};
use akrs_runtime::{
    Engine, EngineEvent, EnginePhase, SceneState, Settings, SettingsTab, SkipMode,
    TransitionPhase,
    format_play_time, format_timestamp,
    SaveMetadata,
};
use macroquad::prelude::*;
use std::path::PathBuf;

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

/// UI 缩放因子：当前窗口相对于 1920×1080 设计基准的最小轴比例。
/// 所有绝对像素尺寸（字号、按钮尺寸、边距）都乘以此因子，
/// 使布局在任何窗口尺寸下看起来一致。
fn ui_scale(sw: f32, sh: f32) -> f32 {
    (sw / BASE_WIDTH).min(sh / BASE_HEIGHT)
}

/// 计算合适的窗口尺寸：约占屏幕面积的 1/2。
/// 窗口尺寸 = 屏幕尺寸 × sqrt(0.5) ≈ 0.707
fn calculate_window_size(screen_w: i32, screen_h: i32) -> (i32, i32) {
    let scale = 0.707;
    let w = (screen_w as f32 * scale) as i32;
    let h = (screen_h as f32 * scale) as i32;
    (w, h)
}

/// 获取屏幕尺寸（尽力而为的跨平台方案）。
fn get_screen_size() -> (i32, i32) {
    // 尝试使用 miniquad 的平台 API
    #[cfg(target_os = "windows")]
    {
        // Windows: 尝试获取主显示器尺寸
        if let Ok(width) = std::env::var("SCREEN_WIDTH") {
            if let Ok(height) = std::env::var("SCREEN_HEIGHT") {
                if let (Ok(w), Ok(h)) = (width.parse::<i32>(), height.parse::<i32>()) {
                    return (w, h);
                }
            }
        }
    }
    // 默认假设为 1920×1080 屏幕
    (1920, 1080)
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
}

/// 确认对话框的类型，用于显示不同的提示文本。
#[derive(Clone, Copy, Debug, PartialEq)]
enum ConfirmType {
    /// 返回标题界面的确认。
    BackToTitle,
    /// 未应用设置时退出设置菜单的确认。
    UnappliedSettings,
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
    /// Quick-save to the dedicated quick-save slot (slot 0).
    QuickSave,
    /// Quick-load from the dedicated quick-save slot (slot 0).
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
}

/// 窗口配置 for macroquad。
/// 启用高 DPI 渲染以避免"假高清"模糊问题。
/// 窗口尺寸约为屏幕面积的 1/2，系统自动居中。
pub fn window_conf() -> macroquad::miniquad::conf::Conf {
    let (screen_w, screen_h) = get_screen_size();
    let (win_w, win_h) = calculate_window_size(screen_w, screen_h);

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
pub async fn run(mut engine: Engine) {
    let mut assets = AssetManager::new();
    // Load Chinese font for proper CJK text rendering, with system-font fallback.
    let (font, fallback_font) = load_font_with_fallback();
    set_fallback_font(fallback_font);
    // Intercept the window-close (X) button so we can autosave before exiting.
    prevent_quit();
    // Load persistent settings (text speed, volume, etc.) before starting so
    // the player's preferences from the previous session are honored.
    engine.load_settings();
    // If a crash-recovery autosave exists from a previous run, prompt the
    // player to resume before showing the title screen.
    let mut ui_mode = if engine.has_autosave() {
        UiMode::AutoSavePrompt
    } else {
        UiMode::Normal
    };
    let mut buttons: Vec<ButtonRect> = Vec::new();
    let mut _prev_music: Option<String> = None;
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
    // 上一次实际应用到窗口的全屏状态。
    // window_conf() 中 fullscreen 初始化为 false，故此处同步为 false。
    // 每帧检测 settings.fullscreen 是否与此值不一致，若不一致则切换窗口全屏状态，
    // 这样既能响应设置菜单中的切换，也能在启动时应用上次保存的偏好。
    let mut last_fullscreen_applied: bool = false;
    // 已应用的全屏设置值。只有点击"应用"按钮时才会更新此值。
    // 启动时用 engine.settings().fullscreen 初始化（应用上次保存的偏好）。
    let mut applied_fullscreen: bool = engine.settings().fullscreen;
    // 设置菜单进入时保存的设置快照，用于检测是否有未应用的更改。
    let mut settings_snapshot: Option<Settings> = None;
    // 确认对话框的类型（返回标题/未应用设置退出）。
    let mut confirm_type: Option<ConfirmType> = None;
    // 确认对话框返回后应切换到的 UI 模式。
    let mut confirm_return_mode: UiMode = UiMode::Normal;
    // 是否有"继续游戏"存档。
    let mut has_continue_save: bool = engine.has_continue_save();
    // 进入设置菜单前的 UI 模式（用于返回时恢复）。
    let mut settings_prev_mode: UiMode = UiMode::Normal;

    // Check title music
    if !assets.check_music("title_bgm.mp3") {
        eprintln!("[Warning] title_bgm.mp3 not found — using black screen + silence");
    }

    loop {
        let dt = get_frame_time();
        let (sw, sh) = (screen_width(), screen_height());
        // UI scale factor relative to the 1280×720 design baseline.
        let scale = ui_scale(sw, sh);

        // 全屏状态同步：使用 applied_fullscreen（已应用的值）而非正在编辑的值。
        // 这样设置菜单中切换全屏开关不会立即生效，只有点击"应用"后才生效。
        // 从全屏恢复为窗口时，恢复为屏幕面积的 1/2 大小。
        {
            let want_fullscreen = applied_fullscreen;
            if want_fullscreen != last_fullscreen_applied {
                set_fullscreen(want_fullscreen);
                // 如果从全屏恢复为窗口模式，设置窗口大小为 1/2 屏幕
                if !want_fullscreen && last_fullscreen_applied {
                    let (screen_w, screen_h) = get_screen_size();
                    let (win_w, win_h) = calculate_window_size(screen_w, screen_h);
                    request_new_screen_size(win_w as f32, win_h as f32);
                }
                last_fullscreen_applied = want_fullscreen;
            }
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
                    if name.is_empty() {
                        _prev_music = None;
                    } else {
                        _prev_music = Some(name.clone());
                        assets.check_music(name);
                    }
                }
                EngineEvent::SoundPlayed { name } => {
                    assets.check_sound(name);
                }
                EngineEvent::Warning { message } => {
                    eprintln!("[Engine Warning] {}", message);
                }
                EngineEvent::Error { message } => {
                    eprintln!("[Engine Error] {}", message);
                }
                _ => {}
            }
        }

        // Handle title music
        if engine.phase() == EnginePhase::Title && !title_music_played {
            if assets.check_music("title_bgm.mp3") {
                // Would play music here; macroquad 0.3 audio is limited
            }
            title_music_played = true;
        }

        // Clear buttons for this frame
        buttons.clear();

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
                    // 保存"继续游戏"存档
                    let _ = engine.save_continue();
                    has_continue_save = true;
                    let source = engine.source().to_string();
                    let saved_settings = engine.settings().clone();
                    if let Ok(mut new_engine) = Engine::new(&source) {
                        *new_engine.settings_mut() = saved_settings;
                        engine = new_engine;
                        title_music_played = false;
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
                    settings_snapshot = None;
                    // 应用设置后返回之前的模式
                    ui_mode = settings_prev_mode;
                }
                PendingUiAction::DiscardSettings => {
                    // 恢复进入设置菜单前的设置快照
                    if let Some(snapshot) = settings_snapshot.clone() {
                        *engine.settings_mut() = snapshot;
                    }
                    settings_snapshot = None;
                    // 放弃设置后返回之前的模式
                    ui_mode = settings_prev_mode;
                }
                PendingUiAction::StoryEndToTitle => {
                    // 故事结束自动返回标题：重置引擎，删除继续存档，不显示继续游戏按钮
                    let _ = engine.delete_continue();
                    has_continue_save = false;
                    let source = engine.source().to_string();
                    let saved_settings = engine.settings().clone();
                    if let Ok(mut new_engine) = Engine::new(&source) {
                        *new_engine.settings_mut() = saved_settings;
                        engine = new_engine;
                        title_music_played = false;
                    }
                    hud_hidden = false;
                    settings_snapshot = None;
                    ui_mode = UiMode::Normal;
                }
            }
        }

        // Settings layout is computed once per frame and shared by both the
        // draw call and the interaction handler below.
        let settings_layout = compute_settings_layout(sw, sh, scale);

        if ui_mode == UiMode::AutoSavePrompt {
            // Crash-recovery prompt: dark backdrop + centered dialog.
            draw_rectangle(0.0, 0.0, sw, sh, Color::new(0.05, 0.05, 0.1, 1.0));
            draw_dim_overlay(sw, sh, 0.6);
            draw_autosave_prompt(&engine, &mut buttons, sw, sh, &font, scale);
        } else if ui_mode == UiMode::SettingsMenu {
            // 进入设置菜单时保存当前设置的快照（仅一次），并重置标签页
            if settings_snapshot.is_none() {
                settings_snapshot = Some(engine.settings().clone());
                settings_active_tab = SettingsTab::Text;
            }
            // 设置菜单内部绘制背景和所有控件
            draw_settings_menu(&mut engine, &settings_layout, &font, dropdown_open, skip_dropdown_open, settings_active_tab, scale);
        } else if ui_mode == UiMode::ConfirmDialog {
            // 确认对话框：先绘制底层界面（保持上下文可见），再叠加 8% 黑色 + 对话框。
            // confirm_return_mode 记录了确认对话框返回后应恢复的模式，据此绘制底层。
            match confirm_return_mode {
                UiMode::SettingsMenu => {
                    if settings_snapshot.is_none() {
                        settings_snapshot = Some(engine.settings().clone());
                    }
                    draw_settings_menu(&mut engine, &settings_layout, &font, dropdown_open, skip_dropdown_open, settings_active_tab, scale);
                }
                UiMode::Normal => {
                    if engine.phase() == EnginePhase::Title {
                        draw_title_screen(&mut buttons, sw, sh, &mut assets, &font, scale, has_continue_save).await;
                    } else if engine.phase() == EnginePhase::StoryEnded {
                        draw_scene(&engine, &mut assets, sw, sh, true, &font, scale).await;
                    } else {
                        // 游戏中：只绘制场景（不绘制可交互 HUD，因为确认对话框期间不需要 HUD 交互）。
                        draw_scene(&engine, &mut assets, sw, sh, !hud_hidden, &font, scale).await;
                    }
                }
                _ => {
                    // 其他模式（存档/读档菜单等）：绘制不透明背景作为兜底。
                    draw_rectangle(0.0, 0.0, sw, sh, Color::new(0.05, 0.05, 0.1, 1.0));
                }
            }
            // 8% 黑色叠层（透明度 0.08），让底层界面仍可见但变暗。
            draw_rectangle(0.0, 0.0, sw, sh, Color::new(0.0, 0.0, 0.0, 0.08));
            // 居中确认对话框
            draw_confirm_dialog(&mut buttons, sw, sh, &font, scale, confirm_type);
        } else if ui_mode != UiMode::Normal {
            // Save/Load menus: full-screen opaque background + full-screen grid.
            draw_rectangle(0.0, 0.0, sw, sh, Color::new(0.05, 0.05, 0.1, 1.0));
            match ui_mode {
                UiMode::SaveMenu => draw_save_menu(&engine, &mut buttons, sw, sh, &font, scale, save_page, save_displayed_slots),
                UiMode::LoadMenu => draw_load_menu(&engine, &mut buttons, sw, sh, &font, scale, load_page, load_displayed_slots),
                _ => {}
            }
        } else if engine.phase() == EnginePhase::Title {
            draw_title_screen(&mut buttons, sw, sh, &mut assets, &font, scale, has_continue_save).await;
        } else if engine.phase() == EnginePhase::StoryEnded {
            draw_scene(&engine, &mut assets, sw, sh, true, &font, scale).await;
        } else {
            // In-game: draw the scene. When the HUD is hidden, only the
            // background and characters are drawn (no dialogue box, choices,
            // or HUD), letting the player admire the scene unobstructed.
            draw_scene(&engine, &mut assets, sw, sh, !hud_hidden, &font, scale).await;
            if !hud_hidden {
                // 检测鼠标是否在 HUD 触发区域内，更新显隐进度。
                let (tx, ty, tw, th) = hud_trigger_rect(sw, sh, scale);
                let (mx, my) = mouse_position();
                let hovered = mx >= tx && mx <= tx + tw && my >= ty && my <= ty + th;
                hud_visibility.update(hovered, dt);
                // 完全隐藏时不绘制也不注册按钮，避免误触；
                // 半显示/全显示时绘制并注册（下沉过程可见，但低于 0.5 不可点击）。
                if hud_visibility.is_interactable() {
                    draw_hud_buttons(&mut buttons, sw, sh, &font, scale, &hud_visibility, engine.is_skip_active());
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
            if ui_mode == UiMode::SettingsMenu {
                if let Some((target, pending)) = handle_settings_interaction(
                    &mut engine, &settings_layout, &mut dragging_slider, &mut dropdown_open,
                    &mut skip_dropdown_open, &mut settings_active_tab, scale,
                    &settings_snapshot, &mut settings_prev_mode,
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
            } else if is_mouse_button_pressed(MouseButton::Left) {
                let (mx, my) = mouse_position();
                if let Some(action) = handle_click(
                    mx, my, &buttons, &mut engine, &ui_mode, &mut hud_hidden,
                    sw, sh, scale,
                ) {
                    // Handle non-transition actions immediately.
                    match action {
                        ButtonAction::QuickSave => {
                            engine.save(0);
                        }
                        ButtonAction::QuickLoad => {
                            if engine.saves().has_save(0) {
                                engine.load(0);
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
                        ButtonAction::StartGame => {
                            ui_transition.start(UiMode::Normal, PendingUiAction::StartGame);
                        }
                        ButtonAction::ContinueGame => {
                            ui_transition.start(UiMode::Normal, PendingUiAction::ContinueGame);
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
                            // Transition action.
                            if let Some((target, pending)) = handle_button_action(action) {
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
            } else if ui_mode == UiMode::SettingsMenu {
                // 设置菜单按 Esc：检测是否有未应用的更改
                dragging_slider = None;
                let has_changes = if let Some(snapshot) = settings_snapshot.clone() {
                    let current = engine.settings();
                    current.text_speed != snapshot.text_speed
                        || current.bgm_volume != snapshot.bgm_volume
                        || current.sfx_volume != snapshot.sfx_volume
                        || current.auto_recovery != snapshot.auto_recovery
                        || current.fullscreen != snapshot.fullscreen
                        || current.resolution != snapshot.resolution
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
        // UI transitions.
        if !ui_transition.active && ui_mode == UiMode::Normal && !hud_hidden && engine.phase() != EnginePhase::Title {
            if is_key_pressed(KeyCode::Space) || is_key_pressed(KeyCode::Enter) {
                engine.advance();
            }
        }

        // Draw UI transition overlay (black fade) on top of everything.
        if ui_transition.active {
            let alpha = ui_transition.overlay_alpha();
            draw_rectangle(0.0, 0.0, sw, sh, Color::new(0.0, 0.0, 0.0, alpha));
        }

        next_frame().await;
    }
}

// ─── Drawing functions ───

async fn draw_title_screen(buttons: &mut Vec<ButtonRect>, sw: f32, sh: f32, assets: &mut AssetManager, font: &Option<Font>, scale: f32, has_continue_save: bool) {
    // 标题页背景图（cover 模式，16:9 裁切适应）
    // 从 assets/title.png 加载（用户放在资源目录根下
    let title_bg = assets.get_texture(AssetKind::Title, "./title.png").await;
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
        // 回退：天蓝色渐变背景
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

    // 文本与控件离左边的距离（约 1% 屏幕宽度）
    let left_pad = sw * 0.01;

    // 主标题（左上角，居左对齐）
    let title = "Akizuki*Rustgal";
    let title_font_size = 56.0 * scale;
    let title_x = left_pad;
    let title_y = 40.0 * scale;
    draw_text_f(
        title,
        title_x,
        title_y + title_font_size,
        title_font_size,
        Color::new(0.1, 0.3, 0.6, 1.0),
        font,
    );

    // 副标题（主标题下方，居左对齐）
    let subtitle = "夏夜观心Extra";
    let sub_size = 28.0 * scale;
    let sub_x = title_x;
    let sub_y = title_y + title_font_size + 20.0 * scale;
    draw_text_f(
        subtitle,
        sub_x,
        sub_y + sub_size,
        sub_size,
        Color::new(0.25, 0.45, 0.7, 1.0),
        font,
    );

    // 按钮（左下角，变窄）
    let btn_w = 220.0 * scale;
    let btn_h = 52.0 * scale;
    let btn_x = title_x;

    let mut labels: Vec<(&str, ButtonAction)> = Vec::new();

    // 如果有"继续游戏"存档，在"开始游戏"前显示"继续游戏"按钮
    if has_continue_save {
        labels.push(("继续游戏", ButtonAction::ContinueGame));
    }

    labels.extend_from_slice(&[
        ("开始游戏", ButtonAction::StartGame),
        ("读取存档", ButtonAction::LoadGame),
        ("设置", ButtonAction::Settings),
        ("退出", ButtonAction::Quit),
    ]);

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
        Color::new(0.4, 0.6, 0.85, 0.6));

    // 从下往上排列按钮
    let total_btn_h = labels.len() as f32 * btn_h + (labels.len() - 1) as f32 * 14.0 * scale;
    let bottom_pad = 60.0 * scale;
    let mut btn_y = panel_bottom - bottom_pad - total_btn_h;

    for (label, action) in &labels {
        draw_button(btn_x, btn_y, btn_w, btn_h, label, buttons, *action, font, scale);
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

async fn draw_background(scene: &SceneState, assets: &mut AssetManager, sw: f32, sh: f32) {
    if let Some(bg) = &scene.background {
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
                Color::new(1.0, 1.0, 1.0, bg.alpha),
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
                bg.alpha,
            ));
        }
    } else {
        // Default: black background
        draw_rectangle(0.0, 0.0, sw, sh, BLACK);
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
    // 渐变延伸到屏幕底部，使用 64 段绘制实现更平滑过渡
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
        let t = i as f32 / (gradient_segments - 1) as f32;
        let alpha = alpha_top * (1.0 - t) + alpha_bottom * t;
        // 只在对话框区域内绘制，但渐变计算覆盖到屏幕底部
        if seg_y < box_y + box_h {
            let draw_h = segment_h.min(box_y + box_h - seg_y);
            draw_rectangle(box_x, seg_y, box_w, draw_h, Color::new(0.55, 0.78, 0.95, alpha));
        }
    }
    // 边框已移除（用户反馈边框线看着难受）

    let name_font_size = 42.0 * scale;
    let text_font_size = 39.0 * scale;
    let text_left_padding = 120.0 * scale;

    // 内容整体下移 5%、右移 2%
    let content_offset_y = box_h * 0.05;
    let content_offset_x = sw * 0.02;

    // 角色名（仅在非旁白时显示）
    if !dialogue.speaker.is_empty() {
        draw_text_f(
            &dialogue.speaker,
            box_x + 20.0 * scale + content_offset_x,
            box_y + 36.0 * scale + content_offset_y,
            name_font_size,
            Color::new(0.1, 0.25, 0.5, 1.0),
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
        Color::new(0.08, 0.12, 0.2, 1.0),
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

fn draw_dim_overlay(sw: f32, sh: f32, alpha: f32) {
    draw_rectangle(0.0, 0.0, sw, sh, Color::new(0.0, 0.0, 0.0, alpha));
}

/// Crash-recovery prompt shown at startup when an autosave is detected.
///
/// Draws a centered modal dialog with a semi-transparent backdrop and two
/// choices: resume the autosave, or discard it and start fresh.
fn draw_autosave_prompt(engine: &Engine, buttons: &mut Vec<ButtonRect>, sw: f32, sh: f32, font: &Option<Font>, scale: f32) {
    // Full-screen semi-transparent backdrop (drawn over the dim overlay).
    draw_rectangle(0.0, 0.0, sw, sh, Color::new(0.0, 0.0, 0.0, 0.55));

    // Centered dialog panel.
    let dialog_w = (720.0 * scale).min(sw - 80.0 * scale);
    let dialog_h = (360.0 * scale).min(sh - 80.0 * scale);
    let dialog_x = (sw - dialog_w) / 2.0;
    let dialog_y = (sh - dialog_h) / 2.0;

    // Panel background + border.
    draw_rectangle(
        dialog_x,
        dialog_y,
        dialog_w,
        dialog_h,
        Color::new(0.08, 0.06, 0.15, 0.97),
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
    let title = "检测到未正常退出";
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
    let line1 = "检测到上次未正常退出的游戏进度。";
    let line2 = "是否继续上次的游戏？";
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

    draw_button(btn1_x, btn_y, btn_w, btn_h, "继续游戏", buttons, ButtonAction::ContinueAutosave, font, scale);
    draw_button(btn2_x, btn_y, btn_w, btn_h, "重新开始", buttons, ButtonAction::DiscardAutosave, font, scale);
}

/// Draw a confirmation dialog for returning to title or discarding settings.
/// 注意：全屏 8% 黑色叠层已由调用方绘制，此函数只绘制居中的对话框面板。
fn draw_confirm_dialog(buttons: &mut Vec<ButtonRect>, sw: f32, sh: f32, font: &Option<Font>, scale: f32, confirm_type: Option<ConfirmType>) {
    // Centered dialog panel.
    let dialog_w = (600.0 * scale).min(sw - 80.0 * scale);
    let dialog_h = (280.0 * scale).min(sh - 80.0 * scale);
    let dialog_x = (sw - dialog_w) / 2.0;
    let dialog_y = (sh - dialog_h) / 2.0;

    // Panel background + border.
    draw_rectangle(
        dialog_x,
        dialog_y,
        dialog_w,
        dialog_h,
        Color::new(0.08, 0.06, 0.15, 0.97),
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
        Some(ConfirmType::BackToTitle) => ("确认返回标题", "确定要返回标题界面吗？\n当前进度将自动保存为「继续游戏」。"),
        Some(ConfirmType::UnappliedSettings) => ("设置未应用", "设置更改尚未应用。\n确定要放弃更改并退出吗？"),
        None => ("确认", "确定要执行此操作吗？"),
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
    let btn_w = 180.0 * scale;
    let btn_h = 50.0 * scale;
    let gap = 30.0 * scale;
    let total_w = btn_w * 2.0 + gap;
    let btn1_x = center_x - total_w / 2.0;
    let btn2_x = btn1_x + btn_w + gap;
    let btn_y = dialog_y + dialog_h - btn_h - 30.0 * scale;

    draw_button(btn1_x, btn_y, btn_w, btn_h, "确定", buttons, ButtonAction::ConfirmYes, font, scale);
    draw_button(btn2_x, btn_y, btn_w, btn_h, "取消", buttons, ButtonAction::ConfirmNo, font, scale);
}

// ─── Menu drawing ───

/// Draw the full-screen panel background + title for the save/load menus.
fn draw_panel(sw: f32, sh: f32, title: &str, font: &Option<Font>, scale: f32) {
    // 天蓝色全屏背景。
    draw_rectangle(0.0, 0.0, sw, sh, Color::new(0.1, 0.2, 0.4, 1.0));
    // 天蓝色边框。
    draw_rectangle_lines(0.0, 0.0, sw, sh, 2.0 * scale, Color::new(0.45, 0.7, 0.95, 0.8));

    let title_size = 52.0 * scale;
    let tw = measure_text_f(title, font, title_size as u16, 1.0).width;
    draw_text_f(title, (sw - tw) / 2.0, sh * 0.08, title_size, WHITE, font);
}

fn draw_save_menu(engine: &Engine, buttons: &mut Vec<ButtonRect>, sw: f32, sh: f32, font: &Option<Font>, scale: f32, page: usize, displayed_slots: usize) {
    draw_panel(sw, sh, "保存游戏", font, scale);

    let saves = engine.saves();
    let max_slots = saves.max_slots();
    let all_saves = saves.list_saves();

    draw_slot_grid(sw, sh, font, scale, page, displayed_slots, max_slots, &all_saves, buttons, true);

    // Back button (bottom-left).
    let back_w = 200.0 * scale;
    let back_h = 52.0 * scale;
    draw_button(
        40.0 * scale,
        sh - back_h - 30.0 * scale,
        back_w,
        back_h,
        "返回",
        buttons,
        ButtonAction::CloseMenu,
        font,
        scale,
    );
}

fn draw_load_menu(engine: &Engine, buttons: &mut Vec<ButtonRect>, sw: f32, sh: f32, font: &Option<Font>, scale: f32, page: usize, displayed_slots: usize) {
    draw_panel(sw, sh, "读取存档", font, scale);

    let saves = engine.saves();
    let max_slots = saves.max_slots();
    let all_saves = saves.list_saves();

    draw_slot_grid(sw, sh, font, scale, page, displayed_slots, max_slots, &all_saves, buttons, false);

    // Back button (bottom-left).
    let back_w = 200.0 * scale;
    let back_h = 52.0 * scale;
    draw_button(
        40.0 * scale,
        sh - back_h - 30.0 * scale,
        back_w,
        back_h,
        "返回",
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
fn draw_slot_grid(
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
) {
    let cols = 4; // 2 rows × 4 columns = SLOTS_PER_PAGE
    let cell_w = 340.0 * scale;
    let cell_h = 200.0 * scale;
    let gap_x = 24.0 * scale;
    let gap_y = 24.0 * scale;

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
        if let Some(t) = draw_slot_cell(x, y, cell_w, cell_h, slot, meta_clone.as_ref(), buttons, font, scale, action) {
            hovered_tooltip = Some(t);
        }
    }

    // Page navigation (bottom-right): [←] [page/total] [→], plus an extra
    // "+" button on the last page when more slots can be revealed.
    let total_pages = ((displayed_slots + SLOTS_PER_PAGE - 1) / SLOTS_PER_PAGE).max(1);
    let can_add_page = displayed_slots < max_slots;
    draw_page_nav(buttons, sw, sh, font, scale, page, total_pages, can_add_page);

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
/// Returns `Some(text)` containing the full save description when the mouse
/// hovers over a populated cell, so the caller can render a tooltip with the
/// untruncated text on top of every other element.
fn draw_slot_cell(
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
) -> Option<String> {
    let (mx, my) = mouse_position();
    let hover = mx >= x && mx <= x + w && my >= y && my <= y + h;

    // Cell background（天蓝色主题）。
    draw_rectangle(x, y, w, h, Color::new(0.15, 0.3, 0.55, 0.95));
    draw_rectangle_lines(x, y, w, h, 1.5 * scale, Color::new(0.45, 0.7, 0.95, 0.7));

    let pad = 12.0 * scale;
    let slot_label = format!("存档位 {}", slot + 1);
    draw_text_f(
        &slot_label,
        x + pad,
        y + 24.0 * scale,
        18.0 * scale,
        Color::new(0.8, 0.9, 1.0, 1.0),
        font,
    );

    let mut tooltip: Option<String> = None;

    if let Some(m) = meta {
        let ts = format_timestamp(m.timestamp);
        draw_text_f(
            &ts,
            x + pad,
            y + 46.0 * scale,
            14.0 * scale,
            Color::new(0.7, 0.7, 0.8, 1.0),
            font,
        );

        // Chapter name: 1 line, ellipsized if it overflows.
        let section_size = 16.0 * scale;
        let section = fit_text(&m.section_name, font, section_size, w - 2.0 * pad);
        draw_text_f(&section, x + pad, y + 68.0 * scale, section_size, WHITE, font);

        // Description: up to 2 lines, character-wrapped, ellipsized on overflow.
        let desc_size = 14.0 * scale;
        let desc_lines = wrap_text_cn(&m.description, font, desc_size, w - 2.0 * pad, 2);
        let mut desc_y = y + 90.0 * scale;
        for line in &desc_lines {
            draw_text_f(line, x + pad, desc_y, desc_size, Color::new(0.75, 0.75, 0.85, 1.0), font);
            desc_y += desc_size + 4.0 * scale;
        }

        // On hover, surface the full description as a tooltip.
        if hover {
            tooltip = Some(m.description.clone());
        }
    } else {
        // Empty slot: centered "空" + a dimming overlay (visually disabled).
        let empty_size = 24.0 * scale;
        let label = "空";
        let lw = measure_text_f(label, font, empty_size as u16, 1.0).width;
        draw_text_f(
            label,
            x + (w - lw) / 2.0,
            y + h / 2.0 + 8.0 * scale,
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
    let btn_w = 60.0 * scale;
    let btn_h = 44.0 * scale;
    let gap = 16.0 * scale;

    let label = format!("{}/{}", page + 1, total_pages);
    let label_size = 20.0 * scale;
    let lw = measure_text_f(&label, font, label_size as u16, 1.0).width;
    let label_w = lw + 24.0 * scale;

    // The "+" button sits to the left of "←" and only appears on the last page
    // when more slots can still be revealed.
    let is_last_page = page + 1 >= total_pages;
    let show_add = is_last_page && can_add_page;
    let add_w = if show_add { btn_w + gap } else { 0.0 };

    let total_w = add_w + btn_w * 2.0 + gap * 2.0 + label_w;
    let x0 = sw - total_w - 40.0 * scale;
    let y = sh - btn_h - 30.0 * scale;

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
    /// 标签页按钮位置（文本、音频、画面、快进）。
    tab_rects: [Rect4; 4],
    /// 内容区顶部 y（标签页下方）。
    #[allow(dead_code)]
    content_top: f32,
    /// X position of the left-aligned labels.
    label_x: f32,
    /// X position of the value text shown to the right of each slider.
    value_x: f32,
    /// 文本标签页：第 0 行为文本速度滑块。
    text_row_mid: f32,
    text_slider_track: Rect4,
    text_slider_hit: Rect4,
    /// 音频标签页：第 0 行 BGM，第 1 行 SFX。
    audio_row_mids: [f32; 2],
    audio_slider_tracks: [Rect4; 2],
    audio_slider_hits: [Rect4; 2],
    /// 画面标签页：第 0 行 auto_recovery，第 1 行 fullscreen，第 2 行 resolution。
    display_row_mids: [f32; 3],
    display_toggles: [Rect4; 2],
    display_dropdown: Rect4,
    /// 快进标签页：第 0 行 skip_unread，第 1 行 skip_mode 下拉。
    skip_row_mids: [f32; 2],
    skip_toggle: Rect4,
    skip_dropdown: Rect4,
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
    let title_size = 44.0 * scale;
    let title_top = 30.0 * scale;

    // 标签页（浏览器风格，位于标题下方）
    let tab_labels = ["文本", "音频", "画面", "快进"];
    let tab_h = 52.0 * scale;
    let tab_top = title_top + title_size + 20.0 * scale;
    let tab_start_x = 80.0 * scale;
    let tab_w = 140.0 * scale;
    let tab_gap = 4.0 * scale;
    let mut tab_rects = [Rect4::default(); 4];
    for (i, _) in tab_labels.iter().enumerate() {
        tab_rects[i] = Rect4 {
            x: tab_start_x + i as f32 * (tab_w + tab_gap),
            y: tab_top,
            w: tab_w,
            h: tab_h,
        };
    }

    // 内容区（标签页下方）
    let content_top = tab_top + tab_h + 30.0 * scale;
    let _content_bottom = sh - 120.0 * scale;

    let label_x = panel_x + 80.0 * scale;
    let control_x = panel_x + 320.0 * scale;
    let track_w = (panel_w - 320.0 * scale - 240.0 * scale).max(160.0 * scale);
    let value_x = control_x + track_w + 24.0 * scale;

    // 通用行高
    let row_h = 88.0 * scale;
    let track_h = 16.0 * scale;
    let toggle_w = 100.0 * scale;
    let toggle_h = 40.0 * scale;

    // 文本标签页（1 行：文本速度）
    let text_row_mid = content_top + row_h * 0.5 + row_h;
    let text_slider_track = Rect4 {
        x: control_x, y: text_row_mid - track_h / 2.0,
        w: track_w, h: track_h,
    };
    let text_slider_hit = Rect4 {
        x: control_x - 8.0 * scale, y: text_row_mid - 26.0 * scale,
        w: track_w + 16.0 * scale, h: 52.0 * scale,
    };

    // 音频标签页（2 行：BGM、SFX）
    let mut audio_row_mids = [0.0; 2];
    let mut audio_slider_tracks = [Rect4::default(); 2];
    let mut audio_slider_hits = [Rect4::default(); 2];
    for i in 0..2 {
        let mid = content_top + row_h * 0.5 + (i as f32 + 1.0) * row_h;
        audio_row_mids[i] = mid;
        audio_slider_tracks[i] = Rect4 {
            x: control_x, y: mid - track_h / 2.0,
            w: track_w, h: track_h,
        };
        audio_slider_hits[i] = Rect4 {
            x: control_x - 8.0 * scale, y: mid - 26.0 * scale,
            w: track_w + 16.0 * scale, h: 52.0 * scale,
        };
    }

    // 画面标签页（3 行：自动续播、全屏、分辨率）
    let mut display_row_mids = [0.0; 3];
    let mut display_toggles = [Rect4::default(); 2];
    for i in 0..3 {
        display_row_mids[i] = content_top + row_h * 0.5 + (i as f32 + 1.0) * row_h;
    }
    for i in 0..2 {
        let mid = display_row_mids[i];
        display_toggles[i] = Rect4 {
            x: control_x, y: mid - toggle_h / 2.0,
            w: toggle_w, h: toggle_h,
        };
    }
    let display_dropdown = Rect4 {
        x: control_x, y: display_row_mids[2] - 22.0 * scale,
        w: 260.0 * scale, h: 44.0 * scale,
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
        x: control_x, y: skip_row_mids[1] - 22.0 * scale,
        w: 260.0 * scale, h: 44.0 * scale,
    };

    // 两个按钮：应用和取消
    let btn_w = 200.0 * scale;
    let btn_h = 56.0 * scale;
    let btn_gap = 48.0 * scale;
    let total_btn_w = btn_w * 2.0 + btn_gap;
    let btn_start_x = (sw - total_btn_w) / 2.0;
    let btn_y = panel_y + panel_h - btn_h - 40.0 * scale;
    let apply_btn = Rect4 {
        x: btn_start_x, y: btn_y, w: btn_w, h: btn_h,
    };
    let cancel_btn = Rect4 {
        x: btn_start_x + btn_w + btn_gap, y: btn_y, w: btn_w, h: btn_h,
    };

    SettingsLayout {
        panel_x, panel_y, panel_w, panel_h,
        tab_rects, content_top,
        label_x, value_x,
        text_row_mid, text_slider_track, text_slider_hit,
        audio_row_mids, audio_slider_tracks, audio_slider_hits,
        display_row_mids, display_toggles, display_dropdown,
        skip_row_mids, skip_toggle, skip_dropdown,
        apply_btn, cancel_btn,
    }
}

/// Draw the interactive full-screen settings menu. Reads live values from the
/// engine so dragging a slider is reflected immediately.  The dropdown list
/// is drawn last (via `draw_dropdown_list`) so it floats above the back button.
fn draw_settings_menu(engine: &mut Engine, layout: &SettingsLayout, font: &Option<Font>, dropdown_open: bool, skip_dropdown_open: bool, active_tab: SettingsTab, scale: f32) {
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
    let title = "设置";
    let title_size = 44.0 * scale;
    let tw = measure_text_f(title, font, title_size as u16, 1.0).width;
    let title_x = (layout.panel_w - tw) / 2.0;
    let title_y = 30.0 * scale;
    draw_text_f(title, title_x, title_y + title_size, title_size, WHITE, font);

    // 标签页（浏览器风格）
    let tab_labels = ["文本", "音频", "画面", "快进"];
    let tab_tabs = [SettingsTab::Text, SettingsTab::Audio, SettingsTab::Display, SettingsTab::Skip];
    let tab_label_size = 24.0 * scale;
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
        layout.tab_rects[3].x + layout.tab_rects[3].w - layout.tab_rects[0].x, 1.5 * scale,
        Color::new(0.45, 0.7, 0.95, 0.6));

    let settings = engine.settings();
    let label_size = 28.0 * scale;
    let value_size = 26.0 * scale;

    match active_tab {
        SettingsTab::Text => {
            // 文本速度
            draw_text_f("文本速度", layout.label_x, layout.text_row_mid + 8.0 * scale, label_size, WHITE, font);
            draw_slider_track(layout.text_slider_track, settings.text_speed / 999.0, scale);
            draw_text_f(
                &format!("{:.0} 字/秒", settings.text_speed),
                layout.value_x,
                layout.text_row_mid + 8.0 * scale,
                value_size,
                WHITE,
                font,
            );
        }
        SettingsTab::Audio => {
            // BGM 音量
            draw_text_f("BGM 音量", layout.label_x, layout.audio_row_mids[0] + 8.0 * scale, label_size, WHITE, font);
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
            draw_text_f("音效音量", layout.label_x, layout.audio_row_mids[1] + 8.0 * scale, label_size, WHITE, font);
            draw_slider_track(layout.audio_slider_tracks[1], settings.sfx_volume, scale);
            draw_text_f(
                &format!("{:.0}%", settings.sfx_volume * 100.0),
                layout.value_x,
                layout.audio_row_mids[1] + 8.0 * scale,
                value_size,
                WHITE,
                font,
            );
        }
        SettingsTab::Display => {
            // 自动恢复
            draw_text_f("自动恢复", layout.label_x, layout.display_row_mids[0] + 8.0 * scale, label_size, WHITE, font);
            draw_toggle(layout.display_toggles[0], settings.auto_recovery, font, scale);
            // 全屏模式
            draw_text_f("全屏模式", layout.label_x, layout.display_row_mids[1] + 8.0 * scale, label_size, WHITE, font);
            draw_toggle(layout.display_toggles[1], settings.fullscreen, font, scale);
            // 分辨率下拉
            draw_text_f("分辨率", layout.label_x, layout.display_row_mids[2] + 8.0 * scale, label_size, WHITE, font);
            draw_dropdown_box_resolution(layout.display_dropdown, settings.resolution, font, dropdown_open, scale);
        }
        SettingsTab::Skip => {
            // 允许跳过未读文本
            draw_text_f("允许跳过未读文本", layout.label_x, layout.skip_row_mids[0] + 8.0 * scale, label_size, WHITE, font);
            draw_toggle(layout.skip_toggle, settings.skip_unread, font, scale);
            // 快进模式下拉
            draw_text_f("快进模式", layout.label_x, layout.skip_row_mids[1] + 8.0 * scale, label_size, WHITE, font);
            draw_dropdown_box_skip_mode(layout.skip_dropdown, settings.skip_mode, font, skip_dropdown_open, scale);
        }
    }

    // 两个按钮：应用和取消（视觉绘制，点击由 handle_settings_interaction 处理）
    let mut btns = Vec::new();
    draw_button(
        layout.apply_btn.x,
        layout.apply_btn.y,
        layout.apply_btn.w,
        layout.apply_btn.h,
        "应用",
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
        "取消",
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
    if skip_dropdown_open && active_tab == SettingsTab::Skip {
        draw_dropdown_list_skip_mode(layout.skip_dropdown, settings.skip_mode, font, scale);
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

/// 快进模式下拉菜单的折叠框。
fn draw_dropdown_box_skip_mode(r: Rect4, mode: SkipMode, font: &Option<Font>, open: bool, scale: f32) {
    draw_rectangle(r.x, r.y, r.w, r.h, Color::new(0.15, 0.25, 0.45, 0.9));
    draw_rectangle_lines(r.x, r.y, r.w, r.h, 1.5 * scale, Color::new(0.45, 0.7, 0.95, 0.8));
    let label = match mode {
        SkipMode::TextOnly => "仅显示文本",
        SkipMode::WithVoice => "包含语音",
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
fn draw_dropdown_list_skip_mode(r: Rect4, mode: SkipMode, font: &Option<Font>, scale: f32) {
    let item_h = 32.0 * scale;
    let options = [SkipMode::TextOnly, SkipMode::WithVoice];
    let labels = ["仅显示文本", "包含语音"];
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
    active_tab: &mut SettingsTab,
    scale: f32,
    settings_snapshot: &Option<Settings>,
    _settings_prev_mode: &mut UiMode,
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
            (SettingsTab::Audio, 0) => Some(layout.audio_slider_tracks[0]),
            (SettingsTab::Audio, 1) => Some(layout.audio_slider_tracks[1]),
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
        // 先检查标签页点击
        let tab_tabs = [SettingsTab::Text, SettingsTab::Audio, SettingsTab::Display, SettingsTab::Skip];
        for (i, tab) in tab_tabs.iter().enumerate() {
            if point_in_rect(mx, my, layout.tab_rects[i]) {
                *active_tab = *tab;
                *dropdown_open = false;
                *skip_dropdown_open = false;
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
            }
            SettingsTab::Audio => {
                // BGM、SFX 滑块
                for i in 0..2 {
                    if point_in_rect(mx, my, layout.audio_slider_hits[i]) {
                        *dragging_slider = Some((SettingsTab::Audio, i));
                        update_slider_value(engine, SettingsTab::Audio, i, mx, layout.audio_slider_tracks[i]);
                        return None;
                    }
                }
            }
            SettingsTab::Display => {
                // 开关
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
                    || current.auto_recovery != snapshot.auto_recovery
                    || current.fullscreen != snapshot.fullscreen
                    || current.resolution != snapshot.resolution
                    || current.skip_unread != snapshot.skip_unread
                    || current.skip_mode != snapshot.skip_mode
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
        (SettingsTab::Audio, 0) => settings.bgm_volume = t,
        (SettingsTab::Audio, 1) => settings.sfx_volume = t,
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
        | ButtonAction::PrevPage | ButtonAction::NextPage
        | ButtonAction::AddPage => None,
        ButtonAction::ConfirmYes => Some((UiMode::Normal, PendingUiAction::None)), // 由调用者处理具体确认逻辑
        ButtonAction::ConfirmNo => Some((UiMode::Normal, PendingUiAction::None)),  // 返回上一个模式
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

    let bg_color = if hover {
        Color::new(0.45, 0.7, 0.95, 0.95)
    } else {
        Color::new(0.3, 0.55, 0.85, 0.9)
    };
    draw_rectangle(x, y, w, h, bg_color);
    draw_rectangle_lines(x, y, w, h, 2.0 * scale,
        if hover { Color::new(0.7, 0.88, 1.0, 1.0) } else { Color::new(0.5, 0.75, 1.0, 0.8) });

    // Font size scales with the button height, capped to keep labels legible.
    let font_size = (h * 0.4).min(28.0 * scale);
    let tw = measure_text_f(label, font, font_size as u16, 1.0).width;
    draw_text_f(
        label,
        x + (w - tw) / 2.0,
        y + h / 2.0 + font_size / 3.0,
        font_size,
        WHITE,
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
const HUD_BTN_COUNT: usize = 8;
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
    buttons: &mut Vec<ButtonRect>,
    sw: f32,
    sh: f32,
    font: &Option<Font>,
    scale: f32,
    visibility: &HudVisibility,
    skip_active: bool,
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

    let hud_buttons: [(&str, ButtonAction); 8] = [
        ("快进", ButtonAction::FastForward),
        ("快存", ButtonAction::QuickSave),
        ("快读", ButtonAction::QuickLoad),
        ("存档", ButtonAction::OpenSaveMenu),
        ("读档", ButtonAction::OpenLoadMenu),
        ("标题", ButtonAction::BackToTitle),
        ("设置", ButtonAction::OpenSettings),
        ("隐藏", ButtonAction::ToggleHide),
    ];

    let mut x = start_x;
    for (label, action) in &hud_buttons {
        let is_active = match action {
            ButtonAction::FastForward => skip_active,
            _ => false,
        };
        draw_small_button(x, start_y, btn_w, btn_h, label, buttons, *action, font, scale, alpha, is_active);
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
) {
    let (mx, my) = mouse_position();
    let hover = mx >= x && mx <= x + w && my >= y && my <= y + h;
    let pressed = hover && is_mouse_button_down(MouseButton::Left);

    // 三态颜色（天蓝色主题），激活态（如快进中）使用更亮的颜色
    let (bg_r, bg_g, bg_b, bg_a_base, border_r, border_g, border_b, border_a_base, scale_factor, dy) = if pressed {
        // 按下：颜色加深、按压下移 1px、轻微缩小
        (0.2, 0.45, 0.75, 0.95, 0.45, 0.7, 0.95, 1.0, 0.96, 1.0 * scale)
    } else if hover || active {
        // 悬停或激活：高亮、放大
        (0.35, 0.6, 0.9, 0.92, 0.6, 0.82, 1.0, 1.0, 1.06, 0.0)
    } else {
        // 默认：低亮度半透明
        (0.2, 0.4, 0.7, 0.70, 0.4, 0.6, 0.85, 0.55, 1.00, 0.0)
    };

    // 按缩放因子调整绘制尺寸（以中心为基准）
    let draw_w = w * scale_factor;
    let draw_h = h * scale_factor;
    let draw_x = x + (w - draw_w) / 2.0;
    let draw_y = y + (h - draw_h) / 2.0 + dy;

    let bg_color = Color::new(bg_r, bg_g, bg_b, bg_a_base * alpha);
    draw_rectangle(draw_x, draw_y, draw_w, draw_h, bg_color);
    draw_rectangle_lines(
        draw_x, draw_y, draw_w, draw_h,
        1.5 * scale,
        Color::new(border_r, border_g, border_b, border_a_base * alpha),
    );

    let font_size = 20.0 * scale;
    let tw = measure_text_f(label, font, font_size as u16, 1.0).width;
    // 文字透明度随 alpha 走；悬停/按下时略提亮
    let text_alpha = if pressed { 0.95 } else if hover { 1.0 } else { 0.9 } * alpha;
    let text_color = Color::new(1.0, 1.0, 1.0, text_alpha);
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
