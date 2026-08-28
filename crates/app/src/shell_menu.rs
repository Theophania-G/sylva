//! 真实 Shell 上下文菜单（IContextMenu2/3）：右键栅栏内对象 = 桌面右键菜单。
//!
//! 通过 `SHCreateItemFromParsingName` + `IShellItem::BindToHandler(BHID_SFUIObject)`
//! 拿到文件/文件夹/快捷方式的**标准 Shell 右键菜单**（打开/编辑/打印/剪切/复制/
//! 删除/重命名/属性/发送到…），并在菜单顶部注入 Sylva 自己的「移出栅栏」「重命名」。
//!
//! ## 为什么必须用 IContextMenu2/3
//!
//! Windows 10/11 的 Shell 右键菜单大量使用**所有者绘制**（owner-draw）菜单项
//! （图标、渐变、现代样式的子菜单等）。这些项由 Shell 扩展通过
//! `WM_MEASUREITEM`/`WM_DRAWITEM`/`WM_INITMENUPOPUP`/`WM_MENUCHAR` 绘制。
//! 只用 `IContextMenu`(v1) + 裸 `TrackPopupMenu` 时，这些消息落到 owner 窗口的
//! `DefWindowProcW` 上被丢弃，所有者绘制项渲染成空白——看起来就是
//! 「没有 Windows 右键列表」。修复：把菜单 owner 设为一个**消息窗口**，
//! 其窗口过程把上述消息转发给 `IContextMenu2::HandleMenuMsg` /
//! `IContextMenu3::HandleMenuMsg2`，Shell 扩展据此绘制完整菜单。
//!
//! 前提：调用方已完成 COM 初始化（`CoInitializeEx`，App 启动早期完成）。
//! `InvokeCommand` 必须在其 COM 对象存活期间调用（本函数内保持引用即可）。

use std::cell::RefCell;
use std::mem::size_of;
use std::sync::OnceLock;

use windows::core::{Interface, PCSTR, PCWSTR};
use windows::Win32::Foundation::{HANDLE, HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::Com::IBindCtx;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Shell::{
    BHID_SFUIObject, IContextMenu, IContextMenu2, IContextMenu3, IShellItem,
    SHCreateItemFromParsingName, CMF_NORMAL, CMINVOKECOMMANDINFO,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu, DestroyWindow, InsertMenuW,
    RegisterClassW, TrackPopupMenu, HWND_MESSAGE, MF_BYPOSITION, MF_SEPARATOR, MF_STRING,
    SW_SHOWNORMAL, TPM_NONOTIFY, TPM_RETURNCMD, TPM_RIGHTBUTTON, WM_DRAWITEM, WM_INITMENUPOPUP,
    WM_MEASUREITEM, WM_MENUCHAR, WM_MENUCOMMAND, WM_MENUDRAG, WM_MENURBUTTONUP, WNDCLASSW,
    WNDCLASS_STYLES, WS_POPUP,
};

/// Shell 项命令 ID 区间（`QueryContextMenu` 的 idCmdFirst..=idCmdLast）。
const SHELL_CMD_FIRST: u32 = 0x7000;
const SHELL_CMD_LAST: u32 = 0x7FFF;

/// 注入的 Sylva 命令 ID（在 Shell 区间之外，避免冲突）。
/// 「移出栅栏」= 0x8000，「重命名」= 0x8001。
pub const CMD_REMOVE: usize = 0x8000;
const CMD_RENAME: usize = 0x8001;

/// 菜单动作结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellMenuAction {
    /// 点击了 Sylva 注入的「移出栅栏」。
    Remove,
    /// 点击了 Sylva 注入的「重命名」。
    Rename,
    /// 点击了 Shell 真实命令（已通过 `InvokeCommand` 执行）。
    Invoked,
}

/// 菜单消息宿主窗口类名（消息窗口，仅用于接收菜单 owner-draw 消息）。
const MENU_HOST_CLASS: &str = "SylvaMenuHost";

// 菜单期间持有 Shell 菜单接口，供宿主窗口过程转发 owner-draw 消息。
// 全程主线程、模态（TrackPopupMenu 阻塞），无跨线程访问。
thread_local! {
    static MENU_CTX2: RefCell<Option<IContextMenu2>> = const { RefCell::new(None) };
    static MENU_CTX3: RefCell<Option<IContextMenu3>> = const { RefCell::new(None) };
}

/// 菜单宿主窗口类只注册一次。
static MENU_CLASS_REGISTERED: OnceLock<()> = OnceLock::new();

/// 弹出 `path` 对应的真实 Shell 右键菜单并执行选中命令。
///
/// `linked`：该路径位于链接栅栏的存储文件夹内。链接栅栏即文件夹镜像，
/// 「移出栅栏」与 Shell「删除」等价（移出不删文件，镜像 ≤4s 就把图标加回来），
/// 故不再注入「移出栅栏」，避免菜单重复。
///
/// 返回 `None` 表示菜单被取消/无法创建；文件路径无效时同样返回 `None`。
pub fn show(path: &str, _hwnd: HWND, sx: i32, sy: i32, linked: bool) -> Option<ShellMenuAction> {
    let wide_path = wide(path);
    // 路径 → IShellItem → 默认上下文菜单
    let item: IShellItem = match unsafe {
        SHCreateItemFromParsingName::<PCWSTR, Option<&IBindCtx>, IShellItem>(
            PCWSTR(wide_path.as_ptr()),
            None,
        )
    } {
        Ok(i) => i,
        Err(e) => {
            tracing::warn!(path, "SHCreateItemFromParsingName 失败: {e}");
            return None;
        }
    };
    let ctx: IContextMenu = match unsafe {
        item.BindToHandler::<Option<&IBindCtx>, IContextMenu>(None, &BHID_SFUIObject)
    } {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(path, "BindToHandler(IContextMenu) 失败: {e}");
            return None;
        }
    };

    // 取 IContextMenu3（优先）/ IContextMenu2：转发 owner-draw 菜单消息必需。
    // IContextMenu3 : IContextMenu2 : IContextMenu，cast 失败说明该扩展只实现了 v1，
    // 此时 owner-draw 项无法绘制（极少数旧扩展），其余项仍可正常显示。
    let ctx3: Option<IContextMenu3> = ctx.cast::<IContextMenu3>().ok();
    let ctx2: IContextMenu2 = match ctx.cast::<IContextMenu2>() {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(path, "获取 IContextMenu2 失败: {e}");
            return None;
        }
    };
    tracing::debug!(path, ctx3 = ctx3.is_some(), "Shell 菜单接口就绪");

    let menu = unsafe { CreatePopupMenu().unwrap_or_default() };
    if menu.is_invalid() {
        tracing::warn!(path, "创建 Shell 菜单句柄失败");
        return None;
    }

    // 顶部注入 Sylva 命令 + 分隔线；Shell 真实项从其后位置开始追加。
    // 链接栅栏跳过「移出栅栏」（与 Shell「删除」等价，见 `show` 文档）。
    unsafe {
        let mut pos = 0u32;
        if !linked {
            let _ = InsertMenuW(
                menu,
                pos,
                MF_BYPOSITION | MF_STRING,
                CMD_REMOVE,
                PCWSTR(wide("移出栅栏").as_ptr()),
            );
            pos += 1;
        }
        let _ = InsertMenuW(
            menu,
            pos,
            MF_BYPOSITION | MF_STRING,
            CMD_RENAME,
            PCWSTR(wide("重命名").as_ptr()),
        );
        let _ = InsertMenuW(menu, pos + 1, MF_BYPOSITION | MF_SEPARATOR, 0, PCWSTR::null());
        // 追加 Shell 项；HRESULT 低 16 位为新增项数（忽略）
        let _ = ctx2.QueryContextMenu(menu, pos + 2, SHELL_CMD_FIRST, SHELL_CMD_LAST, CMF_NORMAL);
    }

    // 菜单 owner 用消息窗口：它负责把 owner-draw 消息转发给 IContextMenu2/3，
    // 使 Shell 扩展能绘制出完整的「桌面右键菜单」。
    let hinst = match unsafe { GetModuleHandleW(None) } {
        Ok(m) => HINSTANCE(m.0),
        Err(e) => {
            tracing::warn!(path, "获取模块句柄失败: {e}");
            unsafe {
                let _ = DestroyMenu(menu);
            }
            return None;
        }
    };
    if !ensure_menu_host_class(hinst) {
        unsafe {
            let _ = DestroyMenu(menu);
        }
        return None;
    }
    let host = match unsafe {
        CreateWindowExW(
            windows::Win32::UI::WindowsAndMessaging::WINDOW_EX_STYLE(0),
            PCWSTR(wide(MENU_HOST_CLASS).as_ptr()),
            PCWSTR::null(),
            WS_POPUP,
            0,
            0,
            0,
            0,
            Some(HWND_MESSAGE),
            None,
            Some(hinst),
            None,
        )
    } {
        Ok(h) => h,
        Err(e) => {
            tracing::warn!(path, "创建菜单宿主窗口失败: {e}");
            unsafe {
                let _ = DestroyMenu(menu);
            }
            return None;
        }
    };

    // 模态期间保持接口存活：存进 thread_local，宿主窗口过程从中读取转发
    // （clone 维持引用计数，原句柄继续用于 InvokeCommand）
    MENU_CTX2.with(|c| *c.borrow_mut() = Some(ctx2.clone()));
    MENU_CTX3.with(|c| *c.borrow_mut() = ctx3);

    let cmd = unsafe {
        TrackPopupMenu(
            menu,
            TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_NONOTIFY,
            sx,
            sy,
            Some(0),
            host,
            None,
        )
        .0 as usize
    };

    MENU_CTX2.with(|c| *c.borrow_mut() = None);
    MENU_CTX3.with(|c| *c.borrow_mut() = None);
    unsafe {
        let _ = DestroyMenu(menu);
        let _ = DestroyWindow(host);
    }

    match cmd {
        CMD_REMOVE => Some(ShellMenuAction::Remove),
        CMD_RENAME => Some(ShellMenuAction::Rename),
        c if (SHELL_CMD_FIRST as usize..=SHELL_CMD_LAST as usize).contains(&c) => {
            // 命令偏移量（低字），cast 成 LPSTR 即 MAKEINTRESOURCE 语义
            let offset = (c - SHELL_CMD_FIRST as usize) as isize;
            let info = CMINVOKECOMMANDINFO {
                cbSize: size_of::<CMINVOKECOMMANDINFO>() as u32,
                fMask: 0,
                hwnd: _hwnd,
                // MAKEINTRESOURCEA(offset)：按命令索引执行 Shell 动词
                lpVerb: PCSTR::from_raw(offset as *const u8),
                lpParameters: PCSTR::null(),
                lpDirectory: PCSTR::null(),
                nShow: SW_SHOWNORMAL.0,
                dwHotKey: 0,
                hIcon: HANDLE::default(),
            };
            unsafe {
                let _ = ctx2.InvokeCommand(&info);
            }
            Some(ShellMenuAction::Invoked)
        }
        _ => None,
    }
}

/// 注册菜单宿主窗口类（幂等）。
fn ensure_menu_host_class(hinst: HINSTANCE) -> bool {
    if MENU_CLASS_REGISTERED.get().is_some() {
        return true;
    }
    let class_name = wide(MENU_HOST_CLASS);
    let wc = WNDCLASSW {
        style: WNDCLASS_STYLES(0),
        lpfnWndProc: Some(menu_host_proc),
        cbClsExtra: 0,
        cbWndExtra: 0,
        hInstance: hinst,
        hIcon: Default::default(),
        hCursor: Default::default(),
        hbrBackground: Default::default(),
        lpszMenuName: PCWSTR::null(),
        lpszClassName: PCWSTR(class_name.as_ptr()),
    };
    let atom = unsafe { RegisterClassW(&wc) };
    if atom == 0 {
        tracing::warn!("注册菜单宿主窗口类失败");
        return false;
    }
    let _ = MENU_CLASS_REGISTERED.set(());
    true
}

/// 菜单宿主窗口过程：把菜单 owner-draw 消息转发给 IContextMenu2/3。
///
/// - `WM_INITMENUPOPUP` / `WM_DRAWITEM` / `WM_MEASUREITEM` → `HandleMenuMsg`；
/// - `WM_MENUCHAR`（键盘助记符）→ 优先 `HandleMenuMsg2` 并把 lResult 返回，
///   否则退回 v1 的 `HandleMenuMsg`；
/// - `WM_MENUDRAG` / `WM_MENUCOMMAND` / `WM_MENURBUTTONUP` → `HandleMenuMsg2`。
unsafe extern "system" fn menu_host_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_INITMENUPOPUP | WM_DRAWITEM | WM_MEASUREITEM => {
            let handled = MENU_CTX2.with(|c| {
                if let Some(ctx) = c.borrow().as_ref() {
                    ctx.HandleMenuMsg(msg, wparam, lparam).is_ok()
                } else {
                    false
                }
            });
            if handled {
                return LRESULT(0);
            }
        }
        WM_MENUCHAR | WM_MENUDRAG | WM_MENUCOMMAND | WM_MENURBUTTONUP => {
            // IContextMenu3 能返回 lResult（WM_MENUCHAR 需要）
            let r = MENU_CTX3.with(|c| {
                if let Some(ctx) = c.borrow().as_ref() {
                    let mut lr = LRESULT(0);
                    if ctx
                        .HandleMenuMsg2(msg, wparam, lparam, Some(&mut lr))
                        .is_ok()
                    {
                        return Some(lr);
                    }
                }
                None
            });
            if let Some(lr) = r {
                return lr;
            }
            // 只有 v2 时，WM_MENUCHAR 交给 HandleMenuMsg（返回值不可达，仅尽力）
            if msg == WM_MENUCHAR {
                let handled = MENU_CTX2.with(|c| {
                    if let Some(ctx) = c.borrow().as_ref() {
                        ctx.HandleMenuMsg(msg, wparam, lparam).is_ok()
                    } else {
                        false
                    }
                });
                if handled {
                    return LRESULT(0);
                }
            }
        }
        _ => {}
    }
    DefWindowProcW(hwnd, msg, wparam, lparam)
}

/// 把字符串转成 UTF-16（含结尾 NUL），供宽字符 API 使用。
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}
