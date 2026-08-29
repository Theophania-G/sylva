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
use std::collections::{HashMap, HashSet};
use std::mem::size_of;
use std::sync::{Mutex, OnceLock};

use windows::core::{Interface, PCSTR, PCWSTR};
use windows::Win32::Foundation::{HANDLE, HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::Com::IBindCtx;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Shell::{
    BHID_SFUIObject, IContextMenu, IContextMenu2, IContextMenu3, IShellItem,
    SHCreateItemFromParsingName, CMF_NORMAL, CMINVOKECOMMANDINFO,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu, DestroyWindow, DispatchMessageW,
    GetMessageW, InsertMenuW, PostMessageW, PostQuitMessage, RegisterClassW, TrackPopupMenu,
    TranslateMessage, HWND_MESSAGE, MF_BYPOSITION, MF_SEPARATOR, MF_STRING, MSG, SW_SHOWNORMAL,
    TPM_NONOTIFY, TPM_RETURNCMD, TPM_RIGHTBUTTON, WM_DRAWITEM, WM_INITMENUPOPUP, WM_MEASUREITEM,
    WM_MENUCHAR, WM_MENUCOMMAND, WM_MENUDRAG, WM_MENURBUTTONUP, WM_NULL, WNDCLASSW, WNDCLASS_STYLES,
    WS_POPUP,
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
pub enum ShellMenuResult {
    /// 点击了 Sylva 注入的「移出栅栏」。
    Remove,
    /// 点击了 Sylva 注入的「重命名」。
    Rename,
    /// 点击了 Shell 真实命令（已通过 `InvokeCommand` 执行）。
    Invoked,
    /// 用户取消菜单（Esc / 点击别处关闭）。
    Canceled,
    /// 菜单无法创建（路径无效 / COM 失败 / 资源不足等）。
    Failed,
}

/// 菜单消息宿主窗口类名（消息窗口，仅用于接收菜单 owner-draw 消息）。
const MENU_HOST_CLASS: &str = "SylvaMenuHost";

// 菜单期间持有 Shell 菜单接口，供宿主窗口过程转发 owner-draw 消息。
// 菜单全程在主线程模态运行（TrackPopupMenu 阻塞），无跨线程访问。
thread_local! {
    static MENU_CTX2: RefCell<Option<IContextMenu2>> = const { RefCell::new(None) };
    static MENU_CTX3: RefCell<Option<IContextMenu3>> = const { RefCell::new(None) };
}

/// 菜单宿主窗口类只注册一次。
static MENU_CLASS_REGISTERED: OnceLock<()> = OnceLock::new();

/// 已预热（加载并初始化过 Shell 扩展）的文件类型键集合。
static PRIMED: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();

fn primed_set() -> std::sync::MutexGuard<'static, HashSet<String>> {
    PRIMED.get_or_init(|| Mutex::new(HashSet::new())).lock().unwrap()
}

/// 弹出 `path` 对应的真实 Shell 右键菜单并执行选中命令。
///
/// `managed`：该项为 Sylva 管理项（库内项/链接镜像项，见 context_menu::handle_context_menu）。
/// 链接栅栏即文件夹镜像，「移出栅栏」与 Shell「删除」等价（移出不删文件，镜像 ≤4s 就把
/// 图标加回来），库内项移出同样只此一个意义，故不再注入「移出栅栏」，避免菜单重复。
///
/// 菜单全程在**主线程**上弹出（模态）——与 Windows 桌面右键行为完全一致：
/// 点击菜单外区域自动收起、Shell 动词（属性/删除/重命名…）能正常激活前台窗口。
///
/// 慢 Shell 扩展（如百度网盘 YunShellExt）首次加载会卡线程数秒，若首次右键时
/// 扩展尚未加载，Windows 会判 UI 线程无响应（AppHangB1）并结束进程——表现即
/// 「首次右键软件崩溃」。解决：右键前先在**后台线程**把该文件类型的 Shell 扩展
/// 加载并初始化好（`prime_*`），再走主线程菜单。扩展慢初始化只发生一次（这就
/// 是「重开软件就正常」的原因），预热后右键即为「第二次」速度。
pub fn show(path: &str, hwnd: HWND, sx: i32, sy: i32, managed: bool) -> ShellMenuResult {
    // 类型未预热则后台预热；等待期间泵送消息，窗口保持可响应（不触发 AppHang）
    ensure_primed(path, hwnd);
    run_menu(path, hwnd, sx, sy, managed)
}

/// 启动时预热：把栅栏里已有文件类型的 Shell 扩展在后台加载并初始化。
/// 每个类型挑一个真实路径做一次 `QueryContextMenu`（就是这一步加载扩展、
/// 触发慢首次初始化），把成本移到用户交互之前。
pub fn prime_startup(paths: &[String]) {
    let keys = unique_type_keys(paths);
    if keys.is_empty() {
        return;
    }
    let by_key: HashMap<String, String> = paths.iter().map(|p| (type_key(p), p.clone())).collect();
    let _ = std::thread::Builder::new()
        .name("sylva-menu-prime".into())
        .spawn(move || {
            let _ = sylva_shell::com::init();
            for key in &keys {
                if let Some(path) = by_key.get(key) {
                    let _ = std::panic::catch_unwind(|| prime_one(path));
                }
                primed_set().insert(key.clone());
            }
        });
}

/// 该类型的 Shell 菜单尚未预热：后台预热，并在等待期间泵送消息。
/// 预热完成（或线程创建失败）后返回；调用方随后直接走主线程菜单。
fn ensure_primed(path: &str, hwnd: HWND) {
    let key = type_key(path);
    if primed_set().contains(&key) {
        return;
    }
    let Some(rx) = prime_path_async(path, hwnd) else {
        return; // 线程创建失败：照常弹菜单（罕见，退化为旧行为）
    };
    let _ = pump_until(rx);
}

/// 后台预热单个文件类型，完成时发一个空消息唤醒等待方的消息泵。
fn prime_path_async(path: &str, hwnd: HWND) -> Option<std::sync::mpsc::Receiver<()>> {
    let key = type_key(path);
    let path = path.to_string();
    let hwnd_usize = hwnd.0 as usize;
    let (tx, rx) = std::sync::mpsc::channel();
    let ok = std::thread::Builder::new()
        .name("sylva-menu-prime".into())
        .spawn(move || {
            let _ = sylva_shell::com::init();
            let _ = std::panic::catch_unwind(|| prime_one(&path));
            primed_set().insert(key);
            let _ = tx.send(());
            // 唤醒等待方的消息泵（等待方阻塞在 GetMessage 上时）
            unsafe {
                let _ = PostMessageW(
                    Some(HWND(hwnd_usize as *mut core::ffi::c_void)),
                    WM_NULL,
                    WPARAM(0),
                    LPARAM(0),
                );
            }
        })
        .is_ok();
    if ok {
        Some(rx)
    } else {
        None
    }
}

/// 加载并初始化 `path` 文件类型的 Shell 扩展（创建 IContextMenu + QueryContextMenu，
/// 随后释放）。慢首次初始化（扩展连主进程/建缓存等）就发生在这里。
fn prime_one(path: &str) -> bool {
    let wide_path = wide(path);
    let item: IShellItem = match unsafe {
        SHCreateItemFromParsingName::<PCWSTR, Option<&IBindCtx>, IShellItem>(
            PCWSTR(wide_path.as_ptr()),
            None,
        )
    } {
        Ok(i) => i,
        Err(e) => {
            tracing::warn!(path, "预热: SHCreateItemFromParsingName 失败: {e}");
            return false;
        }
    };
    let ctx: IContextMenu = match unsafe {
        item.BindToHandler::<Option<&IBindCtx>, IContextMenu>(None, &BHID_SFUIObject)
    } {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(path, "预热: BindToHandler(IContextMenu) 失败: {e}");
            return false;
        }
    };
    let ctx2: IContextMenu2 = match ctx.cast::<IContextMenu2>() {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(path, "预热: 获取 IContextMenu2 失败: {e}");
            return false;
        }
    };
    let menu = unsafe { CreatePopupMenu().unwrap_or_default() };
    if menu.is_invalid() {
        return false;
    }
    // QueryContextMenu 触发扩展加载与初始化（慢首次加载就在这一步）
    let _ = unsafe { ctx2.QueryContextMenu(menu, 0, SHELL_CMD_FIRST, SHELL_CMD_LAST, CMF_NORMAL) };
    unsafe {
        let _ = DestroyMenu(menu);
    }
    true
}

/// 文件类型的分类键：有扩展名的按扩展名（小写，含点）区分；无扩展名的文件夹/文件分开。
fn type_key(path: &str) -> String {
    let p = std::path::Path::new(path);
    if let Some(ext) = p.extension() {
        let ext = ext.to_string_lossy().to_lowercase();
        if !ext.is_empty() {
            return format!(".{ext}");
        }
    }
    if p.is_dir() {
        "\u{0}folder".to_string()
    } else {
        "\u{0}file".to_string()
    }
}

/// 去重后的类型键列表（保持输入顺序）。
fn unique_type_keys(paths: &[String]) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for p in paths {
        let k = type_key(p);
        if seen.insert(k.clone()) {
            out.push(k);
        }
    }
    out
}

/// 主线程泵送消息直到 `rx` 收到值（调用方阻塞）。返回 `None` 表示收到 `WM_QUIT`
/// （用户退出，已把退出信号转发给外层消息循环）。
///
/// 不能空等——窗口必须持续处理消息，否则 Windows 判无响应（AppHangB1）。
/// 泵送期间到达的 overlay 事件由 App 的 `ReentryGuard` 丢弃（模态期间行为一致）。
fn pump_until<T>(rx: std::sync::mpsc::Receiver<T>) -> Option<T> {
    loop {
        if let Ok(v) = rx.try_recv() {
            return Some(v);
        }
        let mut msg = MSG::default();
        if unsafe { GetMessageW(&mut msg, None, 0, 0) }.0 == 0 {
            // WM_QUIT：转发退出信号，放弃等待
            unsafe {
                PostQuitMessage(msg.wParam.0 as i32);
            }
            return None;
        }
        unsafe {
            let _ = TranslateMessage(&msg);
            let _ = DispatchMessageW(&msg);
        }
    }
}

/// 在**主线程**上弹出 Shell 右键菜单并执行选中命令（模态，阻塞到菜单关闭）。
/// 由 `show` 调用（见 `show`）；COM 必须在调用线程上已初始化。
fn run_menu(path: &str, hwnd: HWND, sx: i32, sy: i32, managed: bool) -> ShellMenuResult {
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
            return ShellMenuResult::Failed;
        }
    };
    let ctx: IContextMenu = match unsafe {
        item.BindToHandler::<Option<&IBindCtx>, IContextMenu>(None, &BHID_SFUIObject)
    } {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(path, "BindToHandler(IContextMenu) 失败: {e}");
            return ShellMenuResult::Failed;
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
            return ShellMenuResult::Failed;
        }
    };
    tracing::debug!(path, ctx3 = ctx3.is_some(), "Shell 菜单接口就绪");

    let menu = unsafe { CreatePopupMenu().unwrap_or_default() };
    if menu.is_invalid() {
        tracing::warn!(path, "创建 Shell 菜单句柄失败");
        return ShellMenuResult::Failed;
    }

    // 顶部注入 Sylva 命令 + 分隔线；Shell 真实项从其后位置开始追加。
    // Sylva 管理项跳过「移出栅栏」（与「删除」等价，见 `show` 文档）。
    unsafe {
        let mut pos = 0u32;
        if !managed {
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
            return ShellMenuResult::Failed;
        }
    };
    if !ensure_menu_host_class(hinst) {
        unsafe {
            let _ = DestroyMenu(menu);
        }
        return ShellMenuResult::Failed;
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
            return ShellMenuResult::Failed;
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
        CMD_REMOVE => ShellMenuResult::Remove,
        CMD_RENAME => ShellMenuResult::Rename,
        c if (SHELL_CMD_FIRST as usize..=SHELL_CMD_LAST as usize).contains(&c) => {
            // 命令偏移量（低字），cast 成 LPSTR 即 MAKEINTRESOURCE 语义
            let offset = (c - SHELL_CMD_FIRST as usize) as isize;
            let info = CMINVOKECOMMANDINFO {
                cbSize: size_of::<CMINVOKECOMMANDINFO>() as u32,
                fMask: 0,
                hwnd,
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
            ShellMenuResult::Invoked
        }
        _ => ShellMenuResult::Canceled,
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
