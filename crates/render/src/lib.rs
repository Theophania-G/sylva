//! `sylva-render`：渲染层。
//!
//! 负责桌面 overlay 窗口与 DirectComposition / Direct2D / DirectWrite 绘制：
//! - `device`：D3D11 + DXGI + DComp + D2D/DWrite 工厂（GPU 上下文）
//! - `overlay`：挂在桌面壳层下的 overlay 窗口（`SetWindowRgn` 区域穿透、
//!   标题拖动 / 角标缩放 / 双击打开交互）
//! - `surface`：DComp 合成表面（每帧 BeginDraw→D2D 绘制→EndDraw→Commit）
//! - `scene`：与应用无关的场景模型（栅栏矩形、图标、文字）
//! - `draw`：把场景画进 D2D 目标（圆角栅栏、图标位图、文字）
//! - `theme`：配色与尺寸（物理像素）
//! - `compositor`：把场景呈现到 overlay 的视觉树（上传位图 + 每帧提交）
//!
//! 坐标约定：本层全部使用**物理像素**（虚拟屏幕坐标），逻辑坐标→物理像素
//! 的 DPI 换算由 App 层完成。
//!
//! 性能约定（设计文档 §7.2）：D2D 位图/画笔按需创建并缓存；单帧只绘制脏区域
//! 更新；渲染只在内容变化时进行，空闲时 0% CPU。

#![allow(dead_code)] // 骨架阶段；随里程碑推进逐步移除

pub mod compositor;
pub mod device;
pub mod draw;
pub mod overlay;
pub mod scene;
pub mod surface;
pub mod theme;

pub use compositor::Compositor;
pub use device::RenderDevice;
pub use draw::{draw_scene, IconStore, TextFormats};
pub use overlay::{
    run_message_loop, ConsoleHit, ConsoleZone, FenceHit, HitModel, IconHit, OverlayEvent,
    OverlayWindow, RectF, ResizeZone, WidgetHit, WidgetZone, GRIP_SIZE, WM_APP_QUIT,
    WM_SYLVA_INJECT,
};
pub use scene::{
    ConsoleTab, ListColumns, Scene, SceneConsole, SceneEdit, SceneFence, SceneFenceDetail,
    SceneFenceRow, SceneIcon, SceneTab, SceneTodo, SceneTodoRow, SceneWidget, SceneWidgetRow,
};
pub use surface::{CompositionSurface, Frame};
pub use theme::Theme;
