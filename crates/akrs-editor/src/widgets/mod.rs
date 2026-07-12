//! 编辑器自定义 widget 模块入口。
//!
//! 目前仅包含蓝图节点画布（[`blueprint_canvas`]），后续可在此目录下继续
//! 拆分其他自定义 widget。

/// 蓝图节点画布：基于 `iced::canvas::Program` 实现，负责节点/连线/引脚的
/// 自定义绘制与鼠标交互（拖拽节点、拉线、平移画布、右键菜单等）。
pub mod blueprint_canvas;

pub use blueprint_canvas::{BlueprintProgram, BlueprintMsg};
