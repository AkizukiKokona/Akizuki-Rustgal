//! akrs-editor binary entry point.
//!
//! Thin wrapper that delegates to [`akrs_editor::run_editor`]. Adding this
//! file turns `akrs-editor` from a pure library crate into a lib+bin crate,
//! so `cargo build -p akrs-editor` produces an executable
//! (`akrs-editor` / `akrs-editor.exe`) without changing the public library API.

fn main() {
    if let Err(e) = akrs_editor::run_editor() {
        eprintln!("Editor exited with error: {e:?}");
        std::process::exit(1);
    }
}
