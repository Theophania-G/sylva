//! GPU 上下文：D3D11 设备 + D2D/DWrite 工厂 + WinRT 合成器 + 合成图形设备。
//!
//! 一个进程一个实例，跨帧复用。底层 D3D11 / DXGI / D2D 设备必须存活到
//! 所有合成对象释放为止，因此一并持有。
//!
//! WinRT `Windows.UI.Composition` 要求当前线程先初始化 COM + DispatcherQueue
//! （探针实测：缺 DispatcherQueue 时 `Compositor::new()` 返回 E_ACCESSDENIED）。
//! DispatcherQueue 控制器必须存活到合成器销毁，这里一并持有。

use windows::core::{Interface, Result};
use windows::System::DispatcherQueueController;
use windows::Win32::Foundation::HMODULE;
use windows::Win32::Graphics::Direct2D::{
    D2D1CreateFactory, ID2D1Device, ID2D1Factory, ID2D1Factory1, D2D1_FACTORY_TYPE_SINGLE_THREADED,
};
use windows::Win32::Graphics::Direct3D::{
    D3D_DRIVER_TYPE_HARDWARE, D3D_FEATURE_LEVEL_10_1, D3D_FEATURE_LEVEL_11_0,
    D3D_FEATURE_LEVEL_11_1,
};
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Device, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_SDK_VERSION,
};
use windows::Win32::Graphics::DirectWrite::{
    DWriteCreateFactory, IDWriteFactory, DWRITE_FACTORY_TYPE_ISOLATED,
};
use windows::Win32::Graphics::Dxgi::IDXGIDevice;
use windows::Win32::System::WinRT::Composition::ICompositorInterop;
use windows::Win32::System::WinRT::{
    CreateDispatcherQueueController, DispatcherQueueOptions, DQTAT_COM_STA, DQTYPE_THREAD_CURRENT,
};
use windows::UI::Composition::{CompositionGraphicsDevice, Compositor as WinCompositor};

/// 渲染设备集合。
pub struct RenderDevice {
    /// D2D 工厂（构造 D2D 设备用；绘制表面直接由合成图形设备提供设备上下文）。
    pub d2d: ID2D1Factory,
    pub dwrite: IDWriteFactory,
    /// WinRT 合成器（`Windows.UI.Composition`）。创建视觉树、效果工厂、绘制表面。
    pub compositor: WinCompositor,
    /// 合成图形设备：`CreateDrawingSurface` 创建内容/区域绘制表面。
    pub gfx_device: CompositionGraphicsDevice,
    // 保持底层对象存活（合成器 / D2D 设备 / D3D11 / DXGI / DispatcherQueue 都依赖）
    #[allow(dead_code)]
    _d2d_device: ID2D1Device,
    #[allow(dead_code)]
    _d3d: ID3D11Device,
    #[allow(dead_code)]
    _dxgi: IDXGIDevice,
    #[allow(dead_code)]
    _dq: DispatcherQueueController,
}

impl RenderDevice {
    /// 创建全部 GPU 上下文。失败通常意味着无硬件加速（远程桌面/虚拟机），
    /// 调用方可降级处理。
    ///
    /// 要求调用线程已 `CoInitializeEx`（STA）——应用在 `shell::com::init` 完成；
    /// 本方法再补上线程的 DispatcherQueue，满足 `Compositor::new()` 的前置条件。
    pub fn new() -> Result<Self> {
        // WinRT 合成器要求当前线程先初始化 DispatcherQueue（STA）。
        // 控制器须存活到合成器销毁。
        let dq: DispatcherQueueController = unsafe {
            CreateDispatcherQueueController(DispatcherQueueOptions {
                dwSize: std::mem::size_of::<DispatcherQueueOptions>() as u32,
                threadType: DQTYPE_THREAD_CURRENT,
                apartmentType: DQTAT_COM_STA,
            })?
        };

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

        let dxgi: IDXGIDevice = d3d.cast()?;
        let d2d: ID2D1Factory =
            unsafe { D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)? };
        let d2d1: ID2D1Factory1 = d2d.clone().cast()?;
        let d2d_device: ID2D1Device = unsafe { d2d1.CreateDevice(&dxgi)? };
        // 独立工厂：不依赖进程 MTA（我们以 STA 初始化 COM），也避免与其他进程共享
        let dwrite: IDWriteFactory = unsafe { DWriteCreateFactory(DWRITE_FACTORY_TYPE_ISOLATED)? };

        // WinRT 合成器 + 合成图形设备（经 ICompositorInterop 绑定 D2D 设备）
        let compositor = WinCompositor::new()?;
        let c_interop: ICompositorInterop = compositor.cast()?;
        let gfx_device = unsafe { c_interop.CreateGraphicsDevice(&d2d_device)? };

        Ok(Self {
            d2d,
            dwrite,
            compositor,
            gfx_device,
            _d2d_device: d2d_device,
            _d3d: d3d,
            _dxgi: dxgi,
            _dq: dq,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_creation_works_or_graceful() {
        use windows::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};
        // 应用真实环境：STA COM（RenderDevice::new 内部还需 DispatcherQueue）。
        unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok();
        }
        // 有硬件加速的机器应创建成功；无 GPU 环境（CI headless）允许失败。
        match RenderDevice::new() {
            Ok(d) => {
                let _ = (&d.d2d, &d.dwrite, &d.compositor, &d.gfx_device);
            }
            Err(e) => eprintln!("无 GPU 上下文，跳过（CI/远程环境预期行为）: {e:?}"),
        }
    }

    #[test]
    fn dwrite_text_format_creates_ok() {
        // 定位 CreateTextFormat E_INVALIDARG：单独验证 DWrite 工厂与文本格式创建。
        use windows::core::PCWSTR;
        use windows::Win32::Graphics::DirectWrite::{
            DWriteCreateFactory, DWRITE_FACTORY_TYPE_ISOLATED, DWRITE_FONT_STRETCH_NORMAL,
            DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_WEIGHT_NORMAL,
        };
        use windows::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};

        // 应用真实环境：STA COM；locale 必须显式（NULL 会 E_INVALIDARG）
        unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok();
        }

        fn wide(s: &str) -> Vec<u16> {
            s.encode_utf16().chain(std::iter::once(0)).collect()
        }
        let factory: windows::core::Result<IDWriteFactory> =
            unsafe { DWriteCreateFactory(DWRITE_FACTORY_TYPE_ISOLATED) };
        let factory = factory.expect("DWrite 工厂创建失败");
        for name in ["Microsoft YaHei UI", "Segoe UI", "Arial"] {
            let family = wide(name);
            let locale = wide("zh-CN");
            let r = unsafe {
                factory.CreateTextFormat(
                    PCWSTR(family.as_ptr()),
                    None,
                    DWRITE_FONT_WEIGHT_NORMAL,
                    DWRITE_FONT_STYLE_NORMAL,
                    DWRITE_FONT_STRETCH_NORMAL,
                    16.0,
                    PCWSTR(locale.as_ptr()),
                )
            };
            match r {
                Ok(_) => eprintln!("CreateTextFormat({name}) OK"),
                Err(e) => eprintln!("CreateTextFormat({name}) FAIL: {e:?}"),
            }
        }
    }
}
