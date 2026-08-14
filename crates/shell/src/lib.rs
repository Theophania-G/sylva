//! `fence-shell`：Windows 壳层适配层。
//!
//! 负责与 Windows 桌面 Shell 交互：
//! - M1：接管桌面图标层（Progman / WorkerW / SHELLDLL_DefView 探测、隐藏真实图标视图）
//! - M1：通过 `IShellFolder` 枚举桌面图标、`IShellItemImageFactory` 提取图标
//! - M2：右键菜单（`IContextMenu`）、拖拽（`IDropTarget` / `SHDoDragDrop`）
//! - Shell 事件（`SHChangeNotify`）、多显示器 / DPI
//!
//! 本 crate 是分层架构中最贴近系统的一层，API 以 `fence-core` 的领域类型为边界。

#![allow(dead_code)] // M0 桩阶段；随里程碑推进逐步移除

use windows::core::w;
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::WindowsAndMessaging::FindWindowW;

/// 探测桌面根窗口 `Progman`。
///
/// M1 的接管流程以此窗口为入口，向上搜索 `WorkerW` 与 `SHELLDLL_DefView`。
pub fn probe_progman() -> Option<HWND> {
    match unsafe { FindWindowW(w!("Progman"), None) } {
        Ok(hwnd) if !hwnd.is_invalid() => Some(hwnd),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 仅验证调用链可编译、可链接（真实桌面环境下通常返回 Some）。
    #[test]
    fn probe_progman_compiles() {
        let _ = probe_progman();
    }
}
