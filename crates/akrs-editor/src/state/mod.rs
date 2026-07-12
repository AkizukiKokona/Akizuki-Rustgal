//! 编辑器子状态模块入口。
//!
//! 目前仅包含蓝图节点编辑器的纯数据与算法（[`blueprint`]）。
//! 后续可在此目录下继续拆分文件/引擎/打包/翻译等子状态。

/// 蓝图节点编辑器状态（纯数据 + 算法，不含 UI）。
pub mod blueprint;

// 重导出常用类型，便于外部以 `state::BlueprintState` 形式引用。
pub use blueprint::{BlueprintLink, BlueprintNode, BlueprintState, NodeKind};
