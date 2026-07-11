//! akrs-game: Graphical game launcher using wgpu/winit renderer.
//!
//! Reads a script file (default: scripts/demo.akrs) and launches
//! the wgpu/winit-based graphical renderer.
//!
//! Usage:
//!   akrs-game                  — run scripts/demo.akrs
//!   akrs-game <path.akrs>      — run the specified script
//!   akrs-game --project <dir>  — run from project directory

// [DEBUG-TEMP] 临时关闭 windows_subsystem = "windows"，强制显示控制台窗口
// 以便在 Windows 上看到完整的 println!/eprintln! 日志输出。调试完成后恢复。
// #![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

use akrs_core::ProjectConfig;
use akrs_runtime::{Engine, crash::{self, CrashInfo, error_code}};
use std::path::PathBuf;

/// Default script path if no argument is given.
const DEFAULT_SCRIPT: &str = "scripts/demo.akrs";

fn load_script_and_config() -> (String, ProjectConfig, PathBuf) {
    let args: Vec<String> = std::env::args().collect();

    let mut project_dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    // 显式指定的剧本路径（--script），优先级最高，高于 project.json 的 main_script。
    let mut explicit_script: Option<PathBuf> = None;

    // 解析命令行参数
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--project" | "-p" => {
                if i + 1 < args.len() {
                    project_dir = PathBuf::from(&args[i + 1]);
                    i += 2;
                    continue;
                }
            }
            "--script" | "-s" => {
                // 显式剧本路径：编辑器预览时传入，确保运行的就是用户当前编辑的文件。
                if i + 1 < args.len() {
                    explicit_script = Some(PathBuf::from(&args[i + 1]));
                    i += 2;
                    continue;
                }
            }
            other => {
                // 兼容旧用法：裸位置参数当作剧本路径。
                explicit_script = Some(PathBuf::from(other));
            }
        }
        i += 1;
    }

    // 加载项目配置
    let config = ProjectConfig::load(&project_dir);

    // 确定最终剧本路径，优先级：--script 显式参数 > project.json main_script > demo 回退
    let script_path = if let Some(explicit) = explicit_script.clone() {
        explicit
    } else {
        let candidate = project_dir.join(&config.main_script);
        if candidate.exists() {
            candidate
        } else {
            project_dir.join(DEFAULT_SCRIPT)
        }
    };

    // 切换到项目目录，使资源路径正确解析
    let _ = std::env::set_current_dir(&project_dir);

    let source = match std::fs::read_to_string(&script_path) {
        Ok(source) => {
            println!("[akrs-game] Loaded script: {}", script_path.display());
            source
        }
        Err(e) => {
            eprintln!("[akrs-game] Failed to read '{}': {}", script_path.display(), e);
            eprintln!("[akrs-game] Using a minimal fallback script.");
            "# Fallback\n=> Start\nHello!\n<= End\n".to_string()
        }
    };

    (source, config, project_dir)
}

fn main() {
    // [DEBUG-TEMP] 强制分配控制台（Windows），确保所有日志可见
    #[cfg(target_os = "windows")]
    {
        let _ = akrs_render::platform::try_alloc_console();
    }

    // [DEBUG-TEMP] 日志级别强制设为 trace，同时输出到 stderr（控制台）和 debug.log 文件。
    // 用 env_logger 输出到 stderr，format 闭包内同时 append 到 debug.log。
    use std::io::Write;
    let log_file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open("debug.log")
        .ok();
    let file_for_log = log_file.map(std::sync::Mutex::new);
    let mut builder = env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("trace"));
    builder.format(move |buf, record| {
        let ts = buf.timestamp_millis();
        let line = format!("{} [{}] {}\n", ts, record.level(), record.args());
        // stderr（控制台）
        let _ = std::io::stderr().write_all(line.as_bytes());
        // debug.log 文件
        if let Some(ref mtx) = file_for_log {
            if let Ok(mut f) = mtx.lock() {
                let _ = f.write_all(line.as_bytes());
                let _ = f.flush();
            }
        }
        Ok(())
    });
    builder.target(env_logger::Target::Stderr);
    let _ = builder.try_init();
    log::info!("[DEBUG-TEMP] === akrs-game 启动（调试模式：trace 级别日志 + debug.log）===");

    // panic hook：保留原有三管齐下，但额外打印完整 backtrace。
    std::panic::set_hook(Box::new(|panic_info| {
        let msg = format!("{}", panic_info);
        let backtrace = std::backtrace::Backtrace::force_capture();
        let full = format!(
            "Akizuki*Rustgal 发生致命错误（panic）\n\n{}\n\nBacktrace:\n{}\n\n请将此信息反馈给开发者。",
            msg, backtrace
        );
        let _ = std::fs::write("panic.log", &full);
        akrs_runtime::crash::push_log(full.clone());
        akrs_render::platform::show_panic_messagebox(&full, "Akizuki*Rustgal 崩溃");
        eprintln!("{}", full);
    }));

    log::info!("[DEBUG-TEMP] main: 调用 load_script_and_config()");
    let (script, project_config, project_dir) = load_script_and_config();
    log::info!("[DEBUG-TEMP] main: 剧本加载完成，长度 {} 字节", script.len());
    log::info!("[DEBUG-TEMP] main: project_dir = {}", project_dir.display());

    // 编译剧本：失败时不退出，而是降级为"仅标题页"模式。
    // 玩家仍可进入标题页、修改设置，但点击"开始游戏/读档/继续游戏"时
    // 会弹出剧本错误警告（四国语言）。自动恢复在此模式下失效。
    let mut engine = match Engine::new(&script) {
        Ok(engine) => engine,
        Err(errors) => {
            eprintln!("[akrs-game] Script compilation failed:");
            crash::push_log("[akrs-game] 剧本编译失败：");
            for err in &errors {
                let line = format!("  - {:?}", err);
                eprintln!("{}", line);
                crash::push_log(line);
            }
            eprintln!("[akrs-game] 进入仅标题页降级模式（将先弹出蓝屏错误界面）。");
            crash::push_log("[akrs-game] 进入仅标题页降级模式（将先弹出蓝屏错误界面）。");
            // 收集错误摘要供标题页警告对话框显示。
            let error_summary = errors
                .iter()
                .map(|e| {
                    let loc = akrs_core::format_location(&e.span);
                    match &e.hint {
                        Some(h) => format!("{}: {} (hint: {})", loc, e.message, h),
                        None => format!("{}: {}", loc, e.message),
                    }
                })
                .collect::<Vec<_>>()
                .join("\n");
            let mut eng = Engine::new_title_only(error_summary);
            // 设置蓝屏崩溃信息：剧本编译错误，可继续（继续后进入降级标题页）。
            eng.set_crash_info(Some(CrashInfo {
                module_key: "error.module.script_compiler",
                code: error_code::SCRIPT_COMPILE,
                can_continue: true,
            }));
            eng
        }
    };

    // 设置项目标题（克隆字段，避免 project_config 被部分移动后无法再借用）
    engine.set_title(project_config.title.clone(), project_config.subtitle.clone());

    // 加载玩家设置（包括语言偏好）
    engine.load_settings();

    // 根据设置加载对应语言的翻译文件
    let translations_dir = project_dir.join("assets").join("scripts").join("languages");
    let effective_lang = engine.settings().effective_language();
    if translations_dir.exists() {
        engine.load_language(&effective_lang, &translations_dir);
        // 同时加载 UI 翻译文件（若存在）；UI 语言独立于剧本语言
        let effective_ui_lang = engine.settings().effective_ui_language();
        engine.load_ui_language(&effective_ui_lang, &translations_dir);
    }

    akrs_render::run(engine, &project_config);
}
