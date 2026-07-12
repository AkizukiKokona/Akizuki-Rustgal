//! 语法高亮调色板与 UI 配色常量。
//!
//! 定义编辑器语法高亮所用的不透明 RGB 颜色，以及 `=>`/`<=` 配对高亮的背景色。
//! 颜色值源自规格文档中的 RGBA 值（已丢弃 alpha 通道），与 main 分支 egui 版本保持一致。
//!
//! 注：iced 0.13 的 `Color::from_rgb8` 不是 `const fn`，无法用于 `const` 初始化；
//! 这里改用等价的 `Color::from_rgb(r / 255.0, g / 255.0, b / 255.0)`（`from_rgb` 是 const），
//! 计算结果与 `from_rgb8` 完全相同，并保留了原始的 8 位 RGB 数值。

use iced::Color;

/// `#` 章节标题颜色（淡紫，对应规格 (0.9, 0.8, 1.0)）。
pub const COLOR_SECTION: Color = Color::from_rgb(229.0 / 255.0, 204.0 / 255.0, 255.0 / 255.0);
/// `->` `=>` `<=` `~~` 流程控制颜色（橙，对应规格 (1.0, 0.6, 0.3)）。
pub const COLOR_FLOW: Color = Color::from_rgb(255.0 / 255.0, 153.0 / 255.0, 76.0 / 255.0);
/// `@` 指令颜色（绿，对应规格 (0.3, 0.8, 0.3)）。
pub const COLOR_COMMAND: Color = Color::from_rgb(76.0 / 255.0, 204.0 / 255.0, 76.0 / 255.0);
/// `+` `-` 角色方向颜色（蓝，对应规格 (0.3, 0.7, 1.0)）。
pub const COLOR_DIRECTION: Color = Color::from_rgb(76.0 / 255.0, 178.0 / 255.0, 255.0 / 255.0);
/// `$` 变量操作颜色（黄，对应规格 (1.0, 0.8, 0.3)）。
pub const COLOR_VARIABLE: Color = Color::from_rgb(255.0 / 255.0, 204.0 / 255.0, 76.0 / 255.0);
/// `?` `|` 选择分支颜色（紫，对应规格 (0.8, 0.3, 0.8)）。
pub const COLOR_CHOICE: Color = Color::from_rgb(204.0 / 255.0, 76.0 / 255.0, 204.0 / 255.0);
/// `//` 注释颜色（灰，对应规格 (0.4, 0.4, 0.4)）。
pub const COLOR_COMMENT: Color = Color::from_rgb(102.0 / 255.0, 102.0 / 255.0, 102.0 / 255.0);
/// `"..."` 字符串颜色（浅黄，对应规格 (0.9, 0.9, 0.4)）。
pub const COLOR_STRING: Color = Color::from_rgb(229.0 / 255.0, 229.0 / 255.0, 102.0 / 255.0);
/// 默认文字颜色（白）。
pub const COLOR_DEFAULT: Color = Color::from_rgb(255.0 / 255.0, 255.0 / 255.0, 255.0 / 255.0);
/// 配对高亮背景色：光标停在 `=>`/`<=` 时，该指令及其配对指令的背景（亮黄）。
pub const COLOR_PAIR_HIGHLIGHT: Color = Color::from_rgb(255.0 / 255.0, 220.0 / 255.0, 0.0 / 255.0);
