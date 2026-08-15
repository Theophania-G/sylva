//! 图标详情：文件类型标签、大小、修改时间的格式化与按路径补齐。
//!
//! 全部为纯函数（不依赖 Win32），`format_time_utc` 输出 UTC 时间，
//! 本地时区换算由 Shell 层 `time::format_local` 负责（它只换算再委托这里）。

use crate::model::{Icon, ItemKind};

/// 文件类型标签（参考资源管理器「类型」列）。
pub fn type_label(kind: ItemKind, display_name: &str) -> String {
    match kind {
        ItemKind::Folder => "文件夹".into(),
        ItemKind::App => "应用程序".into(),
        ItemKind::Link => "快捷方式".into(),
        ItemKind::Drive => "驱动器".into(),
        ItemKind::Doc => {
            let ext = extension_of(display_name);
            match ext.as_deref() {
                Some("txt") => "文本文档",
                Some("pdf") => "PDF 文档",
                Some("doc") | Some("docx") => "Word 文档",
                Some("xls") | Some("xlsx") => "Excel 工作表",
                Some("ppt") | Some("pptx") => "PowerPoint 演示文稿",
                Some("jpg") | Some("jpeg") | Some("png") | Some("gif") | Some("bmp")
                | Some("webp") | Some("ico") => "图片",
                Some("mp3") | Some("wav") | Some("flac") | Some("aac") | Some("ogg")
                | Some("m4a") => "音频",
                Some("mp4") | Some("mkv") | Some("avi") | Some("mov") | Some("wmv")
                | Some("flv") => "视频",
                Some("zip") | Some("rar") | Some("7z") | Some("tar") | Some("gz") => "压缩文件",
                Some("") => "文件",
                Some(other) => return format!("{} 文件", other.to_ascii_uppercase()),
                None => "文件",
            }
            .into()
        }
        ItemKind::Unknown => "文件".into(),
    }
}

/// 文件名（或路径）的扩展名（小写，不含点）；无扩展名返回 None。
fn extension_of(name: &str) -> Option<String> {
    let stem = name.rsplit(['/', '\\']).next().unwrap_or(name);
    match stem.rfind('.') {
        Some(0) | None => None,
        Some(i) => Some(stem[i + 1..].to_ascii_lowercase()),
    }
}

/// 按路径补齐图标的类型标签 / 修改时间 / 大小（本地文件系统读取）。
/// 无文件系统的虚拟项或读取失败时保持原有值，不影响加入栅栏。
pub fn enrich(icon: &mut Icon, path: &str) {
    if icon.type_label.is_empty() {
        icon.type_label = type_label(icon.kind, &icon.display_name);
    }
    if let Ok(md) = std::fs::metadata(path) {
        if md.is_file() {
            icon.size_bytes = Some(md.len());
        } else if md.is_dir() {
            icon.size_bytes = None;
        }
        if let Ok(t) = md.modified() {
            icon.modified_secs = t
                .duration_since(std::time::UNIX_EPOCH)
                .ok()
                .map(|d| d.as_secs() as i64);
        }
    }
}

/// 字节数 → 可读大小（如 "1.2 MB"）；None（文件夹/未知）→ "—"。
pub fn format_size(bytes: Option<u64>) -> String {
    let Some(b) = bytes else {
        return "—".into();
    };
    const K: u64 = 1024;
    if b < K {
        return format!("{b} B");
    }
    let units = ["KB", "MB", "GB", "TB", "PB"];
    let mut v = b as f64 / K as f64;
    for u in units {
        if v < K as f64 || u == *units.last().unwrap() {
            return if v >= 100.0 {
                format!("{v:.0} {u}")
            } else {
                format!("{v:.1} {u}")
            };
        }
        v /= K as f64;
    }
    format!("{v:.1} PB")
}

/// unix 秒（UTC）→ "YYYY-MM-DD HH:MM"。秒的精度对栅栏列表足够。
/// 使用 civil-from-days 算法（Howard Hinnant），纯 Rust、可单测。
pub fn format_time_utc(unix: i64) -> String {
    let days = unix.div_euclid(86_400);
    let secs_of_day = unix.rem_euclid(86_400);
    let z = days + 719_468;
    let era = (if z >= 0 { z } else { z - 146_096 }) / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    let y = if m <= 2 { y + 1 } else { y };
    let hh = secs_of_day / 3_600;
    let mm = (secs_of_day / 60) % 60;
    format!("{y:04}-{m:02}-{d:02} {hh:02}:{mm:02}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn type_label_covers_common_kinds() {
        assert_eq!(type_label(ItemKind::Folder, "文档"), "文件夹");
        assert_eq!(type_label(ItemKind::App, "chrome"), "应用程序");
        assert_eq!(type_label(ItemKind::Link, "wechat"), "快捷方式");
        assert_eq!(type_label(ItemKind::Doc, "report.pdf"), "PDF 文档");
        assert_eq!(type_label(ItemKind::Doc, "notes.txt"), "文本文档");
        assert_eq!(type_label(ItemKind::Doc, "a.xlsx"), "Excel 工作表");
        assert_eq!(type_label(ItemKind::Doc, "b.png"), "图片");
        assert_eq!(type_label(ItemKind::Doc, "c.mp4"), "视频");
        assert_eq!(type_label(ItemKind::Doc, "d.zip"), "压缩文件");
        assert_eq!(type_label(ItemKind::Doc, "e.mystery"), "MYSTERY 文件");
        assert_eq!(type_label(ItemKind::Doc, "noext"), "文件");
        assert_eq!(type_label(ItemKind::Unknown, "?"), "文件");
    }

    #[test]
    fn extension_detection() {
        assert_eq!(extension_of("a.txt"), Some("txt".into()));
        assert_eq!(extension_of(r"C:\x\y.Report.PDF"), Some("pdf".into()));
        assert_eq!(extension_of("noext"), None);
        assert_eq!(extension_of(".hidden"), None);
    }

    #[test]
    fn size_formatting() {
        assert_eq!(format_size(Some(512)), "512 B");
        assert_eq!(format_size(Some(1536)), "1.5 KB");
        assert_eq!(format_size(Some(1024 * 1024)), "1.0 MB");
        assert_eq!(format_size(Some(3 * 1024 * 1024)), "3.0 MB");
        assert_eq!(format_size(None), "—");
    }

    #[test]
    fn time_formatting_utc() {
        assert_eq!(format_time_utc(0), "1970-01-01 00:00");
        // 2000-01-01 01:04:05 UTC（946_684_800 = 2000-01-01 00:00）
        assert_eq!(format_time_utc(946_688_645), "2000-01-01 01:04");
        // 2026-08-14 12:00 UTC
        assert_eq!(format_time_utc(1_786_708_800), "2026-08-14 12:00");
        // 负值（1970 前）不 panic
        assert_eq!(format_time_utc(-1), "1969-12-31 23:59");
    }

    #[test]
    fn enrich_fills_details_from_real_file() {
        let dir = std::env::temp_dir();
        let path = dir.join("sylva_details_test.bin");
        let _ = std::fs::write(&path, [1u8, 2, 3, 4, 5]);
        if !path.exists() {
            return;
        }
        let mut icon = Icon::new(
            "sylva_details_test.bin".into(),
            "sylva_details_test.bin".into(),
            ItemKind::Doc,
        );
        enrich(&mut icon, path.to_string_lossy().as_ref());
        assert_eq!(icon.type_label, "BIN 文件");
        assert_eq!(icon.size_bytes, Some(5));
        assert!(icon.modified_secs.is_some());
        let _ = std::fs::remove_file(&path);
    }
}
