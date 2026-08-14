//! Android bootstrap for akrs-game.
//!
//! The APK ships the game bundle inside its `assets` directory (same layout
//! as a desktop project root: `assets/`, `scripts/`, `project.json`,
//! `kokona.png`). Android processes cannot read the APK through `std::fs`,
//! so before the game starts we copy every bundled file into the app's
//! internal storage directory and make it the working directory.

use std::path::Path;

/// Extract the bundled files (listed in `manifest.txt`) into the internal
/// storage directory, then switch the working directory there.
///
/// Called from `quad_main` before the macroquad window is created, so it
/// must not touch any rendering state. Asset reads go through miniquad's
/// AAssetManager-based helper which works without a GL context.
pub fn setup() {
    let internal = miniquad::native::android::get_internal_storage_path();
    println!("[akrs-game] internal storage: {}", internal);

    let manifest_bytes = match miniquad::native::android::load_asset_bytes("manifest.txt") {
        Some(bytes) => bytes,
        None => {
            eprintln!("[akrs-game] manifest.txt not found in APK assets, skipping extraction");
            let _ = std::env::set_current_dir(&internal);
            return;
        }
    };

    // Stamp the extracted version with a hash of the manifest, so that
    // updating the APK with changed files re-extracts automatically.
    let stamp = format!(".akrs_extracted_{:08x}", fnv1a(&manifest_bytes));
    let stamp_path = Path::new(&internal).join(&stamp);
    if stamp_path.exists() {
        println!("[akrs-game] assets already extracted, reusing internal storage");
        let _ = std::env::set_current_dir(&internal);
        return;
    }

    let text = String::from_utf8_lossy(&manifest_bytes);
    let mut failed = 0usize;
    let mut extracted = 0usize;
    for line in text.lines() {
        let rel = line.trim();
        if rel.is_empty() {
            continue;
        }
        let data = match miniquad::native::android::load_asset_bytes(rel) {
            Some(d) => d,
            None => {
                eprintln!("[akrs-game] extract failed (asset missing): {}", rel);
                failed += 1;
                continue;
            }
        };
        let dest = Path::new(&internal).join(rel);
        if let Some(parent) = dest.parent() {
            if std::fs::create_dir_all(parent).is_err() {
                eprintln!("[akrs-game] extract failed (mkdir): {}", parent.display());
                failed += 1;
                continue;
            }
        }
        if std::fs::write(&dest, &data).is_err() {
            eprintln!("[akrs-game] extract failed (write): {}", rel);
            failed += 1;
        } else {
            extracted += 1;
        }
    }

    println!("[akrs-game] extracted {} files ({} failed)", extracted, failed);
    if failed == 0 {
        let _ = std::fs::write(&stamp_path, b"1");
    }

    // The game runs with cwd = internal storage so all relative paths
    // (assets/, scripts/, saves/, project.json, kokona.png) just work.
    let _ = std::env::set_current_dir(&internal);
}

/// FNV-1a 32-bit hash (fast, deterministic; good enough for a stamp).
fn fnv1a(data: &[u8]) -> u32 {
    let mut hash: u32 = 0x811c_9dc5;
    for &b in data {
        hash ^= b as u32;
        hash = hash.wrapping_mul(0x0100_0193);
    }
    hash
}
