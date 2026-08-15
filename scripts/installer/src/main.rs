//! Sylva 单文件安装器：把主程序与安装脚本一并内嵌，解压到临时目录后执行安装。
//! 免管理员权限，按当前用户安装（%LOCALAPPDATA%\Programs\Sylva + 快捷方式 +
//! 开机自启 + 控制面板卸载项）。

use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

use windows::core::PCWSTR;
use windows::Win32::UI::WindowsAndMessaging::{
    MessageBoxW, MESSAGEBOX_STYLE, MB_ICONERROR, MB_ICONINFORMATION, MB_OK,
};

/// 内嵌主程序（编译期打包，安装器独立分发）。
const SYLVA_EXE: &[u8] = include_bytes!("../../../target/release/sylva.exe");
/// 内嵌安装脚本（快捷方式 / 自启 / 卸载信息）。
const SETUP_PS1: &str = include_str!("../../../dist/installer/setup.ps1");

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn msgbox(title: &str, text: &str, error: bool) {
    unsafe {
        let t = wide(title);
        let b = wide(text);
        let _ = MessageBoxW(
            None,
            PCWSTR(b.as_ptr()),
            PCWSTR(t.as_ptr()),
            if error {
                MESSAGEBOX_STYLE(MB_OK.0 | MB_ICONERROR.0)
            } else {
                MB_OK | MB_ICONINFORMATION
            },
        );
    }
}

fn main() {
    // 解压到临时目录，避免污染当前目录
    let temp: PathBuf = env::temp_dir().join("SylvaSetup");
    if let Err(e) = fs::create_dir_all(&temp) {
        msgbox("Sylva 安装", &format!("无法创建临时目录: {e}"), true);
        return;
    }
    if let Err(e) = fs::write(temp.join("sylva.exe"), SYLVA_EXE) {
        msgbox("Sylva 安装", &format!("解压主程序失败: {e}"), true);
        return;
    }
    if let Err(e) = fs::write(temp.join("setup.ps1"), SETUP_PS1) {
        msgbox("Sylva 安装", &format!("解压安装脚本失败: {e}"), true);
        return;
    }

    let status = Command::new("powershell")
        .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"])
        .arg(temp.join("setup.ps1"))
        .status();
    let ok = matches!(status, Ok(s) if s.success());

    let _ = fs::remove_dir_all(&temp);
    if ok {
        msgbox("Sylva 安装", "安装完成，Sylva 已启动。\n以后可从桌面快捷方式或开始菜单启动。", false);
    } else {
        msgbox(
            "Sylva 安装",
            "安装未完成，请关闭安全软件后重试，或使用绿色版 sylva.exe。",
            true,
        );
    }
}
