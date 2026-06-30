//! Diagnostic reporter using codespan-reporting for rustc-style error output.
//!
//! Used at both compile-time (proc macro) and runtime (hot reload, CLI).

use codespan_reporting::diagnostic::{Diagnostic, Label};
use codespan_reporting::files::SimpleFiles;
use codespan_reporting::term::{self, Config as CodespanConfig};
use codespan_reporting::term::termcolor::{ColorChoice, StandardStream};

pub struct Reporter {
    files: SimpleFiles<String, String>,
    config: CodespanConfig,
}

impl Reporter {
    pub fn new() -> Self {
        Self { files: SimpleFiles::new(), config: CodespanConfig::default() }
    }

    pub fn add_file(&mut self, name: impl Into<String>, source: impl Into<String>) -> usize {
        self.files.add(name.into(), source.into())
    }

    pub fn emit_lex_errors(&self, file_id: usize, errors: &[crate::lexer::LexError]) {
        let writer = StandardStream::stderr(ColorChoice::Auto);
        for err in errors {
            let diag = Diagnostic::error()
                .with_message(&err.message)
                .with_labels(vec![Label::primary(file_id, err.span.start..err.span.end)]);
            let _ = term::emit(&mut writer.lock(), &self.config, &self.files, &diag);
        }
    }

    pub fn emit_parse_errors(&self, file_id: usize, errors: &[crate::parser::ParseError]) {
        let writer = StandardStream::stderr(ColorChoice::Auto);
        for err in errors {
            let mut diag = Diagnostic::error()
                .with_message(&err.message)
                .with_labels(vec![Label::primary(file_id, err.span.start..err.span.end)]);
            if let Some(hint) = &err.hint {
                diag = diag.with_notes(vec![format!("hint: {}", hint)]);
            }
            let _ = term::emit(&mut writer.lock(), &self.config, &self.files, &diag);
        }
    }

    pub fn emit_check_errors(&self, file_id: usize, errors: &[crate::checker::CheckError]) {
        let writer = StandardStream::stderr(ColorChoice::Auto);
        for err in errors {
            let severity = match err.severity {
                crate::checker::Severity::Error => codespan_reporting::diagnostic::Severity::Error,
                crate::checker::Severity::Warning => codespan_reporting::diagnostic::Severity::Warning,
                crate::checker::Severity::Note => codespan_reporting::diagnostic::Severity::Note,
            };
            let mut diag = Diagnostic::new(severity)
                .with_message(&err.message)
                .with_labels(vec![Label::primary(file_id, err.span.start..err.span.end)]);
            if let Some(hint) = &err.hint {
                diag = diag.with_notes(vec![format!("hint: {}", hint)]);
            }
            let _ = term::emit(&mut writer.lock(), &self.config, &self.files, &diag);
        }
    }

    /// Format errors as strings (for proc macro embedding and CLI output).
    pub fn format_errors(&self, file_id: usize, source: &str, errors: &[String]) -> String {
        let mut output = String::new();
        for (i, err) in errors.iter().enumerate() {
            if i > 0 { output.push('\n'); }
            output.push_str(err);
        }
        output
    }
}

impl Default for Reporter {
    fn default() -> Self { Self::new() }
}

/// Convert a LocSpan to a human-readable "line:col" string.
pub fn format_location(span: &crate::token::LocSpan) -> String {
    format!("{}:{}", span.start_linecol.line, span.start_linecol.col)
}
