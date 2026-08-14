//! 桌面图标枚举。
//!
//! 通过 `IShellFolder`（桌面文件夹）枚举图标，不依赖 `SHELLDLL_DefView` 是否存在，
//! 因此用户开启「隐藏桌面图标」时依然可用。
//!
//! 前提：调用方已完成 COM 初始化（`CoInitializeEx`）。

use std::path::Path;

use windows::core::{HRESULT, PCWSTR};
use windows::Win32::Foundation::HWND;
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::UI::Shell::Common::{
    ITEMIDLIST, STRRET, STRRET_CSTR, STRRET_TYPE, STRRET_WSTR,
};
use windows::Win32::UI::Shell::{
    IShellFolder, SHGetDesktopFolder, SHGetPathFromIDListW, ShellExecuteW, SHCONTF_FOLDERS,
    SHCONTF_NONFOLDERS, SHGDNF,
};
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

use fence_core::model::{ItemId, ItemKind};

// windows-rs 0.62 未自动生成的 shell 属性位（稳定文档值）。
const SFGAO_LINK: u32 = 0x0001_0000;
const SFGAO_FOLDER: u32 = 0x2000_0000;
const SFGAO_FILESYSTEM: u32 = 0x4000_0000;
/// 枚举器意外的空值（正常情况下 EnumObjects 成功必有枚举器）→ E_FAIL。
const E_ENUM_EMPTY: i32 = 0x8000_4005u32 as i32;

/// 一个桌面图标项。持有自己的 PIDL（Drop 时释放）。
#[derive(Debug)]
pub struct DesktopItem {
    pub id: ItemId,
    pub display_name: String,
    pub kind: ItemKind,
    /// 文件系统路径（虚拟项为 None），用于打开 / 拖拽等场景。
    pub path: Option<String>,
    /// 指向该 shell 项的绝对 PIDL，供图标提取 / 拖拽等后续使用。
    pub pidl: *mut ITEMIDLIST,
}

impl Drop for DesktopItem {
    fn drop(&mut self) {
        unsafe { CoTaskMemFree(Some(self.pidl as *const _)) };
    }
}

impl DesktopItem {
    /// 用系统默认动作打开该项（等价于桌面双击）。虚拟项（无路径）跳过。
    pub fn launch(&self) {
        let Some(path) = self.path.as_deref() else {
            return;
        };
        let file = wide(path);
        unsafe {
            let _ = ShellExecuteW(
                None, // 无父窗口
                None, // 默认动作（open）
                PCWSTR(file.as_ptr()),
                None,
                None,
                SW_SHOWNORMAL,
            );
        }
    }
}

/// 枚举桌面全部图标。
pub fn enumerate_desktop_items() -> windows::core::Result<Vec<DesktopItem>> {
    let desktop: IShellFolder = unsafe { SHGetDesktopFolder()? };

    // 枚举所有可见的文件夹与非文件夹（EnumObjects 的 grfflags 是裸 u32，SHCONTF.0 是 i32）
    let flags = (SHCONTF_FOLDERS.0 | SHCONTF_NONFOLDERS.0) as u32;
    let mut enum_opt: Option<windows::Win32::UI::Shell::IEnumIDList> = None;
    unsafe {
        desktop
            .EnumObjects(HWND::default(), flags, &mut enum_opt)
            .ok()?
    };
    let enum_idl =
        enum_opt.ok_or_else(|| windows::core::Error::from_hresult(HRESULT(E_ENUM_EMPTY)))?;

    let mut items = Vec::new();
    // 单元素槽：Windows 每次返回一个 PIDL
    let mut slot = [std::ptr::null_mut::<ITEMIDLIST>()];
    loop {
        let mut fetched: u32 = 0;
        let hr = unsafe { enum_idl.Next(&mut slot, Some(&mut fetched)) };
        if hr.is_err() || fetched == 0 {
            break;
        }
        let pidl = slot[0];
        slot[0] = std::ptr::null_mut();

        if let Some(item) = build_item(&desktop, pidl) {
            items.push(item);
        } else {
            unsafe { CoTaskMemFree(Some(pidl as *const _)) };
        }
    }
    Ok(items)
}

/// 从单个 PIDL 构建 `DesktopItem`；失败时返回 None（由调用方负责释放 pidl）。
fn build_item(desktop: &IShellFolder, pidl: *mut ITEMIDLIST) -> Option<DesktopItem> {
    let display_name = display_name_of(desktop, pidl)?;
    let path = path_of(pidl);
    let kind = kind_of(desktop, pidl, &display_name, path.as_deref());
    let id = item_id(path.clone(), &display_name);

    Some(DesktopItem {
        id,
        display_name,
        kind,
        path,
        pidl,
    })
}

/// 获取显示名称（SHGDNF_NORMAL）。
fn display_name_of(desktop: &IShellFolder, pidl: *const ITEMIDLIST) -> Option<String> {
    let mut strret = STRRET::default();
    unsafe {
        desktop
            .GetDisplayNameOf(pidl, SHGDNF(0), &mut strret)
            .ok()?
    };
    Some(strret_to_string(&strret))
}

/// 文件系统项返回路径；虚拟项返回 None。
fn path_of(pidl: *const ITEMIDLIST) -> Option<String> {
    let mut buf = [0u16; 260];
    let ok = unsafe { SHGetPathFromIDListW(pidl, &mut buf) };
    if !ok.as_bool() {
        return None;
    }
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    Some(String::from_utf16_lossy(&buf[..end]))
}

/// 依据 shell 属性、扩展名与文件系统状态判定类别（仅用于渲染表现，不参与归类）。
///
/// 分层判定，每层都能独立兜底：
/// 1. 扩展名快路径（exe / lnk / url / appref-ms 语义明确）；
/// 2. shell 属性（`GetAttributesOf`，仅当返回非零时采信——Win11 桌面对
///    历史 API 常返回 S_OK + 0，此时不可信）；
/// 3. 文件系统兜底：目录 → Folder，其余有路径 → Doc。
fn kind_of(
    desktop: &IShellFolder,
    pidl: *const ITEMIDLIST,
    name: &str,
    path: Option<&str>,
) -> ItemKind {
    let mut attrs: u32 = 0;
    let pidls = [pidl];
    let hr = unsafe { desktop.GetAttributesOf(&pidls, &mut attrs) };
    // 仅当调用成功且返回了非零属性时才采信 shell 属性
    let attrs_ok = hr.is_ok() && attrs != 0;
    kind_from(attrs, attrs_ok, path, name)
}

/// 纯分类逻辑（可单测），返回顺序即优先级。
fn kind_from(attrs: u32, attrs_ok: bool, path: Option<&str>, name: &str) -> ItemKind {
    // 1. shell 属性（attrs 可信时最权威，捕获虚拟项/快捷方式；Win11 桌面常返回 0 → 跳过）
    if attrs_ok {
        if attrs & SFGAO_FOLDER != 0 {
            return ItemKind::Folder;
        }
        if attrs & SFGAO_LINK != 0 {
            return ItemKind::Link;
        }
    }
    // 2. 扩展名快路径（语义明确的类型）
    if let Some(p) = path {
        let ext = Path::new(p)
            .extension()
            .map(|e| e.to_string_lossy().to_ascii_lowercase());
        match ext.as_deref() {
            Some("exe") | Some("appref-ms") | Some("url") => return ItemKind::App,
            Some("lnk") => return ItemKind::Link,
            _ => {}
        }
    }
    // 3. 文件系统兜底
    if let Some(p) = path {
        if Path::new(p).is_dir() {
            return ItemKind::Folder;
        }
        return ItemKind::Doc;
    }
    let _ = name;
    ItemKind::Unknown
}

/// 稳定标识符：文件系统项用小写路径，虚拟项退回显示名。
fn item_id(path: Option<String>, display_name: &str) -> ItemId {
    path.map(|p| p.to_ascii_lowercase())
        .unwrap_or_else(|| format!("shell:{}", display_name))
}

/// 把 STRRET 转换为 String，并释放 COM 分配的宽字符串。
fn strret_to_string(strret: &STRRET) -> String {
    let ty = STRRET_TYPE(strret.uType as i32);
    if ty == STRRET_WSTR {
        let ptr = unsafe { strret.Anonymous.pOleStr };
        // PWSTR 可能是空指针
        let s = unsafe { ptr.to_string() }.unwrap_or_default();
        unsafe { CoTaskMemFree(Some(ptr.0 as *const _)) };
        s
    } else if ty == STRRET_CSTR {
        let bytes = unsafe { strret.Anonymous.cStr };
        let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
        String::from_utf8_lossy(&bytes[..end]).into_owned()
    } else {
        // STRRET_OFFSET：无父缓冲区时无法解析
        String::new()
    }
}

/// UTF-16 编码（含结尾 NUL），供 Win32 宽字符串参数使用。
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_by_extension() {
        assert_eq!(
            kind_from(0, false, Some(r"C:\Program Files\a.exe"), "a"),
            ItemKind::App
        );
        assert_eq!(
            kind_from(0, false, Some(r"C:\docs\a.txt"), "a"),
            ItemKind::Doc
        );
        assert_eq!(
            kind_from(0, false, Some(r"C:\docs\a.lnk"), "a"),
            ItemKind::Link
        );
        assert_eq!(kind_from(0, false, None, "回收站"), ItemKind::Unknown);
    }

    #[test]
    fn shell_attributes_take_precedence() {
        // SFGAO_FOLDER 置位 → Folder（即使路径是 .exe）
        assert_eq!(
            kind_from(SFGAO_FOLDER, true, Some(r"C:\a.exe"), "a"),
            ItemKind::Folder
        );
        assert_eq!(
            kind_from(SFGAO_LINK, true, Some(r"C:\a.txt"), "a"),
            ItemKind::Link
        );
    }

    #[test]
    fn item_id_prefers_lowercase_path() {
        assert_eq!(
            item_id(Some(r"C:\Docs\A.Exe".into()), "A"),
            "c:\\docs\\a.exe"
        );
        assert_eq!(item_id(None, "回收站"), "shell:回收站");
    }

    #[test]
    fn live_enumerate_real_desktop() {
        // 真实桌面冒烟：COM 初始化后枚举不应出错；隐藏了图标的桌面允许为空。
        if crate::com::init().is_err() {
            return;
        }
        let items = enumerate_desktop_items().expect("枚举桌面图标不应失败");
        eprintln!("enumerated {} desktop items", items.len());
        for it in items.iter().take(8) {
            eprintln!("  - {:?} [{:?}] id={}", it.display_name, it.kind, it.id);
        }
    }
}
