//! Proc macro demo: validates and embeds a .akrs script at compile time.
//!
//! The `akrs!` macro reads, parses, and checks the script file during
//! compilation. If the script contains errors, `cargo build` fails with
//! precise error messages pointing to the .akrs file's line:column.
//!
//! Run with: `cargo run --example proc_macro_demo`

use akrs_macros::akrs;
use akrs_runtime::{Engine, EngineEvent, EnginePhase};

/// This constant is validated at compile time.
/// If the script has errors, compilation fails here.
const SCRIPT: &str = akrs!("scripts/demo.akrs");

fn main() {
    println!("╔══════════════════════════════════════════════════╗");
    println!("║  Akizuki*Rustgal — Proc Macro Demo              ║");
    println!("╚══════════════════════════════════════════════════╝");
    println!();
    println!("✓ Script was validated at compile time");
    println!("✓ Source length: {} bytes", SCRIPT.len());
    println!();

    // Run the validated script using the runtime engine
    let mut engine = Engine::start_running(SCRIPT)
        .expect("script should be valid (checked at compile time)");

    println!("--- Running script ---");
    println!();

    // Process initial events (advance through transitions)
    let mut events = engine.update(1.0);
    print_events(&events);

    // Advance through the story
    while engine.phase() != EnginePhase::StoryEnded {
        let advance_events = engine.advance();

        // Advance through any transitions
        let mut update_events = engine.update(0.1);
        for _ in 0..30 {
            if engine.scene().transition.is_none() {
                break;
            }
            update_events.extend(engine.update(0.1));
        }

        events = advance_events;
        events.extend(update_events);
        print_events(&events);

        // If waiting for input but no events, advance again
        if events.is_empty() && engine.phase() != EnginePhase::StoryEnded {
            continue;
        }
    }

    println!();
    println!("--- Story complete ---");
}

fn print_events(events: &[EngineEvent]) {
    for event in events {
        match event {
            EngineEvent::DialogueShown { speaker, text } => {
                println!("  {}: {}", speaker, text);
            }
            EngineEvent::NarrationShown { text } => {
                println!("  {}", text);
            }
            EngineEvent::BackgroundChanged { name } => {
                println!("  [bg: {}]", name);
            }
            EngineEvent::CharacterEntered { name, .. } => {
                println!("  [{} enters]", name);
            }
            EngineEvent::StoryEnded => {
                println!("  [story end]");
            }
            _ => {}
        }
    }
}
