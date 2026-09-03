//! 主题：配色与尺寸（全部物理像素）。
//!
//! 与应用无关：App 层负责把逻辑尺寸 × DPI 换算后填入。
//! 颜色为直通 alpha（straight alpha），绘制时由 D2D 内部转换为预乘。

use windows::Win32::Graphics::Direct2D::Common::D2D1_COLOR_F;

/// 直通 alpha 的 RGBA 颜色（浮点，0..1）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Color {
    pub r: f32,
    pub g: f32,
    pub b: f32,
    pub a: f32,
}

impl Color {
    pub const fn rgba(r: f32, g: f32, b: f32, a: f32) -> Self {
        Self { r, g, b, a }
    }

    /// 转成 D2D 颜色。
    pub fn to_d2d(&self) -> D2D1_COLOR_F {
        D2D1_COLOR_F {
            r: self.r,
            g: self.g,
            b: self.b,
            a: self.a,
        }
    }
}

/// 文本样式。
#[derive(Debug, Clone, Copy)]
pub struct TextStyle {
    pub font_family: &'static str,
    pub size: f32,
    pub color: Color,
}

/// 主题：栅栏外观、标题、图标网格与文字。
///
/// 渲染目标固定为 96 DPI（1 DIP = 1 物理像素），因此**全部尺寸都是物理像素**。
/// App 层在 `Theme::default()` 后按 `scale = 系统DPI / 96` 把每个 DIP 度量放大；
/// `scale` 同时供绘制层的固定偏移（按钮留白、控制台边距等）按同一比例缩放，
/// 保证高 DPI 下布局比例一致、文字行列不重叠。
#[derive(Debug, Clone)]
pub struct Theme {
    /// DPI 缩放系数（系统 DPI / 96）；默认 1.0 = 100% 缩放。
    pub scale: f32,
    // 栅栏外观
    pub fence_bg: Color,
    pub fence_border: Color,
    pub fence_corner_radius: f32,
    pub fence_padding: f32,
    /// 模糊背景高斯标准偏差（物理像素，`GaussianBlurEffect::SetStandardDeviation`）。
    /// GPU 效果按 sigma 直接设；初值 20 对旧观感微调。
    pub blur_stddev: f32,
    // 标题
    pub title: TextStyle,
    pub title_padding_bottom: f32,
    // 图标
    pub icon_size: f32,
    pub icon_gap: f32,
    pub icon_caption_gap: f32,
    pub label: TextStyle,
    pub caption_max_width: f32,
    // 网格
    pub icon_cols: u32,
    // 列表布局
    pub list_row_gap: f32,
    pub list_label_gap: f32,
}

/// 网格图标下方文件名标签：两行总高 = `label.size` 的倍数（DWrite 每行实际行高约
/// 1.1 倍字号 + 余量）。App 布局（行高/编辑框）与 Render 绘制（标签框）共用同一
/// 常量，保证二者一致不重叠。
pub const GRID_CAPTION_H_MULT: f32 = 2.6;

impl Default for Theme {
    fn default() -> Self {
        // 现代深色半透明栅栏；具体数值在 M4 视觉打磨阶段调整。
        Self {
            scale: 1.0,
            fence_bg: Color::rgba(0.13, 0.15, 0.19, 0.60),
            fence_border: Color::rgba(1.0, 1.0, 1.0, 0.42),
            fence_corner_radius: 12.0,
            fence_padding: 14.0,
            blur_stddev: 20.0,
            title: TextStyle {
                font_family: "Microsoft YaHei UI",
                size: 16.0,
                color: Color::rgba(1.0, 1.0, 1.0, 0.90),
            },
            title_padding_bottom: 10.0,
            icon_size: 48.0,
            // 网格格宽保底 = 1.5×图标宽（见 app 层 grid_cell_w），种子栅栏宽度估算对齐
            icon_gap: 24.0,
            icon_caption_gap: 6.0,
            label: TextStyle {
                font_family: "Microsoft YaHei UI",
                size: 12.0,
                color: Color::rgba(1.0, 1.0, 1.0, 0.85),
            },
            caption_max_width: 80.0,
            icon_cols: 5,
            list_row_gap: 8.0,
            list_label_gap: 10.0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_theme_metrics_are_sane() {
        let t = Theme::default();
        assert!(t.icon_size > 0.0);
        assert!(t.icon_cols > 0);
        assert!(t.fence_corner_radius >= 0.0);
        assert!(t.title.size > 0.0 && t.label.size > 0.0);
        assert!(t.fence_padding >= 0.0);
    }

    #[test]
    fn color_converts_to_d2d_matching_input() {
        let c = Color::rgba(0.2, 0.4, 0.6, 0.8);
        let d = c.to_d2d();
        assert!((d.r - 0.2).abs() < 1e-6);
        assert!((d.a - 0.8).abs() < 1e-6);
    }
}
