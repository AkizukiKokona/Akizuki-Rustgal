//! 文本后端：基于 [cosmic-text](https://crates.io/crates/cosmic-text) 0.18 的
//! `Buffer` / `Layout` 引擎。
//!
//! 提供与 macroquad `text` 模块同名的 API（`Font` / `TextParams` / `draw_text` /
//! `draw_text_ex` / `measure_text` / `load_ttf_font_from_bytes` / `TextDimensions`），
//! 使 `renderer.rs` 仅需换 import 即可迁移。
//!
//! ## 实现
//!
//! - 用 `cosmic_text::FontSystem` 加载/管理字体（含系统字体扫描，作为默认字体来源）。
//! - **真正使用 `Buffer`/`Layout`**：`draw_text_ex` / `measure_text` 内部复用一个
//!   `cosmic_text::Buffer`，调用 `set_text` + `Shaping::Advanced` + `Metrics` 触发
//!   harfrust shaping、bidi 重排、kerning 与字符级字体回退；随后遍历
//!   `buffer.layout_runs()` 的 glyphs，**直接取 Layout 给出的 `glyph.x` / `line_y`
//!   作为字形位置**，不再手动累加 advance。
//! - 每个 layout glyph 经 `glyph.physical(...)` 得到 `PhysicalGlyph`（含 `cache_key`
//!   与整数像素坐标），用 `cosmic_text::SwashCache::get_image_uncached` 光栅化为
//!   8-bit alpha 掩码或彩色图，展开为 RGBA 后经 `wgpu_backend::create_texture`
//!   上传为字形纹理，并按 `CacheKey`（含子像素 bin）缓存，命中即复用。
//! - 换行：`Buffer` 设置 `width_opt = None`，即不自动换行（单行布局）。
//!   `renderer.rs` 中的 `wrap_text_cn` / `draw_text_wrapped` 仍按字符/词手动拆分，
//!   但其每次测量/绘制都走本模块的 Layout 后端，因而每行均享受 shaping/回退。
//!
//! ## 坐标系
//!
//! 全部在逻辑像素（与本渲染后端其余 API 一致）。`draw_text(x, y, …)` 的 `y` 为基线，
//! 与 macroquad 语义一致：文本顶部位于 `y - offset_y`，`offset_y = ascent`。

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

use cosmic_text::fontdb;
use cosmic_text::{
    Attrs, Buffer, CacheKey, Family, FontSystem, Metrics, Shaping, SwashCache, SwashContent,
    Weight,
};

use crate::wgpu_backend::{self, Color, DrawTextureParams, Texture2D, Vec2, WHITE};

// ─── 公共类型（与 macroquad 同名） ──────────────────────────────────────────

/// 字体句柄，与 macroquad `Font` 同名。`Font(0)` 为默认字体（系统字体）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Font(pub usize);

impl Default for Font {
    fn default() -> Font {
        Font(0)
    }
}

/// 字体加载错误，与 macroquad `FontError` 同名。
#[derive(Debug, Clone)]
pub struct FontError(pub String);

impl std::fmt::Display for FontError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "font error: {}", self.0)
    }
}
impl std::error::Error for FontError {}

/// `draw_text_ex` 的参数，与 macroquad `TextParams` 同名同字段。
#[derive(Debug, Clone, Copy)]
pub struct TextParams {
    pub font: Font,
    pub font_size: u16,
    pub font_scale: f32,
    pub font_scale_aspect: f32,
    pub rotation: f32,
    pub color: Color,
}

impl Default for TextParams {
    fn default() -> TextParams {
        TextParams {
            font: Font::default(),
            font_size: 20,
            font_scale: 1.0,
            font_scale_aspect: 1.0,
            color: WHITE,
            rotation: 0.0,
        }
    }
}

/// 文本尺寸，与 macroquad `TextDimensions` 同字段。
/// `draw_text(X, Y, …)` 渲染于 `Rect::new(X, Y - offset_y, width, height)`。
#[derive(Debug, Clone, Copy, Default)]
pub struct TextDimensions {
    pub width: f32,
    pub height: f32,
    pub offset_y: f32,
}

// ─── 内部状态 ───────────────────────────────────────────────────────────────

/// 一个已加载字体的内部句柄。
#[derive(Clone)]
struct FontInner {
    id: fontdb::ID,
    font: Arc<cosmic_text::Font>,
    /// 字体主家族名，用于在 `Buffer::set_text` 时通过 `Family::Name` 指定主字体，
    /// 让 cosmic-text 优先使用该字体并对其缺失字形做字符级回退。
    family: String,
}

/// 一个已缓存字形（按 cosmic-text `CacheKey` 索引，含子像素 bin）。
#[derive(Clone)]
struct CachedGlyph {
    /// 字形纹理；`None` 表示空白字形（如空格），仅有前进宽度，无需绘制。
    texture: Option<Texture2D>,
    /// `placement.left`（字形左边距，相对笔位置）。
    left: i32,
    /// `placement.top`（字形顶部相对基线的高度，正值=在基线上方）。
    top: i32,
    /// `placement.width`。
    w: u32,
    /// `placement.height`。
    h: u32,
}

impl CachedGlyph {
    const fn blank() -> Self {
        CachedGlyph { texture: None, left: 0, top: 0, w: 0, h: 0 }
    }
}

struct TextState {
    font_system: FontSystem,
    swash_cache: SwashCache,
    /// 复用的布局缓冲区（每帧 `set_text` 重置），避免反复分配。
    buffer: Buffer,
    /// 索引 = `Font.0`；`None` 表示该槽位未加载（如默认字体无系统字体可用）。
    fonts: Vec<Option<FontInner>>,
    /// 字形纹理缓存：`CacheKey`（font_id / glyph_id / size / 子像素 bin / weight / flags）。
    glyph_cache: HashMap<CacheKey, CachedGlyph>,
}

thread_local! {
    static TEXT: RefCell<Option<TextState>> = const { RefCell::new(None) };
}

/// 懒初始化文本后端。首次调用任意文本 API 时自动触发；也可由 `renderer::run` 显式调用。
pub fn init_text() {
    TEXT.with(|t| {
        if t.borrow().is_some() {
            return;
        }
        let mut font_system = FontSystem::new();
        // 默认字体：取首个系统字体（若有）；否则 Font(0) 为 None（绘制/测量为空）。
        // 注意：必须先提取 id（释放 db() 的不可变借用），才能调用 get_font（可变借用）。
        let default_id = font_system.db().faces().next().map(|info| info.id);
        let default = default_id.and_then(|id| {
            let family = font_system
                .db()
                .face(id)
                .and_then(|f| f.families.first().map(|(n, _)| n.clone()))
                .unwrap_or_default();
            font_system
                .get_font(id, Weight::NORMAL)
                .map(|font| FontInner { id, font, family })
        });
        *t.borrow_mut() = Some(TextState {
            font_system,
            swash_cache: SwashCache::new(),
            buffer: Buffer::new_empty(Metrics::new(20.0, 24.0)),
            fonts: vec![default],
            glyph_cache: HashMap::new(),
        });
    });
}

fn with_state<R>(f: impl FnOnce(&mut TextState) -> R) -> R {
    init_text();
    TEXT.with(|t| {
        let mut g = t.borrow_mut();
        let st = g.as_mut().expect("text backend not initialized");
        f(st)
    })
}

/// 取字体内部句柄；越界或未加载返回 None。
fn font_inner(st: &TextState, font: Font) -> Option<&FontInner> {
    st.fonts.get(font.0).and_then(|o| o.as_ref())
}

/// 字体在某字号下的 ascent（像素，基线到顶部）。
fn ascent_px(st: &TextState, font: Font, size: u16) -> f32 {
    let Some(fi) = font_inner(st, font) else {
        return size as f32 * 0.8;
    };
    let m = fi.font.as_swash().metrics(&[]);
    if (m.units_per_em as f32) > 0.0 {
        m.ascent / (m.units_per_em as f32) * size as f32
    } else {
        size as f32 * 0.8
    }
}

/// 字体在某字号下的 descent（像素，通常为负）。
fn descent_px(st: &TextState, font: Font, size: u16) -> f32 {
    let Some(fi) = font_inner(st, font) else {
        return -(size as f32) * 0.2;
    };
    let m = fi.font.as_swash().metrics(&[]);
    if (m.units_per_em as f32) > 0.0 {
        m.descent / (m.units_per_em as f32) * size as f32
    } else {
        -(size as f32) * 0.2
    }
}

/// 取字体的主家族名（用于 `Attrs::family(Family::Name(..))`）。未加载返回 None。
fn family_name(st: &TextState, font: Font) -> Option<String> {
    font_inner(st, font).map(|fi| fi.family.clone())
}

/// 构造指定字体的 `Attrs`：已加载则绑定其家族名（让 cosmic-text 优先命中并启用字符级回退），
/// 否则用默认（Sans-Serif，由 cosmic-text 自行匹配系统字体）。
fn attrs_for(family: &Option<String>) -> Attrs<'_> {
    let mut attrs = Attrs::new();
    if let Some(name) = family {
        attrs = attrs.family(Family::Name(name));
    }
    attrs
}

/// 确保字形纹理已缓存；返回其克隆。未命中的字形会现场光栅化并上传纹理。
/// 位置由 Layout 给出的 `PhysicalGlyph` 决定，本函数只负责光栅化与纹理缓存。
fn ensure_glyph(st: &mut TextState, key: CacheKey) -> CachedGlyph {
    if let Some(cg) = st.glyph_cache.get(&key) {
        return cg.clone();
    }
    // 光栅化：disjoint borrow of font_system / swash_cache / glyph_cache。
    let TextState {
        font_system,
        swash_cache,
        glyph_cache,
        ..
    } = st;
    let image = swash_cache.get_image_uncached(font_system, key);
    let cg = match image {
        None => CachedGlyph::blank(),
        Some(img) => {
            let p = img.placement;
            if p.width == 0 || p.height == 0 {
                CachedGlyph { texture: None, left: p.left, top: p.top, w: 0, h: 0 }
            } else {
                // 8-bit alpha 掩码 → RGBA（白色字形着色，最终由 tint 调色）。
                let n = (p.width as usize) * (p.height as usize);
                let mut rgba = Vec::with_capacity(n * 4);
                match img.content {
                    SwashContent::Mask => {
                        for &a in &img.data[..n] {
                            rgba.extend_from_slice(&[255, 255, 255, a]);
                        }
                    }
                    SwashContent::Color => {
                        rgba.extend_from_slice(&img.data[..n * 4]);
                    }
                    SwashContent::SubpixelMask => {
                        // 子像素掩码罕见（彩色液晶）；退化为全不透明白块近似。
                        for _ in 0..n {
                            rgba.extend_from_slice(&[255, 255, 255, 255]);
                        }
                    }
                }
                let tex = wgpu_backend::create_texture(p.width, p.height, &rgba);
                CachedGlyph {
                    texture: Some(tex),
                    left: p.left,
                    top: p.top,
                    w: p.width,
                    h: p.height,
                }
            }
        }
    };
    glyph_cache.insert(key, cg.clone());
    cg
}

// ─── 公共 API（与 macroquad 同名） ──────────────────────────────────────────

/// 从字节加载 TTF 字体，返回新 `Font` 句柄。
pub fn load_ttf_font_from_bytes(bytes: &[u8]) -> Result<Font, FontError> {
    with_state(|st| {
        let source = fontdb::Source::Binary(Arc::new(bytes.to_vec()));
        let ids = st.font_system.db_mut().load_font_source(source);
        if ids.is_empty() {
            return Err(FontError("no faces found in font data".into()));
        }
        let id = ids[0];
        let family = st
            .font_system
            .db()
            .face(id)
            .and_then(|f| f.families.first().map(|(n, _)| n.clone()))
            .unwrap_or_default();
        let font = st
            .font_system
            .get_font(id, Weight::NORMAL)
            .ok_or_else(|| FontError("failed to instantiate font face".into()))?;
        st.fonts.push(Some(FontInner { id, font, family }));
        Ok(Font(st.fonts.len() - 1))
    })
}

/// 测量文本尺寸。`font_scale` 同时作用于 x、y（与 macroquad `measure_text` 一致）。
///
/// 尺寸取自 `Buffer` 的 layout：宽度取各 layout run 的 `line_w` 最大值，
/// 高度取各 run `line_height` 之和，`offset_y` 取首行 baseline（≈ ascent）。
pub fn measure_text(text: &str, font: Option<Font>, font_size: u16, font_scale: f32) -> TextDimensions {
    let font = font.unwrap_or_default();
    with_state(|st| {
        if font_inner(st, font).is_none() || font_size == 0 {
            return TextDimensions::default();
        }
        let family = family_name(st, font);
        let line_h = ascent_px(st, font, font_size) - descent_px(st, font, font_size);
        let metrics = Metrics::new(font_size as f32, line_h.max(1.0));
        let attrs = attrs_for(&family);

        // width_opt = None → 不自动换行，测量自然宽度。
        st.buffer
            .set_metrics_and_size(&mut st.font_system, metrics, None, None);
        st.buffer
            .set_text(&mut st.font_system, text, &attrs, Shaping::Advanced, None);

        let mut width = 0.0f32;
        let mut offset_y = 0.0f32;
        let mut total_height = 0.0f32;
        let mut first = true;
        for run in st.buffer.layout_runs() {
            if run.line_w > width {
                width = run.line_w;
            }
            if first {
                offset_y = run.line_y;
                first = false;
            }
            total_height += run.line_height;
        }
        if first {
            // 无 run（极端空情形）：退回字体度量。
            offset_y = ascent_px(st, font, font_size);
            total_height = line_h;
        }
        TextDimensions {
            width: width * font_scale,
            height: total_height * font_scale,
            offset_y: offset_y * font_scale,
        }
    })
}

/// 用默认字体绘制文本（字号以浮点传入，与 macroquad 一致）。
pub fn draw_text(text: &str, x: f32, y: f32, font_size: f32, color: Color) {
    draw_text_ex(
        text,
        x,
        y,
        TextParams {
            font_size: font_size as u16,
            font_scale: 1.0,
            color,
            ..Default::default()
        },
    );
}

/// 用自定义参数绘制文本。`y` 为基线；`font_scale`/`font_scale_aspect` 缩放字形。
///
/// 实现走 cosmic-text `Buffer`：`set_text`（`Shaping::Advanced`）触发 harfrust
/// shaping / bidi / kerning / 字符级字体回退，随后遍历 `layout_runs()` 的 glyphs，
/// 用 `glyph.physical((0, line_y), 1.0)` 取 `CacheKey` 与整数像素坐标光栅化并绘制。
/// `rotation` 暂未实现（保留字段以兼容 API），文本始终水平绘制。
pub fn draw_text_ex(text: &str, x: f32, y: f32, params: TextParams) {
    let font = params.font;
    let scale_x = params.font_scale * params.font_scale_aspect;
    let scale_y = params.font_scale;
    let size = params.font_size;

    // 先在 TEXT 借用内收集所有绘制指令（含字形纹理克隆），再释放借用后逐一绘制，
    // 避免在持有 TEXT 借用时回调 BACKEND（虽为不同 thread_local，但保持清晰）。
    let cmds: Vec<(Texture2D, f32, f32, f32, f32)> = with_state(|st| {
        let mut out = Vec::new();
        if font_inner(st, font).is_none() || size == 0 {
            return out;
        }
        let family = family_name(st, font);
        let line_h = ascent_px(st, font, size) - descent_px(st, font, size);
        let metrics = Metrics::new(size as f32, line_h.max(1.0));
        let attrs = attrs_for(&family);

        // width_opt = None → 不自动换行（单行布局，与原逐字符实现语义一致）。
        st.buffer
            .set_metrics_and_size(&mut st.font_system, metrics, None, None);
        st.buffer
            .set_text(&mut st.font_system, text, &attrs, Shaping::Advanced, None);

        // 收集每个 layout glyph 的 CacheKey 与 buffer 内整数像素坐标。
        // 首行 baseline（run.line_y）作为屏幕 y 的锚点，保证首行基线落在用户 y。
        let mut baseline_ref = 0.0f32;
        let mut first = true;
        let mut glyph_data: Vec<(CacheKey, i32, i32)> = Vec::new();
        for run in st.buffer.layout_runs() {
            if first {
                baseline_ref = run.line_y;
                first = false;
            }
            for g in run.glyphs {
                let pg = g.physical((0.0, run.line_y), 1.0);
                glyph_data.push((pg.cache_key, pg.x, pg.y));
            }
        }

        // 光栅化（命中纹理缓存即复用）并生成四边形指令。
        // buffer→屏幕映射：screen_x = x + buf_x*scale_x；
        // screen_y = y + (buf_y - baseline_ref)*scale_y，使首行基线落在 y。
        for (key, gx, gy) in glyph_data {
            let cg = ensure_glyph(st, key);
            if let Some(tex) = cg.texture.clone() {
                let buf_x = (gx + cg.left) as f32;
                let buf_y = (gy - cg.top) as f32;
                let dx = x + buf_x * scale_x;
                let dy = y + (buf_y - baseline_ref) * scale_y;
                let dw = cg.w as f32 * scale_x;
                let dh = cg.h as f32 * scale_y;
                out.push((tex, dx, dy, dw, dh));
            }
        }
        out
    });

    for (tex, dx, dy, dw, dh) in cmds {
        wgpu_backend::draw_texture_ex(
            tex,
            dx,
            dy,
            params.color,
            DrawTextureParams {
                dest_size: Some(Vec2::new(dw, dh)),
                ..Default::default()
            },
        );
    }
}
