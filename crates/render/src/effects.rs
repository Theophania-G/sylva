//! WinRT 合成器的高斯模糊效果 shim（`IGraphicsEffectD2D1Interop` 三接口）。
//!
//! `Windows.UI.Composition` 没有内置 GaussianBlurEffect 类（那是 Win2D 的），
//! 运行时通过 `IGraphicsEffectD2D1Interop` 让自定义 `IGraphicsEffect` 映射到
//! D2D1 效果（这里即 `CLSID_D2D1GaussianBlur`）。构建 `CompositionEffectFactory`
//! 时运行时会枚举属性/输入端口，本 shim 的契约在探针中逐项实测敲定：
//!
//! - `GetEffectId` → `CLSID_D2D1GaussianBlur`；
//! - 输入端口：`GetSourceCount=1`（D2D1 高斯模糊恰好 1 个输入），`GetSource(0)`
//!   返回一个 **`CompositionEffectSourceParameter`**（Win2D 同款模式：它实现
//!   `IGraphicsEffectSource`，是运行时认识的"未绑定参数"占位）。返回 null
//!   （→ "Null effect input"）或任意其他 `IGraphicsEffectSource` 占位
//!   （→ "Unexpected effect input type"）均 0x80070057，实测；
//! - 属性：`GetPropertyCount=3`，`GetProperty(0..2)` 分别返回 StandardDeviation
//!   (f32) / Optimization (UINT32=1 BALANCED) / BorderMode (UINT32=0 SOFT)，
//!   越界用 `E_BOUNDS` 收尾（`E_NOTIMPL` 会被当硬失败）；
//! - `GetNamedPropertyMapping`：StandardDeviation/BlurAmount→0、Optimization→1、
//!   BorderMode→2（均 DIRECT 映射），未知名→`E_INVALIDARG`。
//!
//! `SetSourceParameter("Blur", backdrop)` 再把 BackdropBrush 填进名为 "Blur" 的端口。

use std::cell::RefCell;

use windows::core::{implement, Interface, Result, GUID, HSTRING, PCWSTR};
use windows::Foundation::{IPropertyValue, PropertyValue};
use windows::Graphics::Effects::{
    IGraphicsEffect, IGraphicsEffectSource, IGraphicsEffectSource_Impl, IGraphicsEffect_Impl,
};
use windows::Win32::Foundation::{E_BOUNDS, E_INVALIDARG};
use windows::Win32::Graphics::Direct2D::CLSID_D2D1GaussianBlur;
use windows::Win32::System::WinRT::Graphics::Direct2D::{
    IGraphicsEffectD2D1Interop, IGraphicsEffectD2D1Interop_Impl, GRAPHICS_EFFECT_PROPERTY_MAPPING,
    GRAPHICS_EFFECT_PROPERTY_MAPPING_DIRECT,
};
use windows::UI::Composition::CompositionEffectSourceParameter;

#[implement(IGraphicsEffect, IGraphicsEffectSource, IGraphicsEffectD2D1Interop)]
pub struct GaussianBlurEffect {
    name: RefCell<HSTRING>,
    stddev: f32,
    source: IGraphicsEffectSource,
}

impl GaussianBlurEffect {
    /// 构造高斯模糊效果。`stddev` 为物理像素高斯标准偏差。
    pub fn new(stddev: f32) -> Result<Self> {
        let param = CompositionEffectSourceParameter::Create(&HSTRING::from("Blur"))?;
        let source: IGraphicsEffectSource = param.cast()?;
        Ok(Self {
            name: RefCell::new(HSTRING::from("Blur")),
            stddev,
            source,
        })
    }
}

impl IGraphicsEffect_Impl for GaussianBlurEffect_Impl {
    fn Name(&self) -> Result<HSTRING> {
        Ok(self.name.borrow().clone())
    }
    fn SetName(&self, name: &HSTRING) -> Result<()> {
        *self.name.borrow_mut() = name.clone();
        Ok(())
    }
}

impl IGraphicsEffectSource_Impl for GaussianBlurEffect_Impl {}

impl IGraphicsEffectD2D1Interop_Impl for GaussianBlurEffect_Impl {
    fn GetEffectId(&self) -> Result<GUID> {
        Ok(CLSID_D2D1GaussianBlur)
    }
    // COM out 参数由 trait 签名固定为裸指针，写入只能在 unsafe 块内进行；
    // 签名来自 windows-rs 生成的接口（安全方法），在此对指针解引用是实现的固有语义。
    #[allow(clippy::not_unsafe_ptr_arg_deref)]
    fn GetNamedPropertyMapping(
        &self,
        name: &PCWSTR,
        index: *mut u32,
        mapping: *mut GRAPHICS_EFFECT_PROPERTY_MAPPING,
    ) -> Result<()> {
        let s = unsafe { name.to_string() }.unwrap_or_default();
        let (idx, map) = match s.as_str() {
            "StandardDeviation" | "BlurAmount" => (0, GRAPHICS_EFFECT_PROPERTY_MAPPING_DIRECT),
            "Optimization" => (1, GRAPHICS_EFFECT_PROPERTY_MAPPING_DIRECT),
            "BorderMode" => (2, GRAPHICS_EFFECT_PROPERTY_MAPPING_DIRECT),
            _ => return Err(E_INVALIDARG.into()),
        };
        unsafe {
            *index = idx;
            *mapping = map;
        }
        Ok(())
    }
    // D2D1 高斯模糊属性布局：0=StandardDeviation(f32)，1=Optimization(UINT32 枚举)，
    // 2=BorderMode(UINT32 枚举)。运行时按 D2D 效果自身属性数（3）枚举 GetProperty(0..2)。
    fn GetPropertyCount(&self) -> Result<u32> {
        Ok(3)
    }
    fn GetProperty(&self, index: u32) -> Result<IPropertyValue> {
        match index {
            // StandardDeviation（f32 物理像素 sigma）
            0 => PropertyValue::CreateSingle(self.stddev)?.cast(),
            // Optimization：1=BALANCED（D2D 默认）
            1 => PropertyValue::CreateUInt32(1)?.cast(),
            // BorderMode：0=SOFT（D2D 默认）
            2 => PropertyValue::CreateUInt32(0)?.cast(),
            _ => Err(E_BOUNDS.into()),
        }
    }
    fn GetSource(&self, index: u32) -> Result<IGraphicsEffectSource> {
        match index {
            // 输入端口 0 = CompositionEffectSourceParameter("Blur")；
            // SetSourceParameter 把 BackdropBrush 填进该端口。
            0 => Ok(self.source.clone()),
            _ => Err(E_BOUNDS.into()),
        }
    }
    fn GetSourceCount(&self) -> Result<u32> {
        Ok(1)
    }
}
