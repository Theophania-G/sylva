//! `sylva-render`：渲染层。
//!
//! 负责桌面 overlay 窗口与 WinRT `Windows.UI.Composition` / Direct2D / DirectWrite
//! 绘制：
//! - `device`：D3D11 + DXGI + D2D/DWrite 工厂 + WinRT 合成器（GPU 上下文）
//! - `overlay`：挂在桌面壳层下的 overlay 窗口（`SetWindowRgn` 区域穿透、
//!   标题拖动 / 角标缩放 / 双击打开交互）
//! - `surface`：合成绘制表面（每帧 BeginDraw→D2D 绘制→EndDraw）
//! - `effects`：高斯模糊效果 shim（`IGraphicsEffectD2D1Interop`，供 BackdropBrush
//!   实现实时模糊）
//! - `scene`：与应用无关的场景模型（栅栏矩形、图标、文字）
//! - `draw`：把场景画进 D2D 目标（圆角栅栏、图标位图、文字）
//! - `theme`：配色与尺寸（物理像素）
//! - `compositor`：把场景呈现到 overlay 的 WinRT 视觉树（上传位图 + 每帧提交 +
//!   每栅栏实时模糊视觉）
//!
//! 坐标约定：本层全部使用**物理像素**（虚拟屏幕坐标），逻辑坐标→物理像素
//! 的 DPI 换算由 App 层完成。
//!
//! 性能约定（设计文档 §7.2）：D2D 位图/画笔按需创建并缓存；渲染只在内容变化时
//! 进行，空闲时 0% CPU。模糊由 DWM GPU 实时合成（BackdropBrush），无截图、无
//! CPU 高斯、无刷新定时器。

pub mod compositor;
pub mod device;
pub mod draw;
pub mod effects;
pub mod overlay;
pub mod scene;
pub mod surface;
pub mod theme;

pub use compositor::Compositor;
pub use device::RenderDevice;
pub use draw::{draw_scene, IconStore, TextFormats};
pub use overlay::{
    run_message_loop, ConsoleHit, ConsoleZone, FenceHit, HitModel, IconHit, OverlayEvent,
    OverlayWindow, RectF, ResizeZone, GRIP_SIZE, WM_APP_QUIT, WM_SYLVA_INJECT,
};
pub use scene::{
    ListColumns, Scene, SceneConsole, SceneEdit, SceneFence, SceneFenceDetail, SceneFenceRow,
    SceneIcon,
};
pub use surface::{CompositionSurface, Frame};
pub use theme::Theme;
