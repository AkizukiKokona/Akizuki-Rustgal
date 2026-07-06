//! 平台相关的窗口与显示器工具。
//!
//! 主要解决三个问题：
//! 1. **真实显示器尺寸检测**：miniquad 0.3.16 不暴露显示器（物理）尺寸，
//!    仅暴露帧缓冲区尺寸。但帧缓冲区尺寸 = 窗口尺寸，而非显示器尺寸。
//!    为了在创建窗口前就知道屏幕多大（用于计算初始窗口大小、防止超屏），
//!    这里在 Windows 上直接调用 `GetSystemMetrics`，在 Linux 上尽力解析
//!    `xrandr`，其余平台回退到 1920×1080。
//! 2. **窗口居中**：miniquad 0.3.16 不提供 `set_window_position`。
//!    `set_window_size`（调整窗口大小）在大多数平台上保持窗口左上角不动，
//!    导致放大后窗口向右下偏移甚至超出屏幕。这里在 Windows 上通过
//!    `FindWindowW` + `SetWindowPos` 重新居中。
//! 3. **控制台分配**：三端默认静默启动（Windows 上用 `windows_subsystem =
//!    "windows"` 不弹控制台），但「显示终端调试输出」设置项开启后，
//!    需要调用 `AllocConsole` 重新分配控制台。

/// 窗口标题，用于 `FindWindowW` 定位窗口句柄。
/// 必须与 `renderer::window_conf` 中设置的 `window_title` 完全一致。
#[allow(dead_code)]
pub const WINDOW_TITLE: &str = "Akizuki*Rustgal";

/// 获取主显示器的物理像素尺寸（尽力而为）。
///
/// 返回的是显示器自身的分辨率，与窗口大小、DPI 缩放无关。
/// 在多显示器环境下返回主显示器尺寸。
pub fn get_screen_size_physical() -> (i32, i32) {
    #[cfg(target_os = "windows")]
    {
        if let Some(size) = windows_screen_size() {
            return size;
        }
    }

    #[cfg(target_os = "linux")]
    {
        if let Some(size) = linux_screen_size() {
            return size;
        }
    }

    // 兜底：假设 1920×1080（最常见的桌面分辨率）。
    (1920, 1080)
}

/// 把窗口居中到主显示器。
///
/// `w_physical` / `h_physical` 是窗口的**物理**像素尺寸
/// （= 逻辑尺寸 × DPI 倍率）。在不可用平台上为空操作。
pub fn center_window_on_screen(w_physical: i32, h_physical: i32) {
    #[cfg(target_os = "windows")]
    {
        windows_center_window(w_physical, h_physical);
    }

    // Linux/macOS 无可用的无依赖居中手段，依赖窗口管理器的默认行为。
    // macroquad 创建的初始窗口通常会被 WM 居中；调整大小后位置由 WM 决定。
    #[cfg(not(target_os = "windows"))]
    {
        let _ = (w_physical, h_physical);
    }
}

/// 尝试为当前进程分配一个控制台窗口（仅 Windows 有效）。
///
/// 用于「显示终端调试输出」设置项：
/// - `windows_subsystem = "windows"` 默认不弹控制台；
/// - 用户在设置中开启 `debug_terminal` 后，启动时调用此函数分配控制台，
///   使随后的 println!/eprintln! 输出可见。
///
/// 返回 true 表示成功分配或已存在控制台；false 表示分配失败。
/// 在非 Windows 平台始终返回 true（无操作，但不视为错误）。
pub fn try_alloc_console() -> bool {
    #[cfg(target_os = "windows")]
    {
        windows_alloc_console()
    }
    #[cfg(not(target_os = "windows"))]
    {
        true
    }
}

// ─── Windows 实现 ───────────────────────────────────────────────────────────

#[cfg(target_os = "windows")]
mod windows_impl {
    use windows_sys::Win32::Foundation::HWND;
    use windows_sys::Win32::System::Console::{AllocConsole, GetConsoleWindow};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        FindWindowW, GetSystemMetrics, SetWindowPos, HWND_TOP, SM_CXSCREEN, SM_CYSCREEN,
        SWP_NOACTIVATE, SWP_NOSIZE, SWP_NOZORDER,
    };

    /// 调用 `GetSystemMetrics(SM_CXSCREEN/SM_CYSCREEN)` 获取主显示器尺寸。
    pub fn screen_size() -> Option<(i32, i32)> {
        unsafe {
            let w = GetSystemMetrics(SM_CXSCREEN);
            let h = GetSystemMetrics(SM_CYSCREEN);
            if w > 0 && h > 0 {
                Some((w, h))
            } else {
                None
            }
        }
    }

    /// 通过窗口标题找到窗口句柄并居中。
    pub fn center_window(w_physical: i32, h_physical: i32) {
        // 把标题编码为 UTF-16 wide string（结尾的 \0 由 encoding 自动包含在 widestring 里，
        // 但 windows_sys 的 FindWindowW 接受 LPCWSTR，需要一个以 0 结尾的 u16 数组）。
        let title: Vec<u16> = super::WINDOW_TITLE
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();

        unsafe {
            let hwnd: HWND = FindWindowW(std::ptr::null(), title.as_ptr());
            // HWND 为 0 表示找不到窗口，静默放弃（可能在 wasm 或尚未创建）。
            if hwnd == 0 {
                return;
            }
            let (sw, sh) = match screen_size() {
                Some(s) => s,
                None => return,
            };
            let x = ((sw - w_physical) / 2).max(0);
            let y = ((sh - h_physical) / 2).max(0);
            // SWP_NOSIZE: 不改变大小（已由 set_window_size 设置）；
            // SWP_NOZORDER: 不改变 Z 序；
            // SWP_NOACTIVATE: 不激活窗口（避免抢焦点闪烁）。
            let flags = SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE;
            SetWindowPos(hwnd, HWND_TOP, x, y, 0, 0, flags);
        }
    }

    /// 调用 `AllocConsole()` 为当前进程分配一个控制台窗口。
    /// 若进程已有控制台（如从 cmd 启动），`AllocConsole` 会失败，此时视作成功。
    /// 返回 true 表示已有控制台或新分配成功。
    pub fn alloc_console() -> bool {
        unsafe {
            // 若已有控制台窗口，无需再分配。
            if GetConsoleWindow() != 0 {
                return true;
            }
            // AllocConsole 返回 0 表示失败（通常是因为已有控制台）。
            // 这里把"已有控制台"和"分配成功"都视为 true。
            let ok = AllocConsole();
            ok != 0 || GetConsoleWindow() != 0
        }
    }
}

#[cfg(target_os = "windows")]
use windows_impl::{
    alloc_console as windows_alloc_console, center_window as windows_center_window,
    screen_size as windows_screen_size,
};

// ─── Linux 实现（尽力而为） ─────────────────────────────────────────────────

#[cfg(target_os = "linux")]
fn linux_screen_size() -> Option<(i32, i32)> {
    // 尝试解析 `xrandr` 输出查找最大分辨率。仅在 X11 会话下有效；
    // Wayland 下 xrandr 不可用，会回退到默认 1920×1080。
    let output = std::process::Command::new("xrandr")
        .arg("--current")
        .output()
        .ok()?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut best: Option<(i32, i32)> = None;
    for line in stdout.lines() {
        // 形如 "   1920x1080     60.00*+ 50.00 59.94"
        // 或连接器行 "DP-1 connected primary 1920x1080+0+0"
        for token in line.split_whitespace() {
            if let Some((w, h)) = token.split_once('x') {
                if let (Ok(w), Ok(h)) = (w.parse::<i32>(), h.parse::<i32>()) {
                    if w > 0 && h > 0 {
                        let area = w * h;
                        if best.map(|(bw, bh)| bw * bh < area).unwrap_or(true) {
                            best = Some((w, h));
                        }
                    }
                }
            }
        }
    }
    best
}
