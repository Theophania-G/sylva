//! DComp 合成表面：一帧的 BeginDraw → D2D 绘制 → EndDraw → Commit。
//!
//! `CompositionSurface` 持有 DComp 表面；每帧 `begin_frame` 取回 DXGI 表面并
//! 包成 D2D 渲染目标（`Frame`），绘制完成后 `finish` 提交给合成器。

use windows::core::Result;
use windows::Win32::Foundation::POINT;
use windows::Win32::Graphics::Direct2D::Common::{
    D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_PIXEL_FORMAT,
};
use windows::Win32::Graphics::Direct2D::{
    ID2D1Factory, ID2D1RenderTarget, D2D1_FEATURE_LEVEL_DEFAULT, D2D1_RENDER_TARGET_PROPERTIES,
    D2D1_RENDER_TARGET_TYPE_DEFAULT, D2D1_RENDER_TARGET_USAGE_NONE,
};
use windows::Win32::Graphics::DirectComposition::{IDCompositionDevice, IDCompositionSurface};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_ALPHA_MODE_PREMULTIPLIED, DXGI_FORMAT_B8G8R8A8_UNORM,
};
use windows::Win32::Graphics::Dxgi::IDXGISurface;

/// DComp 合成表面（premultiplied BGRA，与图标像素格式一致）。
pub struct CompositionSurface {
    surface: IDCompositionSurface,
    pub width: u32,
    pub height: u32,
}

impl CompositionSurface {
    pub fn new(device: &IDCompositionDevice, width: u32, height: u32) -> Result<Self> {
        let surface = unsafe {
            device.CreateSurface(
                width,
                height,
                DXGI_FORMAT_B8G8R8A8_UNORM,
                DXGI_ALPHA_MODE_PREMULTIPLIED,
            )?
        };
        Ok(Self {
            surface,
            width,
            height,
        })
    }

    /// 挂到视觉树的表面句柄（供 `IDCompositionVisual::SetContent`）。
    pub fn idcomp_surface(&self) -> &IDCompositionSurface {
        &self.surface
    }

    /// 开始一帧：返回 D2D 渲染目标。绘制完成后必须调用 `Frame::finish`。
    pub fn begin_frame(&self, d2d: &ID2D1Factory) -> Result<Frame<'_>> {
        let mut _offset = POINT::default();
        let dxgi_surface: IDXGISurface = unsafe { self.surface.BeginDraw(None, &mut _offset)? };
        let props = D2D1_RENDER_TARGET_PROPERTIES {
            r#type: D2D1_RENDER_TARGET_TYPE_DEFAULT,
            pixelFormat: D2D1_PIXEL_FORMAT {
                format: DXGI_FORMAT_B8G8R8A8_UNORM,
                alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
            },
            dpiX: 0.0,
            dpiY: 0.0,
            usage: D2D1_RENDER_TARGET_USAGE_NONE,
            minLevel: D2D1_FEATURE_LEVEL_DEFAULT,
        };
        let target: ID2D1RenderTarget =
            unsafe { d2d.CreateDxgiSurfaceRenderTarget(&dxgi_surface, &props)? };
        // D2D 渲染目标必须进入 BeginDraw 状态才能绘制，否则 EndDraw 报 WRONG_STATE
        unsafe { target.BeginDraw() };
        Ok(Frame {
            surface: &self.surface,
            target,
        })
    }
}

/// 一帧绘制会话。`finish` 提交 D2D 绘制并结束 DComp 表面。
pub struct Frame<'a> {
    surface: &'a IDCompositionSurface,
    target: ID2D1RenderTarget,
}

impl Frame<'_> {
    pub fn target(&self) -> &ID2D1RenderTarget {
        &self.target
    }

    /// 结束绘制并提交表面（随后调用方应 `dcomp.Commit()`）。
    pub fn finish(self) -> Result<()> {
        unsafe { self.target.EndDraw(None, None)? };
        unsafe { self.surface.EndDraw()? };
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // 表面依赖真实 GPU + 窗口，初始化在 device 测试中覆盖；
    // 这里仅做常量/结构的编译级验证。
    #[test]
    fn pixel_format_constants_are_valid() {
        let _ = D2D1_PIXEL_FORMAT {
            format: DXGI_FORMAT_B8G8R8A8_UNORM,
            alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
        };
    }
}
