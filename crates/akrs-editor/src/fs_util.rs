//! 文件系统与平台工具函数。
//!
//! 从 egui 主分支的编辑器源码移植而来，提供以下能力：
//!
//! - 文件名规范化（[`sanitize_filename`]）。
//! - 项目根目录推断（[`infer_project_root`]）。
//! - 递归目录复制（[`copy_dir_recursive`]，返回 `Result` 以传播错误）。
//! - 在系统文件管理器中打开路径（[`open_path_in_file_manager`]）。
//! - 在系统浏览器中打开 URL（[`open_url_in_browser`]）。
//! - 从 `Cargo.toml` 读取所有 `[[bin]]` 目标名（[`read_binary_names`]）。
//! - 检测 `cargo` 是否可用（[`check_cargo`]）。
//!
//! 本模块仅依赖标准库，不引入任何第三方 crate。

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// 规范化用户输入的文件名：去除路径分隔符并确保有 `.akrs` 扩展名。
///
/// - 过滤掉 `/` 与 `\` 两个路径分隔符。
/// - 若结果为空，使用 `untitled.akrs` 作为默认名。
/// - 若不以 `.akrs` 结尾，自动追加该扩展名。
pub fn sanitize_filename(input: &str) -> String {
    let mut name: String = input
        .trim()
        .chars()
        .filter(|c| !matches!(c, '/' | '\\'))
        .collect();
    if name.is_empty() {
        name = "untitled.akrs".to_string();
    }
    if !name.ends_with(".akrs") {
        name.push_str(".akrs");
    }
    name
}

/// 从文件路径向上查找含 `project.json` 或 `assets/` 子目录的祖先目录，作为项目根目录。
///
/// 从给定文件的父目录开始逐级向上查找；若找到包含 `project.json`（文件）或
/// `assets`（目录）的目录则返回该目录，若一直找到根目录都未找到，则返回该文件的
/// 直接父目录。这与 egui 主分支编辑器的行为一致：打开单个剧本文件时能自动定位
/// 资源根，使预览正确读到 `assets/`。
pub fn infer_project_root(path: &Path) -> PathBuf {
    let start = match path.parent() {
        Some(p) => p,
        None => return PathBuf::from("."),
    };
    let mut current: Option<&Path> = Some(start);
    while let Some(dir) = current {
        if dir.join("project.json").is_file() || dir.join("assets").is_dir() {
            return dir.to_path_buf();
        }
        current = dir.parent();
    }
    // 未找到含 project.json 或 assets/ 的目录，返回直接父目录。
    start.to_path_buf()
}

/// 递归复制目录。
///
/// 与 egui 主分支版本不同，本函数返回 [`std::io::Result`]：
/// 内部所有文件系统操作均通过 `?` 传播错误，复制失败时返回 `Err`，
/// 而非用 `let _ =` 静默吞错。若 `src` 不是目录则直接返回 `Ok(())`。
pub fn copy_dir_recursive(src: &Path, dst: &Path) -> std::io::Result<()> {
    if !src.is_dir() {
        return Ok(());
    }
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let path = entry.path();
        let name = entry.file_name();
        let dest = dst.join(name);
        if path.is_dir() {
            copy_dir_recursive(&path, &dest)?;
        } else {
            // std::fs::copy 返回 Result<u64, io::Error>，? 传播错误并丢弃字节数。
            std::fs::copy(&path, &dest)?;
        }
    }
    Ok(())
}

/// 在系统文件管理器中打开指定路径。
///
/// 按平台调用：Windows 用 `explorer`、macOS 用 `open`、其他 Unix 用 `xdg-open`。
/// 打开失败时静默忽略（不返回结果）。
pub fn open_path_in_file_manager(path: &Path) {
    #[cfg(target_os = "windows")]
    {
        let _ = Command::new("explorer").arg(path).spawn();
    }
    #[cfg(target_os = "macos")]
    {
        let _ = Command::new("open").arg(path).spawn();
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let _ = Command::new("xdg-open").arg(path).spawn();
    }
}

/// 在系统默认浏览器中打开指定 URL。
///
/// 按平台调用：Windows 用 `cmd /c start`、macOS 用 `open`、其他 Unix 用 `xdg-open`。
/// 打开失败时静默忽略（不返回结果）。
pub fn open_url_in_browser(url: &str) {
    #[cfg(target_os = "windows")]
    {
        let _ = Command::new("cmd").args(["/c", "start", url]).spawn();
    }
    #[cfg(target_os = "macos")]
    {
        let _ = Command::new("open").arg(url).spawn();
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let _ = Command::new("xdg-open").arg(url).spawn();
    }
}

/// 从 `Cargo.toml` 读取所有 `[[bin]]` 段定义的二进制目标名。
///
/// 搜索两个候选路径：`work_dir/Cargo.toml`（workspace 根）与
/// `work_dir/crates/akrs-game/Cargo.toml`（游戏 crate），合并去重。
///
/// 与 egui 主分支版本相比，本函数采用更健壮的解析：
/// - 正确处理 `name = { ... }` 内联表形式（跳过，不误当作名字）。
/// - 跳过 `#` 注释行，并正确处理字符串字面量后的行内注释。
/// - 支持多个 `[[bin]]` 段（每段各取一个 `name`）。
/// - 忽略 `[package]` 等其他节下的 `name` 字段。
/// - 用 `=` 分割键值并校验键名严格等于 `name`，避免误匹配 `namex` 之类。
///
/// 若所有候选文件都无法读取或解析失败，使用 `work_dir` 的目录名作为后备；
/// 目录名也无法取得时，使用 `akrs-game` 作为最终后备。
pub fn read_binary_names(work_dir: &Path) -> Vec<String> {
    let candidates = [
        work_dir.join("Cargo.toml"),
        work_dir.join("crates").join("akrs-game").join("Cargo.toml"),
    ];

    let mut names: Vec<String> = Vec::new();
    for cargo_toml in &candidates {
        let content = match fs::read_to_string(cargo_toml) {
            Ok(c) => c,
            Err(_) => continue,
        };
        parse_bin_names(&content, &mut names);
    }

    // 后备：使用项目目录名。
    if names.is_empty() {
        if let Some(dir_name) = work_dir.file_name().and_then(|n| n.to_str()) {
            names.push(dir_name.to_string());
        } else {
            names.push("akrs-game".to_string());
        }
    }

    names
}

/// 从单个 `Cargo.toml` 文本内容解析 `[[bin]]` 段的 `name` 字段，追加到 `names`（去重）。
fn parse_bin_names(content: &str, names: &mut Vec<String>) {
    let mut in_bin_section = false;

    for raw_line in content.lines() {
        let trimmed = raw_line.trim();
        // 跳过空行与整行注释。
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        // 节标题判定：[[bin]] 进入 bin 节，其他任何 [xxx] / [[xxx]] 离开 bin 节。
        if trimmed.starts_with("[[") {
            in_bin_section = trimmed == "[[bin]]";
            continue;
        }
        if trimmed.starts_with('[') {
            in_bin_section = false;
            continue;
        }

        if !in_bin_section {
            continue;
        }

        // 解析 key = value。
        let eq_pos = match trimmed.find('=') {
            Some(p) => p,
            None => continue,
        };
        let key = trimmed[..eq_pos].trim();
        if key != "name" {
            continue;
        }
        let value = trimmed[eq_pos + 1..].trim();

        // 跳过内联表形式 name = { ... }（bin 名应为字符串字面量）。
        if value.starts_with('{') {
            continue;
        }

        // 解析字符串字面量值：取两个引号之间的内容（同时正确忽略行内注释）。
        let name = if value.starts_with('"') {
            let rest = &value[1..];
            match rest.find('"') {
                Some(end) => rest[..end].to_string(),
                None => continue, // 未闭合引号，跳过。
            }
        } else if value.starts_with('\'') {
            let rest = &value[1..];
            match rest.find('\'') {
                Some(end) => rest[..end].to_string(),
                None => continue,
            }
        } else {
            // 非字符串字面量（TOML 中 bin name 必须为字符串），跳过。
            continue;
        };

        if !name.is_empty() && !names.contains(&name) {
            names.push(name);
        }
    }
}

/// 检测 `cargo` 是否可用。
///
/// 运行 `cargo --version`（stdout/stderr 重定向到 null），若命令成功执行则返回 `true`。
pub fn check_cargo() -> bool {
    Command::new("cargo")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok()
}

/// 扫描目录中所有扩展名匹配 `exts` 的文件名（相对路径，已排序）。
///
/// 若目录不存在则返回空 Vec。扩展名比较不区分大小写。
/// 用于编辑器右栏的立绘 / 背景 / 音乐预览面板。
pub fn scan_dir(dir: &Path, exts: &[&str]) -> Vec<String> {
    let mut items = Vec::new();
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let lower = name.to_lowercase();
            if exts.iter().any(|ext| lower.ends_with(ext)) {
                items.push(name);
            }
        }
    }
    items.sort();
    items
}
