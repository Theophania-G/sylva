//! WinRT 合成绘制表面：一帧的 BeginDraw → D2D 绘制 → EndDraw。
//!
//! `CompositionSurface` 持有合成图形设备的绘制表面（`CompositionDrawingSurface`）；
//! 每帧 `begin_frame` 经 `ICompositionDrawingSurfaceInterop::BeginDraw` 取回
//! `ID2D1DeviceContext`（Deref 即 `ID2D1RenderTarget`，与既有绘制路径一致），
//! 绘制完成后 `finish` 提交给合成器。`RequestCommitAsync` 由合成器统一调用。

use windows::core::{Interface, Result};
use windows::Foundation::Size;
use windows::Graphics::DirectX::{DirectXAlphaMode, DirectXPixelFormat};
use windows::Win32::Foundation::{POINT, RECT};
use windows::Win32::Graphics::Direct2D::{ID2D1DeviceContext, ID2D1RenderTarget};
use windows::Win32::System::WinRT::Composition::ICompositionDrawingSurfaceInterop;
use windows::UI::Composition::{CompositionDrawingSurface, CompositionGraphicsDevice};

/// 合成绘制表面（premultiplied BGRA，与图标像素格式一致）。
pub struct CompositionSurface {
    surface: CompositionDrawingSurface,
    pub width: u32,
    pub height: u32,
}

impl CompositionSurface {
    pub fn new(gfx: &CompositionGraphicsDevice, width: u32, height: u32) -> Result<Self> {
        let surface = gfx.CreateDrawingSurface(
            Size {
                Width: width as f32,
                Height: height as f32,
            },
            DirectXPixelFormat::B8G8R8A8UIntNormalized,
            DirectXAlphaMode::Premultiplied,
        )?;
        Ok(Self {
            surface,
            width,
            height,
        })
    }

    /// 底层绘制表面（供 `Compositor::CreateSurfaceBrushWithSurface` 挂到视觉）。
    pub fn raw(&self) -> &CompositionDrawingSurface {
        &self.surface
    }

    /// 开始一帧：返回 D2D 设备上下文。绘制完成后必须调用 `Frame::finish`。
    ///
    /// DPI 固定为 96（1 DIP = 1 物理像素），保证 App 层的物理像素布局与
    /// DWrite 的 DIP 字号对齐。
    pub fn begin_frame(&self) -> Result<Frame> {
        let interop: ICompositionDrawingSurfaceInterop = self.surface.cast()?;
        let rect = RECT {
            left: 0,
            top: 0,
            right: self.width as i32,
            bottom: self.height as i32,
        };
        let mut _offset = POINT::default();
        let target: ID2D1DeviceContext = unsafe { interop.BeginDraw(Some(&rect), &mut _offset)? };
        // 绘制表面来自合成图形设备，其设备上下文 DPI 跟随系统；显式 96 保证
        // 1 DIP = 1 物理像素（否则高 DPI 下物理像素布局会被目标 DPI 放大而超界）。
        unsafe { target.SetDpi(96.0, 96.0) };
        Ok(Frame { interop, target })
    }
}

/// 一帧绘制会话。`finish` 提交 D2D 绘制并结束绘制表面。
pub struct Frame {
    interop: ICompositionDrawingSurfaceInterop,
    target: ID2D1DeviceContext,
}

impl Frame {
    pub fn target(&self) -> &ID2D1RenderTarget {
        // ID2D1DeviceContext Deref 到 ID2D1RenderTarget
        &self.target
    }

    /// 结束绘制并提交表面（随后调用方应 `RequestCommitAsync`）。
    pub fn finish(self) -> Result<()> {
        unsafe { self.interop.EndDraw()? };
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // 表面依赖真实 GPU + 合成器，初始化在 device 测试中覆盖；
    // 这里仅做常量/结构的编译级验证。
    #[test]
    fn drawing_surface_pixel_format_is_bgra_premultiplied() {
        // 与图标像素格式一致：合成器用 premultiplied BGRA 绘制。
        let _ = (
            DirectXPixelFormat::B8G8R8A8UIntNormalized,
            DirectXAlphaMode::Premultiplied,
        );
    }
}
