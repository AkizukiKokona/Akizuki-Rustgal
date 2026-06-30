//! akrs-render: Game renderer using macroquad.
//!
//! Reads `SceneState` from `akrs-runtime` and draws:
//! - Background images
//! - Character sprites
//! - Dialogue box with typewriter text
//! - Choice buttons
//! - Title screen (Start, Load, Settings, Exit)
//! - Transition overlays (fade, dissolve)
//!
//! Missing resources are replaced with colored placeholder blocks
//! and a warning is logged to console. The game never crashes.
//!
//! # Usage
//!
//! ```ignore
//! use akrs_render::run;
//! use akrs_runtime::Engine;
//!
//! let engine = Engine::new(SCRIPT)?;
//! run(engine);
//! ```

mod assets;
mod renderer;

pub use renderer::{run, window_conf};
