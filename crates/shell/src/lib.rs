//! `fence-shell`：Windows 壳层适配层。
//!
//! 负责与 Windows 桌面 Shell 交互：
//! - `takeover`：接管桌面图标层（Progman / WorkerW / SHELLDLL_DefView 探测、隐藏/恢复真实图标视图）
//! - `items`：通过 `IShellFolder` 枚举桌面图标
//! - `icons`：通过 `IShellItemImageFactory` 提取图标
//! - M2：右键菜单（`IContextMenu`）、拖拽（`IDropTarget` / `SHDoDragDrop`）
//! - Shell 事件（`SHChangeNotify`）、多显示器 / DPI
//!
//! 本 crate 是分层架构中最贴近系统的一层，API 以 `fence-core` 的领域类型为边界。

#![allow(dead_code)] // 骨架阶段；随里程碑推进逐步移除

pub mod com;
pub mod items;
pub mod takeover;

/// 顶层便捷函数：探测桌面根窗口 `Progman`。
pub fn probe_progman() -> Option<windows::Win32::Foundation::HWND> {
    takeover::probe().map(|h| h.progman)
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
