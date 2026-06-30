//! akrs-game: Graphical game launcher using macroquad renderer.
//!
//! Reads a script file (default: scripts/demo.akrs) and launches
//! the macroquad-based graphical renderer.
//!
//! Usage:
//!   akrs-game                  — run scripts/demo.akrs
//!   akrs-game <path.akrs>      — run the specified script

use akrs_render::window_conf;
use akrs_runtime::Engine;
use macroquad::prelude::*;

/// Default script path if no argument is given.
const DEFAULT_SCRIPT: &str = "scripts/demo.akrs";

fn load_script() -> String {
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| DEFAULT_SCRIPT.to_string());

    match std::fs::read_to_string(&path) {
        Ok(source) => {
            println!("[akrs-game] Loaded script: {}", path);
            source
        }
        Err(e) => {
            eprintln!("[akrs-game] Failed to read '{}': {}", path, e);
            eprintln!("[akrs-game] Using a minimal fallback script.");
            "# Fallback\n=> Start\nHello!\n<= End\n".to_string()
        }
    }
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

    let script = load_script();

    let engine = match Engine::new(&script) {
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

    akrs_render::run(engine).await;
}
