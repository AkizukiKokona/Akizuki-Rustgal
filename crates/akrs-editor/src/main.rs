//! akrs-editor binary entry point.
//!
//! Thin wrapper that delegates to [`akrs_editor::run_editor`]. Adding this
//! file turns `akrs-editor` from a pure library crate into a lib+bin crate,
//! so `cargo build -p akrs-editor` produces an executable
//! (`akrs-editor` / `akrs-editor.exe`) without changing the public library API.

#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

fn main() {
    std::panic::set_hook(Box::new(|panic_info| {
        let msg = format!("{}", panic_info);
        let full = format!(
            "Akizuki*Rustgal 编辑器发生致命错误（panic）\n\n{}\n\n请将此信息反馈给开发者。",
            msg
        );
        let _ = std::fs::write("editor-panic.log", &full);
        eprintln!("{}", full);
    }));
    if let Err(e) = akrs_editor::run_editor() {
        eprintln!("Editor exited with error: {e:?}");
        std::process::exit(1);
    }
}
