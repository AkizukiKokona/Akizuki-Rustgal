//! 蓝图节点编辑器状态（纯数据 + 算法，不含 UI）。
//!
//! 从 egui 主分支的编辑器源码（第 224-916 行）完整移植而来。
//! 本模块仅包含数据结构与纯逻辑，不涉及任何 iced widget 渲染。
//!
//! 提供蓝图节点/连线的增删、类型推断、颜色与标签、节点尺寸估算、
//! 脚本与蓝图的双向转换（[`BlueprintState::from_script`] / [`BlueprintState::to_script`]）、
//! 自动布局（[`BlueprintState::auto_layout`]）、命中测试（节点 / 输入输出引脚）、
//! 连线解析与注释折叠等能力。

use iced::{Color, Point, Rectangle, Size, Vector};
use std::collections::{HashMap, HashSet};

/// 蓝图节点类型，由 [`BlueprintState::detect_kind`] 根据行首标记推断。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum NodeKind {
    /// `#` 章节标题。
    Section,
    /// `角色: "..."` 对话。
    Dialogue,
    /// `"..."` 旁白。
    Narration,
    /// `@` 指令。
    Command,
    /// `+` / `-` 立绘方向。
    Direction,
    /// `?` 选择块开始。
    Choice,
    /// `|` 选择选项。
    ChoiceOption,
    /// `->` 跳转。
    Flow,
    /// `=>` 访问子章节。
    Visit,
    /// `<=` 从子章节返回。
    Return,
    /// `~~` 等待。
    Wait,
    /// `end` 故事结束。
    StoryEnd,
    /// `ending` 结局声明。
    Ending,
    /// `unlock` 解锁。
    Unlock,
    /// `$` 变量操作。
    VarOp,
    /// `//` 注释。
    Comment,
    /// 其他无法识别的行。
    Other,
}

/// 蓝图节点：对应剧本中的一行（或多行）指令。
#[derive(Clone)]
pub struct BlueprintNode {
    /// 节点唯一 ID。
    pub id: usize,
    /// 节点类型。
    pub kind: NodeKind,
    /// 画布坐标（不含 pan 偏移）。
    pub pos: Point,
    /// 节点文本内容（即 `.akrs` 脚本行）。
    pub text: String,
}

/// 蓝图连线：从某节点的输出引脚连到另一节点的输入引脚。
#[derive(Clone)]
pub struct BlueprintLink {
    /// 源节点 ID（输出引脚）。
    pub from: usize,
    /// 目标节点 ID（输入引脚）。
    pub to: usize,
}

/// 蓝图编辑器状态。
#[derive(Clone)]
pub struct BlueprintState {
    /// 全部节点（按创建顺序）。
    pub nodes: Vec<BlueprintNode>,
    /// 全部连线。
    pub links: Vec<BlueprintLink>,
    /// 下一个待分配的节点 ID。
    pub next_id: usize,
    /// 画布平移偏移。
    pub pan: Vector,
    /// 当前选中的节点 ID。
    pub selected: Option<usize>,
    /// 正在拖动的节点 ID。
    pub drag_node: Option<usize>,
    /// 拖动偏移（鼠标位置 - 节点位置）。
    pub drag_offset: Vector,
    /// 右键按下时的位置（用于区分"点击"与"拖动"）。
    pub right_press_pos: Option<Point>,
    /// 右键按下后是否发生了拖动。
    pub right_moved: bool,
    /// 正在从某节点输出引脚拉线。
    pub connecting_from: Option<usize>,
    /// 拉线时鼠标当前位置（画布坐标）。
    pub connecting_pos: Point,
    /// 右键菜单弹出位置（屏幕坐标）。
    pub context_menu_pos: Option<Point>,
    /// 正在编辑文本的节点 ID（双击触发，就地编辑）。
    pub editing_node: Option<usize>,
    /// 注释是否折叠隐藏到下一个非注释积木中（true=折叠，false=独立显示）。
    pub collapse_comments: bool,
    /// 画布缩放系数（1.0 = 100%）。
    pub zoom: f32,
    /// 是否处于触摸模式（检测到 Event::Touch 后置 true，粘性保持）。
    /// 触摸模式下：放大引脚便于点按、空白处单指拖拽平移画布、
    /// 工具栏显示触摸操作提示。
    pub touch_mode: bool,
    /// 触摸模式下空白处单指拖拽平移进行中（primary 按下在空白处时置 true）。
    pub touch_panning: bool,
}

impl Default for BlueprintState {
    fn default() -> Self {
        Self {
            nodes: Vec::new(),
            links: Vec::new(),
            next_id: 0,
            pan: Vector::ZERO,
            selected: None,
            drag_node: None,
            drag_offset: Vector::ZERO,
            right_press_pos: None,
            right_moved: false,
            connecting_from: None,
            connecting_pos: Point::ORIGIN,
            context_menu_pos: None,
            editing_node: None,
            collapse_comments: true,
            zoom: 1.0,
            touch_mode: false,
            touch_panning: false,
        }
    }
}

impl BlueprintState {
    /// 添加一个新节点，返回其 ID。
    pub fn add_node(&mut self, kind: NodeKind, pos: Point, text: String) -> usize {
        let id = self.next_id;
        self.next_id += 1;
        self.nodes.push(BlueprintNode { id, kind, pos, text });
        id
    }

    /// 计算下一个新节点的合适放置位置（最后一个节点下方，或默认起点）。
    /// 用于从预览面板"添加为节点"等无需指定位置的添加操作。
    pub fn next_placement_pos(&self) -> Point {
        if let Some(last) = self.nodes.last() {
            let size = Self::node_size(&last.text);
            Point::new(last.pos.x, last.pos.y + size.y + 24.0)
        } else {
            Point::new(80.0, 80.0)
        }
    }

    /// 用新文本替换选中节点的文本（并重新推断类型）。
    /// 用于蓝图模式下"替换选中节点"操作。若无选中节点则返回 false。
    pub fn replace_selected_text(&mut self, text: String) -> bool {
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
    pub fn remove_node(&mut self, id: usize) {
        self.nodes.retain(|n| n.id != id);
        self.links.retain(|l| l.from != id && l.to != id);
        if self.selected == Some(id) {
            self.selected = None;
        }
    }

    /// 添加一条连线（去重：同 from→to 只保留一条）。
    pub fn add_link(&mut self, from: usize, to: usize) {
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

    /// 根据文本推断节点类型。
    pub fn detect_kind(text: &str) -> NodeKind {
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
    pub fn kind_color(kind: NodeKind) -> Color {
        match kind {
            NodeKind::Section => Color::from_rgb8(70, 110, 200),
            NodeKind::Dialogue => Color::from_rgb8(60, 140, 80),
            NodeKind::Narration => Color::from_rgb8(90, 160, 100),
            NodeKind::Command => Color::from_rgb8(200, 140, 50),
            NodeKind::Direction => Color::from_rgb8(150, 80, 200),
            NodeKind::Choice => Color::from_rgb8(200, 180, 50),
            NodeKind::ChoiceOption => Color::from_rgb8(220, 200, 80),
            NodeKind::Flow => Color::from_rgb8(50, 180, 200),
            NodeKind::Visit => Color::from_rgb8(80, 150, 200),
            NodeKind::Return => Color::from_rgb8(120, 120, 130),
            NodeKind::Wait => Color::from_rgb8(200, 100, 150),
            NodeKind::StoryEnd => Color::from_rgb8(200, 60, 60),
            NodeKind::Ending => Color::from_rgb8(180, 80, 160),
            NodeKind::Unlock => Color::from_rgb8(160, 100, 180),
            NodeKind::VarOp => Color::from_rgb8(100, 130, 160),
            NodeKind::Comment => Color::from_rgb8(110, 110, 120),
            NodeKind::Other => Color::from_rgb8(100, 100, 110),
        }
    }

    /// 节点类型对应的中文标签。
    /// 对于命令行（@开头），返回具体命令名（背景/音乐等），而非笼统的"命令"。
    pub fn node_label(text: &str, kind: NodeKind) -> String {
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

    /// 估算节点尺寸（宽度随最长行自适应，高度随行数自适应）。
    pub fn node_size(text: &str) -> Vector {
        // 估算每字符宽度：中文约 12px，ASCII 约 7px。
        let char_width = |c: char| if c.is_ascii() { 7.0 } else { 12.0 };
        let max_line_w = text
            .lines()
            .map(|line| line.chars().map(char_width).sum::<f32>())
            .fold(0.0f32, f32::max);
        // 宽度 = 正文最大行宽 + 左右内边距，限制在 [200, 420]。
        let width = (max_line_w + 24.0).clamp(200.0, 420.0);
        let header_h = 22.0;
        let line_count = text.lines().count().max(1);
        let body_h = (line_count as f32 * 15.0 + 12.0).max(36.0);
        Vector::new(width, header_h + body_h)
    }

    /// 从脚本文本生成蓝图节点（自动布局 + 顺序连线）。
    /// 生成后调用 auto_layout 整理布局，避免节点堆叠。
    pub fn from_script(&mut self, text: &str) {
        self.nodes.clear();
        self.links.clear();
        self.next_id = 0;
        self.selected = None;

        // 先生成所有节点（位置暂为默认），同时建立顺序连线。
        let mut prev_id: Option<usize> = None;
        for line in text.lines() {
            if line.trim().is_empty() {
                continue;
            }
            let kind = Self::detect_kind(line);
            let id = self.add_node(kind, Point::ORIGIN, line.to_string());
            // 与上一个节点顺序连线（含章节节点→本章第一个积木，保持流程连贯）。
            if let Some(prev) = prev_id {
                self.links.push(BlueprintLink { from: prev, to: id });
            }
            prev_id = Some(id);
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
    /// - **都没连**（孤立）：不导出到脚本中（调用方可通过 [`BlueprintState::orphan_comments`]
    ///   获取孤立注释列表，在保存/离开时警告用户这些注释无法迁移）。
    ///
    /// 非注释节点按连线顺序（DFS 遍历）输出；无连线则按创建顺序输出。
    pub fn to_script(&self) -> String {
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
        let mut lines: Vec<String> = Vec::new();
        for id in &order {
            if let Some(comments) = before_comments.get(id) {
                for c in comments {
                    lines.push(c.clone());
                }
            }
            if let Some(node) = self.nodes.iter().find(|n| n.id == *id) {
                lines.push(node.text.clone());
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
    pub fn orphan_comments(&self) -> Vec<String> {
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

    /// 自动整理布局：按章节分组，组内垂直排列，章节间水平排列。
    /// 同一章节内节点超过 max_rows 个时自动换列，避免单列过长。
    /// 节点间距根据节点实际高度累加，确保不重叠。
    pub fn auto_layout(&mut self) {
        if self.nodes.is_empty() {
            return;
        }
        // 整理策略：按章节分组分列，每列宽度取该列最宽节点自适应，
        // 避免宽节点与相邻列重叠（"该堆叠的堆叠"——同列紧凑、列间不重叠）。
        // 折叠注释时跳过注释节点（它们隐藏到下一个非注释积木中，不占布局位置）。
        let collapse = self.collapse_comments;
        let skip = |n: &BlueprintNode| collapse && n.kind == NodeKind::Comment;
        let max_rows = 6usize; // 每列最多 6 个节点，超过则换列
        let gap = 24.0; // 同列节点之间的垂直间距
        let col_gap = 40.0; // 列之间的水平间距
        let top_margin = 20.0;
        let left_margin = 20.0;

        // 第一遍：计算所有节点尺寸。
        let sizes: Vec<Vector> = self
            .nodes
            .iter()
            .map(|n| Self::node_size(&n.text))
            .collect();

        // 确定每个节点所属的列号（跳过注释）。
        // 章节节点开始新的一列；同一列超过 max_rows 个也换列。
        let mut col_of: Vec<usize> = vec![0; self.nodes.len()];
        let mut cur_col = 0usize;
        let mut row_in_col = 0usize;
        let mut any_placed = false;
        for (i, node) in self.nodes.iter().enumerate() {
            if skip(node) {
                col_of[i] = cur_col; // 注释归到当前列（不显示，位置无意义）
                continue;
            }
            if node.kind == NodeKind::Section && any_placed {
                cur_col += 1;
                row_in_col = 0;
            }
            if row_in_col >= max_rows {
                cur_col += 1;
                row_in_col = 0;
            }
            col_of[i] = cur_col;
            row_in_col += 1;
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

        // 第二遍：放置节点，同列内 y 坐标按实际高度累加。跳过注释（不占垂直空间）。
        let mut col_y = vec![top_margin; num_cols];
        for (i, node) in self.nodes.iter_mut().enumerate() {
            if skip(node) {
                continue;
            }
            let c = col_of[i];
            node.pos = Point::new(col_x[c], col_y[c]);
            col_y[c] += sizes[i].y + gap;
        }
    }

    /// 命中测试：返回指定画布坐标处的节点 ID（优先命中标题栏）。
    pub fn node_at(&self, canvas_pos: Point) -> Option<usize> {
        // 从后往前测试（后绘制的在上层）。
        for node in self.nodes.iter().rev() {
            // 折叠注释时不参与命中。
            if self.collapse_comments && node.kind == NodeKind::Comment {
                continue;
            }
            let size = Self::node_size(&node.text);
            let rect = Rectangle::new(node.pos, Size::new(size.x, size.y));
            if rect.contains(canvas_pos) {
                return Some(node.id);
            }
        }
        None
    }

    /// 命中测试：返回指定画布坐标处的输出引脚所属节点 ID。
    /// `r` 为引脚命中半径（触摸模式下应放大以便点按）。
    pub fn output_pin_at(&self, canvas_pos: Point, r: f32) -> Option<usize> {
        for node in &self.nodes {
            if self.collapse_comments && node.kind == NodeKind::Comment {
                continue;
            }
            let size = Self::node_size(&node.text);
            let rect = Rectangle::new(node.pos, Size::new(size.x, size.y));
            let pin = Point::new(rect.center_x(), rect.y + rect.height);
            if canvas_pos.distance(pin) <= r {
                return Some(node.id);
            }
        }
        None
    }

    /// 命中测试：返回指定画布坐标处的输入引脚所属节点 ID。
    /// `r` 为引脚命中半径（触摸模式下应放大以便点按）。
    pub fn input_pin_at(&self, canvas_pos: Point, r: f32) -> Option<usize> {
        for node in &self.nodes {
            if self.collapse_comments && node.kind == NodeKind::Comment {
                continue;
            }
            let size = Self::node_size(&node.text);
            let rect = Rectangle::new(node.pos, Size::new(size.x, size.y));
            let pin = Point::new(rect.center_x(), rect.y);
            if canvas_pos.distance(pin) <= r {
                return Some(node.id);
            }
        }
        None
    }

    /// 返回指定节点的入线来源节点 ID（取第一条）。
    pub fn incoming(&self, id: usize) -> Option<usize> {
        self.links.iter().find(|l| l.to == id).map(|l| l.from)
    }

    /// 返回指定节点的出线目标节点 ID（取第一条）。
    pub fn outgoing(&self, id: usize) -> Option<usize> {
        self.links.iter().find(|l| l.from == id).map(|l| l.to)
    }

    /// 折叠注释时，把一个节点（可能是注释）解析为它对应的非注释"来源"节点：
    /// 若该节点本身非注释则返回它；否则沿入线回溯到最近的非注释节点。
    pub fn resolve_from(&self, id: usize) -> Option<usize> {
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
    pub fn resolve_to(&self, id: usize) -> Option<usize> {
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
    pub fn comments_for_node(&self, id: usize) -> Vec<String> {
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
