//! 文本后端（任务2 内置重做）：基于 [cosmic-text](https://crates.io/crates/cosmic-text) 0.18。
//!
//! 提供与 macroquad `text` 模块同名的 API（`Font` / `TextParams` / `draw_text` /
//! `draw_text_ex` / `measure_text` / `load_ttf_font_from_bytes` / `TextDimensions`），
//! 使 `renderer.rs` 仅需换 import 即可迁移。
//!
//! ## 实现
//!
//! - 用 `cosmic_text::FontSystem` 加载/管理字体（含系统字体扫描，作为默认字体来源）。
//! - 用 `cosmic_text::Font::as_swash()` 取得 `swash::FontRef`，通过其 `charmap` 取字形 id、
//!   `glyph_metrics` 取前进宽度。
//! - 用 `cosmic_text::SwashCache::get_image_uncached` 光栅化单字形为 8-bit alpha 掩码，
//!   展开为 RGBA 后经 `wgpu_backend::create_texture` 上传为字形纹理。
//! - 字形纹理按 `(字体索引, 字符, 字号)` 缓存，命中即复用，避免重复光栅化。
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
    CacheKey, CacheKeyFlags, FontSystem, SwashCache, SwashContent, Weight,
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
}

/// 一个已缓存字形。
struct CachedGlyph {
    /// 字形纹理；`None` 表示空白字形（如空格），仅有前进宽度。
    texture: Option<Texture2D>,
    /// placement.left（字形左边距，相对笔位置）。
    left: f32,
    /// placement.top（字形顶部相对基线的高度，正值=在基线上方）。
    top: f32,
    /// placement.width。
    w: f32,
    /// placement.height。
    h: f32,
    /// 前进宽度（像素）。
    advance: f32,
}

struct TextState {
    font_system: FontSystem,
    swash_cache: SwashCache,
    /// 索引 = `Font.0`；`None` 表示该槽位未加载（如默认字体无系统字体可用）。
    fonts: Vec<Option<FontInner>>,
    glyph_cache: HashMap<(usize, char, u16), CachedGlyph>,
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
        // 注意：必须先提取 id 释放 db() 的不可变借用，才能调用 get_font（可变借用）。
        let default_id = font_system.db().faces().next().map(|info| info.id);
        let default = default_id.and_then(|id| {
            font_system
                .get_font(id, Weight::NORMAL)
                .map(|font| FontInner { id, font })
        });
        *t.borrow_mut() = Some(TextState {
            font_system,
            swash_cache: SwashCache::new(),
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

/// 确保字形已缓存；返回其引用。未命中的字形会现场光栅化并上传纹理。
fn ensure_glyph<'a>(st: &'a mut TextState, font: Font, ch: char, size: u16) -> Option<&'a CachedGlyph> {
    // 先检查缓存命中（避免无谓借用冲突）。
    if st.glyph_cache.contains_key(&(font.0, ch, size)) {
        return st.glyph_cache.get(&(font.0, ch, size));
    }

    let fi = font_inner(st, font)?.clone(); // clone Arc<Font>
    let sw = fi.font.as_swash();
    let gid = sw.charmap().map(ch as u32);
    if gid == 0 {
        // 缺字形：缓存一个空白项（advance=0），避免重复查找。
        st.glyph_cache.insert(
            (font.0, ch, size),
            CachedGlyph { texture: None, left: 0.0, top: 0.0, w: 0.0, h: 0.0, advance: 0.0 },
        );
        return st.glyph_cache.get(&(font.0, ch, size));
    }
    let advance = sw.glyph_metrics(&[]).scale(size as f32).advance_width(gid);

    // 光栅化：disjoint borrow of font_system / swash_cache。
    let (key, _, _) = CacheKey::new(fi.id, gid, size as f32, (0.0, 0.0), Weight::NORMAL, CacheKeyFlags::empty());
    let TextState { font_system, swash_cache, .. } = st;
    let image = swash_cache.get_image_uncached(font_system, key);
    let glyph = match image {
        None => CachedGlyph { texture: None, left: 0.0, top: 0.0, w: 0.0, h: 0.0, advance },
        Some(img) => {
            let p = img.placement;
            let w = p.width;
            let h = p.height;
            if w == 0 || h == 0 {
                CachedGlyph { texture: None, left: p.left as f32, top: p.top as f32, w: 0.0, h: 0.0, advance }
            } else {
                // 8-bit alpha 掩码 → RGBA（白色字形着色，最终由 tint 调色）。
                let n = (w as usize) * (h as usize);
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
                let tex = wgpu_backend::create_texture(w, h, &rgba);
                CachedGlyph {
                    texture: Some(tex),
                    left: p.left as f32,
                    top: p.top as f32,
                    w: w as f32,
                    h: h as f32,
                    advance,
                }
            }
        }
    };
    st.glyph_cache.insert((font.0, ch, size), glyph);
    st.glyph_cache.get(&(font.0, ch, size))
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
        let font = st
            .font_system
            .get_font(id, Weight::NORMAL)
            .ok_or_else(|| FontError("failed to instantiate font face".into()))?;
        st.fonts.push(Some(FontInner { id, font }));
        Ok(Font(st.fonts.len() - 1))
    })
}

/// 测量文本尺寸。`font_scale` 同时作用于 x、y（与 macroquad `measure_text` 一致）。
pub fn measure_text(text: &str, font: Option<Font>, font_size: u16, font_scale: f32) -> TextDimensions {
    let font = font.unwrap_or_default();
    with_state(|st| {
        if font_inner(st, font).is_none() {
            return TextDimensions::default();
        }
        let ascent = ascent_px(st, font, font_size);
        let descent = descent_px(st, font, font_size);
        let line_height = ascent - descent;
        let mut width = 0.0f32;
        for ch in text.chars() {
            if let Some(g) = ensure_glyph(st, font, ch, font_size) {
                width += g.advance;
            }
        }
        TextDimensions {
            width: width * font_scale,
            height: line_height * font_scale,
            offset_y: ascent * font_scale,
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
        if font_inner(st, font).is_none() {
            return out;
        }
        let mut pen = 0.0f32;
        for ch in text.chars() {
            if let Some(g) = ensure_glyph(st, font, ch, size) {
                if let Some(tex) = g.texture.clone() {
                    let dx = x + (pen + g.left) * scale_x;
                    let dy = y - g.top * scale_y;
                    let dw = g.w * scale_x;
                    let dh = g.h * scale_y;
                    out.push((tex, dx, dy, dw, dh));
                }
                pen += g.advance;
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
