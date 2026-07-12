//! `=>` / `<=` 流程标记配对逻辑。
//!
//! 纯文本层扫描与配对，用于编辑器的配对高亮、悬停提示与大纲视图。
//! 不修改任何脚本语法或编译逻辑，仅影响视觉呈现。
//!
//! 配对规则（经典括号栈匹配，按文本顺序扫描，不考虑嵌套语义或作用域边界）：
//! - `=>`（访问子章节）入栈，`<=`（从子章节返回）弹出栈顶 `=>` 并互相配对。
//! - 栈空时遇到的 `<=` 无配对（孤立返回）；栈中剩余的 `=>` 无配对（未闭合访问）。

/// 流程标记种类。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlowKind {
    /// `=> target`：访问子章节。
    Visit,
    /// `<=`：从子章节返回。
    Return,
}

/// 文本中扫描到的一个 `=>`/`<=` 流程标记。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlowMark {
    /// 标记种类（`=>` 或 `<=`）。
    pub kind: FlowKind,
    /// 标记在全文中的起始字符索引（`=>`/`<=` 首字符）。
    pub char_start: usize,
    /// 标记在全文中的结束字符索引（exclusive，`=>`/`<=` 末尾）。
    pub char_end: usize,
    /// 标记所在行号（从 0 开始）。
    pub line: usize,
    /// `=>` 的目标章节名（`=> Sub` 中的 `Sub`）；`<=` 为 `None`。
    pub target: Option<String>,
}

/// 扫描全文，按文本顺序收集所有行首的 `=>`/`<=` 流程标记。
///
/// 判定与 lexer 一致：仅识别行首（去除前导空白后）的 `=>`/`<=`。表达式中的
/// `<=`（如 `if a <= b`）不会出现在行首语句位置，故按行首判定即可消歧。
/// 字符索引按 `char` 计数（与编辑器光标一致），非字节偏移。
pub fn scan_flow_marks(text: &str) -> Vec<FlowMark> {
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

/// 计算每个 [`FlowMark`] 的配对索引。
///
/// 返回 `pairs[i]` = 第 i 个 mark 的配对 mark 索引，无配对为 `None`。
pub fn compute_flow_pairs(marks: &[FlowMark]) -> Vec<Option<usize>> {
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

/// 返回字符索引 `char_idx` 所在的 [`FlowMark`] 索引。
///
/// 光标位置为字符间隙；`char_idx` 落在 `[char_start, char_end]`（含端点）即视为停在该标记上。
pub fn mark_at_cursor(marks: &[FlowMark], char_idx: usize) -> Option<usize> {
    marks.iter().position(|m| char_idx >= m.char_start && char_idx <= m.char_end)
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
