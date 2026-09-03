//! 右键/托盘菜单：菜单构建、动作分发、剪贴板与文件选择。

use crate::*;
pub(crate) const MENU_ICON_OPEN: usize = 1;
pub(crate) const MENU_ICON_REMOVE: usize = 2;
pub(crate) const MENU_PASTE: usize = 2500;
pub(crate) const MENU_DELETE_FENCE: usize = 5000;
pub(crate) const MENU_RENAME_FENCE: usize = 6000;
pub(crate) fn handle_tray_menu(rt: &mut Runtime) {
    const MENU_TRAY_CONSOLE: usize = 8200;
    const MENU_TRAY_QUIT: usize = 8201;
    let menu = popup_menu();
    if menu.is_invalid() {
        return;
    }
    unsafe {
        let s = wide("显示 Sylva 控制中心");
        let _ = AppendMenuW(menu, MF_STRING, MENU_TRAY_CONSOLE, PCWSTR(s.as_ptr()));
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        let s = wide("退出 Sylva");
        let _ = AppendMenuW(menu, MF_STRING, MENU_TRAY_QUIT, PCWSTR(s.as_ptr()));
    }
    let (sx, sy) = cursor_screen();
    let cmd = unsafe {
        TrackPopupMenu(
            menu,
            TPM_RETURNCMD | TPM_NONOTIFY,
            sx,
            sy,
            Some(0),
            rt.hwnd,
            None,
        )
        .0 as usize
    };
    unsafe {
        let _ = DestroyMenu(menu);
    }
    match cmd {
        MENU_TRAY_CONSOLE => {
            let open = !rt.desk.console_open;
            rt.desk.console_open = open;
            let _ = rt.store.save(&rt.desk);
            start_panel_tween(rt, if open { 1.0 } else { 0.0 });
        }
        MENU_TRAY_QUIT => unsafe {
            let _ = PostMessageW(Some(rt.hwnd), WM_APP_QUIT, WPARAM(0), LPARAM(0));
        },
        _ => {}
    }
}

/// 图标右键菜单动作。
pub(crate) enum IconMenuAction {
    Open,
    Remove,
}

/// 栅栏右键菜单动作（精简版：粘贴 / 重命名 / 删除）。
pub(crate) enum FenceMenuAction {
    Paste,
    Rename,
    Delete,
}

/// 处理右键：弹出上下文菜单并执行选中动作（菜单为模态，阻塞到关闭）。
pub(crate) fn handle_context_menu(
    rt: &mut Runtime,
    fence: usize,
    icon: Option<usize>,
    _pos: (f32, f32),
) {
    let (sx, sy) = cursor_screen();
    if let Some(ii) = icon {
        // 该项文件路径（打开/删除判定也要用）
        let path = rt
            .desk
            .fences
            .get(fence)
            .and_then(|f| f.icon_ids.get(ii))
            .and_then(|id| rt.desk.icons.get(id))
            .and_then(|ic| ic.path.clone());
        // 该项是否由 Sylva 管理（库内项 / 链接镜像项 / 虚拟项，added=true）：
        // 栅栏内容与文件夹同步，「移出栅栏」与「删除」等价（镜像项移出即删文件、
        // 库内项移出即删引用），菜单不再重复提供「移出栅栏」，只留「删除」。
        // 真实桌面图标（added=false）不同：移出=回未分组区，删除=回收站，保留「移出」。
        let managed = rt
            .desk
            .fences
            .get(fence)
            .and_then(|f| f.icon_ids.get(ii))
            .and_then(|id| rt.desk.icons.get(id))
            .map(|ic| ic.added)
            .unwrap_or(false);
        // 多选集合：右键集合中的任意一项 → 集合操作（打开全部 / 复制 / 移出 / 删除）。
        // 右键未选中的项 → 先单选该项，再走单项逻辑（资源管理器行为）。
        let key = (fence, ii);
        let multi = rt.selected.len() > 1 && rt.selected.contains(&key);
        if multi {
            match multi_icon_context_menu(rt.hwnd, sx, sy, managed) {
                Some(MultiMenuAction::Open) => open_selected(rt),
                Some(MultiMenuAction::Copy) => copy_selected(rt),
                Some(MultiMenuAction::Remove) => remove_selected(rt),
                Some(MultiMenuAction::Delete) => delete_selected(rt),
                None => {}
            }
            return;
        }
        if !rt.selected.contains(&key) {
            rt.selected = vec![key];
        }
        // 有文件路径的项走真实 Shell 右键菜单（等同桌面右键）；虚拟项（无路径）
        // 退回简版「打开」菜单。Shell 菜单在主线程模态弹出，与 Windows 右键
        // 行为一致；慢扩展已由后台预热（见 shell_menu::show），首次右键不卡死。
        match path {
            Some(p) => match shell_menu::show(&p, rt.hwnd, sx, sy, managed) {
                shell_menu::ShellMenuResult::Remove => remove_fence_icon(rt, fence, ii),
                shell_menu::ShellMenuResult::Rename => {
                    start_inplace_rename(rt, EditTarget::Item { fence, icon: ii })
                }
                shell_menu::ShellMenuResult::Invoked => {
                    // 原生动词执行后（如「删除」）：文件若已被移走/删除，立即清掉
                    // 栅栏里的死图标，等价于资源管理器删除后刷新视图。
                    if !std::path::Path::new(&p).exists() {
                        if let Some(id) = rt
                            .desk
                            .fences
                            .get(fence)
                            .and_then(|f| f.icon_ids.get(ii))
                            .cloned()
                        {
                            remove_icon_entirely(rt, &id);
                        }
                        let _ = rt.store.save(&rt.desk);
                    }
                }
                // 真实 Shell 菜单没弹出来（路径无效 / COM 异常 / 工作线程失败等）：
                // 退回简版菜单，保证右键必有反馈，不让「Windows 右击列表」静默消失。
                shell_menu::ShellMenuResult::Failed => {
                    tracing::warn!(path = %p, "Shell 右键菜单创建失败，退回简版菜单");
                    match icon_context_menu(rt.hwnd, sx, sy, managed) {
                        Some(IconMenuAction::Open) => launch_fence_icon(rt, fence, ii),
                        Some(IconMenuAction::Remove) => remove_fence_icon(rt, fence, ii),
                        None => {}
                    }
                }
                // 用户取消菜单（Esc / 点击别处）：什么都不做。
                // 旧实现把「取消」误当成「创建失败」，取消了还会再弹一次简版菜单。
                shell_menu::ShellMenuResult::Canceled => {}
            },
            None => match icon_context_menu(rt.hwnd, sx, sy, managed) {
                Some(IconMenuAction::Open) => launch_fence_icon(rt, fence, ii),
                Some(IconMenuAction::Remove) => remove_fence_icon(rt, fence, ii),
                None => {}
            },
        }
    } else if let Some(action) = fence_context_menu(rt, fence, sx, sy) {
        match action {
            FenceMenuAction::Paste => {
                let paths = clipboard_file_paths();
                if !paths.is_empty() {
                    add_paths_to_fence(rt, fence, &paths);
                } else {
                    tracing::info!("剪贴板中没有文件，跳过粘贴");
                }
            }
            FenceMenuAction::Rename => {
                start_inplace_rename(rt, EditTarget::FenceTitle { fence });
            }
            FenceMenuAction::Delete => {
                // 删除栅栏，不删除链接的文件夹（用户数据不受影响）
                let ids: Vec<String> = rt
                    .desk
                    .fences
                    .get(fence)
                    .map(|f| f.icon_ids.clone())
                    .unwrap_or_default();
                for id in ids {
                    rt.desk.move_icon(&id, None);
                }
                if fence < rt.desk.fences.len() {
                    rt.desk.fences.remove(fence);
                }
                let _ = rt.store.save(&rt.desk);
            }
        }
    }
}

/// 图标右键菜单（简版回退）：打开 / 移出栅栏（Sylva 管理项不提供移出，见 `handle_context_menu`）。
pub(crate) fn icon_context_menu(
    hwnd: HWND,
    sx: i32,
    sy: i32,
    managed: bool,
) -> Option<IconMenuAction> {
    let menu = popup_menu();
    if menu.is_invalid() {
        return None;
    }
    unsafe {
        let s = wide("打开");
        let _ = AppendMenuW(menu, MF_STRING, MENU_ICON_OPEN, PCWSTR(s.as_ptr()));
        if !managed {
            let s2 = wide("移出栅栏");
            let _ = AppendMenuW(menu, MF_STRING, MENU_ICON_REMOVE, PCWSTR(s2.as_ptr()));
        }
    }
    let cmd = unsafe {
        TrackPopupMenu(
            menu,
            TPM_RETURNCMD | TPM_NONOTIFY,
            sx,
            sy,
            Some(0),
            hwnd,
            None,
        )
        .0 as usize
    };
    unsafe {
        let _ = DestroyMenu(menu);
    }
    match cmd as usize {
        MENU_ICON_OPEN => Some(IconMenuAction::Open),
        MENU_ICON_REMOVE => Some(IconMenuAction::Remove),
        _ => None,
    }
}

/// 多选集合的右键菜单动作。
pub(crate) enum MultiMenuAction {
    Open,
    Copy,
    Remove,
    Delete,
}

/// 多选右键菜单：打开全部 / 复制 / 移出栅栏 / 删除。返回选中的动作。
/// `managed`（右键项为 Sylva 管理项，见 `handle_context_menu`）：移出与「删除」等价，
/// 跳过「移出栅栏」，只留 打开/复制/删除。
pub(crate) fn multi_icon_context_menu(
    hwnd: HWND,
    sx: i32,
    sy: i32,
    managed: bool,
) -> Option<MultiMenuAction> {
    const M_OPEN: usize = 1;
    const M_COPY: usize = 2;
    const M_REMOVE: usize = 3;
    const M_DELETE: usize = 4;
    let menu = popup_menu();
    if menu.is_invalid() {
        return None;
    }
    unsafe {
        let _ = AppendMenuW(menu, MF_STRING, M_OPEN, PCWSTR(wide("打开").as_ptr()));
        let _ = AppendMenuW(menu, MF_STRING, M_COPY, PCWSTR(wide("复制").as_ptr()));
        if !managed {
            let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
            let _ = AppendMenuW(menu, MF_STRING, M_REMOVE, PCWSTR(wide("移出栅栏").as_ptr()));
        }
        let _ = AppendMenuW(menu, MF_STRING, M_DELETE, PCWSTR(wide("删除").as_ptr()));
    }
    let cmd = unsafe {
        TrackPopupMenu(
            menu,
            TPM_RETURNCMD | TPM_NONOTIFY,
            sx,
            sy,
            Some(0),
            hwnd,
            None,
        )
        .0 as usize
    };
    unsafe {
        let _ = DestroyMenu(menu);
    }
    match cmd as usize {
        M_OPEN => Some(MultiMenuAction::Open),
        M_COPY => Some(MultiMenuAction::Copy),
        M_REMOVE => Some(MultiMenuAction::Remove),
        M_DELETE => Some(MultiMenuAction::Delete),
        _ => None,
    }
}

/// 把一组路径写入剪贴板（CF_HDROP，与资源管理器「复制」同一格式）。
pub(crate) fn set_clipboard_paths(paths: &[String]) {
    // DROPFILES 头（20 字节）：pFiles 偏移 + 坐标 + 标志；随后每个路径 UTF-16LE + \0，
    // 列表以额外 \0 结束（末路径的 \0 + 结尾 \0 = 双 \0 终止）。
    let header = 20usize;
    let bytes_len: usize = header
        + paths
            .iter()
            .map(|p| p.encode_utf16().count() * 2 + 2)
            .sum::<usize>()
        + 2;
    unsafe {
        if OpenClipboard(None).is_err() {
            return;
        }
        let _ = EmptyClipboard();
        if let Ok(hglobal) = GlobalAlloc(GMEM_MOVEABLE, bytes_len) {
            let ptr = GlobalLock(hglobal);
            if !ptr.is_null() {
                let buf = std::slice::from_raw_parts_mut(ptr as *mut u8, bytes_len);
                buf.fill(0);
                // DROPFILES 头（20 字节）：pFiles=偏移20、pt 坐标、fNC、fWide
                // 偏移：pFiles(0..4) | pt(4..12) | fNC(12..16) | fWide(16..20)
                buf[0..4].copy_from_slice(&(header as u32).to_le_bytes());
                buf[16..20].copy_from_slice(&1u32.to_le_bytes()); // fWide = TRUE（UTF-16）
                let mut off = header;
                for p in paths {
                    for u in p.encode_utf16() {
                        let bytes = u.to_le_bytes();
                        buf[off] = bytes[0];
                        buf[off + 1] = bytes[1];
                        off += 2;
                    }
                    off += 2; // 路径结尾 \0
                }
                // 列表结束双 \0 的第二个由上面的 buf.fill(0) 保证（bytes_len 已计入 +2）
                let _ = GlobalUnlock(hglobal);
                // fWide 位于偏移 16（BOOL），fNC 位于偏移 12
                let _ = SetClipboardData(CF_HDROP, Some(HANDLE(hglobal.0)));
            }
        }
        let _ = CloseClipboard();
    }
}

/// 打开原生选择器（`IFileOpenDialog` + `FOS_PICKFOLDERS`，多选文件夹），返回选中的
/// 绝对路径列表。用户取消（`HRESULT 0x800704C7`）或失败返回 None。
///
/// 单一「添加…」入口：Windows 系统对话框不能文件 + 文件夹混选（硬平台限制），
/// 这里用文件夹模式多选——能选中的（文件夹）直接添加进栅栏；单个/多个文件仍可
/// 拖拽或粘贴进栅栏。
///
/// COM 已在启动早期以 STA 初始化。对话框运行在自己模态消息循环里，期间到达的
/// overlay 事件会被重入守卫丢弃——与 `TrackPopupMenu` 同一套机制，不会破坏状态。
pub(crate) fn pick_paths(owner: HWND) -> Option<Vec<String>> {
    unsafe {
        let dialog: IFileOpenDialog =
            CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER).ok()?;
        let title = PCWSTR(wide("添加到栅栏").as_ptr());
        dialog.SetTitle(title).ok()?;
        let opts = FOS_PICKFOLDERS | FOS_ALLOWMULTISELECT | FOS_FORCEFILESYSTEM;
        dialog.SetOptions(opts).ok()?;
        if dialog.Show(Some(owner)).is_err() {
            return None; // 取消或失败：都不添加
        }
        let items: IShellItemArray = dialog.GetResults().ok()?;
        let count = items.GetCount().ok()?;
        let mut paths = Vec::with_capacity(count as usize);
        for i in 0..count {
            let item: IShellItem = items.GetItemAt(i).ok()?;
            let name: PWSTR = item.GetDisplayName(SIGDN_FILESYSPATH).ok()?;
            // FOS_PICKFOLDERS + FOS_FORCEFILESYSTEM：返回的必是文件系统路径
            let s = name.to_string().ok()?;
            paths.push(s);
        }
        Some(paths)
    }
}

/// 选择单个文件夹（新建栅栏时用）：弹出系统文件夹选择对话框，返回选中路径。
pub(crate) fn pick_folder(owner: HWND) -> Option<String> {
    unsafe {
        let dialog: IFileOpenDialog =
            CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER).ok()?;
        let title = PCWSTR(wide("选择栅栏链接的文件夹").as_ptr());
        dialog.SetTitle(title).ok()?;
        let opts = FOS_PICKFOLDERS | FOS_FORCEFILESYSTEM;
        dialog.SetOptions(opts).ok()?;
        if dialog.Show(Some(owner)).is_err() {
            return None;
        }
        let item: IShellItem = dialog.GetResult().ok()?;
        let name: PWSTR = item.GetDisplayName(SIGDN_FILESYSPATH).ok()?;
        name.to_string().ok()
    }
}

/// 栅栏右键菜单：粘贴 / 重命名 / 删除（精简版）。
pub(crate) fn fence_context_menu(
    rt: &mut Runtime,
    _fence: usize,
    sx: i32,
    sy: i32,
) -> Option<FenceMenuAction> {
    let hwnd = rt.hwnd;
    let main = popup_menu();
    if main.is_invalid() {
        return None;
    }
    unsafe {
        let s = wide("粘贴文件");
        let _ = AppendMenuW(main, MF_STRING, MENU_PASTE, PCWSTR(s.as_ptr()));
        let s = wide("重命名栅栏");
        let _ = AppendMenuW(main, MF_STRING, MENU_RENAME_FENCE, PCWSTR(s.as_ptr()));
        let _ = AppendMenuW(main, MF_SEPARATOR, 0, PCWSTR::null());
        let s = wide("删除栅栏");
        let _ = AppendMenuW(main, MF_STRING, MENU_DELETE_FENCE, PCWSTR(s.as_ptr()));
    }
    let cmd = unsafe {
        TrackPopupMenu(
            main,
            TPM_RETURNCMD | TPM_NONOTIFY,
            sx,
            sy,
            Some(0),
            hwnd,
            None,
        )
        .0 as usize
    };
    unsafe {
        let _ = DestroyMenu(main);
    }
    match cmd {
        MENU_PASTE => Some(FenceMenuAction::Paste),
        MENU_RENAME_FENCE => Some(FenceMenuAction::Rename),
        MENU_DELETE_FENCE => Some(FenceMenuAction::Delete),
        _ => None,
    }
}

/// 创建一个弹出菜单句柄（失败返回无效句柄，后续用 `is_invalid` 判空）。
pub(crate) fn popup_menu() -> HMENU {
    unsafe { CreatePopupMenu().unwrap_or_default() }
}

/// 把字符串转成 UTF-16（含结尾 NUL），供 Win32 宽字符 API 使用。
pub(crate) fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}
