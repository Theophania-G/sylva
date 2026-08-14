//! 壳层接管：探测桌面窗口层级，隐藏真实图标视图，并提供恢复。
//!
//! 反冲突原则（设计文档 §6.2）：
//! 1. **动态探测**层级，不假设经典 Progman→WorkerW→DefView 结构；
//! 2. **绝不重挂/销毁他人窗口**——只隐藏 `SysListView32`（保留句柄）+ 插入自己的
//!    overlay 窗口作为 sibling；Wallpaper Engine 的窗口树不被动过；
//! 3. 可恢复：卸载时把隐藏的 ListView 原样恢复。
//!
//! 注意：图标枚举走 `IShellFolder`（见 `items.rs`），不依赖 DefView 是否存在，
//! 因此用户开启「隐藏图标」时本模块的接管流程依然成立。

use windows::core::{BOOL, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumChildWindows, FindWindowExW, FindWindowW, GetClassNameW, GetParent, SendMessageW,
    ShowWindow, SW_HIDE, SW_SHOW,
};

/// 触发桌面 WorkerW 生成的私有消息（广泛使用的 Progman 技巧）。
const WM_SPAWN_WORKERW: u32 = 0x052C;

const CLASS_PROGMAN: &str = "Progman";
const CLASS_DEFVIEW: &str = "SHELLDLL_DefView";
const CLASS_LISTVIEW: &str = "SysListView32";

/// 桌面层级探测结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DesktopHierarchy {
    /// Progman 根窗口。
    pub progman: HWND,
    /// 持有 SHELLDLL_DefView 的窗口（通常是 WorkerW；DefView 不存在时为 None）。
    pub worker: Option<HWND>,
    /// 真实桌面图标视图 `SHELLDLL_DefView`。
    pub def_view: Option<HWND>,
    /// 真实图标列表 `SysListView32`（DefView 的子窗口）。
    pub list_view: Option<HWND>,
}

impl DesktopHierarchy {
    /// overlay 窗口应挂靠的父窗口：优先 DefView 所在层级，
    /// 无 DefView（用户隐藏了图标）时退回 Progman。
    pub fn overlay_parent(&self) -> HWND {
        self.worker.or(self.def_view).unwrap_or(self.progman)
    }

    /// 隐藏真实图标列表。返回 `false` 表示本就没有可隐藏的图标视图。
    pub fn hide_icons(&self) -> bool {
        match self.list_view {
            Some(lv) if !lv.is_invalid() => {
                let _ = unsafe { ShowWindow(lv, SW_HIDE) };
                true
            }
            _ => false,
        }
    }

    /// 恢复真实图标列表。
    pub fn restore_icons(&self) {
        if let Some(lv) = self.list_view {
            if !lv.is_invalid() {
                let _ = unsafe { ShowWindow(lv, SW_SHOW) };
            }
        }
    }
}

/// 探测桌面窗口层级。
///
/// 流程：
/// 1. 定位 Progman；
/// 2. 发送 spawn-worker 消息，确保图标层存在；
/// 3. 搜索持有 `SHELLDLL_DefView` 的窗口（顶层或 Progman 子树）；
/// 4. 从 DefView 定位 `SysListView32` 与父 WorkerW。
pub fn probe() -> Option<DesktopHierarchy> {
    let progman = find_class_window(CLASS_PROGMAN)?;
    // 触发 WorkerW / DefView 生成（幂等）
    unsafe { SendMessageW(progman, WM_SPAWN_WORKERW, Default::default(), Default::default()) };

    let def_view = find_def_view();
    let worker = def_view
        .and_then(|dv| unsafe { GetParent(dv) }.ok())
        .filter(|p| !p.is_invalid());
    let list_view = def_view.and_then(|dv| find_class_child(dv, CLASS_LISTVIEW));

    Some(DesktopHierarchy {
        progman,
        worker,
        def_view,
        list_view,
    })
}

/// 查找指定类名的顶层窗口。
fn find_class_window(class_name: &str) -> Option<HWND> {
    let class = wide(class_name);
    let hwnd = unsafe { FindWindowW(PCWSTR(class.as_ptr()), None) }.ok()?;
    (!hwnd.is_invalid()).then_some(hwnd)
}

/// 查找 `SHELLDLL_DefView`：
/// 先试顶层（部分系统上 DefView 是顶层窗口），再递归 Progman 子树
/// （Wallpaper Engine 会把 DefView 重挂到它新建的 WorkerW 下）。
fn find_def_view() -> Option<HWND> {
    if let Some(hwnd) = find_class_window(CLASS_DEFVIEW) {
        return Some(hwnd);
    }
    let progman = find_class_window(CLASS_PROGMAN)?;
    find_class_descendant(progman, CLASS_DEFVIEW)
}

/// 在 `hwnd` 的直接子窗口中查找指定类名的窗口。
fn find_class_child(hwnd: HWND, class_name: &str) -> Option<HWND> {
    let class = wide(class_name);
    unsafe { FindWindowExW(Some(hwnd), None, PCWSTR(class.as_ptr()), None) }
        .ok()
        .filter(|h| !h.is_invalid())
}

/// 递归枚举回调的上下文。
struct FindCtx {
    class_name: String,
    found: Option<HWND>,
}

/// 在 `hwnd` 子树中递归查找指定类名的窗口（返回第一个匹配）。
fn find_class_descendant(hwnd: HWND, class_name: &str) -> Option<HWND> {
    let mut ctx = FindCtx {
        class_name: class_name.to_string(),
        found: None,
    };
    let _ = unsafe {
        EnumChildWindows(Some(hwnd), Some(enum_child_cb), LPARAM(&mut ctx as *mut _ as isize));
    };
    ctx.found
}

unsafe extern "system" fn enum_child_cb(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let ctx = &mut *(lparam.0 as *mut FindCtx);
    if ctx.found.is_some() {
        return BOOL(0); // 已找到，停止
    }
    if class_of(hwnd).as_deref() == Some(ctx.class_name.as_str()) {
        ctx.found = Some(hwnd);
        BOOL(0)
    } else {
        BOOL(1) // 继续枚举
    }
}

/// 读取窗口类名。
fn class_of(hwnd: HWND) -> Option<String> {
    let mut buf = [0u16; 256];
    let n = unsafe { GetClassNameW(hwnd, &mut buf) };
    (n > 0).then(|| String::from_utf16_lossy(&buf[..n as usize]))
}

/// 转换 `&str` 为以 NUL 结尾的 UTF-16 宽字符串。
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wide_conversion_roundtrip() {
        assert_eq!(wide_to_string_test(&wide("Progman")), "Progman");
        assert_eq!(wide_to_string_test(&wide("SHELLDLL_DefView")), "SHELLDLL_DefView");
    }

    fn wide_to_string_test(w: &[u16]) -> String {
        let end = w.iter().position(|&c| c == 0).unwrap_or(w.len());
        String::from_utf16_lossy(&w[..end])
    }

    #[test]
    fn probe_compiles_and_is_consistent() {
        // 真实桌面环境下运行；只要返回了 Progman，层级字段必须自洽。
        if let Some(h) = probe() {
            assert!(!h.progman.is_invalid());
            if let Some(dv) = h.def_view {
                assert_eq!(class_of(dv).as_deref(), Some(CLASS_DEFVIEW));
            }
            if let Some(w) = h.worker {
                // 父窗口要么是 WorkerW，要么是 Progman 本身
                let cls = class_of(w);
                assert!(cls.is_some());
            }
            if let Some(lv) = h.list_view {
                assert_eq!(class_of(lv).as_deref(), Some(CLASS_LISTVIEW));
            }
        }
    }
}
