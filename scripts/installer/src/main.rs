//! Sylva 图形化安装程序：亮色安装窗口，可自选安装位置，按当前用户安装。
//!
//! - **DPI**：内嵌 Per-Monitor v2 清单（见 build.rs）——高缩放下原生渲染不再模糊，
//!   跨屏/改缩放在 `WM_DPICHANGED` 实时重排布局并重建字体（否则整窗被拉伸、文字错位）。
//! - **安装位置**：路径编辑框 + 「浏览…」文件夹选择器（`IFileOpenDialog` + `FOS_PICKFOLDERS`）。
//! - **布局**：96 DIP 基准，运行时按 `dpi/96` 缩放；底部按钮按客户区右下角对齐。
//! - 免管理员权限：默认装到 `%LOCALAPPDATA%\Programs\Sylva`，建桌面/开始菜单快捷方式，
//!   可选开机自启，注册控制面板卸载项，安装完成后自动启动。

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::env;
use std::fs;
#[cfg(windows)]
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::Command;

use windows::core::{w, PCWSTR, PWSTR};
use windows::Win32::Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    CreateFontIndirectW, CreateSolidBrush, DeleteObject, RedrawWindow, SetBkColor, SetTextColor,
    FONT_CHARSET, FONT_CLIP_PRECISION, FONT_OUTPUT_PRECISION, FONT_QUALITY, HBRUSH, HDC, HFONT,
    LOGFONTW, RDW_ALLCHILDREN, RDW_INVALIDATE, RDW_UPDATENOW,
};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, IBindCtx, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::{PBM_SETBARCOLOR, PBM_SETPOS, PBM_SETRANGE32, PBS_SMOOTH};
use windows::Win32::UI::HiDpi::{
    GetDpiForSystem, GetDpiForWindow, SetProcessDpiAwarenessContext,
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows::Win32::UI::Shell::{
    FileOpenDialog, IFileOpenDialog, IShellItem, SHCreateItemFromParsingName, FOS_FORCEFILESYSTEM,
    FOS_PICKFOLDERS, SIGDN_FILESYSPATH,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyIcon, DestroyWindow, DispatchMessageW, GetClientRect,
    GetMessageW, GetSystemMetrics, GetWindowLongPtrW, IsDialogMessageW, LoadIconW, LoadImageW,
    MessageBoxW, PostQuitMessage, RegisterClassW, SendMessageW, SetWindowLongPtrW, SetWindowPos,
    SetWindowTextW, ShowWindow, TranslateMessage, BM_GETCHECK, BM_SETCHECK, BN_CLICKED,
    BS_AUTOCHECKBOX, BS_DEFPUSHBUTTON, CS_HREDRAW, CS_VREDRAW, ES_AUTOHSCROLL, GWLP_USERDATA,
    HICON, HMENU, ICON_BIG, ICON_SMALL, IDC_STATIC, IMAGE_ICON, LR_DEFAULTCOLOR, MESSAGEBOX_STYLE,
    MSG, SM_CXSCREEN, SM_CYSCREEN, STM_SETICON, SWP_NOACTIVATE, SWP_NOSIZE, SWP_NOZORDER, SW_SHOW,
    WINDOW_EX_STYLE, WINDOW_STYLE, WM_COMMAND, WM_CTLCOLORBTN, WM_CTLCOLOREDIT, WM_CTLCOLORSTATIC,
    WM_DESTROY, WM_DPICHANGED, WM_GETTEXT, WM_PAINT, WM_SETFONT, WM_SETICON, WNDCLASSW, WS_CAPTION,
    WS_CHILD, WS_CLIPSIBLINGS, WS_EX_CLIENTEDGE, WS_MINIMIZEBOX, WS_OVERLAPPED, WS_SYSMENU,
    WS_TABSTOP, WS_VISIBLE,
};

/// 内嵌主程序（编译期打包，安装器独立分发）。
const SYLVA_EXE: &[u8] = include_bytes!("../../../target/release/sylva.exe");

// 控件 ID（WM_COMMAND 分发用）
const ID_BTN_INSTALL: usize = 1001;
const ID_BTN_CANCEL: usize = 1002;
const ID_BTN_BROWSE: usize = 1003;
const ID_PROGRESS: usize = 1004;
const ID_STATUS: usize = 1005;
const ID_CHK_AUTOSTART: usize = 1006;
const ID_PATH: usize = 1007;

/// 本模块未从 windows-rs 引入的少数消息/样式位，直接以数值定义。
const SS_ICON: u32 = 0x0003;
const SS_CENTERIMAGE: u32 = 0x0200;
const BST_CHECKED: usize = 1;

/// 安装面板白底黑字（深灰辅助文字）
const PANEL_BG: COLORREF = COLORREF(0x00_FF_FF_FF); // 白
const TEXT_FG: COLORREF = COLORREF(0x00_00_00_00); // 黑
const TEXT_MUTED: COLORREF = COLORREF(0x00_6B_6B_6B); // 灰（副标题/提示/状态）
const ACCENT: COLORREF = COLORREF(0x00_3B_82_F6); // RGB(59,130,246) 进度条蓝

/// 子进程不弹出控制台窗口（CREATE_NO_WINDOW）
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

struct Installer {
    hwnd: HWND,
    /// 头部图标（`LoadImageW` 按 DPI 加载，销毁窗口时释放）。
    hicon: HICON,
    hicon_static: HWND,
    htitle: HWND,
    hsub: HWND,
    hsection: HWND,
    hhint: HWND,
    hpath: HWND,
    hbtn_browse: HWND,
    hchk: HWND,
    hprogress: HWND,
    hstatus: HWND,
    hbtn_install: HWND,
    hbtn_cancel: HWND,
    brush: HBRUSH,
    font_title: HFONT,
    font_section: HFONT,
    font_ui: HFONT,
    /// 当前窗口 DPI（字体/布局基准）。
    dpi: u32,
    /// 安装进行中：屏蔽按钮重入（模态错误框的消息循环会送达点击）。
    busy: bool,
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// DIP → 物理像素（以窗口 DPI 缩放）。
fn dl(dip: i32, dpi: u32) -> i32 {
    ((dip as f32) * (dpi as f32) / 96.0).round() as i32
}

/// 加载 exe 内嵌主图标（资源 ID 1 = build.rs 嵌入的 sylva.ico）。
#[allow(clippy::manual_dangling_ptr)]
fn app_icon(hinstance: HINSTANCE) -> HICON {
    unsafe { LoadIconW(Some(hinstance), PCWSTR(1 as *const u16)) }.unwrap_or_default()
}

/// 按 DPI 尺寸从资源加载头部图标（多尺寸 ico，LoadImage 取最接近的帧）。
#[allow(clippy::manual_dangling_ptr)]
fn load_header_icon(hinstance: HINSTANCE, dpi: u32) -> HICON {
    let size = dl(48, dpi);
    let h = unsafe {
        LoadImageW(
            Some(hinstance),
            PCWSTR(1 as *const u16),
            IMAGE_ICON,
            size,
            size,
            LR_DEFAULTCOLOR,
        )
    };
    match h {
        Ok(handle) => HICON(handle.0),
        Err(_) => app_icon(hinstance),
    }
}

/// 创建 UI 字体（ClearType，尺寸按 DPI 缩放）。
fn make_font(size_dip: i32, weight: i32, dpi: u32) -> HFONT {
    let mut lf: LOGFONTW = unsafe { std::mem::zeroed() };
    lf.lfHeight = -dl(size_dip, dpi);
    lf.lfWeight = weight;
    lf.lfCharSet = FONT_CHARSET(1); // DEFAULT_CHARSET
    lf.lfOutPrecision = FONT_OUTPUT_PRECISION(0);
    lf.lfClipPrecision = FONT_CLIP_PRECISION(0);
    lf.lfQuality = FONT_QUALITY(5); // CLEARTYPE_QUALITY
    let face = wide("Microsoft YaHei UI");
    for (i, c) in face.iter().take(31).enumerate() {
        lf.lfFaceName[i] = *c;
    }
    unsafe { CreateFontIndirectW(&lf) }
}

/// (标题 20/600, 小节 14/600, 正文 12/400)。
fn build_fonts(dpi: u32) -> (HFONT, HFONT, HFONT) {
    (
        make_font(20, 600, dpi),
        make_font(14, 600, dpi),
        make_font(12, 400, dpi),
    )
}

fn msgbox(title: &str, text: &str, error: bool) {
    unsafe {
        let t = wide(title);
        let b = wide(text);
        let _ = MessageBoxW(
            None,
            PCWSTR(b.as_ptr()),
            PCWSTR(t.as_ptr()),
            MESSAGEBOX_STYLE(if error { 0x10 } else { 0x40 }),
        );
    }
}

/// 重绘整窗（含子控件），让进度/状态在安装期间即时可见。
fn flush(app: &Installer) {
    unsafe {
        let _ = RedrawWindow(
            Some(app.hwnd),
            None,
            None,
            RDW_INVALIDATE | RDW_UPDATENOW | RDW_ALLCHILDREN,
        );
    }
}

fn set_status(app: &Installer, text: &str) {
    unsafe {
        let t = wide(text);
        let _ = SetWindowTextW(app.hstatus, PCWSTR(t.as_ptr()));
    }
    flush(app);
}

fn set_progress(app: &Installer, percent: u32) {
    unsafe {
        let _ = SendMessageW(
            app.hprogress,
            PBM_SETPOS,
            Some(WPARAM(percent as usize)),
            Some(LPARAM(0)),
        );
    }
    flush(app);
}

fn reg_add(key: &str, value: &str, data: &str) {
    let _ = Command::new("reg")
        .args(["add", key, "/v", value, "/t", "REG_SZ", "/d", data, "/f"])
        .creation_flags(CREATE_NO_WINDOW)
        .status();
}

/// 默认安装位置：%LOCALAPPDATA%\Programs\Sylva。
fn default_path() -> String {
    env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(env::temp_dir)
        .join("Programs")
        .join("Sylva")
        .to_string_lossy()
        .into_owned()
}

fn get_path(app: &Installer) -> String {
    unsafe {
        let mut buf = [0u16; 1025];
        let n = SendMessageW(
            app.hpath,
            WM_GETTEXT,
            Some(WPARAM(1024)),
            Some(LPARAM(buf.as_mut_ptr() as isize)),
        )
        .0 as usize;
        let n = n.min(1024);
        String::from_utf16_lossy(&buf[..n])
    }
}

fn set_path(app: &Installer, path: &str) {
    unsafe {
        let p = wide(path);
        let _ = SetWindowTextW(app.hpath, PCWSTR(p.as_ptr()));
    }
}

/// 原生文件夹选择器（`IFileOpenDialog` + `FOS_PICKFOLDERS` 单选），预置到当前路径。
/// 用户取消返回 None。
fn pick_folder(owner: HWND, current: &str) -> Option<String> {
    unsafe {
        let dialog: IFileOpenDialog =
            CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER).ok()?;
        dialog
            .SetTitle(PCWSTR(wide("选择安装文件夹").as_ptr()))
            .ok()?;
        dialog
            .SetOptions(FOS_PICKFOLDERS | FOS_FORCEFILESYSTEM)
            .ok()?;
        // 预置到当前安装路径，减少翻找
        let cur = wide(current);
        if !cur.is_empty() {
            let item = SHCreateItemFromParsingName::<PCWSTR, Option<&IBindCtx>, IShellItem>(
                PCWSTR(cur.as_ptr()),
                None,
            );
            if let Ok(item) = item {
                let _ = dialog.SetFolder(&item);
            }
        }
        if dialog.Show(Some(owner)).is_err() {
            return None; // 取消
        }
        let item: IShellItem = dialog.GetResult().ok()?;
        let name: PWSTR = item.GetDisplayName(SIGDN_FILESYSPATH).ok()?;
        name.to_string().ok()
    }
}

fn run_install(app: &mut Installer) -> bool {
    // 读取用户选择/输入的安装目录；空则回退默认
    let raw = get_path(app);
    let trimmed = raw.trim().to_string();
    let dest = if trimmed.is_empty() {
        PathBuf::from(default_path())
    } else {
        PathBuf::from(&trimmed)
    };
    let autostart = unsafe { SendMessageW(app.hchk, BM_GETCHECK, None, None).0 == 1 };

    app.busy = true;
    set_status(app, "正在安装…");
    set_progress(app, 5);

    if let Err(e) = fs::create_dir_all(&dest) {
        app.busy = false;
        set_status(app, "安装失败");
        msgbox(
            "Sylva 安装",
            &format!("无法创建安装目录：\n{}\n\n（{e}）", dest.display()),
            true,
        );
        return false;
    }
    if let Err(e) = fs::write(dest.join("sylva.exe"), SYLVA_EXE) {
        app.busy = false;
        set_status(app, "安装失败");
        msgbox(
            "Sylva 安装",
            &format!(
                "写入主程序失败：\n{}（{e}）",
                dest.join("sylva.exe").display()
            ),
            true,
        );
        return false;
    }
    set_progress(app, 35);

    // 桌面 + 开始菜单快捷方式（PowerShell 编码命令，中文安全）
    let exe_ps = dest
        .join("sylva.exe")
        .to_string_lossy()
        .replace('\\', "\\\\")
        .replace('\'', "''");
    let script = format!(
        "$ws=New-Object -ComObject WScript.Shell; \
         $d='{exe}'; \
         $l=$ws.CreateShortcut((Join-Path ([Environment]::GetFolderPath('Desktop')) 'Sylva.lnk')); \
         $l.TargetPath=$d; $l.WorkingDirectory=Split-Path $d; \
         $l.Description='Sylva 桌面栅栏整理器'; $l.Save(); \
         $m=Join-Path ([Environment]::GetFolderPath('Programs')) 'Sylva'; \
         New-Item -ItemType Directory -Force -Path $m|Out-Null; \
         $l2=$ws.CreateShortcut((Join-Path $m 'Sylva.lnk')); \
         $l2.TargetPath=$d; $l2.WorkingDirectory=Split-Path $d; \
         $l2.Description='Sylva 桌面栅栏整理器'; $l2.Save()",
        exe = exe_ps
    );
    let encoded = encode_command(&script);
    let ok = Command::new("powershell")
        .args([
            "-NoProfile",
            "-ExecutionPolicy",
            "Bypass",
            "-EncodedCommand",
            &encoded,
        ])
        .creation_flags(CREATE_NO_WINDOW)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !ok {
        app.busy = false;
        set_status(app, "安装失败");
        msgbox(
            "Sylva 安装",
            "创建快捷方式失败，请关闭安全软件后重试。",
            true,
        );
        return false;
    }
    set_progress(app, 60);

    // 开机自启（仅勾选时）+ 卸载信息
    if autostart {
        let exe_quoted = format!("\"{}\"", dest.join("sylva.exe").display());
        reg_add(
            r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run",
            "Sylva",
            &exe_quoted,
        );
    }
    let unreg = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Uninstall\Sylva";
    let dest_disp = dest.to_string_lossy();
    let exe_quoted = format!("\"{}\"", dest.join("sylva.exe").display());
    reg_add(unreg, "DisplayName", "Sylva 桌面栅栏整理器");
    reg_add(unreg, "DisplayVersion", "0.1.0");
    reg_add(unreg, "Publisher", "Sylva");
    reg_add(unreg, "InstallLocation", &dest_disp);
    reg_add(unreg, "DisplayIcon", &exe_quoted);
    reg_add(
        unreg,
        "UninstallString",
        &format!("\"{}\"", dest.join("uninstall.cmd").display()),
    );
    reg_add(unreg, "NoModify", "1");
    reg_add(unreg, "NoRepair", "1");
    set_progress(app, 85);

    // 卸载脚本（cmd 转义：% → %%，& → ^&，防路径特殊字符破坏命令）
    let dest_cmd = dest_disp.replace('%', "%%").replace('&', "^&");
    let uninstall = format!(
        "@echo off\r\nsetlocal\r\nset DEST={}\r\ntaskkill /IM sylva.exe /F >nul 2>&1\r\nreg delete \"HKCU\\Software\\Microsoft\\Windows\\CurrentVersion\\Run\" /v Sylva /f >nul 2>&1\r\nreg delete \"HKCU\\Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\Sylva\" /f >nul 2>&1\r\ndel \"%USERPROFILE%\\Desktop\\Sylva.lnk\" >nul 2>&1\r\nrd /s /q \"%APPDATA%\\Microsoft\\Windows\\Start Menu\\Programs\\Sylva\" >nul 2>&1\r\nrd /s /q \"%DEST%\" >nul 2>&1\r\necho Sylva 已卸载。\r\npause\r\n",
        dest_cmd
    );
    let _ = fs::write(dest.join("uninstall.cmd"), uninstall);
    set_progress(app, 95);

    // 启动
    let _ = Command::new(dest.join("sylva.exe"))
        .creation_flags(CREATE_NO_WINDOW)
        .spawn();
    set_status(app, "安装完成，Sylva 已启动。");
    set_progress(app, 100);
    true
}

/// 把脚本编码为 UTF-16LE Base64（PowerShell -EncodedCommand，中文安全）。
fn encode_command(script: &str) -> String {
    let bytes: Vec<u8> = script
        .encode_utf16()
        .flat_map(|u| u.to_le_bytes())
        .collect();
    base64_encode(&bytes)
}

/// 极简 Base64（不引入第三方依赖；脚本很小，无需性能）。
fn base64_encode(data: &[u8]) -> String {
    const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in data.chunks(3) {
        let b = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            T[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            T[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

unsafe fn place(h: HWND, x: i32, y: i32, w: i32, hh: i32) {
    let _ = SetWindowPos(h, None, x, y, w, hh, SWP_NOZORDER | SWP_NOACTIVATE);
}

/// 按当前 DPI 重排全部子控件并应用字体。DPI 变化时同时重建字体。
fn relayout(app: &mut Installer, dpi: u32) {
    unsafe {
        let mut rc = RECT::default();
        let _ = GetClientRect(app.hwnd, &mut rc);
        let (cw, ch) = (rc.right, rc.bottom);

        // 头部：图标 + 标题 + 副标题（左对齐，间距 12）
        place(
            app.htitle,
            dl(84, dpi),
            dl(20, dpi),
            dl(532, dpi),
            dl(36, dpi),
        );
        place(
            app.hsub,
            dl(84, dpi),
            dl(62, dpi),
            dl(532, dpi),
            dl(22, dpi),
        );
        place(
            app.hicon_static,
            dl(24, dpi),
            dl(24, dpi),
            dl(48, dpi),
            dl(48, dpi),
        );

        // 安装位置区：小节标题 + 提示 + 路径框 + 浏览按钮
        place(
            app.hsection,
            dl(24, dpi),
            dl(116, dpi),
            dl(592, dpi),
            dl(24, dpi),
        );
        place(
            app.hhint,
            dl(24, dpi),
            dl(146, dpi),
            dl(592, dpi),
            dl(20, dpi),
        );
        place(
            app.hpath,
            dl(24, dpi),
            dl(178, dpi),
            dl(496, dpi),
            dl(32, dpi),
        );
        place(
            app.hbtn_browse,
            dl(532, dpi),
            dl(178, dpi),
            dl(84, dpi),
            dl(32, dpi),
        );

        // 自启勾选框 / 进度条 / 状态
        place(
            app.hchk,
            dl(24, dpi),
            dl(228, dpi),
            dl(420, dpi),
            dl(26, dpi),
        );
        place(
            app.hprogress,
            dl(24, dpi),
            dl(300, dpi),
            dl(592, dpi),
            dl(24, dpi),
        );
        place(
            app.hstatus,
            dl(24, dpi),
            dl(336, dpi),
            dl(592, dpi),
            dl(20, dpi),
        );

        // 底部按钮：右下角对齐（取消最右，安装靠左并作为默认按钮）
        let btn_y = ch - dl(24, dpi) - dl(36, dpi);
        let btn_right = cw - dl(24, dpi) - dl(96, dpi);
        place(app.hbtn_cancel, btn_right, btn_y, dl(96, dpi), dl(36, dpi));
        place(
            app.hbtn_install,
            btn_right - dl(12, dpi) - dl(96, dpi),
            btn_y,
            dl(96, dpi),
            dl(36, dpi),
        );
    }

    // DPI 变化时重建字体（布局不变则沿用）
    if dpi != app.dpi {
        let (t, s, u) = build_fonts(dpi);
        unsafe {
            let _ = DeleteObject(app.font_title.into());
            let _ = DeleteObject(app.font_section.into());
            let _ = DeleteObject(app.font_ui.into());
        }
        app.font_title = t;
        app.font_section = s;
        app.font_ui = u;
        app.dpi = dpi;
    }
    apply_fonts(app);
}

/// 给各控件套上对应字体（标题/小节用加粗字号，其余用正文）。
fn apply_fonts(app: &Installer) {
    unsafe {
        let set = |h: HWND, f: HFONT| {
            let _ = SendMessageW(h, WM_SETFONT, Some(WPARAM(f.0 as usize)), Some(LPARAM(1)));
        };
        set(app.htitle, app.font_title);
        set(app.hsection, app.font_section);
        set(app.hsub, app.font_ui);
        set(app.hhint, app.font_ui);
        set(app.hstatus, app.font_ui);
        set(app.hpath, app.font_ui);
        set(app.hbtn_browse, app.font_ui);
        set(app.hchk, app.font_ui);
        set(app.hbtn_install, app.font_ui);
        set(app.hbtn_cancel, app.font_ui);
    }
}

unsafe extern "system" fn wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let app_ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA);
    match msg {
        WM_COMMAND => {
            let mut to_destroy = None;
            if app_ptr != 0 {
                let app = &mut *(app_ptr as *mut Installer);
                let id = wparam.0 & 0xFFFF;
                let code = wparam.0 >> 16;
                if code == BN_CLICKED as usize {
                    match id {
                        ID_BTN_INSTALL if !app.busy => {
                            let ok = run_install(app);
                            if ok {
                                to_destroy = Some(app.hwnd);
                            }
                        }
                        ID_BTN_BROWSE if !app.busy => {
                            let cur = get_path(app);
                            if let Some(p) = pick_folder(hwnd, &cur) {
                                set_path(app, &p);
                            }
                        }
                        ID_BTN_CANCEL if !app.busy => {
                            to_destroy = Some(hwnd);
                        }
                        _ => {}
                    }
                }
            }
            if let Some(h) = to_destroy {
                let _ = DestroyWindow(h);
            }
            LRESULT(0)
        }
        // wParam=HDC，lParam=控件 hwnd（注意别接反）
        WM_CTLCOLORSTATIC => {
            if app_ptr != 0 {
                let app = &*(app_ptr as *const Installer);
                let hdc = HDC(wparam.0 as *mut core::ffi::c_void);
                let ctl = HWND(lparam.0 as *mut core::ffi::c_void);
                let fg = if ctl == app.hsub || ctl == app.hhint || ctl == app.hstatus {
                    TEXT_MUTED
                } else {
                    TEXT_FG
                };
                let _ = SetTextColor(hdc, fg);
                let _ = SetBkColor(hdc, PANEL_BG);
                return LRESULT(app.brush.0 as isize);
            }
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
        WM_CTLCOLOREDIT | WM_CTLCOLORBTN => {
            if app_ptr != 0 {
                let app = &*(app_ptr as *const Installer);
                let hdc = HDC(wparam.0 as *mut core::ffi::c_void);
                let _ = SetTextColor(hdc, TEXT_FG);
                let _ = SetBkColor(hdc, PANEL_BG);
                return LRESULT(app.brush.0 as isize);
            }
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
        // 跨显示器/改缩放：按系统建议矩形调整窗口，重排子控件
        WM_DPICHANGED => {
            let dpi = (wparam.0 >> 16) as u32;
            let rc = &*(lparam.0 as *const RECT);
            let _ = SetWindowPos(
                hwnd,
                None,
                rc.left,
                rc.top,
                rc.right - rc.left,
                rc.bottom - rc.top,
                SWP_NOZORDER | SWP_NOACTIVATE,
            );
            if app_ptr != 0 {
                let app = &mut *(app_ptr as *mut Installer);
                relayout(app, dpi);
            }
            LRESULT(0)
        }
        WM_PAINT => DefWindowProcW(hwnd, msg, wparam, lparam),
        WM_DESTROY => {
            if app_ptr != 0 {
                let app = Box::from_raw(app_ptr as *mut Installer);
                let _ = DeleteObject(app.brush.into());
                let _ = DeleteObject(app.font_title.into());
                let _ = DeleteObject(app.font_section.into());
                let _ = DeleteObject(app.font_ui.into());
                if !app.hicon.is_invalid() {
                    let _ = DestroyIcon(app.hicon);
                }
            }
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

fn main() {
    // 清单已声明 PerMonitorV2；此处再调一次作双保险（已有感知时返回 Err，忽略即可）。
    let _ = unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
    }

    let class_name = w!("SylvaInstallerWnd");
    let hinstance = HINSTANCE(unsafe { GetModuleHandleW(None) }.unwrap().0);
    let wc = WNDCLASSW {
        style: CS_HREDRAW | CS_VREDRAW,
        lpfnWndProc: Some(wnd_proc),
        cbClsExtra: 0,
        cbWndExtra: 0,
        hInstance: hinstance,
        hIcon: app_icon(hinstance),
        hCursor: Default::default(),
        hbrBackground: unsafe { CreateSolidBrush(PANEL_BG) },
        lpszMenuName: PCWSTR::null(),
        lpszClassName: class_name,
    };
    let _atom = unsafe { RegisterClassW(&wc) };

    // 初始按系统 DPI 建窗；跨屏/改缩放在 WM_DPICHANGED 里实时调整
    let dpi0 = unsafe { GetDpiForSystem() };
    let (ww, wh) = (dl(640, dpi0), dl(524, dpi0));
    let hwnd = unsafe {
        CreateWindowExW(
            Default::default(),
            class_name,
            w!("Sylva 安装程序"),
            WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX,
            0,
            0,
            ww,
            wh,
            None,
            None,
            Some(hinstance),
            None,
        )
    };
    let Ok(hwnd) = hwnd else {
        msgbox("Sylva 安装", "无法创建安装窗口。", true);
        return;
    };
    // 窗口图标（标题栏 / 任务栏 / Alt+Tab）用与主程序相同的 sylva 图标
    unsafe {
        let hicon = app_icon(hinstance);
        let _ = SendMessageW(
            hwnd,
            WM_SETICON,
            Some(WPARAM(ICON_BIG as usize)),
            Some(LPARAM(hicon.0 as isize)),
        );
        let _ = SendMessageW(
            hwnd,
            WM_SETICON,
            Some(WPARAM(ICON_SMALL as usize)),
            Some(LPARAM(hicon.0 as isize)),
        );
    }
    // 居中
    unsafe {
        let scr = GetSystemMetrics(SM_CXSCREEN);
        let scy = GetSystemMetrics(SM_CYSCREEN);
        let _ = SetWindowPos(
            hwnd,
            None,
            (scr - ww) / 2,
            (scy - wh) / 2,
            0,
            0,
            SWP_NOSIZE | SWP_NOZORDER,
        );
    }

    let dpi = unsafe { GetDpiForWindow(hwnd) };
    let brush = unsafe { CreateSolidBrush(PANEL_BG) };
    let hicon = load_header_icon(hinstance, dpi);

    let mut app = Installer {
        hwnd,
        hicon,
        hicon_static: HWND::default(),
        htitle: HWND::default(),
        hsub: HWND::default(),
        hsection: HWND::default(),
        hhint: HWND::default(),
        hpath: HWND::default(),
        hbtn_browse: HWND::default(),
        hchk: HWND::default(),
        hprogress: HWND::default(),
        hstatus: HWND::default(),
        hbtn_install: HWND::default(),
        hbtn_cancel: HWND::default(),
        brush,
        font_title: HFONT::default(),
        font_section: HFONT::default(),
        font_ui: HFONT::default(),
        dpi,
        busy: false,
    };
    let app_ptr = &mut app as *mut Installer;
    unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, app_ptr as isize) };

    // 创建子控件：位置/尺寸交给 relayout 统一按 DPI 计算
    let mk = |class: &[u16],
              text: &[u16],
              style: WINDOW_STYLE,
              ex: WINDOW_EX_STYLE,
              id: usize|
     -> HWND {
        unsafe {
            CreateWindowExW(
                ex,
                PCWSTR(class.as_ptr()),
                PCWSTR(text.as_ptr()),
                style,
                0,
                0,
                0,
                0,
                Some(hwnd),
                Some(HMENU(id as isize as *mut core::ffi::c_void)),
                Some(hinstance),
                None,
            )
            .unwrap_or_default()
        }
    };
    let st = |s: u32| WINDOW_STYLE(s); // 从数值构 WINDOW_STYLE
    let idc = IDC_STATIC as usize;

    app.htitle = mk(
        &wide("STATIC"),
        &wide("Sylva 桌面栅栏整理器"),
        st(WS_CHILD.0 | WS_VISIBLE.0 | WS_CLIPSIBLINGS.0),
        Default::default(),
        idc,
    );
    app.hsub = mk(
        &wide("STATIC"),
        &wide("桌面图标整理器 · Windows 10/11 · 免管理员权限"),
        st(WS_CHILD.0 | WS_VISIBLE.0 | WS_CLIPSIBLINGS.0),
        Default::default(),
        idc,
    );
    app.hsection = mk(
        &wide("STATIC"),
        &wide("安装位置"),
        st(WS_CHILD.0 | WS_VISIBLE.0 | WS_CLIPSIBLINGS.0),
        Default::default(),
        idc,
    );
    app.hhint = mk(
        &wide("STATIC"),
        &wide("将安装到所选文件夹，并创建桌面 / 开始菜单快捷方式。"),
        st(WS_CHILD.0 | WS_VISIBLE.0 | WS_CLIPSIBLINGS.0),
        Default::default(),
        idc,
    );
    app.hicon_static = mk(
        &wide("STATIC"),
        &wide(""),
        st(WS_CHILD.0 | WS_VISIBLE.0 | WS_CLIPSIBLINGS.0 | SS_ICON | SS_CENTERIMAGE),
        Default::default(),
        idc,
    );
    app.hpath = mk(
        &wide("EDIT"),
        &wide(""),
        st(WS_CHILD.0 | WS_VISIBLE.0 | WS_TABSTOP.0 | WS_CLIPSIBLINGS.0 | (ES_AUTOHSCROLL as u32)),
        WS_EX_CLIENTEDGE,
        ID_PATH,
    );
    app.hbtn_browse = mk(
        &wide("BUTTON"),
        &wide("浏览…"),
        st(WS_CHILD.0 | WS_VISIBLE.0 | WS_TABSTOP.0 | WS_CLIPSIBLINGS.0),
        Default::default(),
        ID_BTN_BROWSE,
    );
    app.hchk = mk(
        &wide("BUTTON"),
        &wide("开机时自动启动 Sylva"),
        st(WS_CHILD.0 | WS_VISIBLE.0 | WS_TABSTOP.0 | WS_CLIPSIBLINGS.0 | (BS_AUTOCHECKBOX as u32)),
        Default::default(),
        ID_CHK_AUTOSTART,
    );
    app.hprogress = mk(
        &wide("msctls_progress32"),
        &wide(""),
        st(WS_CHILD.0 | WS_VISIBLE.0 | WS_CLIPSIBLINGS.0 | PBS_SMOOTH),
        Default::default(),
        ID_PROGRESS,
    );
    app.hstatus = mk(
        &wide("STATIC"),
        &wide("准备就绪"),
        st(WS_CHILD.0 | WS_VISIBLE.0 | WS_CLIPSIBLINGS.0),
        Default::default(),
        ID_STATUS,
    );
    app.hbtn_install = mk(
        &wide("BUTTON"),
        &wide("安装"),
        st(WS_CHILD.0
            | WS_VISIBLE.0
            | WS_TABSTOP.0
            | WS_CLIPSIBLINGS.0
            | (BS_DEFPUSHBUTTON as u32)),
        Default::default(),
        ID_BTN_INSTALL,
    );
    app.hbtn_cancel = mk(
        &wide("BUTTON"),
        &wide("取消"),
        st(WS_CHILD.0 | WS_VISIBLE.0 | WS_TABSTOP.0 | WS_CLIPSIBLINGS.0),
        Default::default(),
        ID_BTN_CANCEL,
    );

    // 初始化：默认安装路径、勾选自启、进度条范围与颜色、头部图标
    set_path(&app, &default_path());
    unsafe {
        let _ = SendMessageW(app.hchk, BM_SETCHECK, Some(WPARAM(BST_CHECKED)), None);
        let _ = SendMessageW(
            app.hprogress,
            PBM_SETRANGE32,
            Some(WPARAM(0)),
            Some(LPARAM(100)),
        );
        let _ = SendMessageW(
            app.hprogress,
            PBM_SETBARCOLOR,
            Some(WPARAM(ACCENT.0 as usize)),
            Some(LPARAM(0)),
        );
        let _ = SendMessageW(
            app.hicon_static,
            STM_SETICON,
            Some(WPARAM(hicon.0 as usize)),
            None,
        );
    }

    // 首次布局 + 字体（也为后续 WM_DPICHANGED 铺好基准）
    relayout(&mut app, dpi);

    unsafe {
        let _ = ShowWindow(hwnd, SW_SHOW);
    }

    let mut msg = MSG::default();
    unsafe {
        while GetMessageW(&mut msg, None, 0, 0).into() {
            // IsDialogMessage：Tab 切换焦点、Enter 触发默认按钮（安装）、Esc 走取消
            if IsDialogMessageW(hwnd, &msg).into() {
                continue;
            }
            let _ = TranslateMessage(&msg);
            let _ = DispatchMessageW(&msg);
        }
    }
}
