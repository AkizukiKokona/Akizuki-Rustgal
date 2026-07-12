//! 剧本行解析与标签函数。
//!
//! 从 egui 主分支的编辑器源码移植而来，提供以下能力：
//!
//! - 立绘指令行解析（[`parse_sprite_line`]）：从 `+ 角色 ...` 行提取预览参数。
//! - 命令行解析（[`parse_command_line`]）：从 `@cmd target ...` 行提取命令与资源名。
//! - 位置标签（[`position_label`]）：把 [`Position`] 转为可读文字。
//! - 引擎阶段标签（[`phase_label`]）：把 [`EnginePhase`] 转为可读文字。
//! - 诊断格式化（[`format_errors`]）：把编译诊断转为带严重性前缀的单行字符串。
//!
//! 依赖：`akrs_core`、`akrs_runtime`。

use akrs_core::{format_location, CompileError, ErrSeverity, Position};
use akrs_runtime::EnginePhase;

/// [`parse_sprite_line`] 的解析结果。
///
/// 字段对应立绘预览所需的全部参数。
#[derive(Debug, Clone, PartialEq)]
pub struct ParsedSpriteLine {
    /// 角色名（`+ 角色` 中的角色部分）。
    pub character_name: String,
    /// 选中资源名：有 pose 时为 pose，否则为角色名。
    pub selected: String,
    /// 横向位置百分比（默认 0.5）。
    pub x: f32,
    /// 纵向位置百分比（默认 1.0）。
    pub y: f32,
    /// 缩放系数（默认 1.0）。
    pub scale: f32,
}

/// 解析剧本中的 `+ 角色 ...` 立绘指令行，提取预览参数。
///
/// 支持的语法（与 parser.rs 的 `parse_direction` 一致）：
/// - `+ 角色`
/// - `+ 角色 (pose)`
/// - `+ 角色 at 0.45,0.56`
/// - `+ 角色 at 0.45,0.56 size 1.10`
/// - `+ 角色 居中`（中文位置词）
/// - `+ 角色 (pose) swap`（差分更换）
///
/// 无 `at`/位置词时 x 默认 0.5，y 默认 1.0；无 `size` 时 scale 默认 1.0。
/// 有 pose 时 `selected` = pose，否则 `selected` = 角色名。
///
/// 非 `+` 开头的行返回 `None`。
///
/// ## 与主分支的差异
///
/// 采用 `split_whitespace` 按 token 解析 `at`/`size`/位置词，避免旧版用
/// `trimmed.find("at ")` 子串匹配导致角色名或参数中包含 `at ` 片段时误匹配的问题。
pub fn parse_sprite_line(line: &str) -> Option<ParsedSpriteLine> {
    let trimmed = line.trim_start();
    if !trimmed.starts_with('+') {
        return None;
    }
    // 跳过 `+` 和后续空白。
    let rest = trimmed[1..].trim_start();
    if rest.is_empty() {
        return None;
    }

    // 读取角色名：到空格 / `(` / 行尾为止。
    let name_end = rest
        .find(|c: char| c.is_whitespace() || c == '(')
        .unwrap_or(rest.len());
    let character_name = rest[..name_end].to_string();
    if character_name.is_empty() {
        return None;
    }
    let mut remaining = rest[name_end..].trim_start();

    // 可选 pose：`(pose)`。
    let mut pose: Option<String> = None;
    if remaining.starts_with('(') {
        let close = remaining.find(')')?;
        let p = remaining[1..close].trim();
        if !p.is_empty() {
            pose = Some(p.to_string());
        }
        remaining = remaining[close + 1..].trim_start();
    }

    // 扫描剩余 token，提取 at / size / 位置词。
    let mut x = 0.5_f32;
    let mut y = 1.0_f32;
    let mut scale = 1.0_f32;

    let mut tokens = remaining.split_whitespace();
    while let Some(tok) = tokens.next() {
        match tok {
            "at" => {
                // `at x` 或 `at x,y`（百分比位置）。
                let val = tokens.next()?;
                let (xv, yv) = if let Some(comma) = val.find(',') {
                    let xv: f32 = val[..comma].trim().parse().ok()?;
                    let yv: f32 = val[comma + 1..].trim().parse().ok()?;
                    (xv, Some(yv))
                } else {
                    let xv: f32 = val.parse().ok()?;
                    (xv, None)
                };
                x = xv;
                if let Some(yv) = yv {
                    y = yv;
                }
            }
            "size" => {
                let val = tokens.next()?;
                let s: f32 = val.parse().ok()?;
                if s > 0.0 {
                    scale = s;
                }
            }
            // 位置词（与 Position::from_name + x_fraction 一致）。
            "left" | "居左" | "左" => x = 0.25,
            "center" | "centre" | "居中" | "中" => x = 0.5,
            "right" | "居右" | "右" => x = 0.75,
            // 其他词（enters/exits/swap/with transition/from 等）跳过。
            _ => {}
        }
    }

    let selected = pose.unwrap_or_else(|| character_name.clone());

    Some(ParsedSpriteLine {
        character_name,
        selected,
        x,
        y,
        scale,
    })
}

/// [`parse_command_line`] 的解析结果。
#[derive(Debug, Clone, PartialEq)]
pub struct ParsedCommandLine {
    /// 命令名（`@bg` / `@music` 等的 `bg` / `music` 部分）。
    pub command: String,
    /// 目标资源名（第一个参数，字符串或 ident）。
    pub resource: String,
}

/// 解析剧本中的 `@cmd target ...` 命令行，提取命令类型与目标资源名。
///
/// 取 `@` 后第一个词为命令名，第一个参数为资源名（支持 `"string"` 和 ident）。
/// 非 `@` 开头的行返回 `None`；资源名为空时返回 `None`。
pub fn parse_command_line(line: &str) -> Option<ParsedCommandLine> {
    let trimmed = line.trim_start();
    if !trimmed.starts_with('@') {
        return None;
    }
    let rest = trimmed[1..].trim_start();
    // 读取命令名（到空格为止）。
    let cmd_end = rest.find(char::is_whitespace).unwrap_or(rest.len());
    let command = rest[..cmd_end].to_string();
    let args = rest[cmd_end..].trim_start();
    // 取第一个参数作为目标资源名（可能是 ident 或 string）。
    let resource = if args.starts_with('"') {
        // 字符串参数：取引号内内容。
        let close = args[1..].find('"')?;
        args[1..1 + close].to_string()
    } else {
        // ident 参数：到空格为止。
        let end = args.find(char::is_whitespace).unwrap_or(args.len());
        args[..end].to_string()
    };
    if resource.is_empty() {
        return None;
    }
    Some(ParsedCommandLine { command, resource })
}

/// 角色位置的可读标签。
///
/// - [`Position::Left`] → `"左"`
/// - [`Position::Center`] → `"中"`
/// - [`Position::Right`] → `"右"`
/// - [`Position::Custom`] → `"自定义({x:.2})"`
pub fn position_label(p: &Position) -> String {
    match p {
        Position::Left => "左".to_string(),
        Position::Center => "中".to_string(),
        Position::Right => "右".to_string(),
        Position::Custom(x) => format!("自定义({:.2})", x),
    }
}

/// 引擎阶段的可读名称。
///
/// 返回静态字符串，便于直接用于 UI 标签。
pub fn phase_label(phase: EnginePhase) -> &'static str {
    match phase {
        EnginePhase::Title => "标题",
        EnginePhase::Running => "运行中",
        EnginePhase::Transitioning => "过渡中",
        EnginePhase::Waiting => "等待",
        EnginePhase::ChoicePending => "等待选择",
        EnginePhase::StoryEnded => "故事结束",
    }
}

/// 将编译诊断格式化为带严重性标签的单行字符串列表。
///
/// 每条诊断格式为 `[错误/警告/提示] message - 位于 loc`，有提示时追加 `（提示：hint）`。
/// 严重性前缀按 [`ErrSeverity`] 映射：`Error`→`错误`、`Warning`→`警告`、`Note`→`提示`。
pub fn format_errors(errors: &[CompileError]) -> Vec<String> {
    errors
        .iter()
        .map(|e| {
            let sev = match e.severity {
                ErrSeverity::Error => "错误",
                ErrSeverity::Warning => "警告",
                ErrSeverity::Note => "提示",
            };
            let loc = format_location(&e.span);
            match &e.hint {
                Some(h) => format!("[{}] {} - 位于 {}（提示：{}）", sev, e.message, loc, h),
                None => format!("[{}] {} - 位于 {}", sev, e.message, loc),
            }
        })
        .collect()
}
