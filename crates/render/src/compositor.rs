//! 合成器：把场景呈现到 overlay 窗口的 DComp 视觉树。
//!
//! 持有 GPU 上下文、合成表面与根视觉。每帧：
//! `BeginDraw → 上传新图标位图 → draw_scene → EndDraw → Commit`。
//! 空闲时不触发任何绘制（0% CPU），只有调用 `present` 才消耗 GPU。

use windows::core::Result;
use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::DirectComposition::{IDCompositionTarget, IDCompositionVisual};

use fence_shell::icons::IconData;

use crate::device::RenderDevice;
use crate::draw::{draw_scene, IconStore, TextFormats};
use crate::scene::Scene;
use crate::surface::CompositionSurface;
use crate::theme::Theme;

/// 桌面合成器。
pub struct Compositor {
    device: RenderDevice,
    surface: CompositionSurface,
    // 持有以维持 target/root 存活（SetRoot 后 DComp 亦引用它们）
    target: IDCompositionTarget,
    root: IDCompositionVisual,
    icons: IconStore,
    formats: TextFormats,
    theme: Theme,
}

impl Compositor {
    /// 绑定到 overlay 窗口，创建覆盖整个虚拟屏幕的合成表面与根视觉。
    pub fn new(
        device: RenderDevice,
        hwnd: HWND,
        width: u32,
        height: u32,
        theme: Theme,
    ) -> Result<Self> {
        let target = unsafe { device.dcomp.CreateTargetForHwnd(hwnd, false)? };
        let surface = CompositionSurface::new(&device.dcomp, width, height)?;
        let root = unsafe { device.dcomp.CreateVisual()? };
        unsafe { root.SetContent(surface.idcomp_surface())? };
        unsafe { target.SetRoot(&root)? };
        // 首次提交让视觉树生效（此时表面全透明，桌面透出）
        unsafe { device.dcomp.Commit()? };

        let formats = TextFormats::new(&device.dwrite, &theme)?;
        Ok(Self {
            device,
            surface,
            target,
            root,
            icons: IconStore::new(),
            formats,
            theme,
        })
    }

    /// 呈现一帧：先上传本次新增的图标位图，再绘制整个场景，最后提交。
    ///
    /// `uploads` 的 `(id, data)` 中 `id` 必须与 `scene` 里 `SceneIcon::bitmap_id`
    /// 一致（App 层预分配），同一 ID 重复上传会覆盖旧位图。
    pub fn present(&mut self, scene: &Scene, uploads: &[(u64, &IconData)]) -> Result<()> {
        let frame = self.surface.begin_frame(&self.device.d2d)?;
        {
            let target = frame.target();
            for (id, data) in uploads {
                self.icons.insert_at(target, *id, data)?;
            }
            draw_scene(target, &self.theme, scene, &self.icons, &self.formats)?;
        }
        frame.finish()?;
        unsafe { self.device.dcomp.Commit()? };
        Ok(())
    }

    /// 场景尺寸（物理像素，即 overlay 覆盖的虚拟屏幕）。
    pub fn size(&self) -> (f32, f32) {
        (self.surface.width as f32, self.surface.height as f32)
    }
}
