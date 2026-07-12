//! akrs-editor 二进制入口。
//!
//! 薄封装，委托给 [`akrs_editor::run_editor`]。保留此文件使 `akrs-editor`
//! 从纯库 crate 变为 lib+bin crate，`cargo build -p akrs-editor` 即产出
//! 可执行文件（`akrs-editor` / `akrs-editor.exe`），不改动公开库 API。

#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

fn main() {
    // 初始化日志（env_logger 默认 warn+，设 RUST_LOG=debug 可看调试日志）。
    let _ = env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn"))
        .format_timestamp_secs()
        .try_init();

    if let Err(e) = akrs_editor::run_editor() {
        eprintln!("[editor] 运行出错: {e:?}");
        std::process::exit(1);
    }
}
