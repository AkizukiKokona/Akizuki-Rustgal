//! akrs-game: Graphical game launcher using macroquad renderer.
//!
//! Reads a script file (default: scripts/demo.akrs) and launches
//! the macroquad-based graphical renderer.
//!
//! Usage:
//!   akrs-game                  — run scripts/demo.akrs
//!   akrs-game <path.akrs>      — run the specified script
//!   akrs-game --project <dir>  — run from project directory

use akrs_core::ProjectConfig;
use akrs_render::window_conf;
use akrs_runtime::Engine;
use macroquad::prelude::*;
use std::path::PathBuf;

/// Default script path if no argument is given.
const DEFAULT_SCRIPT: &str = "scripts/demo.akrs";

fn load_script_and_config() -> (String, ProjectConfig, PathBuf) {
    let args: Vec<String> = std::env::args().collect();

    let mut project_dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let mut script_path = project_dir.join(DEFAULT_SCRIPT);

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
            other => {
                script_path = PathBuf::from(other);
            }
        }
        i += 1;
    }

    // 加载项目配置
    let config = ProjectConfig::load(&project_dir);

    // 如果没有指定脚本路径，使用项目配置中的主剧本
    if args.len() <= 1 || (args.len() == 3 && (args[1] == "--project" || args[1] == "-p")) {
        script_path = project_dir.join(&config.main_script);
        // 如果主剧本不存在，尝试默认路径
        if !script_path.exists() {
            script_path = project_dir.join(DEFAULT_SCRIPT);
        }
    }

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

#[macroquad::main(window_conf())]
async fn main() {
    // Install a panic hook so that if the game crashes the console window
    // stays open long enough for the player to read the error message.
    std::panic::set_hook(Box::new(|panic_info| {
        println!("{}", panic_info);
        println!("Press Enter to exit...");
        let _ = std::io::stdin().read_line(&mut String::new());
    }));

    let (script, project_config, _project_dir) = load_script_and_config();

    let mut engine = match Engine::new(&script) {
        Ok(engine) => engine,
        Err(errors) => {
            eprintln!("[akrs-game] Script compilation failed:");
            for err in &errors {
                eprintln!("  - {:?}", err);
            }
            // Show error in window briefly, then exit
            loop {
                clear_background(Color::new(0.1, 0.0, 0.0, 1.0));
                let msg = "Script Error — check console";
                let tw = measure_text(msg, None, 30, 1.0).width;
                draw_text(msg, (screen_width() - tw) / 2.0, screen_height() / 2.0, 30.0, RED);
                if is_key_pressed(KeyCode::Escape) {
                    break;
                }
                next_frame().await;
            }
            return;
        }
    };

    // 设置项目标题
    engine.set_title(project_config.title, project_config.subtitle);

    akrs_render::run(engine).await;
}
