//! 游戏内自写 UI 的可复用立即模式组件（任务4 最后一项）。
//!
//! 保留立即模式架构（每帧重画、控件函数既绘制又处理输入、用 id + 共享 state
//! 跨帧记忆交互状态），把 `renderer.rs` 中散落于 `draw_settings_menu` /
//! `handle_settings_interaction` / `draw_note_edit_dialog` / `draw_color_tab`
//! 的手写滑块 / 下拉框 / 文本输入控件提炼为三个统一组件：
//!
//! - [`draw_slider`]：水平滑块，绘制轨道+填充+旋钮并处理拖拽，返回（可能更新后的）值。
//! - [`draw_dropdown`]：下拉框，绘制折叠框+（若展开）选项列表并处理开合与选择。
//! - [`draw_text_input`]：文本输入框，绘制框+文本+光标+IME preedit，收集字符 /
//!   Backspace / Enter / IME commit，返回 [`TextInputEvent`]。
//!
//! 跨控件状态集中存于 [`UiWidgetsState`]：正在拖拽的滑块 id、展开的下拉 id、
//! 聚焦的文本输入 id、本帧点击是否已被消费、当前是否可交互。原 `renderer.rs`
//! 中 4 个下拉 bool（`dropdown_open` / `skip_dropdown_open` /
//! `ui_lang_dropdown_open` / `lang_dropdown_open`）合并为
//! `open_dropdown: Option<DropdownId>`；`dragging_slider` 由
//! `Option<(SettingsTab, usize)>` 改为 `Option<SliderId>`。
//!
//! 组件复用 `renderer.rs` 的私有文本助手（`draw_text_f` / `measure_text_f` /
//! `fit_text`，已提升为 `pub(crate)`）与 `Rect4`，保持与原手写控件一致的视觉。

use crate::renderer::{fit_text, measure_text_f, draw_text_f, Rect4};
use crate::wgpu_backend::prelude::*;

// ─── 标识与共享状态 ──────────────────────────────────────────────────────────

/// 设置菜单中所有滑块的唯一标识。用于 [`UiWidgetsState::dragging_slider`]
/// 区分「当前正在拖拽哪一个滑块」。取代原 `Option<(SettingsTab, usize)>`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SliderId {
    /// 文本标签页：文本速度（0..999 字/秒）。
    TextSpeed,
    /// 文本标签页：有语音自动播放间隔（0..5 秒）。
    AutoPlayDelayWithVoice,
    /// 文本标签页：无语音自动播放间隔（0..5 秒）。
    AutoPlayDelayWithoutVoice,
    /// 音频标签页：BGM 音量（0..1）。
    BgmVolume,
    /// 音频标签页：音效音量（0..1）。
    SfxVolume,
    /// 音频标签页：语音音量（0..1）。
    VoiceVolume,
}

/// 设置菜单中所有下拉框的唯一标识。用于 [`UiWidgetsState::open_dropdown`]
/// 区分「当前展开哪一个下拉」（同一时间至多一个展开）。取代原 4 个下拉 bool。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DropdownId {
    /// 画面标签页：分辨率。
    Resolution,
    /// 画面标签页：UI 语言。
    UiLanguage,
    /// 画面标签页：剧本语言。
    ScriptLanguage,
    /// 快进标签页：快进模式。
    SkipMode,
}

/// 文本输入框的唯一标识。`ColorHex` 携带配色字段行索引（0..5，
/// 对应 renderer 中 `ColorField::index()`），使配色页的 5 个 hex 框各自独立聚焦。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TextInputId {
    /// 备注编辑弹窗的输入框。
    NoteEdit,
    /// 配色标签页第 `usize` 行的十六进制输入框。
    ColorHex(usize),
}

/// 立即模式 UI 组件共享状态：跨控件记忆「正在拖拽的滑块 / 展开的下拉 /
/// 聚焦的文本输入 / 本帧是否已有点击被某控件消费 / 当前是否可交互」。
///
/// 由 `renderer::run()` 在主循环中持有一个实例，每帧传给各组件函数。
#[derive(Debug, Default)]
pub(crate) struct UiWidgetsState {
    /// 正在拖拽的滑块 id；`None` 表示无拖拽。
    pub dragging_slider: Option<SliderId>,
    /// 当前展开的下拉 id；`None` 表示全部折叠。同一时间至多一个展开
    /// （设为 `Some(id)` 即隐式关闭其它下拉）。
    pub open_dropdown: Option<DropdownId>,
    /// 当前聚焦的文本输入 id；`None` 表示无文本输入聚焦（此时 IME 应关闭）。
    /// 由 `renderer` 每帧根据当前模式 / `color_edit_active` 重新设定。
    pub focused_text: Option<TextInputId>,
    /// 本帧是否已有控件消费了鼠标按下事件。立即模式中控件按调用顺序处理，
    /// 先命中者置 `true`，后续控件据此跳过点击判定，复刻原集中式 handler
    /// 「命中即 `return`」的语义（避免同一次点击被多个控件响应）。
    pub click_consumed: bool,
    /// 当前控件是否可交互。当设置菜单作为确认对话框的底层背景绘制时为 `false`，
    /// 控件只绘制不响应输入，避免与上层对话框争抢同一帧的鼠标事件。
    pub interactable: bool,
}

impl UiWidgetsState {
    /// 每帧开头重置瞬态标志（聚焦 / 点击消费）。`dragging_slider` 与
    /// `open_dropdown` 为持续状态，不在此重置——由各自的释放 / 切换逻辑维护。
    pub(crate) fn begin_frame(&mut self) {
        self.focused_text = None;
        self.click_consumed = false;
    }
}

/// [`draw_text_input`] 的返回事件。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TextInputEvent {
    /// 无事件。
    None,
    /// 缓冲区被修改（字符输入 / IME commit / Backspace）。调用方可据此实时套用
    /// （如配色 hex 输入实时解析套色）。
    Modified,
    /// 聚焦状态下按下了 Enter。调用方据此提交（如备注编辑确认保存）或结束编辑
    /// （如配色 hex 输入框失焦）。
    Enter,
}

// ─── 内部助手 ────────────────────────────────────────────────────────────────

/// 点 (mx, my) 是否在矩形 r 内。
fn point_in(mx: f32, my: f32, r: Rect4) -> bool {
    mx >= r.x && mx <= r.x + r.w && my >= r.y && my <= r.y + r.h
}

/// 由鼠标 x 计算滑块值（按 track 宽度线性映射并夹取到 [min, max]）。
fn slider_value_from_mouse(track: Rect4, min: f32, max: f32, mx: f32) -> f32 {
    let t = if track.w > 0.0 {
        ((mx - track.x) / track.w).clamp(0.0, 1.0)
    } else {
        0.0
    };
    min + t * (max - min)
}

// ─── 组件：滑块 ──────────────────────────────────────────────────────────────

/// 立即模式水平滑块：绘制轨道 + 填充 + 旋钮，并处理拖拽，返回（可能更新后的）值。
///
/// - 用 `id` 区分不同滑块；`state.dragging_slider` 记录正在拖拽的 id。
/// - `track` 为轨道矩形（细条），`hit` 为可点击命中区（通常比 track 更高，便于抓取）。
/// - `min` / `max` 为值域，`value` 为当前值。拖拽时按鼠标 x 计算新值并返回。
/// - 仅当 `state.interactable` 为 `true` 时响应输入；始终绘制。
/// - 视觉与原 `draw_slider_track` 完全一致。
pub(crate) fn draw_slider(
    state: &mut UiWidgetsState,
    id: SliderId,
    track: Rect4,
    hit: Rect4,
    min: f32,
    max: f32,
    value: f32,
    scale: f32,
) -> f32 {
    let mut v = value;
    let (mx, my) = mouse_position();
    if state.interactable {
        // 继续已开始的拖拽：鼠标按下期间跟随鼠标 x。
        if state.dragging_slider == Some(id) && is_mouse_button_down(MouseButton::Left) {
            v = slider_value_from_mouse(track, min, max, mx);
        } else if !state.click_consumed
            && is_mouse_button_pressed(MouseButton::Left)
            && point_in(mx, my, hit)
        {
            // 全新按下且命中本滑块：开始拖拽，立即跳到鼠标位置。
            state.dragging_slider = Some(id);
            state.click_consumed = true;
            v = slider_value_from_mouse(track, min, max, mx);
        }
        // 释放结束拖拽（仅当本 id 正在拖拽）。
        if is_mouse_button_released(MouseButton::Left) && state.dragging_slider == Some(id) {
            state.dragging_slider = None;
        }
    }
    // 绘制：轨道、填充、旋钮。复刻原 draw_slider_track 视觉。
    let f = if (max - min).abs() > 0.0 {
        ((v - min) / (max - min)).clamp(0.0, 1.0)
    } else {
        0.0
    };
    draw_rectangle(track.x, track.y, track.w, track.h, Color::new(0.2, 0.2, 0.3, 0.8));
    draw_rectangle(
        track.x,
        track.y,
        track.w * f,
        track.h,
        Color::new(0.36, 0.61, 0.84, 0.9),
    );
    let knob_x = track.x + track.w * f;
    let knob_y = track.y + track.h / 2.0;
    draw_circle(knob_x, knob_y, 9.0 * scale, Color::new(0.8, 0.9, 1.0, 1.0));
    v
}

// ─── 组件：下拉框 ────────────────────────────────────────────────────────────

/// 立即模式下拉框：绘制折叠框 + （若展开）选项列表，并处理开合与选择。
///
/// - 用 `id` 区分；`state.open_dropdown` 记录哪个下拉展开（同一时间至多一个）。
/// - `box_label` 为折叠框显示文本（可能与选项文本不同——例如 UI 语言下拉在
///   「跟随剧本语言」时折叠框显示 `跟随剧本语言 (xx)`，而列表首项为 `原文`）。
/// - `options` 为展开列表的选项文本，`selected` 为当前选中项索引（用于高亮）。
/// - 返回 `Some(idx)` 表示用户本次点击选择了第 `idx` 项（调用方据此写回设置）；
///   返回 `None` 表示无选择（可能切换了开合或点击了列表外部）。
/// - 仅当 `state.interactable` 为 `true` 时响应输入；始终绘制。
/// - 应在所有其它控件之后调用，使展开的列表渲染于最上层（z-order 正确）。
/// - 视觉与原 `draw_dropdown_box_*` / `draw_dropdown_list_*` 一致。
pub(crate) fn draw_dropdown(
    state: &mut UiWidgetsState,
    id: DropdownId,
    r: Rect4,
    box_label: &str,
    options: &[String],
    selected: usize,
    scale: f32,
    font: &Option<Font>,
) -> Option<usize> {
    let open = state.open_dropdown == Some(id);
    let (mx, my) = mouse_position();
    let mut chosen: Option<usize> = None;

    if state.interactable
        && !state.click_consumed
        && is_mouse_button_pressed(MouseButton::Left)
    {
        if point_in(mx, my, r) {
            // 点击折叠框：切换开合。开则关，关则开（并隐式关掉其它下拉）。
            state.open_dropdown = if open { None } else { Some(id) };
            state.click_consumed = true;
        } else if open {
            // 已展开：判定是否点中某选项。
            let item_h = 32.0 * scale;
            for i in 0..options.len() {
                let item_rect = Rect4 {
                    x: r.x,
                    y: r.y + r.h + i as f32 * item_h,
                    w: r.w,
                    h: item_h,
                };
                if point_in(mx, my, item_rect) {
                    chosen = Some(i);
                    state.open_dropdown = None;
                    state.click_consumed = true;
                    break;
                }
            }
            // 点中选项以外的任何位置：关闭本下拉（消费此点击，避免下层控件误响应）。
            if chosen.is_none() {
                state.open_dropdown = None;
                state.click_consumed = true;
            }
        }
    }

    // 绘制折叠框（复刻原 draw_dropdown_box_* 视觉）。
    draw_rectangle_rounded(r.x, r.y, r.w, r.h, 6.0 * scale, Color::new(0.15, 0.25, 0.45, 0.9));
    draw_rectangle_lines_rounded(
        r.x, r.y, r.w, r.h, 6.0 * scale, 1.5 * scale, Color::new(0.45, 0.7, 0.95, 0.8),
    );
    let label_size = 20.0 * scale;
    draw_text_f(
        box_label,
        r.x + 12.0 * scale,
        r.y + r.h / 2.0 + 7.0 * scale,
        label_size,
        WHITE,
        font,
    );
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

    // 绘制展开列表（复刻原 draw_dropdown_list_* 视觉）。
    if open {
        let item_h = 32.0 * scale;
        let list_h = item_h * options.len() as f32;
        draw_rectangle_rounded(
            r.x, r.y + r.h, r.w, list_h, 6.0 * scale, Color::new(0.12, 0.2, 0.35, 0.97),
        );
        draw_rectangle_lines_rounded(
            r.x, r.y + r.h, r.w, list_h, 6.0 * scale, 1.0 * scale, Color::new(0.45, 0.7, 0.95, 0.6),
        );
        let item_size = 18.0 * scale;
        for (i, opt) in options.iter().enumerate() {
            let iy = r.y + r.h + i as f32 * item_h;
            let is_selected = i == selected;
            let color = if is_selected {
                Color::new(0.5, 0.75, 1.0, 0.95)
            } else {
                WHITE
            };
            draw_text_f(
                opt,
                r.x + 12.0 * scale,
                iy + item_h / 2.0 + 6.0 * scale,
                item_size,
                color,
                font,
            );
        }
    }

    chosen
}

// ─── 组件：文本输入 ──────────────────────────────────────────────────────────

/// 立即模式文本输入框：绘制框 + 文本 + 光标 + IME preedit，收集字符 / Backspace /
/// Enter / IME commit，返回 [`TextInputEvent`]。
///
/// - `active` 为是否处于激活（聚焦且可交互）状态。激活时收集输入、绘制光标与
///   preedit；非激活时仅按 `inactive_display` 静态绘制。聚焦与否由调用方管理
///   （配色 hex 框的点击聚焦在 `handle_settings_interaction`；备注弹窗恒激活）。
/// - `bg` / `border` 为框背景与边框色（调用方按激活态决定，保持与原各处视觉一致）。
/// - `cursor_inset` 为光标距框上下的内缩（配色 hex 用 6*scale，备注用 10*scale）。
/// - `inactive_display` 为非激活时显示的文本（如配色 hex 显示当前色值）。
/// - `buffer` 为输入缓冲区（激活时被本函数修改）；`accept_char` 为字符过滤函数
///   （如配色 hex 仅接受十六进制字符与 `#`）；`max_chars` 为最大字符数。
/// - 同时消费直接按键字符（`get_char_pressed`）与 IME 确认文本（`take_ime_commit`）；
///   激活时若存在 IME preedit，则在光标处绘制候选串（带下划线）。
/// - IME 的启用由 `renderer::run()` 根据 `UiWidgetsState::focused_text` 调用
///   `Window::set_ime_allowed` 完成；本组件只负责读取 preedit / 消费 commit。
pub(crate) fn draw_text_input(
    active: bool,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    text_size: f32,
    bg: Color,
    border: Color,
    cursor_inset: f32,
    inactive_display: &str,
    buffer: &mut String,
    scale: f32,
    font: &Option<Font>,
    accept_char: &dyn Fn(char) -> bool,
    max_chars: usize,
) -> TextInputEvent {
    let mut event = TextInputEvent::None;

    if active {
        // Backspace：删除末尾字符。
        if is_key_pressed(KeyCode::Backspace) {
            if buffer.pop().is_some() {
                event = TextInputEvent::Modified;
            }
        }
        // 直接按键字符。
        while let Some(c) = get_char_pressed() {
            if c.is_control() || !accept_char(c) {
                continue;
            }
            if buffer.chars().count() < max_chars {
                buffer.push(c);
                event = TextInputEvent::Modified;
            }
        }
        // IME 确认文本（整串，逐字符按过滤/上限入缓冲区）。
        while let Some(s) = take_ime_commit() {
            for c in s.chars() {
                if c.is_control() || !accept_char(c) {
                    continue;
                }
                if buffer.chars().count() < max_chars {
                    buffer.push(c);
                    event = TextInputEvent::Modified;
                }
            }
        }
        // Enter：返回给调用方处理（提交 / 失焦）。
        if is_key_pressed(KeyCode::Enter) {
            event = TextInputEvent::Enter;
        }
    }

    // 绘制框。
    draw_rectangle(x, y, w, h, bg);
    draw_rectangle_lines(x, y, w, h, 1.5 * scale, border);

    // 绘制文本。激活时显示缓冲区内容，非激活显示 inactive_display。
    let text_pad = 10.0 * scale;
    let max_text_w = w - 2.0 * text_pad;
    let raw = if active { buffer.as_str() } else { inactive_display };
    let display_text = fit_text(raw, font, text_size, max_text_w - 8.0 * scale);
    draw_text_f(
        &display_text,
        x + text_pad,
        y + h / 2.0 + text_size / 2.5,
        text_size,
        Color::new(0.9, 0.92, 0.98, 1.0),
        font,
    );

    if active {
        let tw = measure_text_f(&display_text, font, text_size as u16, 1.0).width;
        // 光标：闪烁竖线（约 1Hz 周期）。
        let cursor_blink = (get_time() * 2.0).floor() as i64 % 2 == 0;
        if cursor_blink {
            let cx = x + text_pad + tw + 2.0 * scale;
            draw_rectangle(cx, y + cursor_inset, 2.0 * scale, h - 2.0 * cursor_inset, WHITE);
        }
        // IME preedit：在光标处绘制候选串（带下划线表示尚未确认）。
        if let Some((preedit, _)) = ime_preedit() {
            if !preedit.is_empty() {
                let px = x + text_pad + tw + 4.0 * scale;
                let py = y + h / 2.0 + text_size / 2.5;
                draw_text_f(
                    &preedit,
                    px,
                    py,
                    text_size,
                    Color::new(0.85, 0.9, 1.0, 1.0),
                    font,
                );
                let pw = measure_text_f(&preedit, font, text_size as u16, 1.0).width;
                draw_rectangle(px, py + 3.0 * scale, pw, 1.5 * scale, Color::new(0.6, 0.8, 1.0, 0.9));
            }
        }
    }

    event
}
