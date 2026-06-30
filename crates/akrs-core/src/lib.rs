//! akrs-core: The .akrs script language toolkit.
//!
//! Shared between compile-time (proc macros) and runtime (hot reload).
//! Contains: lexer, parser, AST, type checker, VM, diagnostic reporter.
//!
//! This crate uses NO proc_macro types, making it usable in all contexts.

pub mod ast;
pub mod checker;
pub mod diagnostic;
pub mod lexer;
pub mod parser;
pub mod token;
pub mod value;
pub mod vm;

// Re-export key types
pub use ast::*;
pub use checker::{Checker, CheckError, Severity};
pub use diagnostic::{Reporter, format_location};
pub use lexer::{Lexer, LexError};
pub use parser::{Parser, ParseError};
pub use token::{Token, TokenKind, LocSpan, LineCol, Span};
pub use value::{Value, Type};
pub use vm::{Vm, VmEvent, VmState, VmError, ChoiceInfo};

/// Full compile pipeline: source text → checked program.
/// Returns the program and any errors (errors may include warnings).
pub fn compile(source: &str) -> (Option<Program>, Vec<CompileError>) {
    let mut errors = Vec::new();

    let tokens = match Lexer::new(source).tokenize() {
        Ok(t) => t,
        Err(lex_errs) => {
            for e in lex_errs {
                errors.push(CompileError {
                    message: e.message,
                    span: e.span,
                    hint: None,
                    severity: ErrSeverity::Error,
                });
            }
            return (None, errors);
        }
    };

    let program = match Parser::new(tokens).parse() {
        Ok(p) => p,
        Err(parse_errs) => {
            for e in parse_errs {
                errors.push(CompileError {
                    message: e.message,
                    span: e.span,
                    hint: e.hint,
                    severity: ErrSeverity::Error,
                });
            }
            return (None, errors);
        }
    };

    let check_errs = Checker::new().check(&program);
    let has_errors = check_errs.iter().any(|e| e.severity == Severity::Error);
    for e in check_errs {
        errors.push(CompileError {
            message: e.message,
            span: e.span,
            hint: e.hint,
            severity: match e.severity {
                Severity::Error => ErrSeverity::Error,
                Severity::Warning => ErrSeverity::Warning,
                Severity::Note => ErrSeverity::Note,
            },
        });
    }

    if has_errors { (None, errors) } else { (Some(program), errors) }
}

/// Full compile pipeline with resource reference checking.
/// Pass a list of known resource filenames to validate @bg/@music/@sound references.
pub fn compile_with_resources(source: &str, resources: Vec<String>) -> (Option<Program>, Vec<CompileError>) {
    let mut errors = Vec::new();

    let tokens = match Lexer::new(source).tokenize() {
        Ok(t) => t,
        Err(lex_errs) => {
            for e in lex_errs {
                errors.push(CompileError {
                    message: e.message,
                    span: e.span,
                    hint: None,
                    severity: ErrSeverity::Error,
                });
            }
            return (None, errors);
        }
    };

    let program = match Parser::new(tokens).parse() {
        Ok(p) => p,
        Err(parse_errs) => {
            for e in parse_errs {
                errors.push(CompileError {
                    message: e.message,
                    span: e.span,
                    hint: e.hint,
                    severity: ErrSeverity::Error,
                });
            }
            return (None, errors);
        }
    };

    let check_errs = Checker::new().with_resources(resources).check(&program);
    let has_errors = check_errs.iter().any(|e| e.severity == Severity::Error);
    for e in check_errs {
        errors.push(CompileError {
            message: e.message,
            span: e.span,
            hint: e.hint,
            severity: match e.severity {
                Severity::Error => ErrSeverity::Error,
                Severity::Warning => ErrSeverity::Warning,
                Severity::Note => ErrSeverity::Note,
            },
        });
    }

    if has_errors { (None, errors) } else { (Some(program), errors) }
}

/// Unified compile error from any stage.
#[derive(Debug, Clone)]
pub struct CompileError {
    pub message: String,
    pub span: LocSpan,
    pub hint: Option<String>,
    pub severity: ErrSeverity,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ErrSeverity {
    Error,
    Warning,
    Note,
}

/// Compile and create a ready-to-run VM.
pub fn compile_and_create_vm(source: &str) -> Result<Vm, Vec<CompileError>> {
    let (program, errors) = compile(source);
    match program {
        Some(p) => Ok(Vm::new(&p)),
        None => Err(errors),
    }
}

/// Compile with resource checking and create a ready-to-run VM.
pub fn compile_and_create_vm_with_resources(
    source: &str,
    resources: Vec<String>,
) -> Result<Vm, Vec<CompileError>> {
    let (program, errors) = compile_with_resources(source, resources);
    match program {
        Some(p) => Ok(Vm::new(&p)),
        None => Err(errors),
    }
}
