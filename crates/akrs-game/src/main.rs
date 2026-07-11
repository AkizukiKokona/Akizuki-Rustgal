//! akrs-game: Graphical game launcher using wgpu/winit renderer.
//!
//! Reads a script file (default: scripts/demo.akrs) and launches
//! the wgpu/winit-based graphical renderer.
//!
//! Usage:
//!   akrs-game                  — run scripts/demo.akrs
//!   akrs-game <path.akrs>      — run the specified script
//!   akrs-game --project <dir>  — run from project directory

#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

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
    // 初始化统一日志：默认 warn+，设 RUST_LOG=debug 可看调试日志。
    let _ = env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).try_init();

    // Install a panic hook so that if the game crashes the error message is
    // visible even under `windows_subsystem = "windows"` (no console).
    // 三管齐下：写 panic.log 文件 + 弹 Windows 消息框 + 追加到崩溃日志缓冲。
    std::panic::set_hook(Box::new(|panic_info| {
        let msg = format!("{}", panic_info);
        let full = format!(
            "Akizuki*Rustgal 发生致命错误（panic）\n\n{}\n\n请将此信息反馈给开发者。",
            msg
        );
        // 1. 写到 panic.log（当前目录），便于用户直接提交给开发者。
        let _ = std::fs::write("panic.log", &full);
        // 2. 追加到崩溃日志缓冲（供蓝屏"导出日志"按钮使用）。
        akrs_runtime::crash::push_log(full.clone());
        // 3. Windows 上弹消息框（GUI 子系统无控制台，println! 不可见）。
        akrs_render::platform::show_panic_messagebox(&full, "Akizuki*Rustgal 崩溃");
        // 4. 若已有控制台（debug_terminal 或从 cmd 启动），也打印到 stderr。
        eprintln!("{}", full);
    }));

    let (script, project_config, project_dir) = load_script_and_config();

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
