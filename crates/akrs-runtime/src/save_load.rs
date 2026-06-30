//! Save/load system with multiple slots.
//!
//! Each save slot stores the VM state, settings, and metadata.
//! Saves are serialized as JSON for portability and debuggability.

use akrs_core::VmState;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Metadata for a save slot (displayed in the load menu).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SaveMetadata {
    pub slot: usize,
    pub timestamp: u64,
    pub section_name: String,
    pub play_time_secs: u64,
    pub description: String,
}

/// A complete save slot.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SaveSlot {
    pub metadata: SaveMetadata,
    pub vm_state: VmState,
    pub settings: SettingsSnapshot,
}

/// Settings snapshot stored in save (subset of full Settings).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SettingsSnapshot {
    pub text_speed: f32,
    pub bgm_volume: f32,
    pub sfx_volume: f32,
}

/// Special slot number recorded in the autosave's metadata.
///
/// This value is intentionally outside the range `0..max_slots` so the
/// autosave never collides with a regular slot and is never returned by
/// `list_saves`.
const AUTOSAVE_SLOT: usize = usize::MAX;

/// Manages multiple save slots.
pub struct SaveManager {
    save_dir: PathBuf,
    max_slots: usize,
}

impl SaveManager {
    /// Create a new save manager. Creates the save directory if needed.
    pub fn new(save_dir: impl Into<PathBuf>, max_slots: usize) -> Self {
        let dir = save_dir.into();
        let _ = std::fs::create_dir_all(&dir);
        Self { save_dir: dir, max_slots }
    }

    /// Get the path for a given slot number.
    fn slot_path(&self, slot: usize) -> PathBuf {
        self.save_dir.join(format!("save_{:03}.json", slot))
    }

    /// Save current game state to a slot.
    pub fn save(
        &self,
        slot: usize,
        vm_state: VmState,
        section_name: &str,
        play_time_secs: u64,
        description: &str,
    ) -> Result<SaveMetadata, String> {
        if slot >= self.max_slots {
            return Err(format!("slot {} out of range (max {})", slot, self.max_slots));
        }

        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);

        let metadata = SaveMetadata {
            slot,
            timestamp,
            section_name: section_name.to_string(),
            play_time_secs,
            description: description.to_string(),
        };

        let save = SaveSlot {
            metadata: metadata.clone(),
            vm_state,
            settings: SettingsSnapshot {
                text_speed: 30.0,
                bgm_volume: 0.8,
                sfx_volume: 1.0,
            },
        };

        let json = serde_json::to_string_pretty(&save)
            .map_err(|e| format!("failed to serialize save: {}", e))?;

        std::fs::write(self.slot_path(slot), json)
            .map_err(|e| format!("failed to write save file: {}", e))?;

        Ok(metadata)
    }

    /// Load a save slot.
    pub fn load(&self, slot: usize) -> Result<SaveSlot, String> {
        if slot >= self.max_slots {
            return Err(format!("slot {} out of range (max {})", slot, self.max_slots));
        }

        let path = self.slot_path(slot);
        let content = std::fs::read_to_string(&path)
            .map_err(|e| format!("failed to read save file: {}", e))?;

        let save: SaveSlot = serde_json::from_str(&content)
            .map_err(|e| format!("failed to parse save file: {}", e))?;

        Ok(save)
    }

    /// Check if a slot has a save.
    pub fn has_save(&self, slot: usize) -> bool {
        self.slot_path(slot).exists()
    }

    /// Delete a save slot.
    pub fn delete(&self, slot: usize) -> Result<(), String> {
        let path = self.slot_path(slot);
        if path.exists() {
            std::fs::remove_file(&path)
                .map_err(|e| format!("failed to delete save: {}", e))?;
        }
        Ok(())
    }

    /// List all save metadata (for displaying the load menu).
    pub fn list_saves(&self) -> Vec<Option<SaveMetadata>> {
        (0..self.max_slots)
            .map(|slot| {
                let path = self.slot_path(slot);
                if path.exists() {
                    std::fs::read_to_string(&path)
                        .ok()
                        .and_then(|content| serde_json::from_str::<SaveSlot>(&content).ok())
                        .map(|save| save.metadata)
                } else {
                    None
                }
            })
            .collect()
    }

    /// Get the save directory path.
    pub fn save_dir(&self) -> &Path {
        &self.save_dir
    }

    /// Maximum number of slots.
    pub fn max_slots(&self) -> usize {
        self.max_slots
    }

    // ─── Autosave (crash-recovery) ───
    //
    // The autosave lives in its own file (`autosave.json`) and is completely
    // independent of the numbered slots. It is written when the player closes
    // the window unexpectedly and consumed (deleted) on the next launch.

    /// Path to the autosave file.
    fn autosave_path(&self) -> PathBuf {
        self.save_dir.join("autosave.json")
    }

    /// Save the current game state to the autosave slot.
    ///
    /// This does not occupy a regular slot and is not listed by `list_saves`.
    pub fn save_autosave(
        &self,
        vm_state: VmState,
        section_name: &str,
        play_time_secs: u64,
        description: &str,
    ) -> Result<SaveMetadata, String> {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);

        let metadata = SaveMetadata {
            slot: AUTOSAVE_SLOT,
            timestamp,
            section_name: section_name.to_string(),
            play_time_secs,
            description: description.to_string(),
        };

        let save = SaveSlot {
            metadata: metadata.clone(),
            vm_state,
            settings: SettingsSnapshot {
                text_speed: 30.0,
                bgm_volume: 0.8,
                sfx_volume: 1.0,
            },
        };

        let json = serde_json::to_string_pretty(&save)
            .map_err(|e| format!("failed to serialize autosave: {}", e))?;

        std::fs::write(self.autosave_path(), json)
            .map_err(|e| format!("failed to write autosave file: {}", e))?;

        Ok(metadata)
    }

    /// Load the autosave slot.
    pub fn load_autosave(&self) -> Result<SaveSlot, String> {
        let path = self.autosave_path();
        let content = std::fs::read_to_string(&path)
            .map_err(|e| format!("failed to read autosave file: {}", e))?;

        let save: SaveSlot = serde_json::from_str(&content)
            .map_err(|e| format!("failed to parse autosave file: {}", e))?;

        Ok(save)
    }

    /// Check whether an autosave exists.
    pub fn has_autosave(&self) -> bool {
        self.autosave_path().exists()
    }

    /// Delete the autosave (called on a clean exit or after it has been loaded).
    ///
    /// Succeeds (no-op) when no autosave is present.
    pub fn delete_autosave(&self) -> Result<(), String> {
        let path = self.autosave_path();
        if path.exists() {
            std::fs::remove_file(&path)
                .map_err(|e| format!("failed to delete autosave: {}", e))?;
        }
        Ok(())
    }
}

/// Format a Unix timestamp as a human-readable string.
pub fn format_timestamp(ts: u64) -> String {
    let secs = ts % 60;
    let mins = (ts / 60) % 60;
    let hours = (ts / 3600) % 24;
    let days = ts / 86400;
    if days > 0 {
        format!("{}d {:02}:{:02}:{:02}", days, hours, mins, secs)
    } else {
        format!("{:02}:{:02}:{:02}", hours, mins, secs)
    }
}

/// Format play time as "HH:MM:SS".
pub fn format_play_time(secs: u64) -> String {
    let s = secs % 60;
    let m = (secs / 60) % 60;
    let h = secs / 3600;
    format!("{:02}:{:02}:{:02}", h, m, s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use akrs_core::Value;
    use std::collections::HashMap;

    #[test]
    fn test_save_load_roundtrip() {
        let dir = std::env::temp_dir().join("akrs_test_save");
        let _ = std::fs::remove_dir_all(&dir);
        let manager = SaveManager::new(&dir, 10);

        let vm_state = VmState {
            ip: 5,
            section: 2,
            variables: {
                let mut m = HashMap::new();
                m.insert("affection".to_string(), Value::Int(10));
                m
            },
            call_stack: vec![(0, 3)],
        };

        // Save
        let metadata = manager.save(3, vm_state.clone(), "Chapter2", 3600, "Test save")
            .unwrap();
        assert_eq!(metadata.slot, 3);
        assert_eq!(metadata.section_name, "Chapter2");

        // Load
        let loaded = manager.load(3).unwrap();
        assert_eq!(loaded.vm_state.ip, 5);
        assert_eq!(loaded.vm_state.section, 2);
        assert_eq!(loaded.metadata.description, "Test save");

        // List
        let saves = manager.list_saves();
        assert_eq!(saves.len(), 10);
        assert!(saves[3].is_some());
        assert!(saves[0].is_none());

        // Delete
        manager.delete(3).unwrap();
        assert!(!manager.has_save(3));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_format_play_time() {
        assert_eq!(format_play_time(0), "00:00:00");
        assert_eq!(format_play_time(65), "00:01:05");
        assert_eq!(format_play_time(3661), "01:01:01");
    }

    #[test]
    fn test_autosave_roundtrip() {
        let dir = std::env::temp_dir().join("akrs_test_autosave_roundtrip");
        let _ = std::fs::remove_dir_all(&dir);
        let manager = SaveManager::new(&dir, 10);

        let vm_state = VmState {
            ip: 7,
            section: 1,
            variables: {
                let mut m = HashMap::new();
                m.insert("affection".to_string(), Value::Int(42));
                m
            },
            call_stack: vec![(2, 4)],
        };

        // Initially there is no autosave.
        assert!(!manager.has_autosave());
        assert!(manager.load_autosave().is_err());

        // Save the autosave.
        let metadata = manager
            .save_autosave(vm_state.clone(), "Chapter3", 7200, "Autosave progress")
            .unwrap();
        assert_eq!(metadata.section_name, "Chapter3");
        assert_eq!(metadata.play_time_secs, 7200);
        assert_eq!(metadata.description, "Autosave progress");
        assert!(manager.has_autosave());

        // Load it back and verify the state survived the round-trip.
        let loaded = manager.load_autosave().unwrap();
        assert_eq!(loaded.vm_state.ip, 7);
        assert_eq!(loaded.vm_state.section, 1);
        assert_eq!(loaded.vm_state.call_stack, vec![(2, 4)]);
        assert_eq!(
            loaded.vm_state.variables.get("affection"),
            Some(&Value::Int(42))
        );
        assert_eq!(loaded.metadata.section_name, "Chapter3");
        assert_eq!(loaded.metadata.description, "Autosave progress");

        // Delete the autosave.
        manager.delete_autosave().unwrap();
        assert!(!manager.has_autosave());
        assert!(manager.load_autosave().is_err());

        // Deleting again is a no-op (still Ok).
        assert!(manager.delete_autosave().is_ok());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_autosave_independent_from_normal_saves() {
        let dir = std::env::temp_dir().join("akrs_test_autosave_independent");
        let _ = std::fs::remove_dir_all(&dir);
        let manager = SaveManager::new(&dir, 10);

        let normal_state = VmState {
            ip: 3,
            section: 0,
            variables: HashMap::new(),
            call_stack: vec![],
        };
        let auto_state = VmState {
            ip: 99,
            section: 5,
            variables: HashMap::new(),
            call_stack: vec![(1, 1)],
        };

        // Write a normal save in slot 0 and an autosave.
        manager
            .save(0, normal_state.clone(), "Normal", 100, "Normal save")
            .unwrap();
        manager
            .save_autosave(auto_state.clone(), "Auto", 200, "Autosave")
            .unwrap();

        // Both exist independently.
        assert!(manager.has_save(0));
        assert!(manager.has_autosave());

        // list_saves must NOT surface the autosave.
        let saves = manager.list_saves();
        assert_eq!(saves.len(), 10);
        assert!(saves[0].is_some());
        assert_eq!(saves[0].as_ref().unwrap().section_name, "Normal");
        for slot in 1..10 {
            assert!(saves[slot].is_none());
        }

        // Loading slot 0 yields the normal save, not the autosave.
        let normal = manager.load(0).unwrap();
        assert_eq!(normal.metadata.section_name, "Normal");
        assert_eq!(normal.metadata.play_time_secs, 100);
        assert_eq!(normal.vm_state.ip, 3);

        // Loading the autosave yields the autosave, not the normal save.
        let auto = manager.load_autosave().unwrap();
        assert_eq!(auto.metadata.section_name, "Auto");
        assert_eq!(auto.metadata.play_time_secs, 200);
        assert_eq!(auto.vm_state.ip, 99);

        // Deleting the autosave leaves the normal save intact.
        manager.delete_autosave().unwrap();
        assert!(!manager.has_autosave());
        assert!(manager.has_save(0));
        assert_eq!(manager.load(0).unwrap().metadata.section_name, "Normal");

        // Deleting a normal save leaves a fresh autosave intact.
        manager
            .save_autosave(auto_state.clone(), "Auto2", 300, "Autosave2")
            .unwrap();
        manager.delete(0).unwrap();
        assert!(!manager.has_save(0));
        assert!(manager.has_autosave());
        let auto2 = manager.load_autosave().unwrap();
        assert_eq!(auto2.metadata.section_name, "Auto2");
        assert_eq!(auto2.metadata.play_time_secs, 300);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
