//! 文本度量：统一的粗略宽度估算，App（布局）与 Render（绘制）共用同一口径，
//! 避免两处估算不一致导致居中/对齐偏移或文字截断。

/// 估算文本像素宽度：CJK 等宽字符按 `font_size` 宽，ASCII 按 `0.62 × font_size` 宽。
///
/// 这是布局层与绘制层共用的一套近似口径（不用 DWrite 精确测量，
/// 保证布局预留空间与绘制实际占宽一致）。
pub fn estimate_width(text: &str, font_size: f32) -> f32 {
    let units: f32 = text
        .chars()
        .map(|c| if c.is_ascii() { 0.62 } else { 1.0 })
        .sum();
    units * font_size
}

#[cfg(test)]
mod tests {
    use super::estimate_width;

    #[test]
    fn ascii_is_062_per_char() {
        assert_eq!(estimate_width("abc", 10.0), 18.6); // 3 × 0.62 × 10
    }

    #[test]
    fn cjk_is_full_width() {
        assert_eq!(estimate_width("栅栏", 10.0), 20.0); // 2 × 1.0 × 10
    }

    #[test]
    fn mixed_text_sums_units() {
        assert_eq!(estimate_width("a栅b", 10.0), 22.4); // 0.62 + 1.0 + 0.62
    }

    #[test]
    fn empty_text_is_zero() {
        assert_eq!(estimate_width("", 10.0), 0.0);
    }

    #[test]
    fn scales_with_font_size() {
        let half = estimate_width("栅栏abc", 10.0);
        assert_eq!(estimate_width("栅栏abc", 20.0), half * 2.0);
    }
}
