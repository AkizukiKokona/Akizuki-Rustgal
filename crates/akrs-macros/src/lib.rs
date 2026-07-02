//! akrs-macros: Procedural macros for compile-time `.akrs` script validation.
//!
//! The `akrs!` macro reads, parses, and semantically checks a `.akrs` script
//! file **at compile time**. If any errors are found, compilation fails with
//! precise error messages pointing to the `.akrs` file's line and column.
//!
//! On success, the macro expands to a `&'static str` containing the script
//! source text, which can be used to create a VM at runtime.
//!
//! # Architecture
//!
//! ```text
//! .akrs file ──→ akrs_core::Lexer ──→ akrs_core::Parser ──→ akrs_core::Checker
//!                                                                      │
//!                                              ┌───────────────────────┤
//!                                              │ Errors?               │
//!                                              ├──────────┬────────────┤
//!                                              │ Yes      │ No         │
//!                                              ▼          ▼            │
//!                                    emit_error! + abort   quote!(source) ──→ &'static str
//! ```
//!
//! The same `akrs_core` code runs at both compile-time (here, in the proc macro)
//! and runtime (in `akrs_runtime`'s hot-reload path), ensuring identical
//! validation behavior.
//!
//! # Usage
//!
//! ```ignore
//! use akrs_macros::akrs;
//!
//! // Basic: validates syntax, section jumps, variable types
//! const SCRIPT: &str = akrs!("scripts/main.akrs");
//!
//! // With resource reference checking (scans assets/ directory)
//! const SCRIPT: &str = akrs!("scripts/main.akrs", "assets/");
//! ```
//!
//! # Error Messages
//!
//! Errors are emitted at the macro call site, with the `.akrs` file location
//! included in the message text:
//!
//! ```text
//! error: scripts/main.akrs:5:3: undefined section: 'NonExistent'
//!   hint: did you mean 'Day2'?
//! ```
//!
//! This is a fundamental limitation of proc macros: spans can only point to
//! the `.rs` file being compiled, not external files. The `.akrs` line:col
//! is embedded in the message for precise diagnosis.

use proc_macro::TokenStream;
use proc_macro_error2::{abort_if_dirty, emit_error, emit_warning, proc_macro_error};
use quote::quote;
use std::path::{Path, PathBuf};
use syn::{parse_macro_input, LitStr};

/// Parsed input for the `akrs!` macro.
///
/// Accepts:
/// - `akrs!("path/to/script.akrs")` — basic validation
/// - `akrs!("path/to/script.akrs", "path/to/assets/")` — with resource checking
struct AkrsInput {
    script_path: LitStr,
    assets_dir: Option<LitStr>,
}

impl syn::parse::Parse for AkrsInput {
    fn parse(input: syn::parse::ParseStream) -> syn::Result<Self> {
        let script_path: LitStr = input.parse()?;
        let assets_dir = if input.peek(syn::Token![,]) {
            let _: syn::Token![,] = input.parse()?;
            Some(input.parse()?)
        } else {
            None
        };
        Ok(Self { script_path, assets_dir })
    }
}

/// Recursively scan a directory for resource filenames.
/// Returns both bare filenames and relative paths for flexible matching.
fn scan_resources(dir: &Path) -> Vec<String> {
    let mut resources = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                // Recurse into subdirectories
                let sub_resources = scan_resources(&path);
                resources.extend(sub_resources);
                // Also add the directory-relative path
                if let Ok(rel) = path.strip_prefix(dir)
                    && let Some(rel_str) = rel.to_str()
                {
                    resources.push(rel_str.to_string());
                }
            } else if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                // Add bare filename
                resources.push(name.to_string());
                // Also add path relative to the assets root
                if let Ok(rel) = path.strip_prefix(dir)
                    && let Some(rel_str) = rel.to_str()
                {
                    resources.push(rel_str.to_string());
                }
            }
        }
    }
    resources
}

/// The `akrs!` macro: compile-time validation of `.akrs` scripts.
///
/// Reads, parses, and checks a `.akrs` script file at compile time.
/// On success, expands to a `&'static str` containing the validated source.
///
/// # Parameters
///
/// 1. **script_path** (required): Path to the `.akrs` file, relative to
///    the crate's `Cargo.toml` directory (`CARGO_MANIFEST_DIR`).
/// 2. **assets_dir** (optional): Path to the assets directory. When provided,
///    the macro scans it and validates that all `@bg`, `@music`, and `@sound`
///    resource references point to existing files. Missing resources produce
///    warnings, not errors.
///
/// # Compile-Time Checks
///
/// 1. **Lexical**: invalid characters, unterminated strings
/// 2. **Syntactic**: malformed section headers, missing block delimiters
/// 3. **Section references**: `->` and `=>` targets must exist
/// 4. **Variable types**: `+=` and `-=` operands must have compatible types
/// 5. **Variable definitions**: using an undefined variable is an error
/// 6. **Resource references**: `@bg`/`@music`/`@sound` args checked against
///    the assets directory (warnings only)
/// 7. **Choice conditions**: must evaluate to `bool`
/// 8. **Conditional expressions**: `if` conditions must evaluate to `bool`
///
/// # Errors
///
/// If the script contains errors, compilation fails with messages like:
///
/// ```text
/// error: scripts/main.akrs:5:3: undefined section: 'NonExistent'
///   hint: did you mean 'Day2'?
/// ```
///
/// Multiple errors are collected and emitted together (the macro does not
/// abort on the first error).
#[proc_macro]
#[proc_macro_error]
pub fn akrs(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as AkrsInput);
    let rel_path = input.script_path.value();
    let call_span = input.script_path.span();

    // Resolve path relative to CARGO_MANIFEST_DIR
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR")
        .unwrap_or_else(|_| ".".to_string());
    let full_path = PathBuf::from(&manifest_dir).join(&rel_path);

    // Read the .akrs file
    let source = match std::fs::read_to_string(&full_path) {
        Ok(s) => s,
        Err(e) => {
            emit_error!(
                call_span,
                "failed to read .akrs file '{}': {}",
                full_path.display(),
                e
            );
            // Return empty string to allow compilation to continue
            // (errors will have been emitted)
            return quote! { "" }.into();
        }
    };

    // Optionally scan resources for reference checking
    let resources = if let Some(assets_lit) = &input.assets_dir {
        let assets_rel = assets_lit.value();
        let assets_full = PathBuf::from(&manifest_dir).join(&assets_rel);
        if !assets_full.exists() {
            emit_warning!(
                assets_lit.span(),
                "assets directory '{}' does not exist; resource checking skipped",
                assets_full.display()
            );
            Vec::new()
        } else {
            scan_resources(&assets_full)
        }
    } else {
        Vec::new()
    };

    // Compile: lex + parse + check (using shared akrs_core)
    let (_program, errors) = if resources.is_empty() {
        akrs_core::compile(&source)
    } else {
        akrs_core::compile_with_resources(&source, resources)
    };

    // Emit all errors and warnings with .akrs file:line:col information
    for err in &errors {
        let file_loc = format!(
            "{}:{}:{}",
            rel_path,
            err.span.start_linecol.line,
            err.span.start_linecol.col
        );

        let full_msg = match &err.hint {
            Some(hint) => format!("{} — {} (hint: {})", file_loc, err.message, hint),
            None => format!("{} — {}", file_loc, err.message),
        };

        match err.severity {
            akrs_core::ErrSeverity::Error => {
                emit_error!(call_span, "{}", full_msg);
            }
            akrs_core::ErrSeverity::Warning => {
                emit_warning!(call_span, "{}", full_msg);
            }
            akrs_core::ErrSeverity::Note => {
                // Notes are attached to the preceding error/warning
                // We emit them as warnings for visibility
                emit_warning!(call_span, "note: {}", full_msg);
            }
        }
    }

    // Abort if any errors were emitted
    abort_if_dirty();

    // If we reach here, the script is valid.
    // Expand to the source text as a &'static str.
    // quote! properly escapes the string content as a string literal.
    let expanded = quote! {
        #source
    };

    expanded.into()
}

/// The `akrs_include!` macro: embed multiple `.akrs` files at compile time.
///
/// Takes a glob pattern (relative to CARGO_MANIFEST_DIR) and returns a
/// `Vec<(&'static str, &'static str)>` of (filename, source) pairs.
///
/// All files are validated at compile time.
///
/// # Usage
///
/// ```ignore
/// use akrs_macros::akrs_include;
///
/// let scripts: Vec<(&'static str, &'static str)> = akrs_include!("scripts/*.akrs");
/// ```
#[proc_macro]
#[proc_macro_error]
pub fn akrs_include(input: TokenStream) -> TokenStream {
    let pattern_lit = parse_macro_input!(input as LitStr);
    let pattern = pattern_lit.value();
    let call_span = pattern_lit.span();

    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR")
        .unwrap_or_else(|_| ".".to_string());
    let full_pattern = PathBuf::from(&manifest_dir).join(&pattern);

    // Simple glob: handle directory + extension pattern
    // We support "dir/*.akrs" style patterns
    let pattern_str = full_pattern.to_string_lossy().to_string();

    // Find files matching the pattern
    let files = match find_akrs_files(&pattern_str) {
        Ok(f) => f,
        Err(e) => {
            emit_error!(call_span, "failed to scan for .akrs files: {}", e);
            return quote! { Vec::new() }.into();
        }
    };

    if files.is_empty() {
        emit_warning!(call_span, "no .akrs files found matching pattern '{}'", pattern);
        return quote! { Vec::new() }.into();
    }

    let mut names: Vec<String> = Vec::new();
    let mut sources: Vec<String> = Vec::new();

    for file_path in &files {
        let source = match std::fs::read_to_string(file_path) {
            Ok(s) => s,
            Err(e) => {
                emit_error!(call_span, "failed to read '{}': {}", file_path.display(), e);
                continue;
            }
        };

        // Validate each file
        let (_, errors) = akrs_core::compile(&source);
        let display_name = file_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown")
            .to_string();

        for err in &errors {
            let file_loc = format!(
                "{}:{}:{}",
                display_name,
                err.span.start_linecol.line,
                err.span.start_linecol.col
            );

            let full_msg = match &err.hint {
                Some(hint) => format!("{} — {} (hint: {})", file_loc, err.message, hint),
                None => format!("{} — {}", file_loc, err.message),
            };

            match err.severity {
                akrs_core::ErrSeverity::Error => emit_error!(call_span, "{}", full_msg),
                akrs_core::ErrSeverity::Warning => emit_warning!(call_span, "{}", full_msg),
                akrs_core::ErrSeverity::Note => {}
            }
        }

        names.push(display_name);
        sources.push(source);
    }

    abort_if_dirty();

    // Generate Vec of (&str, &str) tuples
    let expanded = quote! {
        vec![
            #((#names, #sources),)*
        ]
    };

    expanded.into()
}

/// Find .akrs files matching a glob-like pattern.
/// Supports: "dir/*.akrs" and "dir/**/*.akrs"
fn find_akrs_files(pattern: &str) -> std::io::Result<Vec<PathBuf>> {
    let path = Path::new(pattern);

    // Extract directory and check if it's a simple "*.akrs" pattern
    let (dir, recursive) = if pattern.contains("**") {
        let dir = pattern.replace("**", "").replace("/*.akrs", "").replace("//", "/");
        (PathBuf::from(&dir), true)
    } else if pattern.ends_with("*.akrs") {
        let dir = path.parent().unwrap_or(Path::new("."));
        (dir.to_path_buf(), false)
    } else {
        // Single file
        if path.exists() {
            return Ok(vec![path.to_path_buf()]);
        }
        return Ok(vec![]);
    };

    let mut result = Vec::new();
    if recursive {
        collect_akrs_recursive(&dir, &mut result)?;
    } else {
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                let p = entry.path();
                if p.extension().and_then(|e| e.to_str()) == Some("akrs") {
                    result.push(p);
                }
            }
        }
    }
    result.sort();
    Ok(result)
}

fn collect_akrs_recursive(dir: &Path, result: &mut Vec<PathBuf>) -> std::io::Result<()> {
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                collect_akrs_recursive(&path, result)?;
            } else if path.extension().and_then(|e| e.to_str()) == Some("akrs") {
                result.push(path);
            }
        }
    }
    Ok(())
}
