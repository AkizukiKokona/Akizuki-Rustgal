//! akrs-runtime: The runtime layer for the Akizuki*Rustgal visual novel engine.
//!
//! This crate provides the game engine that executes `.akrs` scripts at
//! runtime. It is rendering-agnostic — it produces a `SceneState` snapshot
//! each frame that a renderer (e.g., `akrs_render` with macroquad) reads to
//! draw the current scene.
//!
//! # Features
//!
//! - **60fps target**: No GC pauses, no per-frame heap allocations in the hot
//!   path. Transition and typewriter updates are pure float arithmetic.
//! - **Smooth transitions**: Two-phase (Out → Swap → In) animation with cubic
//!   easing for fade, dissolve, slide, wipe, and blur effects.
//! - **Multi-slot save/load**: Serializable VM state stored as JSON files.
//! - **Settings**: Text speed, audio volume, display options.
//! - **Hot reload**: (behind `hot-reload` feature) File watcher re-reads and
//!   re-validates `.akrs` scripts on change, preserving game state.
//! - **Cross-platform**: Works on Windows, macOS, Linux, and Web (wasm32).
//!
//! # Usage
//!
//! ```ignore
//! use akrs_runtime::Engine;
//!
//! // Create engine from script source (validated at compile time by akrs!)
//! let mut engine = Engine::new(SCRIPT)?;
//!
//! // Main game loop
//! loop {
//!     let dt = get_frame_time();
//!     let events = engine.update(dt);
//!     for event in &events {
//!         // Handle events (play sounds, update UI, etc.)
//!     }
//!
//!     // Draw using engine.scene()
//!     draw_scene(engine.scene());
//!
//!     // Handle input
//!     if mouse_clicked() {
//!         let events = engine.advance();
//!     }
//! }
//! ```

pub mod engine;
pub mod game_state;
pub mod save_load;
pub mod settings;
pub mod transition;

#[cfg(feature = "hot-reload")]
pub mod hot_reload;

// Re-export key types
pub use engine::{Engine, EngineEvent, EnginePhase};
pub use game_state::{
    SceneState, BackgroundState, CharacterState, DialogueState,
    ChoicesState, ChoiceOptionState, TransitionOverlay, TransitionPhase,
};
pub use save_load::{SaveManager, SaveSlot, SaveMetadata, format_timestamp, format_play_time};
pub use settings::Settings;
pub use transition::TransitionManager;

#[cfg(feature = "hot-reload")]
pub use hot_reload::HotReloader;
