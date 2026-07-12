//! 语法高亮调色板（不透明 RGB，源自规格文档的 RGBA 值）。
//!
//! 所有颜色均为 sRGB，alpha=1.0。iced 0.13 中只有 `Color::from_rgb` /
//! `Color::from_rgba` 是 `const fn`（浮点版本），`from_rgb8` / `from_rgba8`
//! 不是 const。因此这里用 0.0–1.0 浮点形式构造，与 main 分支 egui 的
//! 0–255 取值一一对应（`x / 255.0`）。

use iced::Color;

/// 章节标题（`#` 开头）：(229, 204, 255)。
pub const COLOR_SECTION: Color = Color::from_rgb(229.0 / 255.0, 204.0 / 255.0, 1.0);
/// 流程标记（`->` `=>` `<=` `~~`）：(255, 153, 76)。
pub const COLOR_FLOW: Color = Color::from_rgb(1.0, 153.0 / 255.0, 76.0 / 255.0);
/// 指令（`@` 开头）：(76, 204, 76)。
pub const COLOR_COMMAND: Color = Color::from_rgb(76.0 / 255.0, 204.0 / 255.0, 76.0 / 255.0);
/// 角色方向（`+` `-` 开头）：(76, 178, 255)。
pub const COLOR_DIRECTION: Color = Color::from_rgb(76.0 / 255.0, 178.0 / 255.0, 1.0);
/// 变量（`$` 开头）：(255, 204, 76)。
pub const COLOR_VARIABLE: Color = Color::from_rgb(1.0, 204.0 / 255.0, 76.0 / 255.0);
/// 选择（`?` `|` 开头）：(204, 76, 204)。
pub const COLOR_CHOICE: Color = Color::from_rgb(204.0 / 255.0, 76.0 / 255.0, 204.0 / 255.0);
/// 注释（`//`）：(102, 102, 102)。
pub const COLOR_COMMENT: Color = Color::from_rgb(102.0 / 255.0, 102.0 / 255.0, 102.0 / 255.0);
/// 字符串字面量（`"..."`）：(229, 229, 102)。
pub const COLOR_STRING: Color = Color::from_rgb(229.0 / 255.0, 229.0 / 255.0, 102.0 / 255.0);
/// 默认色（普通文本）：(255, 255, 255)。
pub const COLOR_DEFAULT: Color = Color::from_rgb(1.0, 1.0, 1.0);
/// 配对高亮背景（`=>`/`<=` 配对时的高亮色）：(255, 220, 0)。
pub const COLOR_PAIR_HIGHLIGHT: Color = Color::from_rgb(1.0, 220.0 / 255.0, 0.0);
