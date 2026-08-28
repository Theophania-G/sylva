//! 右键/托盘菜单：菜单构建、动作分发、剪贴板与文件选择。

use crate::*;
pub(crate) const MENU_ICON_OPEN: usize = 1;
pub(crate) const MENU_ICON_REMOVE: usize = 2;
pub(crate) const MENU_LAYOUT: usize = 2000; // + 0=网格 1=列表
pub(crate) const MENU_ICON_SIZE: usize = 3000; // + 0/1/2 = 32/48/64
pub(crate) const MENU_STYLE: usize = 4000; // 背景风格子菜单：+0=玻璃 +1=透明 +2=颜色
pub(crate) const MENU_PASTE: usize = 2500; // 粘贴剪贴板文件
pub(crate) const MENU_DELETE_FENCE: usize = 5000;
pub(crate) const MENU_RENAME_FENCE: usize = 6000; // 重命名栅栏（栅栏内就地编辑）
pub(crate) const MENU_TINT: usize = 7000; // 背景色调子菜单项：+0=默认，+1..=N 对应预设
                               // 菜单动作的具名常量（match 中常量模式不能含算术）
pub(crate) const MENU_LAYOUT_GRID: usize = MENU_LAYOUT;
pub(crate) const MENU_LAYOUT_LIST: usize = MENU_LAYOUT + 1;
pub(crate) const MENU_ICON_SIZE_SMALL: usize = MENU_ICON_SIZE;
pub(crate) const MENU_ICON_SIZE_MID: usize = MENU_ICON_SIZE + 1;
pub(crate) const MENU_ICON_SIZE_LARGE: usize = MENU_ICON_SIZE + 2;
pub(crate) const MENU_STYLE_GLASS: usize = MENU_STYLE;
pub(crate) const MENU_STYLE_TRANSPARENT: usize = MENU_STYLE + 1;
pub(crate) const MENU_STYLE_COLOR: usize = MENU_STYLE + 2;
pub(crate) const MENU_STYLE_BLUR: usize = MENU_STYLE + 3;
/// 右键菜单「添加…」（单一入口：能选中的直接添加，不做文件/文件夹区分）。
pub(crate) const MENU_ADD: usize = 1500;
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

/// 栅栏右键菜单动作。
pub(crate) enum FenceMenuAction {
    /// 打开原生选择器（文件夹多选模式——Windows 的系统对话框只能单一类型多选；
    /// 选中的文件夹直接添加进本栅栏，文件走拖拽/粘贴）。
    Add,
    /// 从剪贴板粘贴文件进本栅栏。
    Paste,
    /// 就地重命名栅栏标题。
    Rename,
    SetLayout(FenceLayout),
    SetIconSize(f32),
    /// 设置背景风格（玻璃 / 透明 / 颜色）。
    SetStyle(FenceStyle),
    /// 设置背景色调；None = 恢复默认玻璃底色。
    SetTint(Option<[f32; 3]>),
    Delete,
}

/// 处理右键：弹出上下文菜单并执行选中动作（菜单为模态，阻塞到关闭）。
pub(crate) fn handle_context_menu(rt: &mut Runtime, fence: usize, icon: Option<usize>, _pos: (f32, f32)) {
    let (sx, sy) = cursor_screen();
    if let Some(ii) = icon {
        // 该项文件路径（多选移出/链接判定也要用）
        let path = rt
            .desk
            .fences
            .get(fence)
            .and_then(|f| f.icon_ids.get(ii))
            .and_then(|id| rt.desk.icons.get(id))
            .and_then(|ic| ic.path.clone());
        // 路径位于某个链接存储文件夹内：链接栅栏「移出栅栏」与「删除」等价（都删文件，
        // 否则镜像把图标加回来），菜单不再重复提供「移出栅栏」。
        let linked = path.as_ref().map(|p| is_linked_path(rt, p)).unwrap_or(false);
        // 多选集合：右键集合中的任意一项 → 集合操作（打开全部 / 复制 / 移出 / 删除）。
        // 右键未选中的项 → 先单选该项，再走单项逻辑（资源管理器行为）。
        let key = (fence, ii);
        let multi = rt.selected.len() > 1 && rt.selected.contains(&key);
        if multi {
            match multi_icon_context_menu(rt.hwnd, sx, sy, linked) {
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
        // 退回简版「打开 / 移出栅栏」。
        match path {
            Some(p) => match shell_menu::show(&p, rt.hwnd, sx, sy, linked) {
                Some(shell_menu::ShellMenuAction::Remove) => remove_fence_icon(rt, fence, ii),
                Some(shell_menu::ShellMenuAction::Rename) => {
                    start_inplace_rename(rt, EditTarget::Item { fence, icon: ii })
                }
                Some(shell_menu::ShellMenuAction::Invoked) => {
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
                // 真实 Shell 菜单没弹出来（路径无效 / COM 异常等）：退回简版菜单，
                // 保证右键必有反馈，不让「Windows 右击列表」静默消失。
                None => {
                    tracing::warn!(path = %p, "Shell 右键菜单创建失败，退回简版菜单");
                    match icon_context_menu(rt.hwnd, sx, sy) {
                        Some(IconMenuAction::Open) => launch_fence_icon(rt, fence, ii),
                        Some(IconMenuAction::Remove) => remove_fence_icon(rt, fence, ii),
                        None => {}
                    }
                }
            },
            None => match icon_context_menu(rt.hwnd, sx, sy) {
                Some(IconMenuAction::Open) => launch_fence_icon(rt, fence, ii),
                Some(IconMenuAction::Remove) => remove_fence_icon(rt, fence, ii),
                None => {}
            },
        }
    } else if let Some(action) = fence_context_menu(rt, fence, sx, sy) {
        match action {
            FenceMenuAction::Add => {
                if let Some(paths) = pick_paths(rt.hwnd) {
                    add_paths_to_fence(rt, fence, &paths);
                    let _ = rt.store.save(&rt.desk);
                }
            }
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
            FenceMenuAction::SetLayout(l) => {
                if l == FenceLayout::List {
                    let w = list_auto_width(rt, fence);
                    if let Some(f) = rt.desk.fences.get_mut(fence) {
                        f.appearance.layout = l;
                        f.bounds.w = w;
                    }
                } else if let Some(f) = rt.desk.fences.get_mut(fence) {
                    f.appearance.layout = l;
                }
                let _ = rt.store.save(&rt.desk);
            }
            FenceMenuAction::SetIconSize(s) => {
                if let Some(f) = rt.desk.fences.get_mut(fence) {
                    f.appearance.icon_size = s;
                }
                let _ = rt.store.save(&rt.desk);
            }
            FenceMenuAction::SetStyle(style) => {
                if let Some(f) = rt.desk.fences.get_mut(fence) {
                    f.appearance.bg_style = style;
                }
                let _ = rt.store.save(&rt.desk);
            }
            FenceMenuAction::SetTint(tint) => {
                if let Some(f) = rt.desk.fences.get_mut(fence) {
                    f.appearance.tint = tint;
                }
                let _ = rt.store.save(&rt.desk);
            }
            FenceMenuAction::Delete => {
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

/// 图标右键菜单：打开 / 移出栅栏。返回选中的动作。
pub(crate) fn icon_context_menu(hwnd: HWND, sx: i32, sy: i32) -> Option<IconMenuAction> {
    let menu = popup_menu();
    if menu.is_invalid() {
        return None;
    }
    unsafe {
        let s = wide("打开");
        let _ = AppendMenuW(menu, MF_STRING, MENU_ICON_OPEN, PCWSTR(s.as_ptr()));
        let s2 = wide("移出栅栏");
        let _ = AppendMenuW(menu, MF_STRING, MENU_ICON_REMOVE, PCWSTR(s2.as_ptr()));
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
/// `linked`（右键项在链接栅栏的存储文件夹内）：移出=删除（镜像会加回图标），
/// 与「删除」重复，跳过「移出栅栏」，只留 打开/复制/删除。
pub(crate) fn multi_icon_context_menu(hwnd: HWND, sx: i32, sy: i32, linked: bool) -> Option<MultiMenuAction> {
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
        if !linked {
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

/// 栅栏右键菜单：添加 / 布局 / 图标大小 / 透明度 / 背景色调 / 删除栅栏。
pub(crate) fn fence_context_menu(rt: &mut Runtime, fence: usize, sx: i32, sy: i32) -> Option<FenceMenuAction> {
    let hwnd = rt.hwnd;
    let app = &rt.desk.fences.get(fence)?.appearance;

    // 布局子菜单
    let layout_menu = popup_menu();
    if !layout_menu.is_invalid() {
        for (i, l) in [FenceLayout::Grid, FenceLayout::List].iter().enumerate() {
            let flag = if *l == app.layout {
                MF_STRING | MF_CHECKED
            } else {
                MF_STRING
            };
            let s = wide(l.label());
            unsafe {
                let _ = AppendMenuW(layout_menu, flag, MENU_LAYOUT + i, PCWSTR(s.as_ptr()));
            }
        }
    }

    // 图标大小子菜单：仅网格布局有图标大小概念，列表布局下不生成（隐藏该接口）
    let mut size_menu = HMENU::default();
    if app.layout == FenceLayout::Grid {
        size_menu = popup_menu();
        if !size_menu.is_invalid() {
            const SIZES: [(f32, &str); 3] =
                [(32.0, "小（32）"), (48.0, "中（48）"), (64.0, "大（64）")];
            for (i, (sz, lb)) in SIZES.iter().enumerate() {
                let flag = if (app.icon_size - sz).abs() < 0.5 {
                    MF_STRING | MF_CHECKED
                } else {
                    MF_STRING
                };
                let s = wide(lb);
                unsafe {
                    let _ = AppendMenuW(size_menu, flag, MENU_ICON_SIZE + i, PCWSTR(s.as_ptr()));
                }
            }
        }
    }

    // 背景色调子菜单：默认（玻璃底色）+ 预设色板
    let tint_menu = popup_menu();
    if !tint_menu.is_invalid() {
        let s = wide("默认（玻璃底色）");
        let flag_clear = if app.tint.is_none() {
            MF_STRING | MF_CHECKED
        } else {
            MF_STRING
        };
        let _ = unsafe { AppendMenuW(tint_menu, flag_clear, MENU_TINT, PCWSTR(s.as_ptr())) };
        for (i, (lb, c)) in TINT_PRESETS.iter().enumerate() {
            let s = wide(lb);
            let flag = if app.tint == Some(*c) {
                MF_STRING | MF_CHECKED
            } else {
                MF_STRING
            };
            let _ = unsafe { AppendMenuW(tint_menu, flag, MENU_TINT + 1 + i, PCWSTR(s.as_ptr())) };
        }
    }

    // 背景风格子菜单：玻璃 / 透明 / 颜色 / 模糊（四选一，当前风格打勾）
    let style_menu = popup_menu();
    if !style_menu.is_invalid() {
        let styles = [
            FenceStyle::Glass,
            FenceStyle::Outline,
            FenceStyle::Filled,
            FenceStyle::Blur,
        ];
        for (i, st) in styles.iter().enumerate() {
            let flag = if *st == app.bg_style {
                MF_STRING | MF_CHECKED
            } else {
                MF_STRING
            };
            let s = wide(st.label());
            let _ = unsafe { AppendMenuW(style_menu, flag, MENU_STYLE + i, PCWSTR(s.as_ptr())) };
        }
    }

    // 主菜单
    let main = popup_menu();
    if main.is_invalid() {
        unsafe {
            let _ = DestroyMenu(layout_menu);
            let _ = DestroyMenu(size_menu);
            let _ = DestroyMenu(tint_menu);
            let _ = DestroyMenu(style_menu);
        }
        return None;
    }
    unsafe {
        // 单一「添加…」入口：系统对话框无法文件+文件夹混选（平台限制），用文件夹模式
        // 多选——能选中的（文件夹）直接添加，不区分「文件 / 文件夹」两个入口。
        let s = wide("添加…");
        let _ = AppendMenuW(main, MF_STRING, MENU_ADD, PCWSTR(s.as_ptr()));
        if !layout_menu.is_invalid() {
            let s = wide("布局");
            let _ = AppendMenuW(main, MF_POPUP, layout_menu.0 as usize, PCWSTR(s.as_ptr()));
        }
        if !size_menu.is_invalid() {
            let s = wide("图标大小");
            let _ = AppendMenuW(main, MF_POPUP, size_menu.0 as usize, PCWSTR(s.as_ptr()));
        }
        // 背景风格：玻璃 / 透明 / 颜色（三选一）
        if !style_menu.is_invalid() {
            let s = wide("背景风格");
            let _ = AppendMenuW(main, MF_POPUP, style_menu.0 as usize, PCWSTR(s.as_ptr()));
        }
        if !tint_menu.is_invalid() {
            let s = wide("背景色调");
            let _ = AppendMenuW(main, MF_POPUP, tint_menu.0 as usize, PCWSTR(s.as_ptr()));
        }
        let _ = AppendMenuW(main, MF_SEPARATOR, 0, PCWSTR::null());
        let s = wide("重命名栅栏");
        let _ = AppendMenuW(main, MF_STRING, MENU_RENAME_FENCE, PCWSTR(s.as_ptr()));
        let s = wide("粘贴文件（从剪贴板）");
        let _ = AppendMenuW(main, MF_STRING, MENU_PASTE, PCWSTR(s.as_ptr()));
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
        let _ = DestroyMenu(layout_menu);
        let _ = DestroyMenu(size_menu);
        let _ = DestroyMenu(tint_menu);
        let _ = DestroyMenu(style_menu);
        let _ = DestroyMenu(main);
    }

    match cmd as usize {
        MENU_DELETE_FENCE => Some(FenceMenuAction::Delete),
        MENU_RENAME_FENCE => Some(FenceMenuAction::Rename),
        MENU_PASTE => Some(FenceMenuAction::Paste),
        MENU_ADD => Some(FenceMenuAction::Add),
        MENU_STYLE_GLASS => Some(FenceMenuAction::SetStyle(FenceStyle::Glass)),
        MENU_STYLE_TRANSPARENT => Some(FenceMenuAction::SetStyle(FenceStyle::Outline)),
        MENU_STYLE_COLOR => Some(FenceMenuAction::SetStyle(FenceStyle::Filled)),
        MENU_STYLE_BLUR => Some(FenceMenuAction::SetStyle(FenceStyle::Blur)),
        x if x >= MENU_TINT && x <= MENU_TINT + TINT_PRESETS.len() => {
            if x == MENU_TINT {
                Some(FenceMenuAction::SetTint(None))
            } else {
                TINT_PRESETS
                    .get(x - MENU_TINT - 1)
                    .map(|(_, c)| FenceMenuAction::SetTint(Some(*c)))
            }
        }
        MENU_LAYOUT_GRID => Some(FenceMenuAction::SetLayout(FenceLayout::Grid)),
        MENU_LAYOUT_LIST => Some(FenceMenuAction::SetLayout(FenceLayout::List)),
        MENU_ICON_SIZE_SMALL => Some(FenceMenuAction::SetIconSize(32.0)),
        MENU_ICON_SIZE_MID => Some(FenceMenuAction::SetIconSize(48.0)),
        MENU_ICON_SIZE_LARGE => Some(FenceMenuAction::SetIconSize(64.0)),
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

