//! 本地时间格式化：unix 秒 → 本地 "YYYY-MM-DD HH:MM"。
//!
//! unix 秒是 UTC 绝对时刻；这里用 `FileTimeToLocalFileTime` 换算成本地
//! FILETIME 后还原回 unix 秒，再委托 `sylva_core::details::format_time_utc`
//! 做纯 Rust 的历法格式化（算法可单测）。

use windows::Win32::Foundation::{FILETIME, SYSTEMTIME};
use windows::Win32::Storage::FileSystem::FileTimeToLocalFileTime;
use windows::Win32::System::Time::FileTimeToSystemTime;

/// FILETIME → unix 秒（UTC 与本地均可，视来源而定）。
fn filetime_to_unix(ft: &FILETIME) -> i64 {
    let hundred_ns = ((ft.dwHighDateTime as i64) << 32) | ft.dwLowDateTime as i64;
    hundred_ns / 10_000_000 - 11_644_473_600
}

/// unix 秒（UTC）→ 本地时间字符串（"YYYY-MM-DD HH:MM"）。
/// 换算失败时退回 UTC 格式化，保证永不失败。
pub fn format_local(unix: i64) -> String {
    let unix_ft = (unix + 11_644_473_600) * 10_000_000;
    let utc = FILETIME {
        dwLowDateTime: unix_ft as u32,
        dwHighDateTime: (unix_ft >> 32) as u32,
    };
    // 先转 SYSTEMTIME（验证合法），再经 FileTimeToLocalFileTime 得到本地时刻
    let mut st = SYSTEMTIME::default();
    let mut local = FILETIME::default();
    unsafe {
        if FileTimeToSystemTime(&utc, &mut st).is_ok()
            && FileTimeToLocalFileTime(&utc, &mut local).is_ok()
        {
            return sylva_core::details::format_time_utc(filetime_to_unix(&local));
        }
    }
    // 兜底：直接按 UTC 显示（时区偏差，但格式正确、不崩溃）
    sylva_core::details::format_time_utc(unix)
}

/// 已格式化短时间（列表列头用）。空字符串表示无该信息。
pub fn format_modified(secs: Option<i64>) -> String {
    match secs {
        Some(s) => format_local(s),
        None => "—".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filetime_roundtrip_unix() {
        // 2000-01-01 01:04:05 UTC
        let unix = 946_688_645_i64;
        let ft = ((unix + 11_644_473_600) * 10_000_000) as u64;
        let back = filetime_to_unix(&FILETIME {
            dwLowDateTime: ft as u32,
            dwHighDateTime: (ft >> 32) as u32,
        });
        assert_eq!(back, unix);
    }
}
