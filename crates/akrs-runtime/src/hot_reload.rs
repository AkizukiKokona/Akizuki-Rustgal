//! Hot reload: watch `.akrs` script files and reload them at runtime.
//!
//! Uses `notify` + `notify-debouncer-mini` for cross-platform file watching.
//! The engine polls `check_for_changes()` each frame; when a change is detected,
//! it re-reads, re-parses, and re-checks the script, then swaps the VM while
//! preserving game state (variables, current position).
//!
//! This module is behind the `hot-reload` feature and is disabled on Web
//! (wasm32), where file watching is not available.

use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

/// File watcher for hot reload.
pub struct HotReloader {
    receiver: mpsc::Receiver<std::result::Result<Vec<notify_debouncer_mini::DebouncedEvent>, notify::Error>>,
    _debouncer: notify_debouncer_mini::Debouncer<notify::RecommendedWatcher>,
    watch_path: PathBuf,
}

impl HotReloader {
    /// Create a new hot reloader watching a specific file or directory.
    pub fn new(watch_path: impl AsRef<Path>) -> Result<Self, String> {
        let path = watch_path.as_ref().to_path_buf();
        let (tx, rx) = mpsc::channel();

        let mut debouncer = notify_debouncer_mini::new_debouncer(Duration::from_millis(500), tx)
            .map_err(|e| format!("failed to create file watcher: {}", e))?;

        // Watch the parent directory (file-level watching is less reliable)
        let watch_dir = if path.is_dir() {
            path.clone()
        } else {
            path.parent()
                .map(|p| p.to_path_buf())
                .unwrap_or_else(|| PathBuf::from("."))
        };

        debouncer
            .watcher()
            .watch(&watch_dir, notify::RecursiveMode::NonRecursive)
            .map_err(|e| format!("failed to watch '{}': {}", watch_dir.display(), e))?;

        Ok(Self {
            receiver: rx,
            _debouncer: debouncer,
            watch_path: path,
        })
    }

    /// Poll for file changes. Returns `Some(new_source)` if the watched file
    /// was modified and the new content was successfully read.
    pub fn check_for_changes(&self) -> Option<String> {
        loop {
            match self.receiver.try_recv() {
                Ok(Ok(events)) => {
                    for event in &events {
                        // Check if the changed file matches our watched path
                        if self.watch_path.is_file() {
                            if event.path == self.watch_path {
                                return std::fs::read_to_string(&self.watch_path).ok();
                            }
                        } else {
                            // Watching a directory: check for .akrs files
                            if event.path.extension().and_then(|e| e.to_str()) == Some("akrs") {
                                return std::fs::read_to_string(&event.path).ok();
                            }
                        }
                    }
                }
                Ok(Err(_)) => {
                    // Watcher error: ignore and continue
                    continue;
                }
                Err(mpsc::TryRecvError::Empty) => return None,
                Err(mpsc::TryRecvError::Disconnected) => return None,
            }
        }
    }

    /// Get the path being watched.
    pub fn watch_path(&self) -> &Path {
        &self.watch_path
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn test_hot_reload_detects_changes() {
        let dir = std::env::temp_dir().join("akrs_hot_reload_test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let script_path = dir.join("test.akrs");
        std::fs::write(&script_path, "# Start\n\"Hello\"\n").unwrap();

        let reloader = HotReloader::new(&script_path).unwrap();

        // No changes yet
        assert!(reloader.check_for_changes().is_none());

        // Modify the file
        std::thread::sleep(Duration::from_millis(200));
        {
            let mut f = std::fs::File::create(&script_path).unwrap();
            write!(f, "# Start\n\"World\"\n").unwrap();
        }

        // Wait for debounce
        std::thread::sleep(Duration::from_millis(800));

        // Should detect the change
        let new_source = reloader.check_for_changes();
        assert!(new_source.is_some());
        assert!(new_source.unwrap().contains("World"));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
