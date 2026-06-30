//! Token definitions for the .akrs script language.
//!
//! The .akrs syntax uses symbolic markers instead of keywords:
//! - `#` for section headers (not `label`)
//! - `->` for flow navigation (not `jump`)
//! - `=>` for visit with return (not `call`)
//! - `<=` for return from visit
//! - `~~` for story end
//! - `?` for choice blocks (not `menu`)
//! - `|` for choice options
//! - `@` for stage commands
//! - `+` for character entrance
//! - `-` for character exit
//! - `$` for variable operations

use serde::{Deserialize, Serialize};
use std::fmt;

/// Byte range in source text.
pub type Span = std::ops::Range<usize>;

/// Source location: line (1-based) and column (1-based).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct LineCol {
    pub line: usize,
    pub col: usize,
}

/// A span with line/col information for precise error reporting.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct LocSpan {
    pub start: usize,
    pub end: usize,
    pub start_linecol: LineCol,
    pub end_linecol: LineCol,
}

impl LocSpan {
    pub fn new(start: usize, end: usize, start_lc: LineCol, end_lc: LineCol) -> Self {
        Self { start, end, start_linecol: start_lc, end_linecol: end_lc }
    }
    pub fn dummy() -> Self {
        Self { start: 0, end: 0, start_linecol: LineCol { line: 1, col: 1 }, end_linecol: LineCol { line: 1, col: 1 } }
    }
}

/// Token kinds in the .akrs language.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum TokenKind {
    // Structural markers
    /// `#` section header delimiter
    SectionMark,
    /// `->` flow navigation
    FlowArrow,
    /// `=>` visit (with return)
    VisitArrow,
    /// `<=` return from visit (also LtEq in expressions; disambiguated by parser)
    ReturnMark,
    /// `~~` story end
    StoryEnd,

    // Block delimiters
    /// `?` opens/closes choice block
    Question,
    /// `|` choice option separator
    Pipe,

    // Command prefixes
    /// `@` stage command
    At,
    /// `+` character entrance direction
    Plus,
    /// `$` variable reference
    Dollar,

    // Literals
    String(String),
    Integer(i64),
    Float(f64),
    Ident(String),

    // Operators
    Colon,
    LParen,
    RParen,
    Assign,      // =
    PlusEq,      // +=
    MinusEq,     // -=
    EqEq,        // ==
    NotEq,       // !=
    Lt, Gt, LtEq, GtEq,
    Plus2,       // arithmetic + (in expressions)
    Minus2,      // arithmetic - (in expressions)
    Star,
    Slash,
    And,         // and
    Or,          // or
    Not,         // not

    // Keywords (minimal, not Ren'Py-like)
    If,
    Else,
    End,
    Wait,

    // Formatting
    Newline,
    Comma,
    Eof,
}

/// A token with kind and location span.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Token {
    pub kind: TokenKind,
    pub span: LocSpan,
}

impl Token {
    pub fn new(kind: TokenKind, span: LocSpan) -> Self {
        Self { kind, span }
    }
}

impl fmt::Display for TokenKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TokenKind::SectionMark => write!(f, "#"),
            TokenKind::FlowArrow => write!(f, "->"),
            TokenKind::VisitArrow => write!(f, "=>"),
            TokenKind::ReturnMark => write!(f, "<="),
            TokenKind::StoryEnd => write!(f, "~~"),
            TokenKind::Question => write!(f, "?"),
            TokenKind::Pipe => write!(f, "|"),
            TokenKind::At => write!(f, "@"),
            TokenKind::Plus => write!(f, "+"),
            TokenKind::Dollar => write!(f, "$"),
            TokenKind::String(s) => write!(f, "\"{}\"", s),
            TokenKind::Integer(i) => write!(f, "{}", i),
            TokenKind::Float(v) => write!(f, "{}", v),
            TokenKind::Ident(s) => write!(f, "{}", s),
            TokenKind::Colon => write!(f, ":"),
            TokenKind::LParen => write!(f, "("),
            TokenKind::RParen => write!(f, ")"),
            TokenKind::Assign => write!(f, "="),
            TokenKind::PlusEq => write!(f, "+="),
            TokenKind::MinusEq => write!(f, "-="),
            TokenKind::EqEq => write!(f, "=="),
            TokenKind::NotEq => write!(f, "!="),
            TokenKind::Lt => write!(f, "<"),
            TokenKind::Gt => write!(f, ">"),
            TokenKind::LtEq => write!(f, "<="),
            TokenKind::GtEq => write!(f, ">="),
            TokenKind::Plus2 => write!(f, "+"),
            TokenKind::Minus2 => write!(f, "-"),
            TokenKind::Star => write!(f, "*"),
            TokenKind::Slash => write!(f, "/"),
            TokenKind::And => write!(f, "and"),
            TokenKind::Or => write!(f, "or"),
            TokenKind::Not => write!(f, "not"),
            TokenKind::If => write!(f, "if"),
            TokenKind::Else => write!(f, "else"),
            TokenKind::End => write!(f, "end"),
            TokenKind::Wait => write!(f, "wait"),
            TokenKind::Newline => write!(f, "newline"),
            TokenKind::Comma => write!(f, ","),
            TokenKind::Eof => write!(f, "eof"),
        }
    }
}
