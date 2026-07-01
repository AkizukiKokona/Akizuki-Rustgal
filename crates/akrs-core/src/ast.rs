//! AST definitions for the .akrs script language.
//!
//! The AST is designed to be serializable (for compile-time embedding
//! and hot-reload sharing) and uses no proc_macro types.

use crate::token::LocSpan;
use serde::{Deserialize, Serialize};

/// A complete .akrs script program.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Program {
    pub sections: Vec<Section>,
    /// Entry section name (defaults to first section)
    pub entry: Option<String>,
}

/// A named section (delimited by # Name).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Section {
    pub name: String,
    pub nodes: Vec<Node>,
    pub span: LocSpan,
}

/// Top-level nodes within a section.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Node {
    /// Dialogue: `Character: "text"` or `Character (pose): "text"`
    Dialogue {
        speaker: String,
        pose: Option<String>,
        text: String,
        span: LocSpan,
    },
    /// Narration: standalone `"text"`
    Narration {
        text: String,
        span: LocSpan,
    },
    /// Stage command: `@bg name`, `@music file`, `@sound file`
    Command {
        cmd: String,
        args: Vec<String>,
        transition: Option<Transition>,
        span: LocSpan,
    },
    /// Stage direction: `+ Character enters/exits from/to position with transition`
    Direction {
        action: DirectionAction,
        span: LocSpan,
    },
    /// Variable operation: `$var = expr`, `$var += expr`
    VarOp {
        name: String,
        op: VarOpKind,
        expr: Expr,
        span: LocSpan,
    },
    /// Choice block: `? "prompt"` ... `?`
    Choice {
        prompt: Option<String>,
        options: Vec<ChoiceOption>,
        span: LocSpan,
    },
    /// Conditional: `if expr` ... `else` ... `end`
    Conditional {
        branches: Vec<(Expr, Vec<Node>)>,
        else_branch: Option<Vec<Node>>,
        span: LocSpan,
    },
    /// Flow navigation: `-> Target`
    Flow {
        target: String,
        span: LocSpan,
    },
    /// Visit with return: `=> Target`
    Visit {
        target: String,
        span: LocSpan,
    },
    /// Return from visit: `<=`
    Return {
        span: LocSpan,
    },
    /// Wait: `wait 2.0`
    Wait {
        seconds: f64,
        span: LocSpan,
    },
    /// End the story
    StoryEnd {
        span: LocSpan,
    },
}

/// Stage direction actions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DirectionAction {
    pub kind: DirectionKind,
    pub character: String,
    /// 立绘资源名（如 `kokonabody1`），由 `+ 角色 (立绘)` 语法指定。
    pub pose: Option<String>,
    pub position: Option<Position>,
    pub transition: Option<Transition>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum DirectionKind {
    Enter,
    Exit,
}

/// Sprite position.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub enum Position {
    Left,
    Center,
    Right,
    Custom(f32),
}

impl Position {
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "left" | "居左" | "左" => Some(Self::Left),
            "center" | "centre" | "居中" | "中" => Some(Self::Center),
            "right" | "居右" | "右" => Some(Self::Right),
            _ => None,
        }
    }
    pub fn x_fraction(&self) -> f32 {
        match self {
            Self::Left => 0.25,
            Self::Center => 0.5,
            Self::Right => 0.75,
            Self::Custom(x) => *x,
        }
    }
}

/// Transition effect.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub enum Transition {
    Fade,
    FadeBlack,
    FadeWhite,
    SlideLeft,
    SlideRight,
    SlideUp,
    SlideDown,
    Dissolve,
    WipeLeft,
    WipeRight,
    Blur,
    Instant,
}

impl Transition {
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "fade" => Some(Self::Fade),
            "fade_black" => Some(Self::FadeBlack),
            "fade_white" => Some(Self::FadeWhite),
            "slide_left" => Some(Self::SlideLeft),
            "slide_right" => Some(Self::SlideRight),
            "slide_up" => Some(Self::SlideUp),
            "slide_down" => Some(Self::SlideDown),
            "dissolve" => Some(Self::Dissolve),
            "wipe_left" => Some(Self::WipeLeft),
            "wipe_right" => Some(Self::WipeRight),
            "blur" => Some(Self::Blur),
            "instant" | "cut" => Some(Self::Instant),
            _ => None,
        }
    }
    pub fn default_duration(&self) -> f32 {
        match self {
            Self::Fade => 0.6,
            Self::FadeBlack | Self::FadeWhite => 0.8,
            Self::SlideLeft | Self::SlideRight | Self::SlideUp | Self::SlideDown => 0.5,
            Self::Dissolve => 0.8,
            Self::WipeLeft | Self::WipeRight => 0.5,
            Self::Blur => 0.6,
            Self::Instant => 0.0,
        }
    }
}

/// Variable operation kind.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub enum VarOpKind {
    Assign,
    PlusEq,
    MinusEq,
}

/// A choice option within a choice block.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChoiceOption {
    pub text: String,
    pub condition: Option<Expr>,
    pub body: Vec<Node>,
    pub span: LocSpan,
}

/// Expressions for conditions and variable values.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Expr {
    Int(i64),
    Float(f64),
    Str(String),
    Var(String),
    Bool(bool),
    Binary {
        op: BinOp,
        left: Box<Expr>,
        right: Box<Expr>,
        span: LocSpan,
    },
    Unary {
        op: UnOp,
        operand: Box<Expr>,
        span: LocSpan,
    },
}

impl Expr {
    pub fn span(&self) -> LocSpan {
        match self {
            Expr::Binary { span, .. } | Expr::Unary { span, .. } => *span,
            _ => LocSpan::dummy(),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub enum BinOp {
    Add, Sub, Mul, Div,
    Eq, Neq, Lt, Gt, LtEq, GtEq,
    And, Or,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub enum UnOp {
    Neg,
    Not,
}
