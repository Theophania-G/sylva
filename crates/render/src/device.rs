//! GPU 上下文：D3D11 设备 + D2D/DWrite 工厂 + DComp 设备。
//!
//! 一个进程一个实例，跨帧复用。底层 D3D11 / DXGI 设备必须存活到
//! 所有 D2D/DComp 对象释放为止，因此一并持有。

use windows::core::{Interface, Result};
use windows::Win32::Foundation::HMODULE;
use windows::Win32::Graphics::Direct2D::{
    D2D1CreateFactory, ID2D1Factory, D2D1_FACTORY_TYPE_SINGLE_THREADED,
};
use windows::Win32::Graphics::Direct3D::{
    D3D_DRIVER_TYPE_HARDWARE, D3D_FEATURE_LEVEL_10_1, D3D_FEATURE_LEVEL_11_0,
    D3D_FEATURE_LEVEL_11_1,
};
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Device, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_SDK_VERSION,
};
use windows::Win32::Graphics::DirectComposition::{DCompositionCreateDevice, IDCompositionDevice};
use windows::Win32::Graphics::DirectWrite::{
    DWriteCreateFactory, IDWriteFactory, DWRITE_FACTORY_TYPE_SHARED,
};
use windows::Win32::Graphics::Dxgi::IDXGIDevice;

/// 渲染设备集合。
pub struct RenderDevice {
    pub d2d: ID2D1Factory,
    pub dwrite: IDWriteFactory,
    pub dcomp: IDCompositionDevice,
    // 保持底层对象存活（D2D/DComp 依赖它们）
    _d3d: ID3D11Device,
    _dxgi: IDXGIDevice,
}

impl RenderDevice {
    /// 创建全部 GPU 上下文。失败通常意味着无硬件加速（远程桌面/虚拟机），
    /// 调用方可降级处理。
    pub fn new() -> Result<Self> {
        // D3D11 设备需带 BGRA 支持才能被 D2D 使用
        let mut d3d: Option<ID3D11Device> = None;
        unsafe {
            D3D11CreateDevice(
                None, // 默认适配器
                D3D_DRIVER_TYPE_HARDWARE,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                Some(&[
                    D3D_FEATURE_LEVEL_11_1,
                    D3D_FEATURE_LEVEL_11_0,
                    D3D_FEATURE_LEVEL_10_1,
                ]),
                D3D11_SDK_VERSION,
                Some(&mut d3d),
                None,
                None,
            )?
        };
        let d3d = d3d.expect("D3D11CreateDevice 成功返回后必有设备");

        // D3D11 设备 → DXGI 设备 → DComp 设备
        let dxgi: IDXGIDevice = d3d.cast()?;
        let dcomp: IDCompositionDevice = unsafe { DCompositionCreateDevice(&dxgi)? };

        let d2d: ID2D1Factory =
            unsafe { D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)? };
        let dwrite: IDWriteFactory = unsafe { DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)? };

        Ok(Self {
            d2d,
            dwrite,
            dcomp,
            _d3d: d3d,
            _dxgi: dxgi,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_creation_works_or_graceful() {
        // 有硬件加速的机器应创建成功；无 GPU 环境（CI headless）允许失败。
        match RenderDevice::new() {
            Ok(d) => {
                let _ = (&d.d2d, &d.dwrite, &d.dcomp);
            }
            Err(_) => eprintln!("无 GPU 上下文，跳过（CI/远程环境预期行为）"),
        }
    }
}
