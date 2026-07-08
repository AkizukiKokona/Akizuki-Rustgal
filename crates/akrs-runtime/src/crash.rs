//! 崩溃降级机制：蓝屏错误界面所需的错误信息与终端日志缓冲。
//!
//! 当引擎遇到不可忽略的错误（剧本编译失败、剧本运行时错误等）时，
//! 渲染层会弹出仿 Windows 蓝屏的错误界面，向玩家展示：
//! - 报错模块（如实显示）
//! - 错误代码（16 进制，类似 Windows）
//! - 原因分析（错误代码对应的常见问题说明）
//! - 固定警告行
//! - 三个操作按钮：导出日志 / 尝试继续运行 / 退出引擎
//!
//! 本模块提供：
//! - [`error_code`]：错误代码常量（16 进制）。
//! - [`CrashInfo`]：传递给渲染层的错误信息载体。
//! - 日志环形缓冲：保留最近 30 条终端日志，供"导出日志"按钮写出。
//!
//! 日志缓冲使用全局 `LazyLock<Mutex<VecDeque<String>>>`。macroquad 主循环
//! 单线程，争用可忽略；用 `Mutex` 是为了在 panic hook 等非主线程上下文
//! 也能安全写入。

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};

/// 错误代码常量（16 进制显示，模仿 Windows 风格）。
///
/// 代码分段约定：
/// - `0xC001_xxxx` — 剧本相关
/// - `0xC002_xxxx` — 存档相关
/// - `0xC003_xxxx` — 渲染相关
/// - `0xC004_xxxx` — 配置/翻译相关
/// - `0xDEAD_xxxx` — 致命未知错误
///
/// 完整的代码 → 原因对照表见 `docs/错误代码说明.md`。
pub mod error_code {
    /// 剧本编译错误（词法/语法/类型检查失败）。
    pub const SCRIPT_COMPILE: u32 = 0xC001_0001;
    /// 剧本运行时错误（VM 执行期间抛出）。
    pub const SCRIPT_RUNTIME: u32 = 0xC001_0002;
    /// 剧本引用的资源缺失（背景/立绘/音乐/语音找不到）。
    pub const SCRIPT_RESOURCE: u32 = 0xC001_0003;
    /// 存档读取失败（文件损坏或格式不兼容）。
    pub const SAVE_LOAD: u32 = 0xC002_0001;
    /// 渲染层错误（资源加载/绘制异常）。
    pub const RENDER: u32 = 0xC003_0001;
    /// 设置文件解析失败。
    pub const SETTINGS: u32 = 0xC004_0001;
    /// 未知致命错误（兜底）。
    pub const FATAL: u32 = 0xDEAD_0001;
}

/// 蓝屏崩溃信息。
///
/// 渲染层据此绘制蓝屏界面：模块名与原因通过 UI 翻译 key 查询当前语言
/// 的译文，错误代码以 16 进制显示。`can_continue` 决定"尝试继续运行"
/// 按钮是否可用——例如剧本编译错误可继续（降级到仅标题页），而某些
/// 灾难性错误继续后仍可能退出。
#[derive(Debug, Clone)]
pub struct CrashInfo {
    /// 报错模块的 UI 翻译 key（如 `"error.module.script_compiler"`）。
    pub module_key: &'static str,
    /// 错误代码（16 进制显示）。
    pub code: u32,
    /// 是否允许"尝试继续运行"。
    pub can_continue: bool,
}

/// 根据错误代码返回原因分析的 UI 翻译 key。
///
/// 渲染层用 `engine.t_ui(reason_key)` 取得当前语言的原因说明。
pub fn reason_key(code: u32) -> &'static str {
    match code {
        error_code::SCRIPT_COMPILE => "error.reason.script_compile",
        error_code::SCRIPT_RUNTIME => "error.reason.script_runtime",
        error_code::SCRIPT_RESOURCE => "error.reason.script_resource",
        error_code::SAVE_LOAD => "error.reason.save_load",
        error_code::RENDER => "error.reason.render",
        error_code::SETTINGS => "error.reason.settings",
        _ => "error.reason.fatal",
    }
}

/// 把错误代码格式化为 16 进制字符串（带 `0x` 前缀，8 位补零）。
pub fn format_code(code: u32) -> String {
    format!("0x{:08X}", code)
}

// ─── 日志环形缓冲 ───

/// 日志缓冲最大条数。"导出日志"按钮写出最近这些条终端日志；
/// 不足 30 条时按实际数量导出（做好冗余）。
const MAX_LOG_ENTRIES: usize = 30;

static LOG: LazyLock<Mutex<VecDeque<String>>> =
    LazyLock::new(|| Mutex::new(VecDeque::new()));

/// 追加一条日志。超过上限时丢弃最旧的一条。
pub fn push_log(line: impl Into<String>) {
    if let Ok(mut log) = LOG.lock() {
        if log.len() >= MAX_LOG_ENTRIES {
            log.pop_front();
        }
        log.push_back(line.into());
    }
}

/// 取最近若干条日志（最多 30 条），按时间顺序（旧 → 新）返回。
pub fn recent_logs() -> Vec<String> {
    LOG.lock()
        .map(|log| log.iter().cloned().collect())
        .unwrap_or_default()
}

/// 把最近日志导出到指定目录，文件名形如 `crash_log_<unix时间戳>.txt`。
///
/// 返回写入文件的完整路径。目录不存在或不可写时返回 `Err`。
pub fn export_logs(dir: &Path) -> std::io::Result<PathBuf> {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let file_name = format!("crash_log_{}.txt", secs);
    let path = dir.join(&file_name);

    let logs = recent_logs();
    let mut content = String::new();
    content.push_str("Akizuki*Rustgal 崩溃日志导出\n");
    content.push_str(&format!("导出时间戳: {}\n", secs));
    content.push_str(&format!("日志条数: {}\n\n", logs.len()));
    for (i, line) in logs.iter().enumerate() {
        content.push_str(&format!("{:02}. {}\n", i + 1, line));
    }
    if logs.is_empty() {
        content.push_str("（无日志记录）\n");
    }

    std::fs::write(&path, content)?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // 这些测试共享全局 LOG，并行执行会互相干扰（一个测试的 push_log 会插入
    // 到另一个测试的日志流中）。用此锁串行化所有触碰 LOG 的测试。
    static TEST_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn format_code_is_hex() {
        assert_eq!(format_code(error_code::SCRIPT_COMPILE), "0xC0010001");
        assert_eq!(format_code(error_code::FATAL), "0xDEAD0001");
    }

    #[test]
    fn reason_key_matches_code() {
        assert_eq!(reason_key(error_code::SCRIPT_COMPILE), "error.reason.script_compile");
        assert_eq!(reason_key(error_code::FATAL), "error.reason.fatal");
        // 未知代码回退到 fatal
        assert_eq!(reason_key(0x12345678), "error.reason.fatal");
    }

    #[test]
    fn log_ring_buffer_caps_at_max() {
        let _g = TEST_LOCK.lock().unwrap();
        // 重新初始化为空（测试间共享全局状态，先清空）
        {
            let mut log = LOG.lock().unwrap();
            log.clear();
        }
        for i in 0..(MAX_LOG_ENTRIES + 10) as u32 {
            push_log(format!("line {}", i));
        }
        let logs = recent_logs();
        assert_eq!(logs.len(), MAX_LOG_ENTRIES);
        // 最旧的应是第 10 条（前 10 条被丢弃）
        assert_eq!(logs[0], "line 10");
        assert_eq!(logs[logs.len() - 1], "line 39");
    }

    #[test]
    fn export_logs_writes_file() {
        let _g = TEST_LOCK.lock().unwrap();
        let tmp = std::env::temp_dir().join(format!("akrs_crash_test_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&tmp);
        push_log("test log line");
        let path = export_logs(&tmp).unwrap();
        assert!(path.exists());
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("崩溃日志导出"));
        assert!(content.contains("test log line"));
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(&tmp);
    }
}
