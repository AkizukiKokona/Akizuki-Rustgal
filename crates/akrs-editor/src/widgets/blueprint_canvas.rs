//! 蓝图节点画布：基于 [`iced::widget::canvas::Program`] 的自定义绘制与交互。
//!
//! 负责节点矩形、连线贝塞尔曲线、引脚圆点、选中高亮、连线预览的绘制，
//! 以及鼠标交互（拖拽节点、平移画布、从输出引脚拉线、右键菜单、双击编辑）。
//!
//! 坐标系约定（与 [`crate::state::BlueprintState`] 一致）：
//! - 节点 `pos` / `node_size` 均为「画布逻辑坐标」。
//! - 屏幕坐标 → 画布逻辑坐标：`(cursor - bounds.origin - pan) / zoom`。
//! - 画布逻辑坐标 → Frame 绘制坐标：`node.pos * zoom + pan`。

use iced::event::Status;
use iced::mouse::{self, Button, Cursor, Interaction};
use iced::widget::canvas::{
    self, Cache, Event, Frame, Geometry, Path, Program, Stroke, Text,
};
use iced::{
    alignment, Color, Font, Pixels, Point, Rectangle, Renderer, Size, Theme, Vector,
};

/// iced 的鼠标交互态（用于 [`Program::mouse_interaction`] 返回值）。
type MouseInteraction = Interaction;

use crate::state::{BlueprintNode, BlueprintState, NodeKind};

/// 蓝图画布消息：由画布交互产生，提交给 [`crate::EditorApp`] 处理。
#[derive(Debug, Clone)]
pub enum BlueprintMsg {
    /// 选中并开始拖动节点 `id`，`offset` 为鼠标到节点左上角的偏移。
    DragStarted {
        id: usize,
        offset: Vector,
    },
    /// 拖动中的鼠标移动到画布坐标 `pos`。
    DragMoved {
        pos: Point,
    },
    /// 拖动结束（鼠标释放）。
    DragEnded,
    /// 从节点 `from` 的输出引脚开始拉线，鼠标当前位于画布坐标 `pos`。
    ConnectStarted {
        from: usize,
        pos: Point,
    },
    /// 拉线过程中鼠标移动到 `pos`。
    ConnectMoved {
        pos: Point,
    },
    /// 拉线结束；`target` 为命中输入引脚的目标节点（None 表示未命中）。
    ConnectEnded {
        target: Option<usize>,
    },
    /// 开始平移画布，记录起始点 `origin`。
    PanStarted {
        origin: Point,
    },
    /// 平移过程中，鼠标相对上一帧移动了 `delta`。
    PanMoved {
        delta: Vector,
    },
    /// 平移结束。
    PanEnded,
    /// 点击空白处取消选中。
    Deselected,
    /// 右键在画布坐标 `pos` 处释放（弹出右键菜单）。
    RightClicked {
        pos: Point,
    },
    /// 双击节点 `id` 进入就地编辑。
    DoubleClicked {
        id: usize,
    },
    /// 选中节点 `id`（单击节点但未拖动）。
    NodeSelected {
        id: usize,
    },
}

/// 蓝图画布程序：持有当前蓝图状态的一份快照用于绘制。
pub struct BlueprintProgram {
    /// 蓝图状态快照（节点、连线、pan、selected、connecting_from 等）。
    pub state: BlueprintState,
    /// 几何缓存：避免每帧重算静态几何。
    cache: Cache,
}

impl BlueprintProgram {
    /// 创建画布程序，传入当前蓝图状态快照。
    pub fn new(state: BlueprintState) -> Self {
        Self {
            state,
            cache: Cache::default(),
        }
    }

    /// 将屏幕坐标（cursor 减去 bounds 原点）转换为画布逻辑坐标。
    fn to_canvas(screen: Point, bounds: &Rectangle, pan: Vector, zoom: f32) -> Point {
        Point::new(
            (screen.x - bounds.x - pan.x) / zoom,
            (screen.y - bounds.y - pan.y) / zoom,
        )
    }

    /// 画布逻辑坐标 → Frame 绘制坐标（应用 pan 与 zoom）。
    fn to_frame(canvas_pos: Point, pan: Vector, zoom: f32) -> Point {
        Point::new(canvas_pos.x * zoom + pan.x, canvas_pos.y * zoom + pan.y)
    }
}

/// 画布交互态（跨帧持久）。
#[derive(Debug, Clone, Copy, Default)]
pub struct CanvasState {
    /// 当前正在进行的交互类型。
    pub kind: InteractionKind,
    /// 上一次鼠标位置（画布逻辑坐标），用于计算增量。
    pub last_canvas: Point,
    /// 拖动开始时的鼠标位置，用于区分点击与拖拽。
    pub press_origin: Option<Point>,
    /// 是否已移动超过阈值（判定为拖拽而非点击）。
    pub moved: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum InteractionKind {
    #[default]
    Idle,
    /// 正在拖动节点。
    DraggingNode,
    /// 正在平移画布。
    Panning,
    /// 正在从输出引脚拉线。
    Connecting,
}

impl Program<BlueprintMsg, Theme> for BlueprintProgram {
    type State = CanvasState;

    fn update(
        &self,
        state: &mut Self::State,
        event: Event,
        bounds: Rectangle,
        cursor: Cursor,
    ) -> (Status, Option<BlueprintMsg>) {
        let pos = match cursor.position() {
            Some(p) => p,
            None => return (Status::Ignored, None),
        };

        let pan = self.state.pan;
        let zoom = self.state.zoom;
        let canvas_pos = Self::to_canvas(pos, &bounds, pan, zoom);
        let pin_r = if self.state.touch_mode { 14.0 } else { 8.0 };

        match event {
            Event::Mouse(mouse::Event::ButtonPressed(Button::Left)) => {
                state.press_origin = Some(canvas_pos);
                state.moved = false;

                // 优先检测输出引脚（开始连线）
                if let Some(from) = self.state.output_pin_at(canvas_pos, pin_r) {
                    state.kind = InteractionKind::Connecting;
                    state.last_canvas = canvas_pos;
                    return (
                        Status::Captured,
                        Some(BlueprintMsg::ConnectStarted {
                            from,
                            pos: canvas_pos,
                        }),
                    );
                }
                // 其次检测节点（选中并准备拖动）
                if let Some(id) = self.state.node_at(canvas_pos) {
                    let node = self
                        .state
                        .nodes
                        .iter()
                        .find(|n| n.id == id)
                        .expect("节点存在");
                    let offset = Vector::new(
                        canvas_pos.x - node.pos.x,
                        canvas_pos.y - node.pos.y,
                    );
                    state.kind = InteractionKind::DraggingNode;
                    state.last_canvas = canvas_pos;
                    return (
                        Status::Captured,
                        Some(BlueprintMsg::DragStarted { id, offset }),
                    );
                }
                // 空白处：开始平移
                state.kind = InteractionKind::Panning;
                state.last_canvas = canvas_pos;
                return (
                    Status::Captured,
                    Some(BlueprintMsg::PanStarted {
                        origin: canvas_pos,
                    }),
                );
            }
            Event::Mouse(mouse::Event::CursorMoved { .. }) => {
                match state.kind {
                    InteractionKind::DraggingNode => {
                        state.moved = true;
                        state.last_canvas = canvas_pos;
                        return (
                            Status::Captured,
                            Some(BlueprintMsg::DragMoved { pos: canvas_pos }),
                        );
                    }
                    InteractionKind::Panning => {
                        let delta = Vector::new(
                            canvas_pos.x - state.last_canvas.x,
                            canvas_pos.y - state.last_canvas.y,
                        );
                        state.last_canvas = canvas_pos;
                        state.moved = true;
                        return (
                            Status::Captured,
                            Some(BlueprintMsg::PanMoved { delta }),
                        );
                    }
                    InteractionKind::Connecting => {
                        state.last_canvas = canvas_pos;
                        return (
                            Status::Captured,
                            Some(BlueprintMsg::ConnectMoved { pos: canvas_pos }),
                        );
                    }
                    InteractionKind::Idle => {}
                }
            }
            Event::Mouse(mouse::Event::ButtonReleased(Button::Left)) => {
                let kind = state.kind;
                let moved = state.moved;
                state.kind = InteractionKind::Idle;
                state.press_origin = None;
                match kind {
                    InteractionKind::DraggingNode => {
                        if moved {
                            return (Status::Captured, Some(BlueprintMsg::DragEnded));
                        } else if let Some(id) = self.state.drag_node {
                            return (
                                Status::Captured,
                                Some(BlueprintMsg::NodeSelected { id }),
                            );
                        } else {
                            return (Status::Captured, Some(BlueprintMsg::DragEnded));
                        }
                    }
                    InteractionKind::Connecting => {
                        let target = self.state.input_pin_at(canvas_pos, pin_r);
                        return (
                            Status::Captured,
                            Some(BlueprintMsg::ConnectEnded { target }),
                        );
                    }
                    InteractionKind::Panning => {
                        if !moved {
                            return (Status::Captured, Some(BlueprintMsg::Deselected));
                        }
                        return (Status::Captured, Some(BlueprintMsg::PanEnded));
                    }
                    InteractionKind::Idle => {}
                }
            }
            Event::Mouse(mouse::Event::ButtonPressed(Button::Right)) => {
                state.press_origin = Some(canvas_pos);
                state.moved = false;
                return (Status::Captured, None);
            }
            Event::Mouse(mouse::Event::ButtonReleased(Button::Right)) => {
                if !state.moved {
                    return (
                        Status::Captured,
                        Some(BlueprintMsg::RightClicked { pos: canvas_pos }),
                    );
                }
                state.kind = InteractionKind::Idle;
                state.press_origin = None;
                return (Status::Captured, None);
            }
            _ => {}
        }
        (Status::Ignored, None)
    }

    fn draw(
        &self,
        _state: &Self::State,
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: Cursor,
    ) -> Vec<Geometry<Renderer>> {
        // 使用缓存绘制背景 + 节点（静态部分）。
        // 动态部分（连线预览）每帧重绘。
        let background = self.cache.draw(renderer, bounds.size(), |frame| {
            self.draw_to_frame(frame, bounds);
        });

        vec![background]
    }

    fn mouse_interaction(
        &self,
        _state: &Self::State,
        bounds: Rectangle,
        cursor: Cursor,
    ) -> MouseInteraction {
        let pos = match cursor.position() {
            Some(p) => p,
            None => return MouseInteraction::default(),
        };
        let canvas_pos = Self::to_canvas(pos, &bounds, self.state.pan, self.state.zoom);
        let pin_r = if self.state.touch_mode { 14.0 } else { 8.0 };
        if self.state.output_pin_at(canvas_pos, pin_r).is_some()
            || self.state.input_pin_at(canvas_pos, pin_r).is_some()
        {
            return MouseInteraction::Crosshair;
        }
        if self.state.node_at(canvas_pos).is_some() {
            return MouseInteraction::Pointer;
        }
        MouseInteraction::default()
    }
}

impl BlueprintProgram {
    /// 绘制完整画布到 Frame。
    fn draw_to_frame(&self, frame: &mut Frame, bounds: Rectangle) {
        let pan = self.state.pan;
        let zoom = self.state.zoom;

        // 1. 背景填充
        frame.fill(
            &Path::rectangle(Point::ORIGIN, bounds.size()),
            Color::from_rgb8(30, 30, 34),
        );

        // 2. 网格（每 40px 一格，随 pan 偏移）
        let grid_color = Color::from_rgb8(45, 45, 50);
        let spacing = 40.0_f32;
        let off_x = pan.x.rem_euclid(spacing);
        let off_y = pan.y.rem_euclid(spacing);
        let mut x = off_x;
        while x < bounds.width {
            let path = Path::line(Point::new(x, 0.0), Point::new(x, bounds.height));
            frame.stroke(&path, Stroke::default().with_width(1.0).with_color(grid_color));
            x += spacing;
        }
        let mut y = off_y;
        while y < bounds.height {
            let path = Path::line(Point::new(0.0, y), Point::new(bounds.width, y));
            frame.stroke(&path, Stroke::default().with_width(1.0).with_color(grid_color));
            y += spacing;
        }

        // 3. 连线（贝塞尔曲线）
        for link in &self.state.links {
            let from_node = self.state.nodes.iter().find(|n| n.id == link.from);
            let to_node = self.state.nodes.iter().find(|n| n.id == link.to);
            let (Some(from), Some(to)) = (from_node, to_node) else {
                continue;
            };
            let from_size = BlueprintState::node_size(&from.text);
            let to_size = BlueprintState::node_size(&to.text);
            let start = Self::to_frame(
                Point::new(from.pos.x + from_size.x / 2.0, from.pos.y + from_size.y),
                pan,
                zoom,
            );
            let end = Self::to_frame(
                Point::new(to.pos.x + to_size.x / 2.0, to.pos.y),
                pan,
                zoom,
            );
            let ctrl1 = Point::new(start.x, (start.y + end.y) / 2.0);
            let ctrl2 = Point::new(end.x, (start.y + end.y) / 2.0);
            let mut builder = canvas::path::Builder::new();
            builder.move_to(start);
            builder.bezier_curve_to(ctrl1, ctrl2, end);
            let path = builder.build();
            frame.stroke(
                &path,
                Stroke::default()
                    .with_width(2.5)
                    .with_color(Color::from_rgb8(180, 180, 200)),
            );
        }

        // 4. 连线预览（拉线中）
        if let Some(from_id) = self.state.connecting_from {
            if let Some(from) = self.state.nodes.iter().find(|n| n.id == from_id) {
                let size = BlueprintState::node_size(&from.text);
                let start = Self::to_frame(
                    Point::new(from.pos.x + size.x / 2.0, from.pos.y + size.y),
                    pan,
                    zoom,
                );
                let end = Self::to_frame(self.state.connecting_pos, pan, zoom);
                let ctrl1 = Point::new(start.x, (start.y + end.y) / 2.0);
                let ctrl2 = Point::new(end.x, (start.y + end.y) / 2.0);
                let mut builder = canvas::path::Builder::new();
                builder.move_to(start);
                builder.bezier_curve_to(ctrl1, ctrl2, end);
                let path = builder.build();
                frame.stroke(
                    &path,
                    Stroke::default()
                        .with_width(2.0)
                        .with_color(Color::from_rgb8(255, 220, 100)),
                );
            }
        }

        // 5. 节点
        for node in &self.state.nodes {
            if self.state.collapse_comments && node.kind == NodeKind::Comment {
                continue;
            }
            draw_node(frame, node, pan, zoom, self.state.selected == Some(node.id));
        }

        // 6. 引脚圆点（输出=底部绿色，输入=顶部红色）
        for node in &self.state.nodes {
            if self.state.collapse_comments && node.kind == NodeKind::Comment {
                continue;
            }
            let size = BlueprintState::node_size(&node.text);
            let out_pin = Self::to_frame(
                Point::new(node.pos.x + size.x / 2.0, node.pos.y + size.y),
                pan,
                zoom,
            );
            let in_pin = Self::to_frame(
                Point::new(node.pos.x + size.x / 2.0, node.pos.y),
                pan,
                zoom,
            );
            let r = if self.state.touch_mode { 6.0 } else { 4.0 };
            frame.fill(&Path::circle(out_pin, r), Color::from_rgb8(120, 200, 120));
            frame.fill(&Path::circle(in_pin, r), Color::from_rgb8(200, 120, 120));
        }
    }
}

/// 绘制单个节点：标题栏（按类型着色）+ 正文。
fn draw_node(frame: &mut Frame, node: &BlueprintNode, pan: Vector, zoom: f32, selected: bool) {
    let size = BlueprintState::node_size(&node.text);
    let origin = BlueprintProgram::to_frame(node.pos, pan, zoom);
    let header_h = 22.0_f32;
    let body_h = (size.y - header_h).max(14.0);

    // 选中高亮（外边框）
    if selected {
        let hl = Path::rectangle(
            Point::new(origin.x - 2.0, origin.y - 2.0),
            Size::new(size.x + 4.0, size.y + 4.0),
        );
        frame.stroke(
            &hl,
            Stroke::default()
                .with_width(2.0)
                .with_color(Color::from_rgb8(255, 220, 0)),
        );
    }

    // 标题栏背景
    let header_color = BlueprintState::kind_color(node.kind);
    let header_path = Path::rectangle(origin, Size::new(size.x, header_h));
    frame.fill(&header_path, header_color);

    // 正文背景（深色）
    let body_path = Path::rectangle(
        Point::new(origin.x, origin.y + header_h),
        Size::new(size.x, body_h),
    );
    frame.fill(&body_path, Color::from_rgb8(40, 40, 46));

    // 节点边框
    let border = Path::rectangle(origin, Size::new(size.x, size.y));
    frame.stroke(
        &border,
        Stroke::default()
            .with_width(1.0)
            .with_color(Color::from_rgb8(80, 80, 90)),
    );

    // 标题文字（节点类型标签）
    let label = BlueprintState::node_label(&node.text, node.kind);
    frame.fill_text(Text {
        content: label,
        position: Point::new(origin.x + 8.0, origin.y + 3.0),
        color: Color::WHITE,
        size: Pixels(13.0),
        font: Font::DEFAULT,
        horizontal_alignment: alignment::Horizontal::Left,
        vertical_alignment: alignment::Vertical::Top,
        ..Default::default()
    });

    // 正文文字（节点文本，截断显示前 4 行）
    let lines: Vec<&str> = node.text.lines().take(4).collect();
    for (i, line) in lines.iter().enumerate() {
        let display = if line.len() > 48 { &line[..48] } else { *line };
        frame.fill_text(Text {
            content: display.to_string(),
            position: Point::new(
                origin.x + 8.0,
                origin.y + header_h + 4.0 + (i as f32) * 15.0,
            ),
            color: Color::from_rgb8(220, 220, 225),
            size: Pixels(13.0),
            font: Font::DEFAULT,
            horizontal_alignment: alignment::Horizontal::Left,
            vertical_alignment: alignment::Vertical::Top,
            ..Default::default()
        });
    }
}
