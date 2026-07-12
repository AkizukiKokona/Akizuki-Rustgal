//! 编辑器自定义 widget 子模块入口。
//!
//! 目前仅包含蓝图节点编辑器的 iced canvas 画布（[`blueprint_canvas`]）。
//! 后续可在此目录下继续拆分其他自定义 widget。

/// 蓝图节点编辑器的 iced canvas 画布（`canvas::Program` 实现）。
pub mod blueprint_canvas;
