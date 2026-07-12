//! `.akrs` 脚本语法高亮器。
//!
//! 实现 `iced::advanced::text::Highlighter` trait，供 `text_editor` 组件的
//! `highlight_with` 方法使用。高亮策略：
//!
//! - 按行首非空白 token 选择基础颜色（[`line_base_color`]）。
//! - `"..."` 字符串字面量分段着色为字符串色。
//! - `//` 注释整行着色为注释色（优先于其他规则）。
//!
//! 颜色常量复用 [`crate::palette`] 模块。

use iced::advanced::text::highlighter::Format as HighlightFormat;
use iced::advanced::text::Highlighter;
use iced::{Color, Font, Theme};

use crate::palette::*;

/// `.akrs` 脚本语法高亮设置（空结构体，复用全局调色板）。
#[derive(Debug, Clone, PartialEq)]
pub struct AkrsHighlightSettings;

/// `.akrs` 脚本语法高亮器：按行首 token 选择基础颜色，
/// 同时对 `"..."` 字符串字面量和 `//` 注释做分段着色。
/// 复用 [`line_base_color`] 纯逻辑函数。
pub struct AkrsHighlighter {
    /// 当前高亮到的行号（text_editor 要求跟踪）。
    current_line: usize,
}

/// 高亮输出：一个颜色值（对应 `HighlightFormat` 的 `color` 字段）。
#[derive(Debug, Clone, Copy)]
pub struct AkrsHighlight(Color);

impl Highlighter for AkrsHighlighter {
    type Settings = AkrsHighlightSettings;
    type Highlight = AkrsHighlight;
    type Iterator<'a> = std::vec::IntoIter<(std::ops::Range<usize>, Self::Highlight)>;

    fn new(_settings: &Self::Settings) -> Self {
        Self { current_line: 0 }
    }

    fn update(&mut self, _new_settings: &Self::Settings) {}

    fn change_line(&mut self, line: usize) {
        self.current_line = line;
    }

    fn highlight_line(&mut self, line: &str) -> Self::Iterator<'_> {
        let trimmed = line.trim_start();
        let base = line_base_color(trimmed);
        let leading_ws = line.len() - trimmed.len();

        // 收集高亮分片：(字节范围, 颜色)
        let mut spans: Vec<(std::ops::Range<usize>, AkrsHighlight)> = Vec::new();

        // 注释：`//` 后整行着色为注释色（优先于其他规则）
        if let Some(pos) = line.find("//") {
            // 注释前的部分用基础色
            if pos > 0 {
                spans.push((0..pos, AkrsHighlight(base)));
            }
            spans.push((pos..line.len(), AkrsHighlight(COLOR_COMMENT)));
            return spans.into_iter();
        }

        // 字符串字面量：扫描所有 "..." 区间，着色为字符串色
        let mut in_string = false;
        let mut string_start = 0usize;
        let bytes = line.as_bytes();
        let mut i = 0usize;
        let mut last_end = 0usize;
        while i < bytes.len() {
            if bytes[i] == b'"' {
                if !in_string {
                    // 字符串开始前的部分用基础色
                    if i > last_end {
                        spans.push((last_end..i, AkrsHighlight(base)));
                    }
                    string_start = i;
                    in_string = true;
                } else {
                    // 字符串结束（含闭合引号）
                    spans.push((string_start..i + 1, AkrsHighlight(COLOR_STRING)));
                    last_end = i + 1;
                    in_string = false;
                }
            }
            i += 1;
        }
        // 行末剩余部分
        if last_end < line.len() {
            let color = if in_string { COLOR_STRING } else { base };
            spans.push((last_end..line.len(), AkrsHighlight(color)));
        }

        // 如果没有任何分片（空行），返回空迭代器
        let _ = leading_ws;
        spans.into_iter()
    }

    fn current_line(&self) -> usize {
        self.current_line
    }
}

/// 将 [`AkrsHighlight`] 转换为 iced 渲染器需要的 `Format<Font>`。
pub fn akrs_highlight_to_format(h: &AkrsHighlight, _theme: &Theme) -> HighlightFormat<Font> {
    HighlightFormat {
        color: Some(h.0),
        font: None,
    }
}

/// 根据行首非空白 token 选择基础颜色（语法高亮的纯逻辑部分）。
pub fn line_base_color(trimmed: &str) -> Color {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_base_color_classification() {
        assert_eq!(line_base_color("# Title"), COLOR_SECTION);
        assert_eq!(line_base_color("=> Sub"), COLOR_FLOW);
        assert_eq!(line_base_color("<="), COLOR_FLOW);
        assert_eq!(line_base_color("@bg school"), COLOR_COMMAND);
        assert_eq!(line_base_color("+ Aki"), COLOR_DIRECTION);
        assert_eq!(line_base_color("- Aki"), COLOR_DIRECTION);
        assert_eq!(line_base_color("$x = 1"), COLOR_VARIABLE);
        assert_eq!(line_base_color("? \"q\""), COLOR_CHOICE);
        assert_eq!(line_base_color("| \"a\""), COLOR_CHOICE);
        assert_eq!(line_base_color("// comment"), COLOR_COMMENT);
        assert_eq!(line_base_color("ending \"id\""), COLOR_FLOW);
        assert_eq!(line_base_color("plain text"), COLOR_DEFAULT);
    }
}
