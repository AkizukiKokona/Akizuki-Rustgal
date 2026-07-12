//! 蓝图节点编辑器的 iced canvas 画布模块。
//!
//! 基于 [`iced::widget::canvas::Program`] 实现节点/连线/平移/缩放/拖拽/连线交互。
//! 本模块是纯渲染 + 交互层：所有状态变更通过 [`BlueprintMsg`] 反馈给上层 app，
//! 由 app 更新 [`crate::state::BlueprintState`] 后，每帧用其快照重新构造
//! [`BlueprintProgram`]。
//!
//! ## 坐标系约定
//!
//! - **画布逻辑坐标**（`canvas`）：节点 `pos` 所在坐标系，与 `BlueprintState::pan`/`zoom` 无关。
//! - **bounds-local 坐标**（`local`）：以画布 widget 左上角为原点的屏幕像素坐标。
//!   `iced` 的 `Canvas` widget 在调用 `Program::draw` 前已对 renderer 平移
//!   `bounds.position()`，故 `Frame` 内绘制时原点即 bounds 左上角，**无需再叠加
//!   `bounds.position()`**（否则会双重平移）。
//!
//!   转换公式：
//!   - `local = canvas * zoom + pan`
//!   - `canvas = (local - pan) / zoom`
//!   - 光标：`cursor.position()` 返回窗口绝对坐标，
//!     `local = cursor_absolute - bounds.position()`。

use std::collections::HashSet;
use std::time::Instant;

use iced::keyboard;
use iced::mouse;
use iced::widget::canvas;
use iced::widget::canvas::event::{self, Event};
use iced::{border, Color, Font, Pixels, Point, Rectangle, Size, Vector};

use crate::state::{BlueprintState, NodeKind};

// ---------------------------------------------------------------------------
// 消息
// ---------------------------------------------------------------------------

/// 蓝图画布与上层 app 之间的交互消息。
///
/// 画布仅产生消息、不直接修改 `BlueprintState`；app 收到消息后更新状态，
/// 下一帧用新状态构造新的 [`BlueprintProgram`] 快照。
#[derive(Debug, Clone)]
pub enum BlueprintMsg {
    /// 节点拖动提交（拖拽过程中实时发出，松手时画布侧无需再发）。
    NodeMoved { id: usize, pos: Point },
    /// 平移提交（拖拽空白处时实时发出）。
    PanChanged(Vector),
    /// 缩放提交（独立改变 zoom 时发出，例如外部工具栏按钮）。
    ZoomChanged(f32),
    /// 同时提交 pan 与 zoom（光标锚点缩放时发出）。
    ///
    /// iced 0.13 的 `Program::update` 每个事件只能返回一个消息，而光标锚点缩放
    /// 需要同时更新 pan 与 zoom，故增设此合并变体。
    ViewportChanged { pan: Vector, zoom: f32 },
    /// 选中节点变更。
    SelectionChanged(Option<usize>),
    /// 连线创建（从输出引脚拉到输入引脚）。
    LinkCreated { from: usize, to: usize },
    /// 删除选中节点。
    NodeDeleted(usize),
    /// 双击节点请求就地编辑。
    EditRequested(usize),
    /// 右键空白处请求菜单（坐标为窗口绝对坐标，供菜单定位）。
    ContextMenuRequested(Point),
    /// 右键节点请求菜单。
    NodeMenuRequested(usize),
}

// ---------------------------------------------------------------------------
// Program
// ---------------------------------------------------------------------------

/// 蓝图画布的 `canvas::Program` 载体。
///
/// `state` 是从 app 克隆的蓝图状态快照，每帧由 app 重新构造后传入
/// [`canvas::Canvas::new`]。交互态（正在拖哪个节点、正在拉线等）放在
/// [`CanvasState`]（`Self::State`）中，由 iced widget tree 持有并跨帧持久。
#[derive(Clone)]
pub struct BlueprintProgram {
    /// 从 app 克隆的蓝图状态快照。
    pub state: BlueprintState,
}

impl BlueprintProgram {
    /// 用给定的蓝图状态快照构造画布程序。
    pub fn new(state: BlueprintState) -> Self {
        Self { state }
    }

    /// 画布逻辑坐标 → bounds-local 屏幕坐标。
    fn canvas_to_local(&self, canvas: Point) -> Point {
        Point::new(
            canvas.x * self.state.zoom + self.state.pan.x,
            canvas.y * self.state.zoom + self.state.pan.y,
        )
    }

    /// bounds-local 屏幕坐标 → 画布逻辑坐标。
    fn local_to_canvas(&self, local: Point) -> Point {
        Point::new(
            (local.x - self.state.pan.x) / self.state.zoom,
            (local.y - self.state.pan.y) / self.state.zoom,
        )
    }
}

/// 画布的跨帧交互态（`Program::State`）。
///
/// `Program::update` 取 `&self`（不可变快照），故所有可变交互态都放在这里，
/// 由 iced widget tree 持有。
#[derive(Default)]
pub struct CanvasState {
    /// 正在拖动的节点 `(id, 拖动偏移)`，偏移 = 按下时光标画布坐标 - 节点位置。
    drag: Option<(usize, Vector)>,
    /// 正在平移画布 `(按下时的 local 坐标, 按下时的 pan 快照)`。
    pan_drag: Option<(Point, Vector)>,
    /// 正在从某节点输出引脚拉线。
    connecting: Option<usize>,
    /// 上次左键点击时间（双击检测）。
    last_click_time: Option<Instant>,
    /// 上次左键点击的画布逻辑坐标（双击检测）。
    last_click_pos: Option<Point>,
    /// 右键按下时的窗口绝对坐标（区分"点击"与"拖动"）。
    right_press: Option<Point>,
    /// 右键按下后是否发生了拖动。
    right_moved: bool,
    /// 当前键盘修饰键状态（用于滚轮 Ctrl 缩放判断）。
    modifiers: keyboard::Modifiers,
}

impl canvas::Program<BlueprintMsg> for BlueprintProgram {
    type State = CanvasState;

    fn update(
        &self,
        state: &mut CanvasState,
        event: Event,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> (event::Status, Option<BlueprintMsg>) {
        match event {
            // ---------------------------------------------------------------
            // 鼠标移动
            // ---------------------------------------------------------------
            Event::Mouse(mouse::Event::CursorMoved { position }) => {
                let local = position - Vector::new(bounds.x, bounds.y);

                // 拖动节点：实时提交新位置。
                if let Some((id, offset)) = state.drag {
                    let canvas_pos = self.local_to_canvas(local);
                    let new_pos = canvas_pos - offset;
                    return (
                        event::Status::Captured,
                        Some(BlueprintMsg::NodeMoved { id, pos: new_pos }),
                    );
                }

                // 平移画布：以按下点为锚，实时提交绝对 pan。
                if let Some((start_local, start_pan)) = state.pan_drag {
                    let delta = local - start_local;
                    let new_pan = start_pan + delta;
                    return (
                        event::Status::Captured,
                        Some(BlueprintMsg::PanChanged(new_pan)),
                    );
                }

                // 拉线：预览由 draw 用实时光标位置绘制，无需发消息。
                if state.connecting.is_some() {
                    return (event::Status::Captured, None);
                }

                // 右键拖动检测。
                if state.right_press.is_some() {
                    state.right_moved = true;
                }

                (event::Status::Ignored, None)
            }

            // ---------------------------------------------------------------
            // 鼠标按下
            // ---------------------------------------------------------------
            Event::Mouse(mouse::Event::ButtonPressed(button)) => {
                let local = match cursor.position() {
                    Some(p) => p - Vector::new(bounds.x, bounds.y),
                    None => return (event::Status::Ignored, None),
                };
                let canvas_pos = self.local_to_canvas(local);

                match button {
                    mouse::Button::Left => {
                        // 双击检测（400ms 内、位移 < 6 画布单位）。
                        let now = Instant::now();
                        let is_double =
                            match (state.last_click_time.as_ref(), state.last_click_pos) {
                                (Some(t), Some(p)) => {
                                    t.elapsed().as_millis() < 400
                                        && p.distance(canvas_pos) < 6.0
                                }
                                _ => false,
                            };
                        state.last_click_time = Some(now);
                        state.last_click_pos = Some(canvas_pos);

                        if is_double {
                            if let Some(id) = self.state.node_at(canvas_pos) {
                                return (
                                    event::Status::Captured,
                                    Some(BlueprintMsg::EditRequested(id)),
                                );
                            }
                            return (event::Status::Captured, None);
                        }

                        // 1) 输出引脚命中 → 开始拉线。
                        let pin_r = if self.state.touch_mode { 12.0 } else { 8.0 };
                        if let Some(id) = self.state.output_pin_at(canvas_pos, pin_r) {
                            state.connecting = Some(id);
                            return (event::Status::Captured, None);
                        }

                        // 2) 节点命中 → 开始拖动并选中。
                        if let Some(id) = self.state.node_at(canvas_pos) {
                            let node = self
                                .state
                                .nodes
                                .iter()
                                .find(|n| n.id == id)
                                .expect("命中节点必然存在");
                            let offset = canvas_pos - node.pos;
                            state.drag = Some((id, offset));
                            return (
                                event::Status::Captured,
                                Some(BlueprintMsg::SelectionChanged(Some(id))),
                            );
                        }

                        // 3) 空白处 → 开始平移。
                        state.pan_drag = Some((local, self.state.pan));
                        (event::Status::Captured, None)
                    }
                    mouse::Button::Right => {
                        state.right_press = cursor.position();
                        state.right_moved = false;
                        (event::Status::Captured, None)
                    }
                    _ => (event::Status::Ignored, None),
                }
            }

            // ---------------------------------------------------------------
            // 鼠标释放
            // ---------------------------------------------------------------
            Event::Mouse(mouse::Event::ButtonReleased(button)) => {
                let local = match cursor.position() {
                    Some(p) => p - Vector::new(bounds.x, bounds.y),
                    None => return (event::Status::Ignored, None),
                };
                let canvas_pos = self.local_to_canvas(local);

                match button {
                    mouse::Button::Left => {
                        // 结束拖动（位置已在 CursorMoved 实时提交）。
                        if state.drag.is_some() {
                            state.drag = None;
                            return (event::Status::Captured, None);
                        }
                        // 结束拉线：命中输入引脚则创建连线。
                        if let Some(from_id) = state.connecting {
                            let pin_r = if self.state.touch_mode { 14.0 } else { 10.0 };
                            if let Some(to_id) = self.state.input_pin_at(canvas_pos, pin_r) {
                                if to_id != from_id {
                                    state.connecting = None;
                                    return (
                                        event::Status::Captured,
                                        Some(BlueprintMsg::LinkCreated {
                                            from: from_id,
                                            to: to_id,
                                        }),
                                    );
                                }
                            }
                            state.connecting = None;
                            return (event::Status::Captured, None);
                        }
                        // 结束平移。
                        if state.pan_drag.is_some() {
                            state.pan_drag = None;
                            return (event::Status::Captured, None);
                        }
                        (event::Status::Ignored, None)
                    }
                    mouse::Button::Right => {
                        let moved = state.right_moved;
                        let press_pos = state.right_press.take();
                        state.right_moved = false;
                        if !moved {
                            // 右键单击：节点上 → 节点菜单；空白处 → 上下文菜单。
                            if let Some(id) = self.state.node_at(canvas_pos) {
                                return (
                                    event::Status::Captured,
                                    Some(BlueprintMsg::NodeMenuRequested(id)),
                                );
                            }
                            if let Some(p) = press_pos.or(cursor.position()) {
                                return (
                                    event::Status::Captured,
                                    Some(BlueprintMsg::ContextMenuRequested(p)),
                                );
                            }
                        }
                        (event::Status::Captured, None)
                    }
                    _ => (event::Status::Ignored, None),
                }
            }

            // ---------------------------------------------------------------
            // 滚轮：Ctrl 缩放，否则平移
            // ---------------------------------------------------------------
            Event::Mouse(mouse::Event::WheelScrolled { delta }) => {
                let local = match cursor.position() {
                    Some(p) => p - Vector::new(bounds.x, bounds.y),
                    None => return (event::Status::Ignored, None),
                };

                if state.modifiers.control() {
                    // 以光标为锚点缩放：保持光标下画布点不动。
                    let (dy, factor) = match delta {
                        mouse::ScrollDelta::Lines { y, .. } => (y, 0.1),
                        mouse::ScrollDelta::Pixels { y, .. } => (y, 0.01),
                    };
                    let zoom = self.state.zoom;
                    let new_zoom = (zoom * (1.0 + dy * factor)).clamp(0.2, 3.0);
                    let canvas_pos = self.local_to_canvas(local);
                    let new_pan = Vector::new(
                        local.x - canvas_pos.x * new_zoom,
                        local.y - canvas_pos.y * new_zoom,
                    );
                    return (
                        event::Status::Captured,
                        Some(BlueprintMsg::ViewportChanged {
                            pan: new_pan,
                            zoom: new_zoom,
                        }),
                    );
                } else {
                    // 平移：内容跟随滚轮方向（自然滚动）。
                    let (delta_v, scale) = match delta {
                        mouse::ScrollDelta::Lines { x, y } => (Vector::new(x, y), 20.0),
                        mouse::ScrollDelta::Pixels { x, y } => (Vector::new(x, y), 1.0),
                    };
                    let new_pan = self.state.pan + delta_v * scale;
                    return (
                        event::Status::Captured,
                        Some(BlueprintMsg::PanChanged(new_pan)),
                    );
                }
            }

            // ---------------------------------------------------------------
            // 键盘
            // ---------------------------------------------------------------
            Event::Keyboard(keyboard::Event::ModifiersChanged(m)) => {
                state.modifiers = m;
                (event::Status::Ignored, None)
            }
            Event::Keyboard(keyboard::Event::KeyPressed { key, .. }) => {
                if matches!(key, keyboard::Key::Named(keyboard::key::Named::Delete)) {
                    if let Some(id) = self.state.selected {
                        return (
                            event::Status::Captured,
                            Some(BlueprintMsg::NodeDeleted(id)),
                        );
                    }
                }
                (event::Status::Ignored, None)
            }
            _ => (event::Status::Ignored, None),
        }
    }

    fn draw(
        &self,
        state: &CanvasState,
        renderer: &iced::Renderer,
        _theme: &iced::Theme,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> Vec<canvas::Geometry> {
        let mut frame = canvas::Frame::new(renderer, bounds.size());
        let zoom = self.state.zoom;
        let pan = self.state.pan;

        // 1) 背景
        frame.fill_rectangle(
            Point::ORIGIN,
            bounds.size(),
            Color::from_rgb8(18, 18, 26),
        );

        // 2) 网格点
        let spacing = (40.0 * zoom).max(8.0);
        let dot_color = Color::from_rgb8(45, 45, 58);
        let start_x = rem_euclid_f32(pan.x, spacing);
        let start_y = rem_euclid_f32(pan.y, spacing);
        let mut gx = start_x;
        while gx < bounds.width {
            let mut gy = start_y;
            while gy < bounds.height {
                frame.fill(&canvas::Path::circle(Point::new(gx, gy), 1.0), dot_color);
                gy += spacing;
            }
            gx += spacing;
        }

        // 3) 连线
        let link_color = Color::from_rgb8(120, 160, 220);
        let link_width = (2.0 * zoom).max(0.5);
        let mut seen: HashSet<(usize, usize)> = HashSet::new();
        for link in &self.state.links {
            // 折叠注释时桥接到最近的非注释节点。
            let from_id = if self.state.collapse_comments {
                self.state.resolve_from(link.from).unwrap_or(link.from)
            } else {
                link.from
            };
            let to_id = if self.state.collapse_comments {
                self.state.resolve_to(link.to).unwrap_or(link.to)
            } else {
                link.to
            };
            if from_id == to_id || !seen.insert((from_id, to_id)) {
                continue;
            }
            let from_node = match self.state.nodes.iter().find(|n| n.id == from_id) {
                Some(n) => n,
                None => continue,
            };
            let to_node = match self.state.nodes.iter().find(|n| n.id == to_id) {
                Some(n) => n,
                None => continue,
            };
            let from_size = BlueprintState::node_size(&from_node.text) * zoom;
            let to_size = BlueprintState::node_size(&to_node.text) * zoom;
            let from_screen = self.canvas_to_local(from_node.pos);
            let to_screen = self.canvas_to_local(to_node.pos);
            let from_pin = Point::new(
                from_screen.x + from_size.x * 0.5,
                from_screen.y + from_size.y,
            );
            let to_pin = Point::new(to_screen.x + to_size.x * 0.5, to_screen.y);
            let ctrl_off = ((to_pin.y - from_pin.y).abs() * 0.5).max(20.0 * zoom);
            let path = canvas::Path::new(|b| {
                b.move_to(from_pin);
                b.bezier_curve_to(
                    Point::new(from_pin.x, from_pin.y + ctrl_off),
                    Point::new(to_pin.x, to_pin.y - ctrl_off),
                    to_pin,
                );
            });
            frame.stroke(
                &path,
                canvas::Stroke::default()
                    .with_width(link_width)
                    .with_color(link_color),
            );
        }

        // 4) 正在拉出的连线预览
        if let Some(from_id) = state.connecting {
            if let Some(from_node) = self.state.nodes.iter().find(|n| n.id == from_id) {
                let from_size = BlueprintState::node_size(&from_node.text) * zoom;
                let from_screen = self.canvas_to_local(from_node.pos);
                let from_pin = Point::new(
                    from_screen.x + from_size.x * 0.5,
                    from_screen.y + from_size.y,
                );
                let cur_local = cursor
                    .position()
                    .map(|p| p - Vector::new(bounds.x, bounds.y))
                    .unwrap_or(from_pin);
                let ctrl_off = 40.0 * zoom;
                let path = canvas::Path::new(|b| {
                    b.move_to(from_pin);
                    b.bezier_curve_to(
                        Point::new(from_pin.x, from_pin.y + ctrl_off),
                        Point::new(cur_local.x, cur_local.y - ctrl_off),
                        cur_local,
                    );
                });
                frame.stroke(
                    &path,
                    canvas::Stroke::default()
                        .with_width(2.0)
                        .with_color(Color::from_rgb8(255, 200, 80)),
                );
            }
        }

        // 5) 节点
        let sel_color = Color::from_rgb8(255, 220, 80);
        let body_color = Color::from_rgb8(200, 210, 220);
        let pin_r = if self.state.touch_mode { 9.0 } else { 5.0 };
        for node in &self.state.nodes {
            // 折叠注释时跳过 Comment 节点（已桥接到下一个非注释积木）。
            if self.state.collapse_comments && node.kind == NodeKind::Comment {
                continue;
            }
            let sz = BlueprintState::node_size(&node.text) * zoom;
            let top_left = self.canvas_to_local(node.pos);
            let w = sz.x;
            let h = sz.y;
            let color = BlueprintState::kind_color(node.kind);
            let radius = border::radius(4.0 * zoom);
            let rect = canvas::Path::rounded_rectangle(top_left, Size::new(w, h), radius);

            // 半透明填充
            frame.fill(&rect, Color { a: 0.15, ..color });
            // 类型描边
            frame.stroke(
                &rect,
                canvas::Stroke::default()
                    .with_width((1.5 * zoom).max(0.5))
                    .with_color(color),
            );
            // 选中描边
            if self.state.selected == Some(node.id) {
                frame.stroke(
                    &rect,
                    canvas::Stroke::default()
                        .with_width((2.5 * zoom).max(1.0))
                        .with_color(sel_color),
                );
            }

            // 左上色块
            let swatch = 10.0 * zoom;
            frame.fill_rectangle(top_left, Size::new(swatch, swatch), color);

            // 标题文字
            let label = BlueprintState::node_label(&node.text, node.kind);
            frame.fill_text(canvas::Text {
                content: label,
                position: Point::new(
                    top_left.x + swatch + 4.0 * zoom,
                    top_left.y + 3.0 * zoom,
                ),
                size: Pixels(13.0 * zoom),
                color,
                ..Default::default()
            });

            // 正文：前 6 行，按 7*zoom 字符宽估算截断
            let max_chars = (((w - 12.0 * zoom) / (7.0 * zoom)).max(1.0)) as usize;
            for (i, line) in node.text.lines().take(6).enumerate() {
                let truncated: String = line.chars().take(max_chars).collect();
                let display = if line.chars().count() > max_chars {
                    format!("{truncated}…")
                } else {
                    truncated
                };
                frame.fill_text(canvas::Text {
                    content: display,
                    position: Point::new(
                        top_left.x + 6.0 * zoom,
                        top_left.y + 22.0 * zoom + i as f32 * 15.0 * zoom,
                    ),
                    size: Pixels(11.0 * zoom),
                    color: body_color,
                    font: Font::MONOSPACE,
                    ..Default::default()
                });
            }

            // 引脚：输出（底部中心）、输入（顶部中心）
            let out_pin = Point::new(top_left.x + w * 0.5, top_left.y + h);
            let in_pin = Point::new(top_left.x + w * 0.5, top_left.y);
            frame.fill(&canvas::Path::circle(out_pin, pin_r), link_color);
            frame.fill(&canvas::Path::circle(in_pin, pin_r), link_color);
        }

        vec![frame.into_geometry()]
    }

    fn mouse_interaction(
        &self,
        state: &CanvasState,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> mouse::Interaction {
        // 进行中的交互统一显示 Grabbing。
        if state.drag.is_some() || state.pan_drag.is_some() || state.connecting.is_some() {
            return mouse::Interaction::Grabbing;
        }
        let local = match cursor.position() {
            Some(p) => p - Vector::new(bounds.x, bounds.y),
            None => return mouse::Interaction::default(),
        };
        let canvas_pos = self.local_to_canvas(local);
        let pin_r = if self.state.touch_mode { 12.0 } else { 8.0 };
        if self.state.output_pin_at(canvas_pos, pin_r).is_some()
            || self.state.input_pin_at(canvas_pos, pin_r).is_some()
        {
            return mouse::Interaction::Crosshair;
        }
        if self.state.node_at(canvas_pos).is_some() {
            return mouse::Interaction::Grab;
        }
        mouse::Interaction::default()
    }
}

/// f32 取模（结果与除数同号），用于网格点起始坐标对齐到 pan。
fn rem_euclid_f32(a: f32, b: f32) -> f32 {
    let r = a % b;
    if r < 0.0 {
        r + b
    } else {
        r
    }
}
