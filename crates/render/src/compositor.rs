//! 合成器：把场景呈现到 overlay 窗口的 DComp 视觉树。
//!
//! 持有 GPU 上下文、合成表面与根视觉。每帧：
//! `BeginDraw → 上传新图标位图 → draw_scene → EndDraw → Commit`。
//! 空闲时不触发任何绘制（0% CPU），只有调用 `present` 才消耗 GPU。
//!
//! 表面尺寸策略（内存优化）：DComp 表面按**内容包围盒**（栅栏 ∪ 控制台）缩放，
//! 而非整屏——全屏 3072×1920×4 ≈ 24MB 的后备缓冲，其中大部分区域是空白桌面
//! （透出），纯属浪费。内容变化（栅栏移动/增删）时按需重建表面；内容远小于
//! 表面时收缩（带迟滞，避免拖动时反复建/缩表面）。
//!
//! 坐标映射：根视觉偏移到内容原点、渲染目标施加平移变换，绘制仍用虚拟屏幕坐标
//! ——`surface_rect` 记录了表面覆盖的虚拟屏幕区域，虚拟坐标 (x,y) 在表面内的
//! 位置是 (x - surface_rect.x, y - surface_rect.y)。

use windows::core::Result;
use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::DirectComposition::{IDCompositionTarget, IDCompositionVisual};
use windows_numerics::Matrix3x2;

use sylva_shell::icons::IconData;

use crate::device::RenderDevice;
use crate::draw::{draw_scene, IconStore, TextFormats};
use crate::overlay::RectF;
use crate::scene::Scene;
use crate::surface::CompositionSurface;
use crate::theme::Theme;

/// 表面四周多留的空白（物理像素）：栅栏描边抗锯齿会向界外渗约 1px，留足余量避免
/// 内容被裁；也吸收小幅拖动，避免每帧重建表面。
const SURFACE_PAD: f32 = 16.0;
/// 收缩迟滞：表面面积超过内容包围盒的这么多倍时才重建缩小（防止拖动时反复建/缩）。
const SHRINK_FACTOR: f32 = 4.0;

/// 桌面合成器。
pub struct Compositor {
    device: RenderDevice,
    surface: CompositionSurface,
    /// 当前表面覆盖的虚拟屏幕区域（原点 + 尺寸）。
    surface_rect: RectF,
    /// overlay 窗口的虚拟屏幕原点：窗口 (0,0) 即虚拟 (origin.0, origin.1)。
    origin: (f32, f32),
    // 持有以维持 target/root 存活（SetRoot 后 DComp 亦引用它们）
    target: IDCompositionTarget,
    root: IDCompositionVisual,
    icons: IconStore,
    formats: TextFormats,
    theme: Theme,
}

impl Compositor {
    /// 绑定到 overlay 窗口。表面先以占位尺寸创建，首次 `present` 按内容包围盒重建。
    ///
    /// `ox/oy` 是 overlay 窗口的虚拟屏幕原点；`width/height` 为虚拟屏幕尺寸。
    pub fn new(
        device: RenderDevice,
        hwnd: HWND,
        ox: f32,
        oy: f32,
        width: u32,
        height: u32,
        theme: Theme,
    ) -> Result<Self> {
        let _ = (width, height);
        let target = unsafe { device.dcomp.CreateTargetForHwnd(hwnd, false)? };
        tracing::debug!("compositor: target 绑定成功");
        let surface = CompositionSurface::new(&device.dcomp, 1, 1)?;
        tracing::debug!("compositor: 表面创建成功（占位 1×1，首帧按内容重建）");
        let root = unsafe { device.dcomp.CreateVisual()? };
        unsafe { root.SetContent(surface.idcomp_surface())? };
        unsafe { target.SetRoot(&root)? };
        tracing::debug!("compositor: 视觉树挂接成功");
        // 首次提交让视觉树生效（此时表面全透明，桌面透出）
        unsafe { device.dcomp.Commit()? };
        tracing::debug!("compositor: 首次提交成功");

        let formats = TextFormats::new(&device.dwrite, &theme)?;
        tracing::debug!("compositor: 文本格式就绪");
        Ok(Self {
            device,
            surface,
            surface_rect: RectF {
                x: ox,
                y: oy,
                w: 1.0,
                h: 1.0,
            },
            origin: (ox, oy),
            target,
            root,
            icons: IconStore::new(),
            formats,
            theme,
        })
    }

    /// 呈现一帧：先按内容包围盒确认表面尺寸，再上传新图标位图、绘制整个场景、提交。
    ///
    /// `uploads` 的 `(id, data)` 中 `id` 必须与 `scene` 里 `SceneIcon::bitmap_id`
    /// 一致（App 层预分配），同一 ID 重复上传会覆盖旧位图。
    pub fn present(&mut self, scene: &Scene, uploads: &[(u64, &IconData)]) -> Result<()> {
        if let Some(bbox) = scene.content_rect() {
            self.ensure_covering(bbox);
        }
        let frame = self.surface.begin_frame(&self.device.d2d)?;
        tracing::debug!("present: begin_frame 成功");
        {
            let target = frame.target();
            // 平移变换：把虚拟屏幕坐标映射进以 `surface_rect` 原点为 (0,0) 的表面。
            // 根视觉已偏移到内容原点，二者抵消后虚拟坐标直接对应屏幕坐标。
            let t = Matrix3x2::translation(-self.surface_rect.x, -self.surface_rect.y);
            unsafe { target.SetTransform(&t) };
            for (id, data) in uploads {
                self.icons.insert_at(target, *id, data)?;
            }
            tracing::debug!("present: 位图上传成功 {}", uploads.len());
            draw_scene(target, &self.theme, scene, &self.icons, &self.formats)?;
            tracing::debug!("present: draw_scene 成功");
        }
        frame.finish()?;
        tracing::debug!("present: finish 成功");
        unsafe { self.device.dcomp.Commit()? };
        tracing::debug!("present: commit 成功");
        Ok(())
    }

    /// 确保表面覆盖 `bbox`（外加 `SURFACE_PAD` 余量）。
    ///
    /// - 内容超出当前表面 → 立即重建（放大），避免内容被裁；
    /// - 内容远小于表面（`SHRINK_FACTOR` 倍以上）→ 重建缩小，把内存还回去；
    /// - 其余情况沿用现有表面（迟滞：拖动/悬停时表面稳定不抖动）。
    ///
    /// 重建时图标位图无需重新上传——它们是设备级资源，跨表面仍有效。
    fn ensure_covering(&mut self, bbox: RectF) {
        let want = RectF {
            x: bbox.x - SURFACE_PAD,
            y: bbox.y - SURFACE_PAD,
            w: bbox.w + 2.0 * SURFACE_PAD,
            h: bbox.h + 2.0 * SURFACE_PAD,
        };
        let cur = self.surface_rect;
        let covers = cur.x <= want.x
            && cur.y <= want.y
            && cur.x + cur.w >= want.x + want.w
            && cur.y + cur.h >= want.y + want.h;
        let oversized = (cur.w * cur.h) > SHRINK_FACTOR * (want.w * want.h);
        if covers && !oversized {
            return;
        }
        let (sw, sh) = (want.w.max(1.0).ceil() as u32, want.h.max(1.0).ceil() as u32);
        let surface = match CompositionSurface::new(&self.device.dcomp, sw, sh) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(?e, sw, sh, "重建合成表面失败，沿用旧表面");
                return;
            }
        };
        tracing::debug!(
            old_w = cur.w,
            old_h = cur.h,
            new_w = sw,
            new_h = sh,
            "合成表面按内容包围盒重建"
        );
        unsafe {
            let _ = self.root.SetContent(surface.idcomp_surface());
            // 表面原点（虚拟屏幕坐标）→ 窗口坐标偏移（窗口 (0,0) 即虚拟 origin）
            let _ = self.root.SetOffsetX2(want.x - self.origin.0);
            let _ = self.root.SetOffsetY2(want.y - self.origin.1);
        }
        self.surface = surface;
        self.surface_rect = want;
    }

    /// 当前合成表面的尺寸（物理像素，即内容包围盒 + 余量）。
    pub fn size(&self) -> (f32, f32) {
        (self.surface.width as f32, self.surface.height as f32)
    }
}
