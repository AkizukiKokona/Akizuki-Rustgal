//! Asset loading and caching.
//!
//! Resources are looked up under `assets/` with subdirectories:
//! - `assets/bg/` — background images
//! - `assets/characters/` — character sprites
//! - `assets/music/` — background music
//! - `assets/sound/` — sound effects
//! - `assets/title/` — title screen resources
//!
//! Missing resources produce a warning and a placeholder is used instead.

use macroquad::prelude::*;
use std::collections::HashMap;
use std::path::PathBuf;

/// Asset category (maps to a subdirectory under `assets/`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AssetKind {
    Bg,
    Character,
    Music,
    Sound,
    #[allow(dead_code)]
    Title,
}

impl AssetKind {
    fn subdir(self) -> &'static str {
        match self {
            AssetKind::Bg => "bg",
            AssetKind::Character => "characters",
            AssetKind::Music => "music",
            AssetKind::Sound => "sound",
            AssetKind::Title => "title",
        }
    }
}

/// Manages loaded textures with lazy loading and caching.
pub struct AssetManager {
    cache: HashMap<String, Option<Texture2D>>,
    base_dir: PathBuf,
}

impl AssetManager {
    /// Create a new asset manager with `assets/` as the base directory.
    pub fn new() -> Self {
        let base_dir = PathBuf::from("assets");
        Self {
            cache: HashMap::new(),
            base_dir,
        }
    }

    /// Resolve a resource name to a full path.
    /// If the name already contains a path separator, use it as-is relative to assets/.
    /// Otherwise, look in the category subdirectory.
    fn resolve_path(&self, kind: AssetKind, name: &str) -> PathBuf {
        if name.contains('/') || name.contains('\\') {
            self.base_dir.join(name)
        } else {
            self.base_dir.join(kind.subdir()).join(name)
        }
    }

    /// Load a texture by resource name. Returns None if the file doesn't exist.
    /// Logs a warning to stderr on missing resources.
    /// Automatically tries appending .png if the file isn't found as-is.
    pub async fn get_texture(&mut self, kind: AssetKind, name: &str) -> Option<Texture2D> {
        let key = format!("{:?}/{}", kind, name);
        if let Some(cached) = self.cache.get(&key) {
            return *cached;
        }

        let path = self.resolve_path(kind, name);
        let path_str = path.to_string_lossy().to_string();

        let final_path = if path.exists() {
            path_str
        } else {
            let with_png = format!("{}.png", path_str);
            if std::path::Path::new(&with_png).exists() {
                with_png
            } else {
                eprintln!("[Warning] Missing resource: {} (expected at {} or {}.png)", name, path_str, path_str);
                self.cache.insert(key, None);
                return None;
            }
        };

        match load_texture(&final_path).await {
            Ok(texture) => {
                texture.set_filter(FilterMode::Linear);
                self.cache.insert(key, Some(texture));
                Some(texture)
            }
            Err(e) => {
                eprintln!("[Warning] Failed to load texture '{}': {}", final_path, e);
                self.cache.insert(key, None);
                None
            }
        }
    }

    /// Check if a music file exists. Logs a warning if missing.
    pub fn check_music(&mut self, name: &str) -> bool {
        let path = self.resolve_path(AssetKind::Music, name);
        if !path.exists() {
            eprintln!("[Warning] Missing music: {} (expected at {})", name, path.to_string_lossy());
            false
        } else {
            true
        }
    }

    /// Check if a sound file exists. Logs a warning if missing.
    pub fn check_sound(&mut self, name: &str) -> bool {
        let path = self.resolve_path(AssetKind::Sound, name);
        if !path.exists() {
            eprintln!("[Warning] Missing sound: {} (expected at {})", name, path.to_string_lossy());
            false
        } else {
            true
        }
    }
}

impl Default for AssetManager {
    fn default() -> Self {
        Self::new()
    }
}
