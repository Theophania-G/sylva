//! Sylva —— 桌面栅栏整理器入口。
//!
//! 启动流程：
//! 1. 进程 DPI 感知 + COM 初始化 + 日志；
//! 2. 壳层接管：探测层级，隐藏真实图标（只隐藏 `SysListView32`，不碰其他窗口树，WE 共存）；
//! 3. GPU 上下文 + overlay 窗口 + 合成器；
//! 4. 枚举真实桌面图标，提取全部图标位图；首次运行创建演示栅栏布局；
//! 5. 按主题网格排布多栅栏并呈现；设置命中模型（`SetWindowRgn` 把窗口区域裁剪为
//!    栅栏并集，区域外点击穿透到桌面——修复全屏死区）；
//! 6. 进入消息循环：标题栏拖动栅栏、右下角缩放（高度自适应内容）、双击图标打开，
//!    变更实时重绘并持久化；Ctrl+C / Ctrl+Shift+F10 恢复真实图标并干净退出。

mod event_bus;
mod logging;

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::OnceLock;

use windows::core::BOOL;
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::System::Console::SetConsoleCtrlHandler;
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, SetProcessDPIAware};

use fence_core::config::ConfigStore;
use fence_core::model::{Desk, Fence, FenceAppearance, FenceState, FenceStyle, Icon, Rect};
use fence_render::{
    run_message_loop, Compositor, ConsoleAction, ConsoleButton, ConsoleHit, ConsoleRow, FenceHit,
    HitModel, IconHit, OverlayEvent, OverlayWindow, RectF, RenderDevice, Scene, SceneConsole,
    SceneFence, SceneIcon, Theme, GRIP_SIZE, WM_APP_QUIT,
};
use fence_shell::items::DesktopItem;
use fence_shell::takeover::DesktopHierarchy;

/// 应用数据目录名（位于 %APPDATA% 下）。
const APP_DIR: &str = "Sylva";

/// 栅栏距屏幕边缘的最小留白（物理像素）。
const FENCE_MARGIN: f32 = 8.0;

/// 栅栏最小宽度（缩放下限，物理像素）。
const MIN_FENCE_W: f32 = 200.0;

// 控制台面板布局（物理像素；几何在 `build_console` 与渲染层 `draw_console` 一致）。
const CONSOLE_W: f32 = 380.0;
const CONSOLE_TOP: f32 = 16.0;
const CONSOLE_MARGIN: f32 = 16.0;
const CONSOLE_ROWS_TOP: f32 = 78.0;
const CONSOLE_ROW_H: f32 = 30.0;
const CONSOLE_ADD_BTN_H: f32 = 26.0;
const CONSOLE_BTN_W: f32 = 84.0;

/// RAII：隐藏的真实图标在退出（含错误路径）时无条件恢复。
/// 反冲突约束的兜底——任何退出路径都不能让桌面图标永久消失。
struct IconGuard {
    hierarchy: DesktopHierarchy,
}

impl IconGuard {
    fn new(hierarchy: DesktopHierarchy) -> Self {
        hierarchy.hide_icons();
        Self { hierarchy }
    }
}

impl Drop for IconGuard {
    fn drop(&mut self) {
        self.hierarchy.restore_icons();
    }
}

/// Ctrl+C 通知主循环退出的 overlay 窗口句柄（仅信号，不做窗口访问）。
static OVERLAY_HWND: OnceLock<usize> = OnceLock::new();

/// App 运行时：领域模型 + 渲染 + 持久化的组合根。
///
/// 由 `OverlayEvent` 回调持有（`Rc<RefCell>`），事件在主线程 wnd_proc 中同步处理，
/// 无跨线程竞争。
struct Runtime {
    desk: Desk,
    items: Vec<DesktopItem>,
    /// item id → items 下标（双击打开时反查）。
    item_index: HashMap<String, usize>,
    /// item id → 已上传的位图 id。
    bitmap_ids: HashMap<String, u64>,
    compositor: Compositor,
    theme: Theme,
    vw: f32,
    vh: f32,
    store: ConfigStore,
}

/// 控制台布局：场景（绘制）+ 命中数据（点击）+ 面板矩形（窗口区域）。
struct ConsoleLayout {
    scene: SceneConsole,
    hits: Vec<ConsoleHit>,
    panel: RectF,
}

fn main() {
    // 全部使用物理像素，必须声明进程 DPI 感知
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
    // M0 桌面状态：加载/校验/回写，确保配置目录就绪
    let store = ConfigStore::new(data_dir.to_path_buf());
    let mut desk = store.load()?;
    desk.validate();
    tracing::info!(
        fences = desk.fences.len(),
        icons = desk.icons.len(),
        "桌面状态已加载"
    );

    // 1) 壳层接管：探测层级并隐藏真实图标（反冲突约束：不重挂/不销毁他人窗口）。
    //    守卫确保后续任何失败都会恢复图标。
    let hierarchy = fence_shell::takeover::probe()
        .ok_or_else(|| fence_core::CoreError::Shell("未找到桌面根窗口 Progman".into()))?;
    let _guard = IconGuard::new(hierarchy);

    // 2) GPU 上下文 + overlay + 合成器
    let device = RenderDevice::new().map_err(|e| fence_core::CoreError::Render(e.to_string()))?;
    let overlay = OverlayWindow::create(hierarchy.overlay_parent())
        .map_err(|e| fence_core::CoreError::Render(e.to_string()))?;
    let (vw, vh) = (overlay.width, overlay.height);
    tracing::info!(vw, vh, "overlay 覆盖虚拟屏幕");

    let theme = Theme::default();
    let compositor = Compositor::new(device, overlay.hwnd, vw, vh, theme.clone())
        .map_err(|e| fence_core::CoreError::Render(e.to_string()))?;

    // 3) 枚举真实桌面图标（IShellFolder 枚举，不依赖 DefView）
    let items = fence_shell::items::enumerate_desktop_items()
        .map_err(|e| fence_core::CoreError::Shell(e.to_string()))?;
    tracing::info!(count = items.len(), "桌面图标枚举完成");

    // 4) 首次运行：无栅栏布局时按稳定顺序创建演示栅栏并持久化。
    //    之后布局由用户拖动/缩放决定，栅栏是显式成员列表（无自动分类）。
    seed_fences(&mut desk, &items, &theme);
    store.save(&desk)?;

    // 图标索引 + 位图映射 + 首次上传数据（正式版放后台加载线程）
    let item_index: HashMap<String, usize> = items
        .iter()
        .enumerate()
        .map(|(i, it)| (it.id.clone(), i))
        .collect();
    let mut bitmap_ids = HashMap::new();
    let mut uploads = Vec::new();
    for (i, item) in items.iter().enumerate() {
        match fence_shell::icons::extract_icon(item, 32) {
            Ok(data) => {
                bitmap_ids.insert(item.id.clone(), i as u64);
                uploads.push((i as u64, data));
            }
            Err(e) => tracing::warn!(name = %item.display_name, "图标提取失败: {e}"),
        }
    }
    tracing::info!(loaded = uploads.len(), "图标位图提取完成");

    // 5) 初始场景 + 呈现 + 命中模型
    let mut rt = Runtime {
        desk,
        items,
        item_index,
        bitmap_ids,
        compositor,
        theme: theme.clone(),
        vw: vw as f32,
        vh: vh as f32,
        store,
    };
    let scene = build_scene(&rt.desk, &rt.theme, &rt.bitmap_ids, rt.vw, rt.vh);
    for (f, sf) in rt.desk.fences.iter_mut().zip(&scene.fences) {
        f.bounds.h = sf.height;
    }
    let upload_refs: Vec<(u64, &fence_shell::icons::IconData)> =
        uploads.iter().map(|(id, d)| (*id, d)).collect();
    rt.compositor
        .present(&scene, &upload_refs)
        .map_err(|e| fence_core::CoreError::Render(e.to_string()))?;
    let model = hit_model_from(&rt.theme, &scene, &rt.desk);

    // 6) 事件回路：App 处理交互 → 重绘 → 返回新命中模型（overlay 据此更新区域）
    let runtime = Rc::new(RefCell::new(rt));
    let runtime2 = runtime.clone();
    overlay.set_event_handler(Box::new(move |ev| {
        handle_event(&mut runtime2.borrow_mut(), ev)
    }));
    overlay.set_model(model);

    // 7) Ctrl+C：通知消息循环干净退出（图标由 _guard 在返回时恢复）
    let _ = OVERLAY_HWND.set(overlay.hwnd.0 as usize);
    unsafe {
        let _ = SetConsoleCtrlHandler(Some(ctrl_handler), true);
    }

    // 测试钩子：设置 SYLVA_AUTOSTOP_MS 后到点自动干净退出（CI/自动验证用）。
    if let Some(ms) = std::env::var("SYLVA_AUTOSTOP_MS")
        .ok()
        .and_then(|v| v.parse().ok())
    {
        let hwnd = overlay.hwnd.0 as usize; // HWND 非 Send，转 usize 跨线程
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(ms));
            unsafe {
                let _ = PostMessageW(
                    Some(HWND(hwnd as *mut core::ffi::c_void)),
                    WM_APP_QUIT,
                    WPARAM(0),
                    LPARAM(0),
                );
            }
        });
        tracing::info!(ms, "SYLVA_AUTOSTOP_MS 已设置，到点自动退出");
    }

    tracing::info!("Sylva 已就绪：标题栏拖动栅栏、右下角缩放、双击图标打开；Ctrl+Shift+F10 退出");
    run_message_loop();

    tracing::info!("已退出");
    Ok(())
}

/// Ctrl+C / 关闭终端：通知主循环退出（信号线程不做窗口访问）。
unsafe extern "system" fn ctrl_handler(_ctrl_type: u32) -> BOOL {
    if let Some(hwnd) = OVERLAY_HWND.get() {
        let _ = PostMessageW(
            Some(HWND(*hwnd as *mut core::ffi::c_void)),
            WM_APP_QUIT,
            WPARAM(0),
            LPARAM(0),
        );
    }
    BOOL(1) // 已处理，阻止默认终止行为（让主循环干净退出）
}

/// 处理一个用户交互事件：更新布局 → 重绘 → 生成新命中模型。
fn handle_event(rt: &mut Runtime, ev: OverlayEvent) -> HitModel {
    match ev {
        OverlayEvent::FenceMove { fence, pos } => {
            // 拖动标题栏：更新栅栏位置，并限制在虚拟屏幕内
            if let Some(f) = rt.desk.fences.get_mut(fence) {
                let (x, y) = pos;
                let max_x = (rt.vw - f.bounds.w - FENCE_MARGIN).max(FENCE_MARGIN);
                let max_y = (rt.vh - FENCE_MARGIN).max(FENCE_MARGIN);
                f.bounds.x = x.clamp(FENCE_MARGIN, max_x);
                f.bounds.y = y.clamp(FENCE_MARGIN, max_y);
            }
        }
        OverlayEvent::FenceResize { fence, size } => {
            // 拖动角标：只改宽度，高度由图标网格自适应（「自适应调整大小」）
            if let Some(f) = rt.desk.fences.get_mut(fence) {
                let (w, _h) = size;
                f.bounds.w = w.max(MIN_FENCE_W).min(rt.vw - FENCE_MARGIN * 2.0);
            }
        }
        OverlayEvent::IconDoubleClicked { fence, icon } => {
            // 双击栅栏内图标：按显式成员列表反查并打开
            let target = rt
                .desk
                .fences
                .get(fence)
                .and_then(|f| f.icon_ids.get(icon))
                .and_then(|id| rt.item_index.get(id))
                .and_then(|&i| rt.items.get(i));
            if let Some(item) = target {
                tracing::info!(name = %item.display_name, "打开桌面图标");
                item.launch();
            }
        }
        OverlayEvent::FenceDragEnd { .. } => {
            // 拖动结束：持久化当前布局
            if let Err(e) = rt.store.save(&rt.desk) {
                tracing::warn!("布局持久化失败: {e}");
            }
        }
        OverlayEvent::ConsoleClick { action } => match action {
            ConsoleAction::NewFence => {
                tracing::info!("新建栅栏");
                new_fence(rt);
            }
            ConsoleAction::CycleStyle { fence } => {
                if let Some(f) = rt.desk.fences.get_mut(fence) {
                    f.appearance.style = f.appearance.style.next();
                    tracing::info!(
                        fence,
                        style = %f.appearance.style.label(),
                        "切换窗口模式"
                    );
                }
            }
        },
    }

    // 重新排布 + 重绘 + 生成新命中模型（含区域）
    let scene = build_scene(&rt.desk, &rt.theme, &rt.bitmap_ids, rt.vw, rt.vh);
    for (f, sf) in rt.desk.fences.iter_mut().zip(&scene.fences) {
        f.bounds.h = sf.height;
    }
    if let Err(e) = rt.compositor.present(&scene, &[]) {
        tracing::warn!("重绘失败: {e}");
    }
    hit_model_from(&rt.theme, &scene, &rt.desk)
}

/// 新建一个栅栏：从图标最多的既有栅栏拆出最多 6 个图标，错位摆放在桌面。
/// 没有可拆分的栅栏时也放一个空栅栏（供后续摆放/缩放）。
fn new_fence(rt: &mut Runtime) {
    let src = rt
        .desk
        .fences
        .iter()
        .enumerate()
        .max_by_key(|(_, f)| f.icon_ids.len())
        .map(|(i, f)| (i, f.icon_ids.len()))
        .filter(|&(_, n)| n > 0);

    let n = rt.desk.fences.len();
    let x = 120.0 + n as f32 * 36.0;
    let y = 120.0 + n as f32 * 36.0;
    let cell = rt.theme.icon_size + rt.theme.icon_gap;
    let w = rt.theme.fence_padding * 2.0 + rt.theme.icon_cols as f32 * cell - rt.theme.icon_gap;

    let mut fence = Fence {
        id: rt.desk.next_fence_id(),
        title: Some(format!("栅栏 {}", n + 1)),
        monitor_id: 0,
        bounds: Rect::new(x, y, w, 0.0), // 高度由布局自适应
        state: FenceState::Expanded,
        icon_ids: Vec::new(),
        appearance: FenceAppearance::default(),
    };

    if let Some((idx, total)) = src {
        let take = total.min(6);
        let moved = rt.desk.fences[idx].icon_ids.split_off(total - take);
        fence.icon_ids = moved;
    }
    rt.desk.fences.push(fence);
}

/// 首次运行：把全部图标按稳定顺序平分成两个演示栅栏，并建立图标元数据。
/// 已有布局时不作任何改动（栅栏是用户显式成员列表）。
fn seed_fences(desk: &mut Desk, items: &[DesktopItem], theme: &Theme) {
    if !desk.fences.is_empty() || items.is_empty() {
        return;
    }
    // 元数据落库：图标按稳定 id 持久化，供渲染标签与成员引用
    for it in items {
        desk.icons.entry(it.id.clone()).or_insert(Icon {
            id: it.id.clone(),
            display_name: it.display_name.clone(),
            kind: it.kind,
        });
    }

    let cell = theme.icon_size + theme.icon_gap;
    let w = theme.fence_padding * 2.0 + theme.icon_cols as f32 * cell - theme.icon_gap;
    let empty: HashMap<String, u64> = HashMap::new();
    let (a, b) = items.split_at(items.len().div_ceil(2));

    let f1 = Fence {
        id: desk.next_fence_id(),
        title: Some("常用".into()),
        monitor_id: 0,
        bounds: Rect::new(80.0, 80.0, w, 0.0), // 高度由布局自适应
        state: FenceState::Expanded,
        icon_ids: a.iter().map(|it| it.id.clone()).collect(),
        appearance: FenceAppearance::default(),
    };
    // 用布局算出的高度定位第二个栅栏（首栅栏暂不在 desk 中，单独算一次几何）
    let sf1 = layout_fence(theme, &f1, desk, &empty);

    // 第二个栅栏用「描边」模式演示多窗口模式
    let app2 = FenceAppearance {
        style: FenceStyle::Outline,
        ..FenceAppearance::default()
    };
    let f2 = Fence {
        id: desk.next_fence_id(),
        title: Some("其他".into()),
        monitor_id: 0,
        bounds: Rect::new(80.0, 80.0 + sf1.height + 40.0, w, 0.0),
        state: FenceState::Expanded,
        icon_ids: b.iter().map(|it| it.id.clone()).collect(),
        appearance: app2,
    };

    desk.fences.push(f1);
    desk.fences.push(f2);
    tracing::info!(
        fences = desk.fences.len(),
        icons = desk.icons.len(),
        "首次运行：已创建演示栅栏布局"
    );
}

/// 把整个桌面状态排布成场景（每个栅栏独立网格排布，高度自适应内容）。
fn build_scene(
    desk: &Desk,
    theme: &Theme,
    bitmap_ids: &HashMap<String, u64>,
    vw: f32,
    vh: f32,
) -> Scene {
    let mut scene = Scene::new(vw, vh);
    for fence in &desk.fences {
        scene
            .fences
            .push(layout_fence(theme, fence, desk, bitmap_ids));
    }
    scene.console = Some(build_console(desk, vw).scene);
    scene
}

/// 构建 Sylva 控制台布局：场景（绘制）+ 按钮命中数据 + 面板矩形。
///
/// 功能入口集中于此：新建栅栏、切换每个栅栏的窗口模式。
fn build_console(desk: &Desk, vw: f32) -> ConsoleLayout {
    let w = CONSOLE_W;
    let h = CONSOLE_ROWS_TOP + desk.fences.len() as f32 * CONSOLE_ROW_H + 12.0;
    let x = (vw - w - CONSOLE_MARGIN).max(8.0);
    let y = CONSOLE_TOP;
    let panel = RectF { x, y, w, h };

    let add_btn = ConsoleButton {
        x: x + 12.0,
        y: y + 40.0,
        w: 140.0,
        h: CONSOLE_ADD_BTN_H,
        label: "＋ 新建栅栏".into(),
    };
    let mut hits = vec![ConsoleHit {
        rect: RectF {
            x: add_btn.x,
            y: add_btn.y,
            w: add_btn.w,
            h: add_btn.h,
        },
        action: ConsoleAction::NewFence,
    }];

    let mut rows = Vec::with_capacity(desk.fences.len());
    for (i, f) in desk.fences.iter().enumerate() {
        let ry = y + CONSOLE_ROWS_TOP + i as f32 * CONSOLE_ROW_H;
        let btn = ConsoleButton {
            x: x + w - CONSOLE_BTN_W - 12.0,
            y: ry + 2.0,
            w: CONSOLE_BTN_W,
            h: 24.0,
            label: "切换样式".into(),
        };
        hits.push(ConsoleHit {
            rect: RectF {
                x: btn.x,
                y: btn.y,
                w: btn.w,
                h: btn.h,
            },
            action: ConsoleAction::CycleStyle { fence: i },
        });
        rows.push(ConsoleRow {
            label: f.title.clone().unwrap_or_else(|| format!("栅栏 {}", i + 1)),
            label_rect: RectF {
                x: x + 12.0,
                y: ry + 4.0,
                w: 120.0,
                h: 20.0,
            },
            mode_label: format!("模式：{}", f.appearance.style.label()),
            mode_rect: RectF {
                x: x + 150.0,
                y: ry + 4.0,
                w: 110.0,
                h: 20.0,
            },
            mode_btn: btn,
        });
    }

    let scene = SceneConsole {
        x,
        y,
        w,
        h,
        title: "Sylva 控制台".into(),
        add_btn,
        rows,
    };
    ConsoleLayout { scene, hits, panel }
}

/// 单个栅栏的网格排布。
///
/// - 宽度：用户控制的 `fence.bounds.w`，决定列数；
/// - 高度：由图标行数自适应（`自适应调整大小`），内容变多自动变高；
/// - 图标：自左向右、自上而下排布。
fn layout_fence(
    theme: &Theme,
    fence: &Fence,
    desk: &Desk,
    bitmap_ids: &HashMap<String, u64>,
) -> SceneFence {
    let app = &fence.appearance;
    let cell = app.icon_size + app.gap;
    let inner_w = (fence.bounds.w - 2.0 * app.padding).max(1.0);
    let cols = ((inner_w / cell).floor() as usize).max(1);
    let n = fence.icon_ids.len();
    let rows = n.div_ceil(cols).max(1);
    let title_block_h = theme.title.size * 1.6 + theme.title_padding_bottom;
    let rows_h = app.icon_size * rows as f32 + app.gap * (rows as f32 - 1.0).max(0.0);
    let height = app.padding
        + title_block_h
        + app.padding
        + rows_h
        + theme.icon_caption_gap
        + theme.label.size * 1.6
        + app.padding;

    let mut icons = Vec::with_capacity(n);
    for (i, id) in fence.icon_ids.iter().enumerate() {
        let label = desk
            .icons
            .get(id)
            .map(|ic| ic.display_name.as_str())
            .unwrap_or("");
        icons.push(SceneIcon {
            label: label.to_string(),
            bitmap_id: bitmap_ids.get(id).copied().unwrap_or(u64::MAX), // 无位图时跳过绘制
            x: fence.bounds.x + app.padding + (i % cols) as f32 * cell,
            y: fence.bounds.y
                + app.padding
                + title_block_h
                + app.padding
                + (i / cols) as f32 * cell,
            size: app.icon_size,
        });
    }

    // 窗口模式 → 填充 / 描边。描边模式内部完全透明，仅画中粗圆角线。
    let theme_border = theme.fence_border;
    let (fill_color, border_color, border_width) = match app.style {
        FenceStyle::Filled => (
            Some(app.bg_color),
            [
                theme_border.r,
                theme_border.g,
                theme_border.b,
                theme_border.a,
            ],
            app.border_width,
        ),
        FenceStyle::Outline => (None, [1.0, 1.0, 1.0, 0.62], 2.5),
        FenceStyle::Glass => (
            Some([0.13, 0.15, 0.19, 0.16]),
            [
                theme_border.r,
                theme_border.g,
                theme_border.b,
                theme_border.a,
            ],
            app.border_width,
        ),
    };

    SceneFence {
        x: fence.bounds.x,
        y: fence.bounds.y,
        width: fence.bounds.w,
        height,
        title: fence.title.clone().unwrap_or_default(),
        icons,
        border_width,
        border_color,
        fill_color,
    }
}

/// 由场景几何生成命中模型：栅栏（标题移动把手 + 右下角缩放把手 + 整体区域）、
/// 图标（双击打开）与控制台按钮（点击触发）。`fence`/`icon` 下标与
/// `desk.fences` / `icon_ids` 对应。
fn hit_model_from(theme: &Theme, scene: &Scene, desk: &Desk) -> HitModel {
    let mut fences = Vec::with_capacity(scene.fences.len());
    let mut icons = Vec::new();
    for (fi, f) in scene.fences.iter().enumerate() {
        let body = RectF {
            x: f.x,
            y: f.y,
            w: f.width,
            h: f.height,
        };
        let title_h =
            (theme.title.size * 1.6 + theme.title_padding_bottom + 2.0 * theme.fence_padding)
                .min(f.height);
        fences.push(FenceHit {
            body,
            title: RectF {
                x: f.x,
                y: f.y,
                w: f.width,
                h: title_h,
            },
            grip: RectF {
                x: f.x + f.width - GRIP_SIZE,
                y: f.y + f.height - GRIP_SIZE,
                w: GRIP_SIZE,
                h: GRIP_SIZE,
            },
            id: fi,
        });
        for (ii, icon) in f.icons.iter().enumerate() {
            icons.push(IconHit {
                rect: RectF {
                    x: icon.x,
                    y: icon.y,
                    w: icon.size,
                    h: icon.size,
                },
                fence: fi,
                icon: ii,
            });
        }
    }
    let console = build_console(desk, scene.width);
    HitModel {
        fences,
        icons,
        console_panel: Some(console.panel),
        console: console.hits,
    }
}
