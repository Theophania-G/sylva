//! 桌面栅栏整理器 —— 入口。
//!
//! M1.5：接管桌面壳层并呈现第一个栅栏（真实图标枚举 + 图标位图 + DComp 渲染）。
//!
//! 启动流程：
//! 1. 进程 DPI 感知 + COM 初始化 + 日志；
//! 2. 壳层接管：探测层级，隐藏真实图标（只隐藏 `SysListView32`，不碰其他窗口树，WE 共存）；
//! 3. GPU 上下文 + overlay 窗口 + 合成器；
//! 4. 枚举真实桌面图标，提取前 N 个图标位图（M1.5 阻塞式；正式版走后台加载线程）；
//! 5. 网格排布一个栅栏并呈现；设置命中区（栅栏内可交互、栅栏外点击穿透）；
//! 6. 进入消息循环；Ctrl+C 时恢复真实图标并干净退出。

mod event_bus;
mod logging;

use std::path::PathBuf;
use std::sync::OnceLock;

use windows::core::BOOL;
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::System::Console::SetConsoleCtrlHandler;
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, SetProcessDPIAware};

use fence_core::config::ConfigStore;
use fence_render::{
    run_message_loop, Compositor, HitRect, OverlayWindow, RenderDevice, Scene, SceneFence,
    SceneIcon, Theme, WM_APP_QUIT,
};
use fence_shell::takeover::DesktopHierarchy;

/// 应用数据目录名（位于 %APPDATA% 下）。
const APP_DIR: &str = "FenceOrganizer";

/// 首个栅栏展示的图标数量上限（演示用；正式版走完整布局）。
const DEMO_ICON_LIMIT: usize = 15;

/// 栅栏演示的固定位置（物理像素）。
const FENCE_POS: (f32, f32) = (120.0, 90.0);

/// Ctrl+C 恢复现场所需的运行时上下文。
struct ShutdownCtx {
    hierarchy: DesktopHierarchy,
    overlay: HWND,
}

// 仅存原始窗口句柄；它们指向与进程同生命周期的窗口，跨线程读取安全
//（Ctrl+C 处理线程只做恢复/通知，不做任何窗口内存访问）。
unsafe impl Send for ShutdownCtx {}
unsafe impl Sync for ShutdownCtx {}

static RUNTIME: OnceLock<ShutdownCtx> = OnceLock::new();

fn main() {
    // M1.5 起全部使用物理像素，必须声明进程 DPI 感知
    unsafe {
        let _ = SetProcessDPIAware();
    }
    // COM：图标枚举/提取需要（APARTMENTTHREADED）
    let _com = fence_shell::com::init();

    let appdata = std::env::var("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."));
    let data_dir = appdata.join(APP_DIR);

    let _guard = match logging::init(&data_dir.join("logs")) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("日志初始化失败: {e}");
            return;
        }
    };

    if let Err(e) = run(&data_dir) {
        tracing::error!("启动失败: {e:?}");
    }
}

fn run(data_dir: &std::path::Path) -> fence_core::Result<()> {
    // M0 桌面状态：加载/校验/回写，确保配置目录就绪（正式版从这里读栅栏布局）
    let store = ConfigStore::new(data_dir.to_path_buf());
    let mut desk = store.load()?;
    desk.validate();
    store.save(&desk)?;
    tracing::info!(
        fences = desk.fences.len(),
        icons = desk.icons.len(),
        "桌面状态已加载"
    );

    // 1) 壳层接管：探测层级并隐藏真实图标（反冲突约束：不重挂/不销毁他人窗口）
    let hierarchy = fence_shell::takeover::probe()
        .ok_or_else(|| fence_core::CoreError::Shell("未找到桌面根窗口 Progman".into()))?;
    let hidden = hierarchy.hide_icons();
    tracing::info!(hidden, "桌面壳层接管完成");

    // 2) GPU 上下文 + overlay + 合成器
    let device = RenderDevice::new().map_err(|e| fence_core::CoreError::Render(e.to_string()))?;
    let overlay = OverlayWindow::create(hierarchy.overlay_parent())
        .map_err(|e| fence_core::CoreError::Render(e.to_string()))?;
    let (vw, vh) = (overlay.width, overlay.height);
    tracing::info!(vw, vh, "overlay 覆盖虚拟屏幕");

    let theme = Theme::default();
    let mut compositor = Compositor::new(device, overlay.hwnd, vw, vh, theme.clone())
        .map_err(|e| fence_core::CoreError::Render(e.to_string()))?;

    // 3) 枚举真实桌面图标（IShellFolder 枚举，不依赖 DefView）
    let items = fence_shell::items::enumerate_desktop_items()
        .map_err(|e| fence_core::CoreError::Shell(e.to_string()))?;
    tracing::info!(count = items.len(), "桌面图标枚举完成");

    // 4) 提取前 N 个图标位图（M1.5 阻塞式演示；正式版放后台加载线程）
    let mut uploads = Vec::new();
    for (i, item) in items.iter().take(DEMO_ICON_LIMIT).enumerate() {
        match fence_shell::icons::extract_icon(item, 32) {
            Ok(data) => uploads.push((i as u64, data)),
            Err(e) => tracing::warn!(name = %item.display_name, "图标提取失败: {e}"),
        }
    }
    tracing::info!(loaded = uploads.len(), "图标位图提取完成");

    // 5) 网格排布一个栅栏（标题区在上，图标自左向右、自上而下）
    let (ox, oy) = FENCE_POS;
    let cols = 5_usize.min(uploads.len().max(1));
    let rows = uploads.len().div_ceil(cols).max(1);
    let cell = theme.icon_size + theme.icon_gap;
    let title_block_h = theme.title.size * 1.6 + theme.title_padding_bottom;
    let rows_block_h =
        theme.icon_size * rows as f32 + theme.icon_gap * (rows as f32 - 1.0).max(0.0);
    let fence_w = theme.fence_padding * 2.0 + cols as f32 * cell - theme.icon_gap;
    let fence_h = theme.fence_padding * 2.0
        + title_block_h
        + rows_block_h
        + theme.label.size * 1.6
        + theme.icon_caption_gap;

    let mut scene_icons = Vec::with_capacity(uploads.len());
    for (idx, &(bitmap_id, _)) in uploads.iter().enumerate() {
        let (row, col) = (idx / cols, idx % cols);
        scene_icons.push(SceneIcon {
            label: items[bitmap_id as usize].display_name.clone(),
            bitmap_id,
            x: ox + theme.fence_padding + col as f32 * cell,
            y: oy + theme.fence_padding + title_block_h + row as f32 * cell,
            size: theme.icon_size,
        });
    }

    let mut scene = Scene::new(vw as f32, vh as f32);
    scene.fences.push(SceneFence {
        x: ox,
        y: oy,
        width: fence_w,
        height: fence_h,
        title: "我的栅栏".into(),
        icons: scene_icons,
    });

    // 6) 呈现 + 命中区（栅栏内可交互，栅栏外点击穿透到桌面）
    let upload_refs: Vec<(u64, &fence_shell::icons::IconData)> =
        uploads.iter().map(|(id, d)| (*id, d)).collect();
    compositor
        .present(&scene, &upload_refs)
        .map_err(|e| fence_core::CoreError::Render(e.to_string()))?;
    overlay.set_hit_rects(&[HitRect {
        x: ox,
        y: oy,
        width: fence_w,
        height: fence_h,
    }]);

    // 7) Ctrl+C：恢复真实图标并让消息循环干净退出
    let _ = RUNTIME.set(ShutdownCtx {
        hierarchy,
        overlay: overlay.hwnd,
    });
    unsafe {
        let _ = SetConsoleCtrlHandler(Some(ctrl_handler), true);
    }

    tracing::info!("首个栅栏已呈现；Ctrl+C 退出并恢复桌面图标");
    run_message_loop();

    tracing::info!("已退出");
    Ok(())
}

/// Ctrl+C / 关闭终端：恢复隐藏的真实图标，并通知主循环退出。
unsafe extern "system" fn ctrl_handler(_ctrl_type: u32) -> BOOL {
    if let Some(ctx) = RUNTIME.get() {
        ctx.hierarchy.restore_icons();
        let _ = PostMessageW(Some(ctx.overlay), WM_APP_QUIT, WPARAM(0), LPARAM(0));
    }
    BOOL(1) // 已处理，阻止默认终止行为（让主循环干净退出）
}
