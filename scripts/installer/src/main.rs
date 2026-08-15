//! Sylva 图形化安装程序：暗色安装窗口 + 进度条，按当前用户安装。
//! 免管理员权限：装到 %LOCALAPPDATA%\Programs\Sylva，建桌面/开始菜单快捷方式，
//! 注册开机自启与控制面板卸载项，安装完成后自动启动。

use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    CreateFontIndirectW, CreateSolidBrush, DeleteObject, SetBkColor, SetTextColor, FONT_CHARSET,
    FONT_CLIP_PRECISION, FONT_OUTPUT_PRECISION, FONT_QUALITY, HBRUSH, HDC, HFONT, LOGFONTW,
};
use windows::Win32::UI::Controls::{PBM_SETBARCOLOR, PBM_SETPOS, PBM_SETRANGE32, PBS_SMOOTH};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW,
    GetSystemMetrics, GetWindowLongPtrW, LoadIconW, PostQuitMessage, RegisterClassW, SendMessageW,
    SetProcessDPIAware, SetWindowLongPtrW, SetWindowPos, SetWindowTextW, ShowWindow,
    TranslateMessage, BN_CLICKED, CS_HREDRAW, CS_VREDRAW, GWLP_USERDATA, HICON, HMENU, IDC_STATIC,
    MSG, SM_CXSCREEN, SM_CYSCREEN, SW_SHOW, SWP_NOZORDER, SWP_NOSIZE, WM_COMMAND,
    WM_CTLCOLORBTN, WM_CTLCOLORSTATIC, WM_DESTROY, WM_PAINT, WNDCLASSW, WS_CAPTION, WS_CHILD,
    WS_MINIMIZEBOX, WS_OVERLAPPED, WS_SYSMENU, WS_TABSTOP, WS_VISIBLE, WINDOW_STYLE, ICON_BIG,
    ICON_SMALL, WM_SETICON,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;

/// 内嵌主程序（编译期打包，安装器独立分发）。
const SYLVA_EXE: &[u8] = include_bytes!("../../../target/release/sylva.exe");

const ID_BTN_INSTALL: usize = 1001;
const ID_BTN_QUIT: usize = 1002;
const ID_PROGRESS: usize = 1003;
const ID_STATUS: usize = 1004;

const DARK_BG: COLORREF = COLORREF(0x00_1E_16_14); // RGB(20,22,30)
const LIGHT_TEXT: COLORREF = COLORREF(0x00_FA_F2_EE); // RGB(238,242,250)
const ACCENT: COLORREF = COLORREF(0x00_F6_82_3B); // RGB(59,130,246)

struct Installer {
    hwnd: HWND,
    hprogress: HWND,
    hstatus: HWND,
    hbtn_install: HWND,
    brush: HBRUSH,
    font_title: HFONT,
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// 加载 exe 内嵌主图标（资源 ID 1 = build.rs 嵌入的 sylva.ico）。
#[allow(clippy::manual_dangling_ptr)]
fn app_icon(hinstance: HINSTANCE) -> HICON {
    unsafe { LoadIconW(Some(hinstance), PCWSTR(1 as *const u16)) }.unwrap_or_default()
}

fn msgbox(title: &str, text: &str, error: bool) {
    unsafe {
        let t = wide(title);
        let b = wide(text);
        let _ = windows::Win32::UI::WindowsAndMessaging::MessageBoxW(
            None,
            PCWSTR(b.as_ptr()),
            PCWSTR(t.as_ptr()),
            windows::Win32::UI::WindowsAndMessaging::MESSAGEBOX_STYLE(if error { 0x10 } else { 0x40 }),
        );
    }
}

fn set_status(app: &Installer, text: &str) {
    unsafe {
        let t = wide(text);
        let _ = SetWindowTextW(app.hstatus, PCWSTR(t.as_ptr()));
    }
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
}

fn reg_add(key: &str, value: &str, data: &str) {
    let _ = Command::new("reg")
        .args(["add", key, "/v", value, "/t", "REG_SZ", "/d", data, "/f"])
        .status();
}

fn run_install(app: &Installer) -> bool {
    let dest = env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| env::temp_dir())
        .join("Programs")
        .join("Sylva");

    set_status(app, "正在安装…");
    set_progress(app, 5);
    if let Err(e) = fs::create_dir_all(&dest) {
        msgbox("Sylva 安装", &format!("无法创建安装目录: {e}"), true);
        return false;
    }
    if let Err(e) = fs::write(dest.join("sylva.exe"), SYLVA_EXE) {
        msgbox("Sylva 安装", &format!("写入主程序失败: {e}"), true);
        return false;
    }
    set_progress(app, 35);

    // 桌面 + 开始菜单快捷方式（PowerShell 编码命令，中文安全）
    let exe_ps = dest.join("sylva.exe").to_string_lossy().replace('\\', "\\\\");
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
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !ok {
        msgbox("Sylva 安装", "创建快捷方式失败，请关闭安全软件后重试。", true);
        return false;
    }
    set_progress(app, 60);

    // 开机自启 + 卸载信息
    let exe_quoted = format!("\"{}\"", dest.join("sylva.exe").display());
    reg_add(
        r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run",
        "Sylva",
        &exe_quoted,
    );
    let unreg = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Uninstall\Sylva";
    reg_add(unreg, "DisplayName", "Sylva 桌面栅栏整理器");
    reg_add(unreg, "DisplayVersion", "0.1.0");
    reg_add(unreg, "Publisher", "Sylva");
    reg_add(unreg, "InstallLocation", &dest.to_string_lossy());
    reg_add(unreg, "DisplayIcon", &exe_quoted);
    reg_add(
        unreg,
        "UninstallString",
        &format!("\"{}\"", dest.join("uninstall.cmd").display()),
    );
    reg_add(unreg, "NoModify", "1");
    reg_add(unreg, "NoRepair", "1");
    set_progress(app, 85);

    // 卸载脚本
    let uninstall = format!(
        "@echo off\r\nsetlocal\r\nset DEST={}\r\ntaskkill /IM sylva.exe /F >nul 2>&1\r\nreg delete \"HKCU\\Software\\Microsoft\\Windows\\CurrentVersion\\Run\" /v Sylva /f >nul 2>&1\r\nreg delete \"HKCU\\Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\Sylva\" /f >nul 2>&1\r\ndel \"%USERPROFILE%\\Desktop\\Sylva.lnk\" >nul 2>&1\r\nrd /s /q \"%APPDATA%\\Microsoft\\Windows\\Start Menu\\Programs\\Sylva\" >nul 2>&1\r\nrd /s /q \"%DEST%\" >nul 2>&1\r\necho Sylva 已卸载。\r\npause\r\n",
        dest.display()
    );
    let _ = fs::write(dest.join("uninstall.cmd"), uninstall);
    set_progress(app, 95);

    // 启动
    let _ = Command::new(&dest.join("sylva.exe")).spawn();
    set_status(app, "安装完成，Sylva 已启动。");
    set_progress(app, 100);
    true
}

/// 把脚本编码为 UTF-16LE Base64（PowerShell -EncodedCommand，中文安全）。
fn encode_command(script: &str) -> String {
    let bytes: Vec<u8> = script.encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
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
        out.push(if chunk.len() > 1 { T[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if chunk.len() > 2 { T[n as usize & 63] as char } else { '=' });
    }
    out
}

unsafe extern "system" fn wnd_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let app_ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA);
    match msg {
        WM_COMMAND => {
            let mut to_destroy = None;
            if app_ptr != 0 {
                let app = &mut *(app_ptr as *mut Installer);
                let id = (wparam.0 & 0xFFFF) as usize;
                let code = (wparam.0 >> 16) as usize;
                if code == BN_CLICKED as usize {
                    match id {
                        ID_BTN_INSTALL => {
                            let ok = run_install(app);
                            if ok {
                                to_destroy = Some(app.hwnd);
                            }
                        }
                        ID_BTN_QUIT => {
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
        WM_CTLCOLORSTATIC | WM_CTLCOLORBTN => {
            if app_ptr != 0 {
                let app = &*(app_ptr as *const Installer);
                let hdc = HDC(lparam.0 as *mut core::ffi::c_void);
                let _ = SetTextColor(hdc, LIGHT_TEXT);
                let _ = SetBkColor(hdc, DARK_BG);
                return LRESULT(app.brush.0 as isize);
            }
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
        WM_PAINT => DefWindowProcW(hwnd, msg, wparam, lparam),
        WM_DESTROY => {
            if app_ptr != 0 {
                let app = Box::from_raw(app_ptr as *mut Installer);
                let _ = DeleteObject(app.brush.into());
                let _ = DeleteObject(app.font_title.into());
            }
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

fn main() {
    unsafe {
        let _ = SetProcessDPIAware();
        let _ = windows::Win32::System::Com::CoInitializeEx(
            None,
            windows::Win32::System::Com::COINIT_APARTMENTTHREADED,
        );
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
        hbrBackground: unsafe { CreateSolidBrush(DARK_BG) },
        lpszMenuName: PCWSTR::null(),
        lpszClassName: class_name,
    };
    let _atom = unsafe { RegisterClassW(&wc) };

    let hwnd = unsafe {
        CreateWindowExW(
            Default::default(),
            class_name,
            w!("Sylva 安装程序"),
            WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX,
            0,
            0,
            480,
            320,
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
        let _ = SendMessageW(hwnd, WM_SETICON, Some(WPARAM(ICON_BIG as usize)), Some(LPARAM(hicon.0 as isize)));
        let _ = SendMessageW(hwnd, WM_SETICON, Some(WPARAM(ICON_SMALL as usize)), Some(LPARAM(hicon.0 as isize)));
    }

    // 居中
    unsafe {
        let scr = GetSystemMetrics(SM_CXSCREEN);
        let scy = GetSystemMetrics(SM_CYSCREEN);
        let _ = SetWindowPos(
            hwnd,
            None,
            (scr - 480) / 2,
            (scy - 320) / 2,
            0,
            0,
            SWP_NOSIZE | SWP_NOZORDER,
        );
    }

    // 标题字体
    let mut lf: LOGFONTW = unsafe { std::mem::zeroed() };
    lf.lfHeight = -22;
    lf.lfWeight = 600;
    lf.lfCharSet = FONT_CHARSET(1);
    lf.lfOutPrecision = FONT_OUTPUT_PRECISION(0);
    lf.lfClipPrecision = FONT_CLIP_PRECISION(0);
    lf.lfQuality = FONT_QUALITY(5);
    let face: Vec<u16> = "Microsoft YaHei UI".encode_utf16().take(31).collect();
    for (i, c) in face.iter().enumerate() {
        lf.lfFaceName[i] = *c;
    }
    let font_title = unsafe { CreateFontIndirectW(&lf) };
    let brush = unsafe { CreateSolidBrush(DARK_BG) };

    let mut app = Installer {
        hwnd,
        hprogress: HWND::default(),
        hstatus: HWND::default(),
        hbtn_install: HWND::default(),
        brush,
        font_title,
    };
    let app_ptr = &mut app as *mut Installer;
    unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, app_ptr as isize) };

    let mk = |class: &[u16], text: &[u16], x: i32, y: i32, w: i32, h: i32, style: WINDOW_STYLE, id: usize| {
        unsafe {
            CreateWindowExW(
                Default::default(),
                PCWSTR(class.as_ptr()),
                PCWSTR(text.as_ptr()),
                style,
                x,
                y,
                w,
                h,
                Some(hwnd),
                Some(HMENU(id as isize as *mut core::ffi::c_void)),
                Some(hinstance),
                None,
            )
            .unwrap_or_default()
        }
    };
    let title = mk(
        &wide("STATIC"),
        &wide("Sylva 桌面栅栏整理器"),
        24,
        18,
        432,
        34,
        WS_CHILD | WS_VISIBLE,
        IDC_STATIC as usize,
    );
    mk(
        &wide("STATIC"),
        &wide("将安装到 %LOCALAPPDATA%\\Programs\\Sylva，并创建桌面快捷方式。"),
        24,
        62,
        432,
        22,
        WS_CHILD | WS_VISIBLE,
        IDC_STATIC as usize,
    );
    mk(
        &wide("STATIC"),
        &wide("版本 0.1.0 · Windows 10/11 · 免管理员权限"),
        24,
        90,
        432,
        20,
        WS_CHILD | WS_VISIBLE,
        IDC_STATIC as usize,
    );
    unsafe {
        let _ = SendMessageW(
            title,
            0x0030,
            Some(WPARAM(font_title.0 as usize)),
            Some(LPARAM(1)),
        ); // WM_SETFONT
    }
    app.hprogress = mk(
        &wide("msctls_progress32"),
        &wide(""),
        24,
        140,
        432,
        22,
        WINDOW_STYLE((WS_CHILD | WS_VISIBLE).0 | PBS_SMOOTH),
        ID_PROGRESS,
    );
    app.hstatus = mk(
        &wide("STATIC"),
        &wide("准备就绪"),
        24,
        172,
        432,
        20,
        WS_CHILD | WS_VISIBLE,
        ID_STATUS,
    );
    app.hbtn_install = mk(
        &wide("BUTTON"),
        &wide("安装"),
        150,
        220,
        80,
        32,
        WS_CHILD | WS_VISIBLE | WS_TABSTOP,
        ID_BTN_INSTALL,
    );
    mk(
        &wide("BUTTON"),
        &wide("退出"),
        248,
        220,
        80,
        32,
        WS_CHILD | WS_VISIBLE | WS_TABSTOP,
        ID_BTN_QUIT,
    );
    unsafe {
        let _ = SendMessageW(app.hprogress, PBM_SETRANGE32, Some(WPARAM(0)), Some(LPARAM(100)));
        let _ = SendMessageW(
            app.hprogress,
            PBM_SETBARCOLOR,
            Some(WPARAM(ACCENT.0 as usize)),
            Some(LPARAM(0)),
        );
        let _ = ShowWindow(hwnd, SW_SHOW);
    }

    let mut msg = MSG::default();
    unsafe {
        while GetMessageW(&mut msg, None, 0, 0).into() {
            let _ = TranslateMessage(&msg);
            let _ = DispatchMessageW(&msg);
        }
    }
}

