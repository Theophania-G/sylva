//! 合成器：把场景呈现到 overlay 窗口的 WinRT 视觉树（`Windows.UI.Composition`）。
//!
//! 持有 GPU 上下文、内容绘制表面与根视觉。每帧：
//! `BeginDraw → 上传新图标位图 → draw_scene → EndDraw → sync_blurs → RequestCommitAsync`。
//! 空闲时不触发任何绘制（0% CPU），只有调用 `present` 才消耗 GPU。
//!
//! ## 视觉树
//! ```text
//! root（ContainerVisual，桌面窗口目标）
//! ├─ [每个模糊栅栏] blur_visual_i（SpriteVisual）
//! │    ├─ Brush = 共享高斯模糊效果刷（BackdropBrush 输入，GPU 实时）
//! │    ├─ Clip = 圆角几何裁剪（与栅栏圆角一致）
//! │    └─ 插在内容表面**下方**
//! └─ content（SpriteVisual：内容绘制表面，画图标/标题/边框）
//! ```
//! 模糊视觉插在内容表面下方：内容表面在模糊栅栏区域保持透明（`fill_color=None`），
//! 透出下方实时模糊的桌面。BackdropBrush 采样窗口背后 DWM 桌面 —— 真正实时：
//! 桌面/其他窗口变化时模糊内容同步更新，无截图、无 CPU 高斯、无定时器。
//!
//! ## 表面尺寸策略（内存优化）
//! 内容表面按**内容包围盒**（栅栏 ∪ 控制台）缩放，而非整屏——全屏
//! 3072×1920×4 ≈ 24MB 的后备缓冲大部分是空白桌面（透出），纯属浪费。内容变化
//! （栅栏移动/增删）时按需重建表面；内容远小于表面时收缩（带迟滞，避免拖动时
//! 反复建/缩表面）。
//!
//! ## 坐标映射
//! overlay 窗口客户端 (0,0) = 虚拟屏 (0,0)（`OverlayWindow::create` 保证），因此
//! 虚拟屏幕坐标就是窗口坐标。`content` 视觉偏移到内容原点，绘制时渲染目标施加
//! 平移变换，绘制仍用虚拟屏幕坐标——`surface_rect` 记录了表面覆盖的虚拟屏幕区域，
//! 虚拟 (x,y) 在表面内的位置是 (x - surface_rect.x, y - surface_rect.y)。模糊视觉
//! 的**偏移** = 栅栏窗口坐标（即栅栏虚拟坐标）；其**裁剪几何**在视觉本地坐标空间
//! （偏移恒 (0,0)，铺满整个视觉），圆角与栅栏一致。

use windows::core::{Interface, Result, HSTRING};
use windows::Graphics::Effects::IGraphicsEffect;
use windows::Win32::Foundation::HWND;
use windows::Win32::System::WinRT::Composition::ICompositorDesktopInterop;
use windows::UI::Composition::Desktop::DesktopWindowTarget;
use windows::UI::Composition::{
    CompositionBackdropBrush, CompositionEffectBrush, CompositionGeometricClip,
    CompositionRoundedRectangleGeometry, ContainerVisual, SpriteVisual,
};
use windows_numerics::{Matrix3x2, Vector2, Vector3};

use sylva_shell::icons::IconData;

use sylva_core::model::FenceLayout;

use crate::device::RenderDevice;
use crate::draw::{draw_scene, IconStore, TextFormats};
use crate::effects::GaussianBlurEffect;
use crate::overlay::RectF;
use crate::scene::{Scene, SceneFence};
use crate::surface::CompositionSurface;
use crate::theme::Theme;

/// 表面四周多留的空白（物理像素）：栅栏描边抗锯齿会向界外渗约 1px，留足余量避免
/// 内容被裁；也吸收小幅拖动，避免每帧重建表面。
const SURFACE_PAD: f32 = 16.0;
/// 收缩迟滞：表面面积超过内容包围盒的这么多倍时才重建缩小（防止拖动时反复建/缩）。
const SHRINK_FACTOR: f32 = 4.0;

/// 一个模糊栅栏的视觉（与 `scene.fences[i]` 对应）。
struct BlurVisual {
    visual: SpriteVisual,
    /// 圆角几何裁剪（持有 geometry 引用；`geom` 字段保持 geometry 存活）。
    #[allow(dead_code)]
    clip: CompositionGeometricClip,
    geom: CompositionRoundedRectangleGeometry,
    /// 几何缓存：避免每帧重复提交相同值。
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    corner: f32,
    opacity: f32,
}

/// 桌面合成器。
pub struct Compositor {
    device: RenderDevice,
    surface: CompositionSurface,
    /// 当前表面覆盖的虚拟屏幕区域（原点 + 尺寸）。
    surface_rect: RectF,
    // 持有以维持 target 存活（SetRoot 后合成器亦引用它；字段只作生命周期锚点，不读）
    #[allow(dead_code)]
    target: DesktopWindowTarget,
    root: ContainerVisual,
    /// 内容视觉：持有内容绘制表面（图标/标题/边框）。
    content: SpriteVisual,
    /// 共享 BackdropBrush：采样窗口背后 DWM 桌面（全部模糊视觉共用）。
    backdrop: CompositionBackdropBrush,
    /// 共享高斯模糊效果刷（惰性创建，stddev 取主题）。
    blur_brush: Option<CompositionEffectBrush>,
    /// 与 `scene.fences` 下标对齐的模糊视觉；None = 该栅栏非模糊。
    blurs: Vec<Option<BlurVisual>>,
    icons: IconStore,
    formats: TextFormats,
    theme: Theme,
}

impl Compositor {
    /// 绑定到 overlay 窗口。内容表面先以占位尺寸创建，首次 `present` 按内容包围盒重建。
    ///
    /// 坐标约定：overlay 窗口客户端 (0,0) = 虚拟屏 (0,0)（见 `OverlayWindow::create`），
    /// 场景里的虚拟屏幕坐标可直接作为窗口坐标使用，无任何换算。
    pub fn new(device: RenderDevice, hwnd: HWND, theme: Theme) -> Result<Self> {
        // 桌面窗口目标（绑定 overlay 窗口）。非置顶：窗口在桌面壳层 z 序中由系统管理，
        // 全屏独占应用运行时 DWM 自动隐藏置顶窗口（wallpaper-engine 同款暂停逻辑）。
        let interop: ICompositorDesktopInterop = device.compositor.cast()?;
        let target = unsafe { interop.CreateDesktopWindowTarget(hwnd, false) }?;
        tracing::debug!("compositor: 桌面窗口目标绑定成功");

        let root = device.compositor.CreateContainerVisual()?;
        target.SetRoot(&root)?;
        tracing::debug!("compositor: 根容器挂接成功");

        let backdrop = device.compositor.CreateBackdropBrush()?;
        tracing::debug!("compositor: BackdropBrush 就绪");

        // 内容视觉：占位 1×1 表面，首帧按内容包围盒重建
        let surface = CompositionSurface::new(&device.gfx_device, 1, 1)?;
        let brush = device
            .compositor
            .CreateSurfaceBrushWithSurface(surface.raw())?;
        let content = device.compositor.CreateSpriteVisual()?;
        content.SetBrush(&brush)?;
        content.SetSize(Vector2 { X: 1.0, Y: 1.0 })?;
        content.SetOffset(Vector3 {
            X: 0.0,
            Y: 0.0,
            Z: 0.0,
        })?;
        root.Children()?.InsertAtTop(&content)?;
        tracing::debug!("compositor: 内容视觉挂载成功");

        // 首次提交让视觉树生效（此时表面全透明，桌面透出）
        device.compositor.RequestCommitAsync()?;
        tracing::debug!("compositor: 首次提交成功");

        let formats = TextFormats::new(&device.dwrite, &theme)?;
        tracing::debug!("compositor: 文本格式就绪");

        Ok(Self {
            device,
            surface,
            surface_rect: RectF {
                x: 0.0,
                y: 0.0,
                w: 1.0,
                h: 1.0,
            },
            target,
            root,
            content,
            backdrop,
            blur_brush: None,
            blurs: Vec::new(),
            icons: IconStore::new(),
            formats,
            theme,
        })
    }

    /// 呈现一帧：先按内容包围盒确认表面尺寸，再上传新图标位图、绘制整个场景、
    /// 同步模糊视觉、提交。
    ///
    /// `uploads` 的 `(id, data)` 中 `id` 必须与 `scene` 里 `SceneIcon::bitmap_id`
    /// 一致（App 层预分配），同一 ID 重复上传会覆盖旧位图。
    pub fn present(&mut self, scene: &Scene, uploads: &[(u64, &IconData)]) -> Result<()> {
        if let Some(bbox) = scene.content_rect() {
            // 侧边栏 Dock 的悬停工具提示会随光标频繁出现/消失，使内容包围盒在
            // 「仅 Dock」与「Dock+工具提示」间切换。若表面随之反复收缩/重建，
            // 重建瞬间新表面未就绪会闪一帧——桌面上只有 Dock 一个栅栏时没有别的
            // 内容撑住表面，肉眼即见快速闪烁（打开控制中心就不闪，正是因为面板
            // 让表面变大、工具提示变化不再越界）。这里探测到 Dock 存在就禁止收缩：
            // 表面第一次覆盖到工具提示范围后保持不再缩小，之后工具提示增删不再触发重建。
            let has_dock = scene.fences.iter().any(|f| f.layout == FenceLayout::Sidebar);
            self.ensure_covering(bbox, has_dock);
        }
        let frame = self.surface.begin_frame()?;
        tracing::debug!("present: begin_frame 成功");
        {
            let target = frame.target();
            // 平移变换：把虚拟屏幕坐标映射进以 `surface_rect` 原点为 (0,0) 的表面。
            // content 视觉已偏移到内容原点，二者抵消后虚拟坐标直接对应窗口坐标。
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
        self.sync_blurs(scene);
        self.device.compositor.RequestCommitAsync()?;
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
    ///
    /// `keep_large`：禁止收缩（侧边栏 Dock 存在时）。Dock 的工具提示随光标频繁
    /// 增删，若允许收缩，表面会在「Dock 小」与「Dock+工具提示大」间反复重建，
    /// 重建瞬间新表面未就绪闪一帧 → 只有侧边栏时肉眼可见的快速闪烁。禁止收缩后
    /// 表面只增不减，工具提示增删不再触发表面重建（见 `present` 的调用说明）。
    fn ensure_covering(&mut self, bbox: RectF, keep_large: bool) {
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
        let oversized = !keep_large && (cur.w * cur.h) > SHRINK_FACTOR * (want.w * want.h);
        if covers && !oversized {
            return;
        }
        let (sw, sh) = (want.w.max(1.0).ceil() as u32, want.h.max(1.0).ceil() as u32);
        let surface = match CompositionSurface::new(&self.device.gfx_device, sw, sh) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(?e, sw, sh, "重建合成表面失败，沿用旧表面");
                return;
            }
        };
        let brush = match self
            .device
            .compositor
            .CreateSurfaceBrushWithSurface(surface.raw())
        {
            Ok(b) => b,
            Err(e) => {
                tracing::warn!(?e, "重建表面刷失败，沿用旧表面");
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
        let _ = self.content.SetBrush(&brush);
        let _ = self.content.SetSize(Vector2 {
            X: sw as f32,
            Y: sh as f32,
        });
        // 表面原点（虚拟屏幕坐标）→ 窗口坐标偏移：窗口客户端 (0,0) = 虚拟 (0,0)
        //（`OverlayWindow::create` 保证），虚拟坐标即窗口坐标，直接使用。
        let _ = self.content.SetOffset(Vector3 {
            X: want.x,
            Y: want.y,
            Z: 0.0,
        });
        self.surface = surface;
        self.surface_rect = want;
    }

    /// 同步模糊视觉与 `scene.fences`：创建/更新/移除每个模糊栅栏的
    /// BackdropBrush + 高斯模糊视觉。非模糊栅栏回收视觉。
    fn sync_blurs(&mut self, scene: &Scene) {
        // 确保 vec 与栅栏对齐（不足补 None）
        while self.blurs.len() < scene.fences.len() {
            self.blurs.push(None);
        }
        for (i, f) in scene.fences.iter().enumerate() {
            if f.blur {
                self.sync_blur(i, f);
            } else if let Some(bv) = self.blurs[i].take() {
                if let Ok(children) = self.root.Children() {
                    if let Err(e) = children.Remove(&bv.visual) {
                        tracing::warn!(?e, i, "移除模糊视觉失败");
                    }
                }
            }
        }
        // 回收尾部多余视觉（栅栏数量减少时）
        while self.blurs.len() > scene.fences.len() {
            if let Some(Some(bv)) = self.blurs.pop() {
                if let Ok(children) = self.root.Children() {
                    if let Err(e) = children.Remove(&bv.visual) {
                        tracing::warn!(?e, "移除尾部模糊视觉失败");
                    }
                }
            }
        }
    }

    /// 同步单个模糊栅栏（`scene.fences[i]`，`f.blur == true`）。
    fn sync_blur(&mut self, i: usize, f: &SceneFence) {
        // 惰性创建共享效果刷（BackdropBrush + 高斯模糊）。失败→该栅栏降级为无模糊
        // （内容表面本就在模糊栅栏区透明，仅图标/标题），不影响其余栅栏。
        if self.blur_brush.is_none() {
            match self.create_blur_brush() {
                Ok(b) => self.blur_brush = Some(b),
                Err(e) => {
                    tracing::warn!(?e, "创建高斯模糊效果刷失败，Blur 栅栏降级为透明");
                    self.blurs[i] = None;
                    return;
                }
            }
        }
        let brush = self.blur_brush.as_ref().expect("上面刚赋值");
        // 窗口客户端 (0,0) = 虚拟 (0,0)：栅栏虚拟坐标直接作为窗口坐标（见 create）。
        let x = f.x;
        let y = f.y;
        let (w, h) = (f.width, f.height);
        let corner = self.theme.fence_corner_radius;
        match &mut self.blurs[i] {
            Some(bv) => {
                // 几何/透明度变化时才提交
                let changed = bv.x != x
                    || bv.y != y
                    || bv.w != w
                    || bv.h != h
                    || bv.corner != corner
                    || bv.opacity != f.alpha;
                if changed {
                    bv.x = x;
                    bv.y = y;
                    bv.w = w;
                    bv.h = h;
                    bv.corner = corner;
                    bv.opacity = f.alpha;
                    if let Err(e) = bv.visual.SetOffset(Vector3 { X: x, Y: y, Z: 0.0 }) {
                        tracing::warn!(?e, i, "更新模糊视觉偏移失败");
                    }
                    if let Err(e) = bv.visual.SetSize(Vector2 { X: w, Y: h }) {
                        tracing::warn!(?e, i, "更新模糊视觉尺寸失败");
                    }
                    if let Err(e) = bv.visual.SetOpacity(f.alpha) {
                        tracing::warn!(?e, i, "更新模糊视觉透明度失败");
                    }
                    if let Err(e) = bv.geom.SetCornerRadius(Vector2 {
                        X: corner,
                        Y: corner,
                    }) {
                        tracing::warn!(?e, i, "更新圆角半径失败");
                    }
                    // 裁剪几何偏移恒为 (0,0)（视觉本地空间），不随栅栏移动而变；
                    // 移动/缩放只更新几何尺寸与视觉自身的偏移/尺寸。
                    if let Err(e) = bv.geom.SetSize(Vector2 { X: w, Y: h }) {
                        tracing::warn!(?e, i, "更新裁剪几何尺寸失败");
                    }
                }
            }
            None => match self.create_blur_visual(brush, &RectF { x, y, w, h }, corner, f.alpha) {
                Ok(bv) => self.blurs[i] = Some(bv),
                Err(e) => tracing::warn!(?e, i, "创建模糊视觉失败，该栅栏降级为透明"),
            },
        }
    }

    /// 创建共享高斯模糊效果刷（BackdropBrush 输入端口 "Blur"）。
    fn create_blur_brush(&self) -> Result<CompositionEffectBrush> {
        let fx = GaussianBlurEffect::new(self.theme.blur_stddev)?;
        let effect: IGraphicsEffect = fx.into();
        let factory = self.device.compositor.CreateEffectFactory(&effect)?;
        let brush = factory.CreateBrush()?;
        brush.SetSourceParameter(&HSTRING::from("Blur"), &self.backdrop)?;
        Ok(brush)
    }

    /// 创建一个模糊栅栏视觉（`rect` 为窗口坐标 = `fence - origin`；裁剪几何铺满视觉本地空间）。
    fn create_blur_visual(
        &self,
        brush: &CompositionEffectBrush,
        rect: &RectF,
        corner: f32,
        opacity: f32,
    ) -> Result<BlurVisual> {
        let visual = self.device.compositor.CreateSpriteVisual()?;
        visual.SetBrush(brush)?;
        visual.SetOffset(Vector3 {
            X: rect.x,
            Y: rect.y,
            Z: 0.0,
        })?;
        visual.SetSize(Vector2 {
            X: rect.w,
            Y: rect.h,
        })?;
        visual.SetOpacity(opacity)?;
        let geom = self.device.compositor.CreateRoundedRectangleGeometry()?;
        geom.SetCornerRadius(Vector2 {
            X: corner,
            Y: corner,
        })?;
        // 裁剪几何在视觉**本地**坐标空间（Offset 相对被裁剪的视觉，非窗口坐标）：
        // 铺满整个视觉即可，(0,0) + 视觉同尺寸；放置由 visual.SetOffset 负责。
        geom.SetOffset(Vector2 { X: 0.0, Y: 0.0 })?;
        geom.SetSize(Vector2 {
            X: rect.w,
            Y: rect.h,
        })?;
        let clip = self
            .device
            .compositor
            .CreateGeometricClipWithGeometry(&geom)?;
        visual.SetClip(&clip)?;
        // 插到内容表面下方：模糊在内容之下，内容在模糊栅栏区透明透出
        self.root.Children()?.InsertBelow(&visual, &self.content)?;
        Ok(BlurVisual {
            visual,
            clip,
            geom,
            x: rect.x,
            y: rect.y,
            w: rect.w,
            h: rect.h,
            corner,
            opacity,
        })
    }

    /// 当前合成表面的尺寸（物理像素，即内容包围盒 + 余量）。
    pub fn size(&self) -> (f32, f32) {
        (self.surface.width as f32, self.surface.height as f32)
    }
}
