//! Sylva —— 桌面栅栏整理器入口。
//!
// 发布版不弹终端窗口（双击直接运行，关掉启动它的 cmd 不影响本进程）；
// 调试版保留控制台便于看日志。
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
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

mod anim;
mod context_menu;
mod editing;
mod file_ops;
mod logging;
mod memory;
mod scene;
mod shell_menu;

pub(crate) use std::cell::RefCell;
pub(crate) use std::collections::HashMap;
pub(crate) use std::path::{Path, PathBuf};
pub(crate) use std::rc::Rc;
pub(crate) use std::sync::OnceLock;
pub(crate) use std::time::Instant;

pub(crate) use windows::core::{BOOL, PCWSTR, PWSTR};
pub(crate) use windows::Win32::Foundation::{
    GetLastError, ERROR_ALREADY_EXISTS, HANDLE, HGLOBAL, HWND, LPARAM, POINT, RECT, WPARAM,
};
pub(crate) use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};
pub(crate) use windows::Win32::System::Console::SetConsoleCtrlHandler;
pub(crate) use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, OpenClipboard, SetClipboardData,
};
pub(crate) use windows::Win32::System::Ole::OleInitialize;
pub(crate) use windows::Win32::System::Memory::{
    GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock, GMEM_MOVEABLE,
};
pub(crate) use windows::Win32::System::Threading::CreateMutexW;
pub(crate) use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetDpiForWindow, SetProcessDpiAwarenessContext,
};
pub(crate) use windows::Win32::UI::Input::Ime::{
    ImmGetContext, ImmReleaseContext, ImmSetCompositionWindow, CFS_POINT, COMPOSITIONFORM,
};
pub(crate) use windows::Win32::UI::Input::KeyboardAndMouse::{
    VK_BACK, VK_DELETE, VK_DOWN, VK_END, VK_ESCAPE, VK_HOME, VK_LEFT, VK_RETURN, VK_RIGHT, VK_UP,
};
pub(crate) use windows::Win32::UI::Shell::{
    DragQueryFileW, FileOpenDialog, IFileOpenDialog, IShellItem, IShellItemArray,
    FOS_ALLOWMULTISELECT, FOS_FORCEFILESYSTEM, FOS_PICKFOLDERS, HDROP, SIGDN_FILESYSPATH,
};
pub(crate) use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, DestroyMenu, GetCursorPos, GetSystemMetrics, PostMessageW,
    SetProcessDPIAware, SystemParametersInfoW, TrackPopupMenu, HMENU,
    MF_SEPARATOR, MF_STRING, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN,
    SM_YVIRTUALSCREEN, SPI_GETWORKAREA, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, TPM_NONOTIFY,
    TPM_RETURNCMD,
};

pub(crate) use sylva_core::config::ConfigStore;
pub(crate) use sylva_core::magnet::{settle_move, settle_resize, FreeSides, FENCE_GAP};
pub(crate) use sylva_core::model::{
    Desk, Fence, FenceAppearance, FenceLayout, FenceState, FenceStyle, Icon, Rect, SidebarPosition,
    Vec2,
};
pub(crate) use sylva_render::{
    run_message_loop, Compositor, ConsoleHit, ConsoleZone, FenceHit, HitModel, IconHit,
    ListColumns, OverlayEvent, OverlayWindow, RectF, RenderDevice, ResizeZone, Scene, SceneConsole,
    SceneEdit, SceneFence, SceneFenceDetail, SceneFenceRow, SceneIcon, Theme, GRID_CAPTION_H_MULT,
    GRIP_SIZE, WM_APP_QUIT, WM_SYLVA_INJECT,
};
pub(crate) use sylva_shell::icons::IconData;
pub(crate) use sylva_shell::items::DesktopItem;
pub(crate) use sylva_shell::takeover::DesktopHierarchy;

use crate::anim::*;
use crate::context_menu::*;
use crate::editing::*;
use crate::file_ops::*;
use crate::scene::*;

/// 栅栏最小宽度（缩放下限，物理像素）。
pub(crate) const MIN_FENCE_W: f32 = 200.0;

/// 栅栏最小高度（缩放下限，物理像素）。
pub(crate) const MIN_FENCE_H: f32 = 60.0;

/// 图标提取边长（物理像素）。取高于所有渲染尺寸的值（最大图标 64、列表 20），
/// 渲染时向下采样才清晰；若按渲染尺寸 32 提取再放大到 48，高 DPI 下发糊。
pub(crate) const ICON_EXTRACT_SIZE: u32 = 64;

// 右键菜单项 ID（分段避免冲突）。

/// 栅栏边框宽度（DIP）：中粗，按用户要求固定（× scale 变物理像素）。
pub(crate) const MEDIUM_BORDER_WIDTH: f32 = 2.0;

// ---- 控制中心（桌面组件 + 栅栏管理）布局常量（DIP，× scale 变物理像素）----
/// 控制台面板默认宽度（用户拖边缘缩放后由 `desk.console_size` 覆盖）。
pub(crate) const CONSOLE_W: f32 = 320.0;
/// 控制台面板最小宽/高（缩放钳制，避免缩到无法交互）。
pub(crate) const CONSOLE_MIN_W: f32 = 260.0;
pub(crate) const CONSOLE_MIN_H: f32 = 170.0;
/// 控制台与屏幕右/上边距（未拖动时的默认摆放位置）。
pub(crate) const CONSOLE_MARGIN: f32 = 24.0;
/// 标题栏高度（拖动把手；关闭按钮位于其中）。
pub(crate) const CONSOLE_TITLE_H: f32 = 40.0;
/// 关闭按钮边长。
pub(crate) const CONSOLE_CLOSE_W: f32 = 32.0;
/// 待办列表距面板左右的内边距。
pub(crate) const CONSOLE_PAD: f32 = 12.0;
/// 折叠胶囊高度（始终可见的控制中心入口）。
pub(crate) const CONSOLE_PILL_H: f32 = 34.0;
/// 组件页：添加按钮高度。
pub(crate) const CONSOLE_ADD_BTN_H: f32 = 34.0;
/// 栅栏管理页：每行高度。
pub(crate) const CONSOLE_FENCE_ROW_H: f32 = 36.0;
/// 栅栏管理页：最多同时显示的行数（超出滚动）。
pub(crate) const CONSOLE_FENCE_MAX_ROWS: usize = 5;
/// 栅栏管理页：选中栅栏详情区高度。
pub(crate) const CONSOLE_FENCE_DETAIL_H: f32 = 218.0;
/// 控制台展开面板最大高度（DIP；内容再多也滚动）。
pub(crate) const CONSOLE_MAX_H: f32 = 640.0;

/// 背景色调预设（标签, RGB 0..1）：菜单项顺序即此处顺序（+1 起）。
pub(crate) const TINT_PRESETS: &[(&str, [f32; 3])] = &[
    ("蓝", [0.32, 0.55, 0.95]),
    ("青", [0.30, 0.80, 0.85]),
    ("绿", [0.40, 0.75, 0.45]),
    ("黄", [0.95, 0.85, 0.40]),
    ("橙", [0.98, 0.62, 0.30]),
    ("红", [0.92, 0.35, 0.35]),
    ("紫", [0.66, 0.45, 0.90]),
    ("白", [0.92, 0.93, 0.96]),
    ("灰", [0.56, 0.58, 0.62]),
];

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
pub(crate) struct Runtime {
    pub(crate) desk: Desk,
    pub(crate) items: Vec<DesktopItem>,
    /// item id → items 下标（双击打开时反查）。
    pub(crate) item_index: HashMap<String, usize>,
    /// item id → 已上传的位图 id。
    pub(crate) bitmap_ids: HashMap<String, u64>,
    pub(crate) compositor: Compositor,
    pub(crate) theme: Theme,
    pub(crate) vw: f32,
    pub(crate) vh: f32,
    /// overlay 窗口在屏幕上的位置 = 虚拟屏原点（客户端 (0,0) = 虚拟 (0,0)）。
    /// 多屏时副屏在左/上可为负；主显示器工作区（`SPI_GETWORKAREA` 为屏幕坐标）
    /// 转虚拟坐标需减去它。显示拓扑变化时随 `DisplayChange` 更新。
    pub(crate) origin: (f32, f32),
    pub(crate) store: ConfigStore,
    /// 当前悬停的图标（栅栏下标, 图标下标）；None = 无悬停。
    pub(crate) hover: Option<(usize, usize)>,
    /// 最近一次光标位置（虚拟屏幕物理坐标）；侧边栏 Dock 放大据此连续计算。
    pub(crate) cursor: Option<(f32, f32)>,
    /// 当前选中的图标集合（框选 / Ctrl 单击多选，如资源管理器）。空 = 无选中。
    pub(crate) selected: Vec<(usize, usize)>,
    /// 框选橡皮筋矩形（所属栅栏下标 + 物理像素矩形）；None = 未在框选。
    pub(crate) select_band: Option<(usize, RectF)>,
    /// overlay 窗口句柄（右键菜单 owner / 就地编辑框父窗口）。
    pub(crate) hwnd: HWND,
    /// 当前激活的 D2D 内联文本编辑（None = 无编辑；键盘/IME 事件直达）。
    pub(crate) edit: Option<InlineEdit>,
    /// 上次 WM_CHAR 的高位代理（emoji 等增补平面字符由两个 WM_CHAR 组成）。
    pub(crate) edit_high: Option<u16>,
    /// 控制台面板动画状态（`AnimTick` 推进；idle 时停止定时器保持 0% CPU）。
    pub(crate) console_anim: ConsoleAnim,
    /// 桌面切换时栅栏整体淡出/淡入补间（None = 无动画，按 `desk.desktop_mode` 取最终值）。
    pub(crate) desktop_fade: Option<PanelTween>,
    /// 栅栏管理页当前选中的栅栏下标。
    pub(crate) selected_fence: usize,
    /// 当前悬停的控制台控件（绘制高亮反馈用）。
    pub(crate) console_hover: Option<ConsoleZone>,
    /// 栅栏管理页列表滚动偏移（物理像素）。
    pub(crate) fence_scroll: f32,
    /// 各栅栏最近一次实际渲染高度（物理像素，`build_scene` 每帧回写）。
    /// 自动高度栅栏 `bounds.h == 0`，碰撞检测/夹屏需要真实高度入算；下标与 `fences` 对齐。
    pub(crate) last_layout_h: Vec<f32>,
    /// 栅栏拖动/缩放补间（多个栅栏可同时动；结束自动移除）。
    pub(crate) fence_tweens: Vec<FenceTween>,
    /// 图标悬停放大补间（一次只有一个悬停图标）。
    pub(crate) icon_hover: Option<IconHoverAnim>,
    /// 桌面层级（保留句柄副本；「切换桌面」时反复隐藏/恢复真实图标）。
    pub(crate) hierarchy: DesktopHierarchy,
    /// overlay 窗口原始指针（动画定时器的启停需要访问它；与进程存活期一致）。
    pub(crate) overlay_ptr: *mut OverlayWindow,
    /// 内部库文件夹（软件目录下）：粘贴/拖入的文件先物理复制进来，栅栏索引库内副本。
    pub(crate) library: PathBuf,
    /// 本次事件中新添加图标的位图（`handle_event` 末尾随场景一起上传）。
    pub(crate) pending_uploads: Vec<(u64, IconData)>,
    /// 最近一次用户交互时间（空闲时修剪工作集用；后台 `SyncLibrary` 不计）。
    pub(crate) last_activity: std::time::Instant,
    /// 最近一次工作集修剪时间（限频：空闲时最多每 60s 一次）。
    pub(crate) last_trim: std::time::Instant,
    /// 侧边栏图标拖动排序状态：Some((fence_idx, icon_idx, cursor_x, cursor_y))。
    pub(crate) sidebar_reorder: Option<(usize, usize, f32, f32)>,
    /// 拖动排序时的图标顺序（fence_idx → Vec<usize>），由 build_scene 每帧计算。
    pub(crate) reorder_slot_order: std::collections::HashMap<usize, Vec<usize>>,
    /// 当前帧各图标渲染位置（fence_idx → Vec<(x, y)>），每帧更新，用于 lerp 动画。
    pub(crate) current_icon_positions: std::collections::HashMap<usize, Vec<(f32, f32)>>,
}

// 事件处理器再入守卫：`handle_event` 打开模态菜单/属性页（`TrackPopupMenu`、Shell 动词
// 的对话框）期间，嵌套消息循环会派发定时器、悬停、注入等其它事件再入回调。外层仍持有
// `Runtime` 可变借用，再入必然 RefCell 借用冲突崩溃；守卫置位后，再入回调直接丢弃事件。
// `ReentryGuard` 在回调结束时（含 panic）自动复位，避免标志位残留卡死后续事件。
thread_local! {
    static HANDLING: RefCell<bool> = const { RefCell::new(false) };
}

struct ReentryGuard;

impl Drop for ReentryGuard {
    fn drop(&mut self) {
        HANDLING.with(|h| *h.borrow_mut() = false);
    }
}

fn main() {
    // 单实例锁：防止重复启动导致多个全屏 overlay 叠层拦截输入（历史故障根因）。
    // 互斥名不带命名空间前缀 = 当前登录会话命名空间，无需管理员特权。
    // 句柄 `_mutex` 保持到 main 退出（进程生命周期），期间重复启动会立即在此退出。
    let name: Vec<u16> = "Sylva.Desktop.Fences"
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let _mutex = match unsafe { CreateMutexW(None, false, PCWSTR(name.as_ptr())) } {
        Ok(h) => h,
        Err(e) => {
            eprintln!("单实例互斥创建失败: {e}");
            return;
        }
    };
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        eprintln!("Sylva 已在运行，本次启动退出（单实例）");
        return;
    }

    // 全部使用物理像素，必须声明进程 DPI 感知。
    // 首选 Per-Monitor v2：窗口按真实物理像素渲染，DWM 不再对整窗位图缩放
    // （消除高缩放下「整窗发糊 + 拖拽/动画重采样与合成竞争 → 闪烁」），
    // 并支持 WM_DPICHANGED 实时重排。旧系统不支持时回退系统感知。
    unsafe {
        let pmv2 = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        if pmv2.is_err() {
            // Win8.1/部分环境不支持 PerMonitorV2：降级为系统感知（GetDpiForSystem 仍可用）。
            let _ = SetProcessDPIAware();
        }
    }
    // COM：图标枚举/提取需要（APARTMENTTHREADED）
    let _com = sylva_shell::com::init();
    // OLE 剪贴板：Shell 右键菜单的复制/剪切/粘贴依赖 OleInitialize
    // （IContextMenu::InvokeCommand 内部调用 OleSetClipboard，未初始化 OLE 时静默失败）
    let _ole = unsafe { OleInitialize(None) };

    // 数据目录：exe 同级的 data 文件夹（与安装器约定一致——安装器在 <安装目录>/data 创建
    // 空目录，卸载时随安装目录一并删除；不再用 %APPDATA%，否则卸载不保留数据也清不掉）。
    let data_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|p| p.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."))
        .join("data");
    // 从旧位置（%APPDATA%\Sylva）迁移：旧版数据不在 exe 同级时搬过来，一次完成。
    if !data_dir.join("desk.json").exists() {
        if let Ok(appdata) = std::env::var("APPDATA") {
            let old_dir = PathBuf::from(appdata).join("Sylva");
            if old_dir.join("desk.json").exists() {
                let _ = std::fs::create_dir_all(&data_dir);
                // 逐文件迁移（跨卷 rename 会失败，fallback 到 copy）
                for entry in std::fs::read_dir(&old_dir).into_iter().flatten().flatten() {
                    let src = entry.path();
                    let dst = data_dir.join(entry.file_name());
                    if std::fs::rename(&src, &dst).is_err() {
                        let _ = std::fs::copy(&src, &dst);
                        let _ = std::fs::remove_file(&src);
                    }
                }
                tracing::info!("已从 {} 迁移数据到 {}", old_dir.display(), data_dir.display());
            }
        }
    }

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

fn run(data_dir: &std::path::Path) -> sylva_core::Result<()> {
    // M0 桌面状态：加载/校验/回写，确保配置目录就绪
    let store = ConfigStore::new(data_dir.to_path_buf());
    let mut desk = store.load()?;
    desk.validate();
    // 内部库文件夹（软件目录下）：粘贴/拖入的文件先物理复制进来，栅栏索引库内副本。
    // 库内文件被外部删除时，栅栏对应项同步移除（定时器 `SyncLibrary` 与启动时各清一次）。
    let library_dir = data_dir.join("library");
    let _ = std::fs::create_dir_all(&library_dir);
    tracing::info!(
        fences = desk.fences.len(),
        icons = desk.icons.len(),
        "桌面状态已加载"
    );

    // 1) 壳层接管：探测层级并隐藏真实图标（反冲突约束：不重挂/不销毁他人窗口）。
    //    守卫确保后续任何失败都会恢复图标。
    let hierarchy = sylva_shell::takeover::probe()
        .ok_or_else(|| sylva_core::CoreError::Shell("未找到桌面根窗口 Progman".into()))?;
    let _guard = IconGuard::new(hierarchy);

    // 2) GPU 上下文 + overlay + 合成器
    let device = RenderDevice::new().map_err(|e| sylva_core::CoreError::Render(e.to_string()))?;
    let overlay = OverlayWindow::create(hierarchy.overlay_parent())
        .map_err(|e| sylva_core::CoreError::Render(e.to_string()))?;
    let (vw, vh) = (overlay.width, overlay.height);
    tracing::info!(vw, vh, "overlay 覆盖虚拟屏幕");

    // 主题字号按 DPI 放大：渲染层把渲染目标固定为 96 DPI（1 DIP = 1 物理像素），
    // 布局/字号全部走物理像素，200% 缩放下图标与文字才会以像素级分辨率渲染（清晰）。
    // 图标位图也按物理尺寸提取（见 ICON_EXTRACT_SIZE），不再把小图放大导致发糊。
    // 注意：所有 DIP 度量必须**一起**缩放（字号、图标、间距、内边距、列宽、控制台），
    // 只放大文字不放大行距/间距正是「行列间重叠」的根因。
    let mut theme = Theme::default();
    // Per-Monitor v2 下用 overlay 所在显示器的 DPI（系统感知回退则同 GetDpiForSystem）。
    // overlay 已在上一步创建，hwnd 可用。
    let dpi = unsafe { GetDpiForWindow(overlay.hwnd) };
    let dpi_scale = dpi as f32 / 96.0;
    apply_theme_scale(&mut theme, dpi_scale);
    tracing::info!(dpi, scale = dpi_scale, "主题按 DPI 缩放");
    // 侧边栏栅栏启动归一化：旧数据可能存了超屏/离屏 bounds（图标多时曾按内容全高
    // 计算导致 top 为负），重新停靠到屏幕内。折叠按钮已移除，历史折叠状态强制展开。
    let mut dock_reanchor = 0;
    for f in desk.fences.iter_mut() {
        if f.appearance.layout == FenceLayout::Sidebar {
            if f.sidebar_collapsed {
                f.sidebar_collapsed = false;
                dock_reanchor += 1;
            }
            let wa = work_area_rect(overlay.x as f32, overlay.y as f32, vw as f32, vh as f32);
            let b = sidebar_dock_rect(theme.scale, f, &wa);
            // 夹在工作区内（任务栏扣除后），下沿不越过任务栏
            let b = clamp_sidebar_work_rect(b, wa, f.appearance.sidebar_pos);
            if f.bounds != b {
                f.bounds = b;
                dock_reanchor += 1;
            }
        }
    }
    if dock_reanchor > 0 {
        tracing::info!(dock_reanchor, "侧边栏栅栏边界已重新停靠到屏幕内");
        let _ = store.save(&desk);
    }
    let compositor = Compositor::new(device, overlay.hwnd, theme.clone())
        .map_err(|e| sylva_core::CoreError::Render(e.to_string()))?;

    // 3) 枚举真实桌面图标（IShellFolder 枚举，不依赖 DefView）
    let mut items = sylva_shell::items::enumerate_desktop_items()
        .map_err(|e| sylva_core::CoreError::Shell(e.to_string()))?;
    tracing::info!(count = items.len(), "桌面图标枚举完成");

    // 4) 首次运行：无栅栏布局时按稳定顺序创建演示栅栏并持久化。
    //    之后布局由用户拖动/缩放决定，栅栏是显式成员列表（无自动分类）。
    seed_fences(&mut desk, &items, &theme);
    store.save(&desk)?;

    // 5) 图标元数据补齐 + 添加项恢复：
    //    - 枚举项：登记 path/详情（added=false），缺失的补进 desk.icons；
    //    - 上次拖入/粘贴的项（desk.icons 里带 path 但不在枚举中）：从路径重建
    //      DesktopItem 追加进 items 池，重启后仍能显示图标、双击打开。
    for it in &items {
        let ic = desk.icons.entry(it.id.clone()).or_insert_with(|| {
            let mut i = Icon::new(it.id.clone(), it.display_name.clone(), it.kind);
            i.path = it.path.clone();
            i
        });
        ic.path = it.path.clone();
        ic.added = false;
        if let Some(p) = it.path.as_deref() {
            sylva_core::details::enrich(ic, p);
        }
    }
    let mut item_index: HashMap<String, usize> = items
        .iter()
        .enumerate()
        .map(|(i, it)| (it.id.clone(), i))
        .collect();
    let mut restored = 0usize;
    for (id, ic) in &desk.icons {
        if item_index.contains_key(id) {
            continue;
        }
        if let Some(path) = ic.path.as_deref() {
            if let Ok(dt) = sylva_shell::items::item_from_path(path) {
                item_index.insert(dt.id.clone(), items.len());
                items.push(dt);
                restored += 1;
            }
        }
    }
    if restored > 0 {
        tracing::info!(restored, "恢复上次添加的图标");
    }

    // 图标索引 + 位图映射 + 首次上传数据（正式版放后台加载线程）
    let mut bitmap_ids = HashMap::new();
    let mut uploads = Vec::new();
    for (i, item) in items.iter().enumerate() {
        match sylva_shell::icons::extract_icon(item, ICON_EXTRACT_SIZE) {
            Ok(data) => {
                bitmap_ids.insert(item.id.clone(), i as u64);
                uploads.push((i as u64, data));
            }
            Err(e) => tracing::warn!(name = %item.display_name, "图标提取失败: {e}"),
        }
    }
    tracing::info!(loaded = uploads.len(), "图标位图提取完成");

    tracing::info!(icons = items.len(), "开始提取图标位图（初始化阶段）");
    memory::report("图标提取前");

    // 5) 初始场景 + 呈现 + 命中模型
    // 读取控制台开关（供动画器初始面板进度）——先取值再构造 Runtime，避免字段求值移动冲突。
    let console_open = desk.console_open;
    let overlay_ptr = &overlay as *const OverlayWindow as *mut OverlayWindow;
    // 上次退出时若处于「原始桌面」模式：恢复真实图标（IconGuard 刚隐藏过），
    // 栅栏保持隐藏（fence_alpha 取 0，不播放动画）。
    if desk.desktop_mode {
        hierarchy.restore_icons();
    }
    // 真实高度缓冲：与栅栏一一对应，`build_scene` 每帧回写（移动 desk 前取长度）。
    let last_layout_h = vec![0.0; desk.fences.len()];
    let mut rt = Runtime {
        desk,
        items,
        item_index,
        bitmap_ids,
        compositor,
        theme: theme.clone(),
        vw: vw as f32,
        vh: vh as f32,
        origin: (overlay.x as f32, overlay.y as f32),
        store,
        hover: None,
        cursor: None,
        selected: Vec::new(),
        select_band: None,
        hwnd: overlay.hwnd,
        edit: None,
        edit_high: None,
        console_anim: ConsoleAnim::new(console_open),
        desktop_fade: None,
        selected_fence: 0,
        console_hover: None,
        fence_scroll: 0.0,
        last_layout_h,
        fence_tweens: Vec::new(),
        icon_hover: None,
        hierarchy,
        overlay_ptr,
        library: library_dir,
        pending_uploads: Vec::new(),
        last_activity: std::time::Instant::now(),
        last_trim: std::time::Instant::now(),
        sidebar_reorder: None,
        reorder_slot_order: std::collections::HashMap::new(),
        current_icon_positions: std::collections::HashMap::new(),
    };
    // 双向同步：启动时清一次——内部库被外部删除的文件、链接文件夹与栅栏的差集，
    // 都同步进栅栏（链接文件夹的预置文件启动即出现）。
    if reconcile_fences(&mut rt) {
        // 链接文件夹同步后图标数变了，侧边栏 bounds 需要重新计算（reanchor 时是空的）
        reanchor_fences(&mut rt);
        let _ = rt.store.save(&rt.desk);
    }
    // 高度策略：`bounds.h == 0` 表示未手动缩放，按内容自适应（增删应用自动长高）。
    // 不在此冻结高度——用户拖边缘/角缩放后才落为具体值。
    let mut scene = build_scene(&mut rt, Instant::now());
    // 启动重叠消解：真实高度已由上面首帧 layout 写入 `last_layout_h`（自动高度栅栏
    // 的 0 高此时可换算成真实可见高度）。历史布局若存在重叠（朋友机「两个栅栏突然
    // 重叠」即由此而来）→ 推离并持久化，随后重排一帧供首帧呈现使用。
    if resolve_overlaps(&mut rt) {
        tracing::info!("启动重叠消解：存在重叠栅栏，已推离并保存");
        let _ = rt.store.save(&rt.desk);
        scene = build_scene(&mut rt, Instant::now());
    }
    // 首帧上传：图标位图（新增项的位图在 build_scene 内推入 pending_uploads）
    let mut upload_refs: Vec<(u64, &sylva_shell::icons::IconData)> =
        uploads.iter().map(|(id, d)| (*id, d)).collect();
    upload_refs.extend(rt.pending_uploads.iter().map(|(id, d)| (*id, d)));
    rt.compositor
        .present(&scene, &upload_refs)
        .map_err(|e| sylva_core::CoreError::Render(e.to_string()))?;
    let model = hit_model_from(&rt.theme, &scene, &rt.desk);
    memory::report("首帧呈现后");
    // Shell 右键菜单预热：后台加载栅栏里文件类型的 Shell 扩展（百度网盘等扩展首次
    // 加载会卡线程数秒），避免「首次右键」在主线程卡死被判无响应。预热在后台线程，
    // 与用户交互并行，不拖慢启动。
    {
        let paths: Vec<String> = rt
            .desk
            .icons
            .values()
            .filter_map(|ic| ic.path.clone())
            .collect();
        shell_menu::prime_startup(&paths);
    }
    // 启动期一次性分配已就绪：把不再活跃的内存页换出工作集（D3D/场景构建等），
    // 降低常驻内存。GPU 侧资源由驱动管理不受影响，用到时自动换回。
    memory::trim();
    memory::report("工作集修剪后");

    // 6) 事件回路：App 处理交互 → 重绘 → 返回新命中模型（overlay 据此更新区域）
    let runtime = Rc::new(RefCell::new(rt));
    let runtime2 = runtime.clone();
    overlay.set_event_handler(Box::new(move |ev| {
        // 模态菜单 / 属性页 / Shell 动词执行期间，嵌套消息循环会派发定时器、悬停、
        // 注入等其它事件再入本回调。此时外层 `handle_event` 仍持有 `Runtime` 的可变
        // 借用，再入必然 RefCell 借用冲突崩溃——一律丢弃再入事件，保持当前命中模型。
        if HANDLING.with(|h| h.replace(true)) {
            return None;
        }
        let _reentry = ReentryGuard;
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

    tracing::info!("Sylva 已就绪：拖边缘/角缩放、标题栏拖动、双击打开、文件拖入/粘贴添加；Ctrl+Alt+T 控制中心，Ctrl+Shift+F10 退出");
    run_message_loop();

    memory::report("退出前");
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

/// 栅栏用于碰撞/夹屏的真实矩形：自动高度（`bounds.h <= 0`）时用最近一次布局
/// 渲染高度。视觉矩形 = 模型矩形（拖拽已无补间），碰撞检测与实际可见区域一致。
fn fence_collision_rect(rt: &Runtime, i: usize) -> Rect {
    let f = &rt.desk.fences[i];
    Rect::new(f.bounds.x, f.bounds.y, f.bounds.w, fence_height(rt, i))
}

/// 除 `skip` 外其它栅栏的碰撞矩形（真实高度入算——自动高度栅栏不再以 0 高漏检，
/// 两个栅栏叠加到同一个自动高度栅栏上就是旧版「栅栏重叠」的根因之一）。
fn other_bounds(rt: &Runtime, skip: usize) -> Vec<Rect> {
    (0..rt.desk.fences.len())
        .filter(|&i| i != skip)
        .map(|i| fence_collision_rect(rt, i))
        .collect()
}

/// 启动重叠消解：加载的布局可能因历史 bug / DPI 变化存在重叠（尤其自动高度栅栏，
/// 旧版碰撞检测把 0 高当真高）。按序把每个栅栏与其它栅栏推离（真实高度入算），
/// 保证互不重叠且都在屏幕内。返回是否有变化（有变化才持久化 + 重排一帧）。
/// 依赖 `last_layout_h` 已由首帧 `build_scene` 填充，故须在首帧布局之后调用。
fn resolve_overlaps(rt: &mut Runtime) -> bool {
    let mut changed = false;
    let screen = screen_rect(rt);
    for i in 0..rt.desk.fences.len() {
        let others = other_bounds(rt, i);
        let cur = fence_collision_rect(rt, i);
        let out = settle_move(&cur, &others, &screen, FENCE_GAP);
        // 只修位置（不改变用户已设的尺寸），并圆整到整数物理像素。
        if (out.x - cur.x).abs() > 0.5 || (out.y - cur.y).abs() > 0.5 {
            rt.desk.fences[i].bounds.x = out.x.round();
            rt.desk.fences[i].bounds.y = out.y.round();
            changed = true;
        }
    }
    changed
}

/// 虚拟屏幕边界（物理像素；栅栏活动范围）。
fn screen_rect(rt: &Runtime) -> Rect {
    Rect::new(0.0, 0.0, rt.vw, rt.vh)
}

/// 主显示器工作区（任务栏扣除后；物理像素）。
/// `SPI_GETWORKAREA` 返回的是**屏幕坐标**（主显示器左上 = (0,0)），而栅栏布局用
/// 虚拟/客户端坐标（客户端 (0,0) = 虚拟屏 (0,0) = 屏幕 (ox, oy)）——转虚拟坐标
/// 需减去 `(ox, oy)`。单屏时 (0,0) 减了无感；多屏主屏非虚拟原点时少了这一步
/// 侧边栏夹屏会整体错位。失败回退整个虚拟屏幕。
fn work_area_rect(ox: f32, oy: f32, vw: f32, vh: f32) -> Rect {
    let mut rc = RECT::default();
    let ok = unsafe {
        SystemParametersInfoW(
            SPI_GETWORKAREA,
            0,
            Some(&mut rc as *mut RECT as *mut core::ffi::c_void),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        )
    };
    if ok.is_err() || rc.right <= rc.left || rc.bottom <= rc.top {
        return Rect::new(0.0, 0.0, vw, vh);
    }
    Rect::new(
        rc.left as f32 - ox,
        rc.top as f32 - oy,
        (rc.right - rc.left) as f32,
        (rc.bottom - rc.top) as f32,
    )
}

/// 该事件是否代表「用户真正点/拖了某处」（内联编辑打开时据此提交/失焦）。
/// 悬停、滚轮、定时器、注入重绘等非交互事件不在此列——它们不该关掉刚打开的编辑框。
fn is_popup_dismiss_event(ev: &OverlayEvent) -> bool {
    matches!(
        ev,
        OverlayEvent::FenceMove { .. }
            | OverlayEvent::FenceResize { .. }
            | OverlayEvent::IconClicked { .. }
            | OverlayEvent::IconDoubleClicked { .. }
            | OverlayEvent::SelectDrag { .. }
            | OverlayEvent::ContextMenu { .. }
            | OverlayEvent::FilesDropped { .. }
            | OverlayEvent::FenceScroll { .. }
            // 控制台交互（点按钮/滚待办/拖动面板/热键开关）都是真实交互，编辑期间应提交。
            | OverlayEvent::ConsoleClick { .. }
            | OverlayEvent::ConsoleScroll { .. }
            | OverlayEvent::ConsoleMove { .. }
            | OverlayEvent::ConsoleResize { .. }
            | OverlayEvent::ConsoleResizeEnd
            | OverlayEvent::ConsoleToggle
    )
}

/// 处理一个用户交互事件：更新布局 → （按需）重绘 → 生成新命中模型。
///
/// 返回 `None` 表示本事件不改变任何可见状态，无需重绘（overlay 保持当前
/// 命中模型与窗口区域）。这是空闲 CPU/GPU 与桌面闪烁的关键门控：
/// 无侧边栏时鼠标扫过（`CursorMove`）与 4s 心跳（`SyncLibrary` 无变化）
/// 都不再触发全量重绘。
fn handle_event(rt: &mut Runtime, ev: OverlayEvent) -> Option<HitModel> {
    // 空闲修剪门控：除周期性的 `SyncLibrary` 外都算用户活动，刷新时间戳。
    // `SyncLibrary` 每 4s 触发一次，若算活动则永远「不空闲」，修剪永不执行。
    if !matches!(ev, OverlayEvent::SyncLibrary) {
        rt.last_activity = std::time::Instant::now();
    }
    // 就地重命名编辑期间，真正的用户交互事件先提交编辑（资源管理器行为：点击别处即确认）。
    // 只对「用户确实在别处点/拖/滚」的事件提交——定时器（SyncLibrary）与悬停高亮
    // （HoverEnter/Leave）不算，否则编辑框会被后台同步或鼠标扫过自动关掉，用户根本没
    // 来得及输入（曾导致重命名看起来完全坏掉）。`EditCommitted` 是注入的「仅重绘」
    // 事件，此时编辑会话已结束，同样不在此处理。判断复用下方透明度滑块的同一语义。
    if rt.edit.is_some() && is_popup_dismiss_event(&ev) {
        dismiss_edit(rt);
    }
    // 重绘门控：默认 true（一切改变可见状态的事件照常重绘），
    // 仅对「可能无事可画」的事件显式置 false。
    let mut redraw = true;
    match ev {
        OverlayEvent::FenceMove { fence, pos } => {
            // 拖动标题栏：不交叉（无重叠推挤）+ 磁吸吸附 + 限制在虚拟屏幕内。
            // 直接落到目标（无补间）：视觉 = 模型 = 鼠标位置，拖拽即时跟手；
            // 且鼠标坐标是整数，拖拽中不存在分数坐标导致的边框发虚。
            let (x, y) = pos;
            let cur = rt.desk.fences.get(fence).map(|f| (f.bounds.w, f.bounds.h));
            if let Some((w, h)) = cur {
                let others = other_bounds(rt, fence);
                let wa = work_area_rect(rt.origin.0, rt.origin.1, rt.vw, rt.vh);
                let cand = Rect::new(x, y, w, h);
                let mut out = settle_move(&cand, &others, &wa, FENCE_GAP);
                // 侧边栏 dock 夹在工作区内（任务栏扣除后），下沿不越过任务栏
                if let Some(f) = rt.desk.fences.get(fence) {
                    if f.appearance.layout == FenceLayout::Sidebar {
                        out = clamp_sidebar_work_rect(out, wa, f.appearance.sidebar_pos);
                    }
                }
                if let Some(f) = rt.desk.fences.get_mut(fence) {
                    f.bounds.x = out.x;
                    f.bounds.y = out.y;
                }
            }
        }
        OverlayEvent::FenceResize { fence, zone, rect } => {
            // 拖边缘/角标：只动可动边，被其它栅栏挡住时停在交界边（不侵入），
            // 边接近时吸附对齐；锚定边不动，最小尺寸/屏幕边界约束在算法内完成。
            // 直接落到目标（无补间），视觉 = 模型 = 鼠标，即时跟手。
            let (nx, ny, nw, nh) = rect;
            let others = other_bounds(rt, fence);
            let wa = work_area_rect(rt.origin.0, rt.origin.1, rt.vw, rt.vh);
            let free = match zone {
                ResizeZone::Right => FreeSides::Right,
                ResizeZone::Bottom => FreeSides::Bottom,
                ResizeZone::BottomRight => FreeSides::BottomRight,
                ResizeZone::Left => FreeSides::Left,
                ResizeZone::BottomLeft => FreeSides::BottomLeft,
                ResizeZone::TopRight => FreeSides::TopRight,
            };
            let cand = Rect::new(nx, ny, nw, nh);
            let mut out = settle_resize(
                &cand,
                &others,
                &wa,
                free,
                MIN_FENCE_W,
                MIN_FENCE_H,
                FENCE_GAP,
            );
            // 侧边栏 dock：厚度轴锁定为「紧贴放大图标」的停靠值——纵向锁 x/w、
            // 横向锁 y/h，只保留用户拖动的那一轴，宽度/高度弹回锁定值，放大图标不再被裁。
            if let Some(f) = rt.desk.fences.get(fence) {
                if f.appearance.layout == FenceLayout::Sidebar {
                    let d = sidebar_dock_rect(rt.theme.scale, f, &wa);
                    out = if f.appearance.sidebar_pos == SidebarPosition::Top {
                        Rect::new(out.x, d.y, out.w, d.h)
                    } else {
                        Rect::new(d.x, out.y, d.w, out.h)
                    };
                    // 夹在工作区内（任务栏扣除后）：纵向 dock 下沿不越过任务栏
                    out = clamp_sidebar_work_rect(
                        out,
                        work_area_rect(rt.origin.0, rt.origin.1, rt.vw, rt.vh),
                        f.appearance.sidebar_pos,
                    );
                }
            }
            if let Some(f) = rt.desk.fences.get_mut(fence) {
                f.bounds = out;
            }
        }
        OverlayEvent::IconDoubleClicked { fence, icon } => {
            // 双击栅栏内图标：若属于多选集合则全部打开，否则打开双击项（资源管理器行为）
            if rt.selected.len() > 1 && rt.selected.contains(&(fence, icon)) {
                let targets = rt.selected.clone();
                for (f, i) in targets {
                    launch_fence_icon(rt, f, i);
                }
            } else {
                launch_fence_icon(rt, fence, icon);
            }
        }
        OverlayEvent::IconClicked { fence, icon, ctrl } => {
            // 单击选中（如资源管理器）：Ctrl+单击切换该图标（不连续多选），普通单击单选
            let key = (fence, icon);
            if ctrl {
                if rt.selected.contains(&key) {
                    rt.selected.retain(|&k| k != key);
                } else {
                    rt.selected.push(key);
                }
            } else {
                rt.selected = vec![key];
            }
        }
        OverlayEvent::SelectDrag {
            fence,
            rect,
            selected,
        } => {
            // 框选拖拽：更新选择集合 + 橡皮筋矩形（App 持有，供绘制）
            rt.selected = selected;
            rt.select_band = Some((
                fence,
                RectF {
                    x: rect.0,
                    y: rect.1,
                    w: rect.2,
                    h: rect.3,
                },
            ));
        }
        OverlayEvent::SelectEnd => {
            // 框选结束：清除橡皮筋显示（选择结果保留）
            rt.select_band = None;
        }
        OverlayEvent::CursorMove { x, y } => {
            // 侧边栏 Dock 放大的连续光标驱动（build_scene 据此每帧算缩放）。
            // 仅当存在 Dock 栅栏时光标才影响画面 → 无 Dock 时不重绘，
            // 桌面任意鼠标移动都不再触发全量重绘（闪烁根治）。
            rt.cursor = Some((x, y));
            redraw = has_sidebar_dock(rt);
        }
        OverlayEvent::CursorLeave => {
            // 光标离开窗口：清除 Dock 放大（下一帧全部恢复 1.0）。
            rt.cursor = None;
            redraw = has_sidebar_dock(rt);
        }
        OverlayEvent::HoverEnter { fence, icon } => {
            rt.hover = Some((fence, icon));
            // 悬停放大补间：从当前状态继续（进入 = 放大方向）
            let base = rt
                .icon_hover
                .filter(|h| h.fence == fence && h.icon == icon)
                .map(|h| h.progress(Instant::now()))
                .unwrap_or(0.0);
            rt.icon_hover = Some(IconHoverAnim {
                fence,
                icon,
                t0: Instant::now(),
                dur: 0.14,
                from: base,
                to: 1.0,
            });
            arm_anim_timer(rt);
        }
        OverlayEvent::HoverLeave => {
            rt.hover = None;
            if let Some(h) = rt.icon_hover {
                let p = h.progress(Instant::now());
                rt.icon_hover = Some(IconHoverAnim {
                    fence: h.fence,
                    icon: h.icon,
                    t0: Instant::now(),
                    dur: 0.18,
                    from: p,
                    to: 0.0,
                });
                arm_anim_timer(rt);
            }
        }
        OverlayEvent::ContextMenu { fence, icon, pos } => {
            handle_context_menu(rt, fence, icon, pos);
        }
        OverlayEvent::FilesDropped { fence, paths } => {
            // 拖入任意文件/文件夹/快捷方式：加入该栅栏（位图进 pending_uploads）
            add_paths_to_fence(rt, fence, &paths);
        }
        OverlayEvent::FenceScroll { fence, delta } => {
            // 滚轮滚动：正=向上（回到开头），负=向下（看更多）。上界由 layout 钳制。
            if let Some(f) = rt.desk.fences.get_mut(fence) {
                let s = rt.theme.scale;
                let step = match f.appearance.layout {
                    FenceLayout::List => LIST_ICON_SIZE * s + rt.theme.list_row_gap,
                    FenceLayout::Grid | FenceLayout::Sidebar => {
                        (f.appearance.icon_size + f.appearance.gap) * s
                    }
                };
                f.scroll = (f.scroll - (delta as f32 / 120.0) * step).max(0.0);
            }
            if let Err(e) = rt.store.save(&rt.desk) {
                tracing::warn!("滚动位置持久化失败: {e}");
            }
        }
        OverlayEvent::FenceDragEnd { .. } => {
            // 拖动结束：持久化当前布局
            if let Err(e) = rt.store.save(&rt.desk) {
                tracing::warn!("布局持久化失败: {e}");
            }
        }
        OverlayEvent::ConsoleClick { zone } => match zone {
            ConsoleZone::Close => {
                // 折叠为胶囊（面板始终可见，不再「消失」）；保存开关状态供重启恢复
                rt.desk.console_open = false;
                if let Err(e) = rt.store.save(&rt.desk) {
                    tracing::warn!("控制台状态持久化失败: {e}");
                }
                start_panel_tween(rt, 0.0);
            }
            ConsoleZone::Expand => {
                // 点击胶囊展开面板
                rt.desk.console_open = true;
                if let Err(e) = rt.store.save(&rt.desk) {
                    tracing::warn!("控制台状态持久化失败: {e}");
                }
                start_panel_tween(rt, 1.0);
            }
            ConsoleZone::DesktopToggle => {
                toggle_desktop(rt);
            }
            ConsoleZone::FenceSelect(i) => {
                if i < rt.desk.fences.len() {
                    rt.selected_fence = i;
                }
            }
            ConsoleZone::FenceLayout(l) => {
                let i = rt
                    .selected_fence
                    .min(rt.desk.fences.len().saturating_sub(1));
                if l == FenceLayout::List {
                    let w = list_auto_width(rt, i);
                    if let Some(f) = rt.desk.fences.get_mut(i) {
                        f.appearance.layout = l;
                        f.bounds.w = w;
                    }
                } else if l == FenceLayout::Sidebar {
                    if let Some(f) = rt.desk.fences.get_mut(i) {
                        f.appearance.layout = l;
                        f.sidebar_collapsed = false;
                        // 停靠到屏幕边缘（侧 = 当前设置的停靠位置），并夹到屏幕内，
                        // 图标多时高度不超过屏幕（超出滚动），不会跑到屏幕外。
                        let wa = work_area_rect(rt.origin.0, rt.origin.1, rt.vw, rt.vh);
                        let b = sidebar_dock_rect(rt.theme.scale, f, &wa);
                        // 夹在工作区内（任务栏扣除后），下沿不越过任务栏
                        f.bounds = clamp_sidebar_work_rect(b, wa, f.appearance.sidebar_pos);
                    }
                } else if let Some(f) = rt.desk.fences.get_mut(i) {
                    f.appearance.layout = l;
                }
                let _ = rt.store.save(&rt.desk);
            }
            ConsoleZone::FenceIconSize(sz) => {
                let i = rt
                    .selected_fence
                    .min(rt.desk.fences.len().saturating_sub(1));
                if let Some(f) = rt.desk.fences.get_mut(i) {
                    f.appearance.icon_size = sz;
                }
                let _ = rt.store.save(&rt.desk);
            }
            ConsoleZone::FenceStyle(style) => {
                let i = rt
                    .selected_fence
                    .min(rt.desk.fences.len().saturating_sub(1));
                if let Some(f) = rt.desk.fences.get_mut(i) {
                    f.appearance.bg_style = style;
                }
                let _ = rt.store.save(&rt.desk);
            }
            ConsoleZone::FenceTint(tint) => {
                let i = rt
                    .selected_fence
                    .min(rt.desk.fences.len().saturating_sub(1));
                if let Some(f) = rt.desk.fences.get_mut(i) {
                    f.appearance.tint = tint;
                }
                let _ = rt.store.save(&rt.desk);
            }
            ConsoleZone::FenceSidebarPos(pos) => {
                let i = rt
                    .selected_fence
                    .min(rt.desk.fences.len().saturating_sub(1));
                if let Some(f) = rt.desk.fences.get_mut(i) {
                    f.appearance.sidebar_pos = pos;
                    // 位置切换：重新停靠到新的一侧（否则只高亮、不移动）
                    if f.appearance.layout == FenceLayout::Sidebar {
                        let wa = work_area_rect(rt.origin.0, rt.origin.1, rt.vw, rt.vh);
                        let b = sidebar_dock_rect(rt.theme.scale, f, &wa);
                        // 夹在工作区内（任务栏扣除后），下沿不越过任务栏
                        f.bounds = clamp_sidebar_work_rect(b, wa, f.appearance.sidebar_pos);
                    }
                }
                let _ = rt.store.save(&rt.desk);
            }
            ConsoleZone::AddFence => {
                // 新建栅栏：弹出文件夹选择器让用户选链接目录
                if let Some(dir) = pick_folder(rt.hwnd) {
                    // 用文件夹名作为栅栏标题
                    let title = std::path::Path::new(&dir)
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_else(|| format!("栅栏 {}", rt.desk.next_fence_id()));
                    let id = rt.desk.next_fence_id();
                    let s = rt.theme.scale;
                    let w = 360.0 * s;
                    let h = 220.0 * s;
                    let start = Rect::new(80.0 * s, 120.0 * s, w, h);
                    let others = (0..rt.desk.fences.len())
                        .map(|j| fence_collision_rect(rt, j))
                        .collect::<Vec<_>>();
                    let screen = screen_rect(rt);
                    let out = settle_move(&start, &others, &screen, FENCE_GAP);
                    let bounds = Rect::new(out.x.round(), out.y.round(), w.round(), h.round());
                    rt.desk.fences.push(Fence {
                        id,
                        title: Some(title),
                        monitor_id: 0,
                        bounds,
                        state: FenceState::Expanded,
                        icon_ids: Vec::new(),
                        appearance: FenceAppearance::default(),
                        scroll: 0.0,
                        storage_path: Some(dir),
                        sidebar_collapsed: false,
                    });
                    rt.selected_fence = rt.desk.fences.len() - 1;
                    // 立即同步链接文件夹内容
                    if reconcile_fences(rt) {
                        tracing::info!("新栅栏已链接文件夹并同步内容");
                    }
                    let _ = rt.store.save(&rt.desk);
                }
            }
            ConsoleZone::RemoveFence => {
                // 移出当前选中的栅栏：成员退回未分组区，栅栏删除。
                // 不删除链接的文件夹（用户数据不受影响）。
                let i = rt
                    .selected_fence
                    .min(rt.desk.fences.len().saturating_sub(1));
                let ids: Vec<String> = rt
                    .desk
                    .fences
                    .get(i)
                    .map(|f| f.icon_ids.clone())
                    .unwrap_or_default();
                for id in ids {
                    rt.desk.move_icon(&id, None);
                }
                if i < rt.desk.fences.len() {
                    rt.desk.fences.remove(i);
                }
                rt.selected_fence = rt
                    .selected_fence
                    .saturating_sub(1)
                    .min(rt.desk.fences.len());
                if let Err(e) = rt.store.save(&rt.desk) {
                    tracing::warn!("移出栅栏持久化失败: {e}");
                }
            }
            ConsoleZone::ChangeStoragePath => {
                // 更改选中栅栏的存储位置：打开文件夹选择器，移动已有库内项到新路径
                let i = rt
                    .selected_fence
                    .min(rt.desk.fences.len().saturating_sub(1));
                if let Some(paths) = pick_paths(rt.hwnd) {
                    if let Some(new_dir) = paths.first() {
                        change_fence_storage(rt, i, new_dir);
                    }
                }
            }
            // 标签页已随小组件一并移除；命中模型不再产生该控件，兜底吞掉。
            ConsoleZone::Tab(_) => {}
        },
        OverlayEvent::ConsoleScroll { delta } => {
            // 栅栏管理页滚轮：滚动栅栏列表
            let s = rt.theme.scale;
            let step = CONSOLE_FENCE_ROW_H * s;
            let max = fence_scroll_max(rt);
            rt.fence_scroll = (rt.fence_scroll - (delta as f32 / 120.0) * step).clamp(0.0, max);
        }
        OverlayEvent::ConsoleHover { zone } => {
            // 控件悬停：存下供下一帧绘制高亮（仅展开面板内上报）
            rt.console_hover = zone;
        }
        OverlayEvent::KeyDown { vk, ctrl } => {
            edit_key(rt, vk, ctrl);
        }
        OverlayEvent::Char { ch } => {
            edit_char(rt, ch);
        }
        OverlayEvent::EditCaret { x } => {
            // 鼠标点编辑框内文本：光标跳到对应字符（不触发「点击别处提交」，
            // 该事件由 overlay 命中模型把框内点击单独路由而来）
            edit_click(rt, x);
        }
        OverlayEvent::ImeStart => {
            if let Some(e) = rt.edit.as_mut() {
                e.composing = true;
                e.comp.clear();
                position_ime_window(rt);
            }
        }
        OverlayEvent::ImeCompose { text, caret } => {
            if let Some(e) = rt.edit.as_mut() {
                e.composing = true;
                e.comp = text;
                let _ = caret;
                position_ime_window(rt);
            }
        }
        OverlayEvent::ImeResult { text } => {
            if let Some(e) = rt.edit.as_mut() {
                e.committing = true;
                e.commit_ime(&text);
                e.committing = false;
                e.composing = false;
            }
        }
        OverlayEvent::ImeEnd => {
            if let Some(e) = rt.edit.as_mut() {
                e.composing = false;
                e.comp.clear();
            }
        }
        OverlayEvent::OverlayFocusLost => {
            // 焦点离开 overlay：内联编辑失焦（待办输入/便签提交文本，重命名提交）
            dismiss_edit(rt);
        }
        OverlayEvent::ConsoleMove { pos } => {
            // 拖动标题栏移动面板：记录左上角（原始坐标 + 增量，避免粘连）
            rt.desk.console_pos = Some(Vec2 { x: pos.0, y: pos.1 });
        }
        OverlayEvent::ConsoleDragEnd => {
            // 拖动结束：持久化面板位置
            if let Err(e) = rt.store.save(&rt.desk) {
                tracing::warn!("控制台位置持久化失败: {e}");
            }
        }
        OverlayEvent::ConsoleResize { rect } => {
            // 拖右/下边缘缩放面板：宽度直接生效；高度换算为「完全展开高度」
            // （可见高度 = 胶囊高 + (展开高 - 胶囊高) × panel 进度，反解展开高）。
            let s = rt.theme.scale;
            let pill_h = CONSOLE_PILL_H * s;
            let panel = rt.console_anim.panel.max(0.05); // 折叠态按最小进度反解，避免除零
            let full_w = rect.2.max(CONSOLE_MIN_W * s);
            let full_h = (pill_h + (rect.3 - pill_h) / panel).max(CONSOLE_MIN_H * s);
            rt.desk.console_size = Some((full_w, full_h));
        }
        OverlayEvent::ConsoleResizeEnd => {
            // 缩放结束：持久化面板尺寸
            if let Err(e) = rt.store.save(&rt.desk) {
                tracing::warn!("控制台尺寸持久化失败: {e}");
            }
        }
        OverlayEvent::ConsoleToggle => {
            // 热键 Ctrl+Alt+T：展开/折叠面板（胶囊始终可见，热键只是切换形态）
            let open = !rt.desk.console_open;
            rt.desk.console_open = open;
            if let Err(e) = rt.store.save(&rt.desk) {
                tracing::warn!("控制台状态持久化失败: {e}");
            }
            if open {
                start_panel_tween(rt, 1.0);
            } else {
                start_panel_tween(rt, 0.0);
            }
        }
        OverlayEvent::TrayToggle => {
            // 托盘图标双击：切换控制中心开合（与 Ctrl+Alt+T 相同）
            let open = !rt.desk.console_open;
            rt.desk.console_open = open;
            let _ = rt.store.save(&rt.desk);
            start_panel_tween(rt, if open { 1.0 } else { 0.0 });
        }
        OverlayEvent::TrayMenu => {
            handle_tray_menu(rt);
        }
        OverlayEvent::EditCommitted => {
            // 就地重命名提交后的注入事件：数据已改好，这里只需让尾部重建场景。
        }
        OverlayEvent::AnimTick => {
            // 动画帧：推进面板/行补间；全部结束后停用定时器，回到空闲 0% CPU。
            if !advance_anim(rt) {
                unsafe { (*rt.overlay_ptr).set_anim_active(false) };
            }
        }
        OverlayEvent::SyncLibrary => {
            // 双向同步：内部库被外部删除 → 栅栏项移除；链接文件夹增删改名 → 栅栏镜像
            // （文件夹是事实来源，栅栏即文件夹；有变化才持久化）
            let changed = reconcile_fences(rt);
            if changed {
                let _ = rt.store.save(&rt.desk);
            }
            // 空闲修剪工作集：启动后用户长时间不操作时，把不再活跃的内存页换出 RAM，
            // 保持低常驻（任务管理器「内存」列）。用户交互换入后再空闲同样触发；限频避免频繁调用。
            const IDLE_TRIM_AFTER: std::time::Duration = std::time::Duration::from_secs(30);
            const TRIM_MIN_INTERVAL: std::time::Duration = std::time::Duration::from_secs(60);
            if rt.last_activity.elapsed() >= IDLE_TRIM_AFTER
                && rt.last_trim.elapsed() >= TRIM_MIN_INTERVAL
            {
                rt.last_trim = std::time::Instant::now();
                memory::trim();
            }
            // 无变化不重绘（消除 4s 心跳脉冲）
            redraw = changed;
        }
        OverlayEvent::DpiChanged { dpi } => {
            // 所在显示器 DPI 变化（Per-Monitor v2）：重算主题缩放（覆盖旧的缩放
            // 字段），并重排栅栏——侧边栏停靠尺寸随缩放变化，普通栅栏夹回屏内。
            let scale = dpi as f32 / 96.0;
            if (scale - rt.theme.scale).abs() < 1e-4 {
                // DPI 实际未变（PerMonitorV2 下窗口跨显示器移动也可能触发）：
                // 无需重画。
                redraw = false;
            } else {
                let mut t = Theme::default();
                apply_theme_scale(&mut t, scale);
                rt.theme = t;
                if reanchor_fences(rt) {
                    let _ = rt.store.save(&rt.desk);
                }
                tracing::info!(dpi, scale, "DPI 变化：主题已重缩放并重排栅栏");
            }
        }
        OverlayEvent::DisplayChange => {
            // 显示拓扑/分辨率变化：重查虚拟屏，重设 overlay 窗口（移动 + 缩放），
            // 再把超屏栅栏夹回屏内——修复「拔掉显示器/改分辨率后栅栏找不到」。
            let vx = unsafe { GetSystemMetrics(SM_XVIRTUALSCREEN) };
            let vy = unsafe { GetSystemMetrics(SM_YVIRTUALSCREEN) };
            let vw = unsafe { GetSystemMetrics(SM_CXVIRTUALSCREEN) } as u32;
            let vh = unsafe { GetSystemMetrics(SM_CYVIRTUALSCREEN) } as u32;
            let changed = (vw as f32 - rt.vw).abs() > 0.5
                || (vh as f32 - rt.vh).abs() > 0.5
                || vx as f32 != rt.origin.0
                || vy as f32 != rt.origin.1;
            if changed {
                unsafe { (*rt.overlay_ptr).resize(vx, vy, vw, vh) };
                rt.vw = vw as f32;
                rt.vh = vh as f32;
                rt.origin = (vx as f32, vy as f32);
                if reanchor_fences(rt) {
                    let _ = rt.store.save(&rt.desk);
                }
                tracing::info!(vx, vy, vw, vh, "显示拓扑变化：overlay 已重设并夹回栅栏");
            }
        }
        OverlayEvent::SidebarReorderDrag { fence, icon, mx, my } => {
            // 侧边栏图标拖动中：记录拖动状态，触发重绘
            // 排序计算在 build_scene 中基于实际图标位置完成
            rt.sidebar_reorder = Some((fence, icon, mx, my));
            redraw = true;
        }
        OverlayEvent::SidebarReorderEnd { fence, from, to } => {
            // 侧边栏图标拖动结束：执行重排
            rt.sidebar_reorder = None;
            rt.reorder_slot_order.clear();
            rt.current_icon_positions.clear();
            if let Some(f) = rt.desk.fences.get_mut(fence) {
                if from < f.icon_ids.len() && to <= f.icon_ids.len() && from != to {
                    let id = f.icon_ids.remove(from);
                    let insert_at = if to > from { to - 1 } else { to };
                    f.icon_ids.insert(insert_at, id);
                    let _ = rt.store.save(&rt.desk);
                    tracing::info!(from, to, "侧边栏图标已重排");
                }
            }
            redraw = true;
        }
    }

    // 重绘门控落点：无可见变化的事件直接返回 None（overlay 保持当前命中模型
    // 与区域），省掉一次 build_scene + D2D 全量绘制 + commit。
    if !redraw {
        return None;
    }

    // 重新排布 + 重绘 + 生成新命中模型（含区域）。
    // 高度策略：`bounds.h == 0` 表示未手动缩放，每帧按内容自适应（增删应用自动长高）；
    // 用户拖角标缩放后 `bounds.h` 变为具体值，高度固定，放不下的图标以 "+N" 提示。
    let scene = build_scene(rt, Instant::now());
    let ups = std::mem::take(&mut rt.pending_uploads);
    let upload_refs: Vec<(u64, &IconData)> = ups.iter().map(|(id, d)| (*id, d)).collect();
    if let Err(e) = rt.compositor.present(&scene, &upload_refs) {
        tracing::warn!("重绘失败: {e}");
    }
    Some(hit_model_from(&rt.theme, &scene, &rt.desk))
}

/// 是否存在侧边栏 Dock 栅栏（决定 `CursorMove` 是否需要驱动重绘）。
fn has_sidebar_dock(rt: &Runtime) -> bool {
    rt.desk
        .fences
        .iter()
        .any(|f| f.appearance.layout == FenceLayout::Sidebar)
}

/// 把默认主题按 DPI 缩放系数放大（`scale = 系统DPI/96`）。
/// 所有 DIP 度量必须一起缩放，只放文字不放行距/间距正是「行列重叠」的根因。
/// `Theme::default()` 是未缩放基准；DPI 变化时用本函数重新应用，覆盖旧缩放。
fn apply_theme_scale(theme: &mut Theme, scale: f32) {
    theme.scale = scale;
    theme.title.size *= scale;
    theme.label.size *= scale;
    theme.icon_size *= scale;
    theme.icon_gap *= scale;
    theme.icon_caption_gap *= scale;
    theme.fence_padding *= scale;
    theme.fence_corner_radius *= scale;
    theme.title_padding_bottom *= scale;
    theme.caption_max_width *= scale;
    theme.list_row_gap *= scale;
    theme.list_label_gap *= scale;
}

/// 栅栏当前实际高度（物理像素）：`bounds.h > 0` 为固定高度；否则（自动高度）用
/// 最近一次布局渲染高度。碰撞检测与夹屏都必须按真实高度入算——自动高度栅栏
/// `bounds.h == 0`，直接当 0 高会把碰撞检测和夹屏一起带偏。
fn fence_height(rt: &Runtime, i: usize) -> f32 {
    match rt.desk.fences.get(i) {
        Some(f) if f.bounds.h > 0.0 => f.bounds.h,
        _ => rt.last_layout_h.get(i).copied().unwrap_or(0.0),
    }
}

/// 把矩形左上角夹回虚拟屏内（宽高不变；矩形宽高本身超出屏幕时贴左/上边缘）。
fn clamp_into_screen(x: f32, y: f32, w: f32, h: f32, vw: f32, vh: f32) -> (f32, f32) {
    (
        x.clamp(0.0, (vw - w).max(0.0)),
        y.clamp(0.0, (vh - h).max(0.0)),
    )
}

/// DPI / 显示拓扑变化后的统一重排：
/// - 侧边栏 Dock：尺寸随主题缩放，重新停靠到屏边（任务栏扣除）。
/// - 普通栅栏：夹回虚拟屏内（真实高度参与，超屏即找回）。
///
/// 返回是否有变化（有变化才需要持久化）。启动恢复与 `WM_DPICHANGED`/
/// `WM_DISPLAYCHANGE` 都走这里，保证同一套语义。
fn reanchor_fences(rt: &mut Runtime) -> bool {
    let mut changed = false;
    for i in 0..rt.desk.fences.len() {
        if rt.desk.fences[i].appearance.layout == FenceLayout::Sidebar {
            let wa = work_area_rect(rt.origin.0, rt.origin.1, rt.vw, rt.vh);
            let b = sidebar_dock_rect(rt.theme.scale, &rt.desk.fences[i], &wa);
            let b = clamp_sidebar_work_rect(b, wa, rt.desk.fences[i].appearance.sidebar_pos);
            if rt.desk.fences[i].bounds != b {
                rt.desk.fences[i].bounds = b;
                changed = true;
            }
        } else {
            let (nx, ny) = clamp_into_screen(
                rt.desk.fences[i].bounds.x,
                rt.desk.fences[i].bounds.y,
                rt.desk.fences[i].bounds.w,
                fence_height(rt, i),
                rt.vw,
                rt.vh,
            );
            if nx != rt.desk.fences[i].bounds.x || ny != rt.desk.fences[i].bounds.y {
                rt.desk.fences[i].bounds.x = nx;
                rt.desk.fences[i].bounds.y = ny;
                changed = true;
            }
        }
    }
    changed
}

/// 把 overlay 窗口设为前台并聚焦（内联编辑接收键盘/IME 的前提）。
fn focus_overlay(rt: &Runtime) {
    // 前台交给隐藏焦点代理，overlay 本体保持 WS_EX_NOACTIVATE（不把桌面壳层提到应用之上）。
    unsafe { (*rt.overlay_ptr).focus_for_input() };
}

/// 内联编辑失焦/点击别处：重命名提交（资源管理器行为）。
fn launch_fence_icon(rt: &mut Runtime, fence: usize, icon: usize) {
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

/// 把栅栏内第 `icon` 个图标移出栅栏：新增项（非桌面枚举）直接删除，桌面图标移回未分组区。
fn remove_fence_icon(rt: &mut Runtime, fence: usize, icon: usize) {
    let id = rt
        .desk
        .fences
        .get(fence)
        .and_then(|f| f.icon_ids.get(icon))
        .cloned();
    if let Some(id) = id {
        remove_fence_icon_by_id(rt, &id);
    }
    let _ = rt.store.save(&rt.desk);
}

/// 按 id 移出栅栏：内部库项直接删除（引用，库文件由「删除」动作负责）；链接文件夹的
/// 镜像项**删除文件**（栅栏即文件夹，否则同步会把图标加回来，用户已确认此语义）；
/// 桌面图标移回未分组区。
fn remove_fence_icon_by_id(rt: &mut Runtime, id: &String) {
    if rt.desk.icons.get(id).map(|i| i.added).unwrap_or(false) {
        if rt
            .desk
            .icons
            .get(id)
            .and_then(|ic| ic.path.clone())
            .map(|p| is_linked_path(rt, &p))
            .unwrap_or(false)
        {
            delete_managed_file(rt, id);
        }
        remove_icon_entirely(rt, id);
    } else {
        rt.desk.move_icon(id, None);
    }
}

/// 选中集合的 id 快照（先收集后操作，避免移除过程中下标/栅栏顺序变化）。
fn selected_ids(rt: &Runtime) -> Vec<String> {
    rt.selected
        .iter()
        .filter_map(|&(f, i)| {
            rt.desk
                .fences
                .get(f)
                .and_then(|f| f.icon_ids.get(i))
                .cloned()
        })
        .collect()
}

/// 选中集合的路径快照（复制到剪贴板用）。
fn selected_paths(rt: &Runtime) -> Vec<String> {
    selected_ids(rt)
        .iter()
        .filter_map(|id| rt.desk.icons.get(id).and_then(|ic| ic.path.clone()))
        .collect()
}

/// 打开全部选中项。
fn open_selected(rt: &mut Runtime) {
    let targets = rt.selected.clone();
    for (f, i) in targets {
        launch_fence_icon(rt, f, i);
    }
}

/// 复制全部选中项到剪贴板（CF_HDROP，与资源管理器「复制」一致，可粘贴到任意文件夹）。
fn copy_selected(rt: &mut Runtime) {
    let paths = selected_paths(rt);
    if paths.is_empty() {
        return;
    }
    set_clipboard_paths(&paths);
}

/// 移出栅栏：选中项全部移出（内部库项删除引用，桌面图标移回未分组区）。
fn remove_selected(rt: &mut Runtime) {
    let ids = selected_ids(rt);
    for id in &ids {
        remove_fence_icon_by_id(rt, id);
    }
    let _ = rt.store.save(&rt.desk);
}

/// 删除选中项：管理区（内部库/链接文件夹）内的文件连同磁盘文件一并删除（文件夹删 →
/// 栅栏项消失，与同步机制一致）；桌面图标移出栅栏（回未分组区，不碰源文件）。
fn delete_selected(rt: &mut Runtime) {
    let ids = selected_ids(rt);
    for id in &ids {
        let added = rt.desk.icons.get(id).map(|ic| ic.added).unwrap_or(false);
        if added {
            delete_managed_file(rt, id);
            remove_icon_entirely(rt, id);
        } else {
            rt.desk.move_icon(id, None);
        }
    }
    let _ = rt.store.save(&rt.desk);
}

/// 开始就地重命名（D2D 内联编辑，Explorer 风格）：Enter/失焦提交，Esc 取消。
/// 编辑框与栅栏/标签同表面渲染，无黑边、无层级问题，IME 中文直接可用。
fn fence_scroll_max(rt: &Runtime) -> f32 {
    let n = rt.desk.fences.len();
    if n <= CONSOLE_FENCE_MAX_ROWS {
        return 0.0;
    }
    (n - CONSOLE_FENCE_MAX_ROWS) as f32 * CONSOLE_FENCE_ROW_H * rt.theme.scale
}

/// 一键切换桌面模式：栅栏 ⇄ 原始桌面。
///
/// 切到原始桌面：立即恢复真实图标，栅栏 0.22s 淡出（淡完不再接收命中）；
/// 切回栅栏：隐藏真实图标，栅栏淡入。控制中心本身始终可见，保证随时能切回。
fn toggle_desktop(rt: &mut Runtime) {
    rt.desk.desktop_mode = !rt.desk.desktop_mode;
    if rt.desk.desktop_mode {
        rt.hierarchy.restore_icons();
        rt.desktop_fade = Some(PanelTween {
            t0: Instant::now(),
            dur: 0.22,
            from: 1.0,
            to: 0.0,
        });
    } else {
        rt.hierarchy.hide_icons();
        rt.desktop_fade = Some(PanelTween {
            t0: Instant::now(),
            dur: 0.22,
            from: 0.0,
            to: 1.0,
        });
    }
    arm_anim_timer(rt);
    if let Err(e) = rt.store.save(&rt.desk) {
        tracing::warn!("桌面模式持久化失败: {e}");
    }
}

/// 当前光标在虚拟屏幕的物理坐标（右键菜单弹出位置 / IME 窗口定位用）。
fn cursor_screen() -> (i32, i32) {
    let mut pt = POINT::default();
    unsafe {
        let _ = GetCursorPos(&mut pt);
    }
    (pt.x, pt.y)
}
