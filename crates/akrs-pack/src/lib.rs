//! akrs-pack: Packaging tool for Akizuki*Rustgal games.
//!
//! Copies the compiled binary and `assets/` directory to an output folder,
//! generates platform-appropriate launch scripts, and produces a distributable
//! package.
//!
//! # Usage
//!
//! ```ignore
//! use akrs_pack::PackConfig;
//!
//! let config = PackConfig {
//!     binary_path: "target/release/akrs".into(),
//!     assets_dir: "assets".into(),
//!     output_dir: "dist/AkizukiRustgal".into(),
//!     release: true,
//!     target: None, // or Some("x86_64-pc-windows-gnu")
//! };
//! akrs_pack::pack(&config)?;
//! ```

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// Configuration for the packaging operation.
#[derive(Debug, Clone)]
pub struct PackConfig {
    /// Path to the compiled game binary.
    pub binary_path: PathBuf,
    /// Path to the assets directory (usually "assets").
    pub assets_dir: PathBuf,
    /// Output directory for the packaged game.
    pub output_dir: PathBuf,
    /// Whether this is a release build (affects optimization info in scripts).
    pub release: bool,
    /// Optional target triple (e.g. "x86_64-pc-windows-gnu").
    /// If None, uses the host target.
    pub target: Option<String>,
}

/// Result of a successful pack operation.
#[derive(Debug)]
pub struct PackResult {
    /// Path to the output directory.
    pub output_dir: PathBuf,
    /// Path to the copied binary.
    pub binary_path: PathBuf,
    /// Path to the copied assets directory.
    pub assets_path: PathBuf,
    /// Path to the generated launch script.
    pub launch_script: PathBuf,
    /// Number of files copied.
    pub files_copied: usize,
}

/// Errors that can occur during packaging.
#[derive(Debug)]
pub enum PackError {
    /// The binary file doesn't exist.
    BinaryNotFound(PathBuf),
    /// The assets directory doesn't exist.
    AssetsDirNotFound(PathBuf),
    /// An I/O error occurred.
    IoError(io::Error),
    /// Failed to create output directory.
    OutputDirCreationFailed(PathBuf, io::Error),
}

impl std::fmt::Display for PackError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PackError::BinaryNotFound(p) => {
                write!(f, "Binary not found: {}", p.display())
            }
            PackError::AssetsDirNotFound(p) => {
                write!(f, "Assets directory not found: {}", p.display())
            }
            PackError::IoError(e) => write!(f, "I/O error: {}", e),
            PackError::OutputDirCreationFailed(p, e) => {
                write!(f, "Failed to create output directory {}: {}", p.display(), e)
            }
        }
    }
}

impl std::error::Error for PackError {}

impl From<io::Error> for PackError {
    fn from(e: io::Error) -> Self {
        PackError::IoError(e)
    }
}

/// Detect the current platform.
fn detect_platform() -> &'static str {
    #[cfg(target_os = "windows")]
    { "windows" }
    #[cfg(target_os = "macos")]
    { "macos" }
    #[cfg(target_os = "linux")]
    { "linux" }
    #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
    { "unknown" }
}

/// Generate the launch script content for the given platform.
fn generate_launch_script(platform: &str, binary_name: &str, release: bool) -> String {
    match platform {
        "windows" => {
            let mut script = String::new();
            script.push_str("@echo off\n");
            script.push_str("REM Akizuki*Rustgal Launch Script\n");
            if release {
                script.push_str("REM Release build\n");
            }
            script.push_str(&format!("start \"\" \"%~dp0{}\"\n", binary_name));
            script
        }
        "macos" | "linux" => {
            let mut script = String::new();
            script.push_str("#!/bin/bash\n");
            script.push_str("# Akizuki*Rustgal Launch Script\n");
            if release {
                script.push_str("# Release build\n");
            }
            script.push_str("DIR=\"$(cd \"$(dirname \"$0\")\" && pwd)\"\n");
            script.push_str(&format!("\"$DIR/{}\"\n", binary_name));
            script
        }
        _ => {
            format!("# Unknown platform — run ./{binary_name} manually\n")
        }
    }
}

/// Get the binary file name (with extension) from the path.
fn binary_filename(binary_path: &Path) -> String {
    binary_path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "game".to_string())
}

/// Recursively copy a directory.
fn copy_dir_recursive(src: &Path, dst: &Path) -> io::Result<usize> {
    if !dst.exists() {
        fs::create_dir_all(dst)?;
    }

    let mut count = 0;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let path = entry.path();
        let file_name = entry.file_name();
        let dest_path = dst.join(&file_name);

        if path.is_dir() {
            count += copy_dir_recursive(&path, &dest_path)?;
        } else {
            fs::copy(&path, &dest_path)?;
            count += 1;
        }
    }

    Ok(count)
}

/// Package the game for distribution.
///
/// This function:
/// 1. Validates that the binary and assets directory exist
/// 2. Creates the output directory structure
/// 3. Copies the binary to the output directory
/// 4. Copies the assets directory recursively
/// 5. Generates a platform-appropriate launch script
///
/// # Errors
///
/// Returns `PackError` if the binary or assets don't exist, or if I/O errors occur.
pub fn pack(config: &PackConfig) -> Result<PackResult, PackError> {
    // Validate binary exists
    if !config.binary_path.exists() {
        return Err(PackError::BinaryNotFound(config.binary_path.clone()));
    }

    // Validate assets directory exists
    if !config.assets_dir.exists() {
        eprintln!("[Warning] Assets directory not found: {} — creating empty assets directory", config.assets_dir.display());
        // Don't fail; create an empty assets directory
    }

    // Create output directory
    fs::create_dir_all(&config.output_dir).map_err(|e| {
        PackError::OutputDirCreationFailed(config.output_dir.clone(), e)
    })?;

    // Copy binary
    let bin_name = binary_filename(&config.binary_path);
    let dest_binary = config.output_dir.join(&bin_name);
    fs::copy(&config.binary_path, &dest_binary)?;

    // On Unix, set executable permission
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&dest_binary)?.permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&dest_binary, perms)?;
    }

    // Copy assets directory
    let dest_assets = config.output_dir.join("assets");
    let files_copied = if config.assets_dir.exists() {
        copy_dir_recursive(&config.assets_dir, &dest_assets)?
    } else {
        fs::create_dir_all(&dest_assets)?;
        0
    };

    // Determine platform
    let platform = config
        .target
        .as_deref()
        .map(|t| {
            if t.contains("windows") {
                "windows"
            } else if t.contains("apple") || t.contains("darwin") {
                "macos"
            } else if t.contains("linux") {
                "linux"
            } else {
                detect_platform()
            }
        })
        .unwrap_or_else(detect_platform);

    // Generate launch script
    let script_content = generate_launch_script(platform, &bin_name, config.release);
    let script_ext = match platform {
        "windows" => "bat",
        _ => "sh",
    };
    let script_name = format!("start.{}", script_ext);
    let script_path = config.output_dir.join(&script_name);
    fs::write(&script_path, &script_content)?;

    // On Unix, make script executable
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&script_path)?.permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&script_path, perms)?;
    }

    // Generate a README
    let readme = generate_readme(platform, &bin_name, config.release);
    let readme_path = config.output_dir.join("README.txt");
    let _ = fs::write(&readme_path, &readme);

    let total_files = files_copied + 1; // +1 for binary

    Ok(PackResult {
        output_dir: config.output_dir.clone(),
        binary_path: dest_binary,
        assets_path: dest_assets,
        launch_script: script_path,
        files_copied: total_files,
    })
}

/// Generate a README file for the packaged game.
fn generate_readme(platform: &str, binary_name: &str, release: bool) -> String {
    let mut readme = String::new();
    readme.push_str("========================================\n");
    readme.push_str("  Akizuki*Rustgal\n");
    readme.push_str("========================================\n\n");
    readme.push_str(&format!("Build type: {}\n\n", if release { "Release" } else { "Debug" }));
    readme.push_str("Contents:\n");
    readme.push_str(&format!("  - {}      (game executable)\n", binary_name));
    readme.push_str("  - assets/        (game resources)\n");
    readme.push_str("  - start.*        (launch script)\n\n");
    readme.push_str("How to run:\n");
    match platform {
        "windows" => {
            readme.push_str("  Double-click 'start.bat' to launch the game.\n");
            readme.push_str("  Alternatively, run the .exe file directly.\n");
        }
        "macos" | "linux" => {
            readme.push_str("  Run './start.sh' from a terminal.\n");
            readme.push_str("  Alternatively, run the binary directly.\n");
        }
        _ => {
            readme.push_str("  Run the executable file directly.\n");
        }
    }
    readme.push_str("\nRequirements:\n");
    readme.push_str("  - No external dependencies required.\n");
    readme.push_str("  - All resources are included in the assets/ directory.\n");
    readme
}

/// CLI entry point for the pack command.
///
/// Parses arguments and runs the pack operation.
/// Designed to be called from `akrs-cli` or as a standalone tool.
pub fn run_pack_cli(args: &[String]) -> Result<(), String> {
    let mut binary_path = PathBuf::from("target/release/akrs");
    let mut assets_dir = PathBuf::from("assets");
    let mut output_dir = PathBuf::from("dist/AkizukiRustgal");
    let mut release = true;
    let mut target: Option<String> = None;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--binary" | "-b" => {
                i += 1;
                if i < args.len() {
                    binary_path = PathBuf::from(&args[i]);
                }
            }
            "--assets" | "-a" => {
                i += 1;
                if i < args.len() {
                    assets_dir = PathBuf::from(&args[i]);
                }
            }
            "--output" | "-o" => {
                i += 1;
                if i < args.len() {
                    output_dir = PathBuf::from(&args[i]);
                }
            }
            "--release" => {
                release = true;
            }
            "--debug" => {
                release = false;
                binary_path = PathBuf::from("target/debug/akrs");
            }
            "--target" => {
                i += 1;
                if i < args.len() {
                    target = Some(args[i].clone());
                    // Adjust binary path for target
                    if release {
                        binary_path = PathBuf::from(format!("target/{}/release/akrs", args[i]));
                    } else {
                        binary_path = PathBuf::from(format!("target/{}/debug/akrs", args[i]));
                    }
                }
            }
            "--help" | "-h" => {
                print_help();
                return Ok(());
            }
            _ => {
                return Err(format!("Unknown argument: {}", args[i]));
            }
        }
        i += 1;
    }

    let config = PackConfig {
        binary_path,
        assets_dir,
        output_dir,
        release,
        target,
    };

    match pack(&config) {
        Ok(result) => {
            println!("Packaging complete!");
            println!("  Output: {}", result.output_dir.display());
            println!("  Binary: {}", result.binary_path.display());
            println!("  Assets: {}", result.assets_path.display());
            println!("  Launch: {}", result.launch_script.display());
            println!("  Files copied: {}", result.files_copied);
            Ok(())
        }
        Err(e) => Err(e.to_string()),
    }
}

fn print_help() {
    println!("akrs-pack — Package Akizuki*Rustgal games for distribution");
    println!();
    println!("Usage: akrs pack [OPTIONS]");
    println!();
    println!("Options:");
    println!("  -b, --binary <PATH>    Path to the game binary (default: target/release/akrs)");
    println!("  -a, --assets <DIR>     Path to the assets directory (default: assets)");
    println!("  -o, --output <DIR>     Output directory (default: dist/AkizukiRustgal)");
    println!("      --release          Release build (default)");
    println!("      --debug            Debug build");
    println!("      --target <TRIPLE>  Target triple (e.g. x86_64-pc-windows-gnu)");
    println!("  -h, --help             Show this help message");
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn test_launch_script_windows() {
        let script = generate_launch_script("windows", "akrs.exe", true);
        assert!(script.contains("@echo off"));
        assert!(script.contains("akrs.exe"));
    }

    #[test]
    fn test_launch_script_linux() {
        let script = generate_launch_script("linux", "akrs", false);
        assert!(script.contains("#!/bin/bash"));
        assert!(script.contains("akrs"));
    }

    #[test]
    fn test_launch_script_macos() {
        let script = generate_launch_script("macos", "akrs", true);
        assert!(script.contains("#!/bin/bash"));
        assert!(script.contains("Release build"));
    }

    #[test]
    fn test_readme_generation() {
        let readme = generate_readme("linux", "akrs", true);
        assert!(readme.contains("Akizuki*Rustgal"));
        assert!(readme.contains("Release"));
        assert!(readme.contains("start.sh"));
    }

    #[test]
    fn test_pack_creates_output() {
        let tmp = std::env::temp_dir().join("akrs_pack_test");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();

        // Create a fake binary
        let bin_path = tmp.join("fake_binary");
        let mut file = fs::File::create(&bin_path).unwrap();
        writeln!(file, "fake binary content").unwrap();

        // Create fake assets
        let assets_dir = tmp.join("assets");
        fs::create_dir_all(assets_dir.join("bg")).unwrap();
        fs::write(assets_dir.join("bg/test.png"), b"fake image").unwrap();
        fs::create_dir_all(assets_dir.join("music")).unwrap();
        fs::write(assets_dir.join("music/test.mp3"), b"fake audio").unwrap();

        let output_dir = tmp.join("output");

        let config = PackConfig {
            binary_path: bin_path,
            assets_dir,
            output_dir,
            release: true,
            target: None,
        };

        let result = pack(&config).unwrap();

        assert!(result.output_dir.exists());
        assert!(result.binary_path.exists());
        assert!(result.assets_path.exists());
        assert!(result.launch_script.exists());
        assert!(result.files_copied >= 3); // binary + 2 asset files

        // Clean up
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_pack_missing_binary() {
        let config = PackConfig {
            binary_path: PathBuf::from("/nonexistent/binary"),
            assets_dir: PathBuf::from("assets"),
            output_dir: PathBuf::from("/tmp/akrs_test_output"),
            release: true,
            target: None,
        };

        let result = pack(&config);
        assert!(matches!(result, Err(PackError::BinaryNotFound(_))));
    }

    #[test]
    fn test_pack_missing_assets_creates_empty() {
        let tmp = std::env::temp_dir().join("akrs_pack_test2");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();

        // Create a fake binary
        let bin_path = tmp.join("fake_binary");
        fs::write(&bin_path, b"fake").unwrap();

        let output_dir = tmp.join("output");
        let missing_assets = tmp.join("nonexistent_assets");

        let config = PackConfig {
            binary_path: bin_path,
            assets_dir: missing_assets,
            output_dir: output_dir.clone(),
            release: false,
            target: None,
        };

        let result = pack(&config).unwrap();
        assert!(result.assets_path.exists());
        assert_eq!(result.files_copied, 1); // just the binary

        // Clean up
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_cli_help() {
        let args = vec!["--help".to_string()];
        let result = run_pack_cli(&args);
        assert!(result.is_ok());
    }
}
