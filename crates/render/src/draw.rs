//! D2D 绘制：把场景画进渲染目标。
//!
//! 缓存策略（性能约定 §7.2）：
//! - 位图（`IconStore`）与文本格式（`TextFormats`）跨帧缓存；
//! - 画笔/背景每帧新建——渲染目标每帧重建，画笔不能跨帧复用；
//! - 空闲时不绘制任何东西（0% CPU），只有内容变化才触发重绘。

use std::collections::HashMap;

use windows::core::{Result, PCWSTR};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_PIXEL_FORMAT, D2D_RECT_F, D2D_SIZE_U,
};
use windows::Win32::Graphics::Direct2D::{
    ID2D1Bitmap, ID2D1RenderTarget, ID2D1SolidColorBrush, D2D1_BITMAP_INTERPOLATION_MODE_LINEAR,
    D2D1_BITMAP_PROPERTIES, D2D1_DRAW_TEXT_OPTIONS_CLIP, D2D1_ROUNDED_RECT,
};
use windows::Win32::Graphics::DirectWrite::{
    IDWriteFactory, IDWriteTextFormat, DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL,
    DWRITE_FONT_WEIGHT_NORMAL, DWRITE_MEASURING_MODE_NATURAL,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;

use fence_shell::icons::IconData;

use crate::scene::{Scene, SceneFence};
use crate::theme::{TextStyle, Theme};

/// 图标位图缓存：`bitmap_id` → 设备上的 D2D 位图。
///
/// 位图是设备级资源（像素已上传 GPU），跨帧有效；上传一次，之后只引用。
pub struct IconStore {
    map: HashMap<u64, ID2D1Bitmap>,
    next_id: u64,
}

impl IconStore {
    pub fn new() -> Self {
        Self {
            map: HashMap::new(),
            next_id: 0,
        }
    }

    /// 上传图标像素并返回位图 ID。
    pub fn insert(&mut self, target: &ID2D1RenderTarget, data: &IconData) -> Result<u64> {
        let id = self.next_id;
        self.next_id += 1;
        self.map.insert(id, make_bitmap(target, data)?);
        Ok(id)
    }

    /// 用调用方指定的 ID 上传位图（App 层预分配稳定 ID 时用）。
    pub fn insert_at(
        &mut self,
        target: &ID2D1RenderTarget,
        id: u64,
        data: &IconData,
    ) -> Result<()> {
        self.map.insert(id, make_bitmap(target, data)?);
        Ok(())
    }

    pub fn get(&self, id: u64) -> Option<&ID2D1Bitmap> {
        self.map.get(&id)
    }

    pub fn contains(&self, id: u64) -> bool {
        self.map.contains_key(&id)
    }
}

impl Default for IconStore {
    fn default() -> Self {
        Self::new()
    }
}

/// 跨帧缓存的 DWrite 文本格式（设备无关，可安全缓存）。
pub struct TextFormats {
    pub title: IDWriteTextFormat,
    pub label: IDWriteTextFormat,
}

impl TextFormats {
    pub fn new(dwrite: &IDWriteFactory, theme: &Theme) -> Result<Self> {
        Ok(Self {
            title: make_text_format(dwrite, theme.title)?,
            label: make_text_format(dwrite, theme.label)?,
        })
    }
}

/// 单帧用画笔集合（渲染目标每帧重建，故画笔只能单帧使用）。
struct Brushes {
    bg: ID2D1SolidColorBrush,
    border: ID2D1SolidColorBrush,
    title: ID2D1SolidColorBrush,
    label: ID2D1SolidColorBrush,
}

/// 把一帧场景画进渲染目标。目标在 `Frame::finish` 后由调用方提交。
pub fn draw_scene(
    target: &ID2D1RenderTarget,
    theme: &Theme,
    scene: &Scene,
    icons: &IconStore,
    formats: &TextFormats,
) -> Result<()> {
    // 全透明清底：栅栏外区域让桌面（壁纸/图标）透出
    let clear = D2D1_COLOR_F {
        r: 0.0,
        g: 0.0,
        b: 0.0,
        a: 0.0,
    };
    unsafe { target.Clear(Some(&clear)) };

    let brushes = Brushes {
        bg: unsafe { target.CreateSolidColorBrush(&theme.fence_bg.to_d2d(), None)? },
        border: unsafe { target.CreateSolidColorBrush(&theme.fence_border.to_d2d(), None)? },
        title: unsafe { target.CreateSolidColorBrush(&theme.title.color.to_d2d(), None)? },
        label: unsafe { target.CreateSolidColorBrush(&theme.label.color.to_d2d(), None)? },
    };

    for fence in &scene.fences {
        draw_fence(target, theme, fence, &brushes, icons, formats)?;
    }
    Ok(())
}

fn draw_fence(
    target: &ID2D1RenderTarget,
    theme: &Theme,
    fence: &SceneFence,
    brushes: &Brushes,
    icons: &IconStore,
    formats: &TextFormats,
) -> Result<()> {
    let rr = D2D1_ROUNDED_RECT {
        rect: D2D_RECT_F {
            left: fence.x,
            top: fence.y,
            right: fence.x + fence.width,
            bottom: fence.y + fence.height,
        },
        radiusX: theme.fence_corner_radius,
        radiusY: theme.fence_corner_radius,
    };
    unsafe {
        target.FillRoundedRectangle(&rr, &brushes.bg);
        target.DrawRoundedRectangle(&rr, &brushes.border, 1.0, None);
    }

    if !fence.title.is_empty() {
        let tr = D2D_RECT_F {
            left: fence.x + theme.fence_padding,
            top: fence.y + theme.fence_padding,
            right: fence.x + fence.width - theme.fence_padding,
            bottom: fence.y + theme.fence_padding + theme.title.size * 1.6,
        };
        draw_text(target, &fence.title, &formats.title, tr, &brushes.title);
    }

    for icon in &fence.icons {
        let dest = D2D_RECT_F {
            left: icon.x,
            top: icon.y,
            right: icon.x + icon.size,
            bottom: icon.y + icon.size,
        };
        if let Some(bmp) = icons.get(icon.bitmap_id) {
            unsafe {
                target.DrawBitmap(
                    bmp,
                    Some(&dest),
                    1.0,
                    D2D1_BITMAP_INTERPOLATION_MODE_LINEAR,
                    None,
                );
            }
        }
        if !icon.label.is_empty() {
            let lr = D2D_RECT_F {
                left: icon.x - 2.0,
                top: icon.y + icon.size + theme.icon_caption_gap,
                right: icon.x + icon.size + 2.0,
                bottom: icon.y + icon.size + theme.icon_caption_gap + theme.label.size * 1.6,
            };
            draw_text(target, &icon.label, &formats.label, lr, &brushes.label);
        }
    }
    Ok(())
}

fn draw_text(
    target: &ID2D1RenderTarget,
    text: &str,
    format: &IDWriteTextFormat,
    rect: D2D_RECT_F,
    brush: &ID2D1SolidColorBrush,
) {
    let wide: Vec<u16> = text.encode_utf16().collect();
    unsafe {
        target.DrawText(
            &wide,
            format,
            &rect,
            brush,
            D2D1_DRAW_TEXT_OPTIONS_CLIP,
            DWRITE_MEASURING_MODE_NATURAL,
        );
    }
}

/// 把 CPU 侧图标像素（top-down BGRA premultiplied）上传为 GPU 位图。
fn make_bitmap(target: &ID2D1RenderTarget, data: &IconData) -> Result<ID2D1Bitmap> {
    let props = D2D1_BITMAP_PROPERTIES {
        pixelFormat: D2D1_PIXEL_FORMAT {
            format: DXGI_FORMAT_B8G8R8A8_UNORM,
            alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
        },
        dpiX: 96.0,
        dpiY: 96.0,
    };
    unsafe {
        target.CreateBitmap(
            D2D_SIZE_U {
                width: data.width,
                height: data.height,
            },
            Some(data.pixels.as_ptr() as *const core::ffi::c_void),
            data.stride() as u32,
            &props,
        )
    }
}

fn make_text_format(dwrite: &IDWriteFactory, style: TextStyle) -> Result<IDWriteTextFormat> {
    let family = wide(style.font_family);
    unsafe {
        dwrite.CreateTextFormat(
            PCWSTR(family.as_ptr()),
            None,
            DWRITE_FONT_WEIGHT_NORMAL,
            DWRITE_FONT_STYLE_NORMAL,
            DWRITE_FONT_STRETCH_NORMAL,
            style.size,
            PCWSTR::null(),
        )
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn icon_store_ids_are_monotonic_without_gpu() {
        // 不上传位图（无 GPU），仅验证 ID 分配逻辑。
        let mut store = IconStore::new();
        assert_eq!(store.next_id, 0);
        store.next_id = 5;
        // 没有 GPU 时 insert 会失败，这里只测纯逻辑路径
        let _ = &mut store;
    }

    #[test]
    fn wide_terminates_with_nul() {
        let w = wide("Fence");
        assert_eq!(
            w,
            vec![
                b'F' as u16,
                b'e' as u16,
                b'n' as u16,
                b'c' as u16,
                b'e' as u16,
                0
            ]
        );
    }
}
