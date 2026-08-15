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

mod event_bus;
mod logging;
mod memory;
mod shell_menu;

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::OnceLock;
use std::time::Instant;

use windows::core::{BOOL, PCWSTR, PWSTR};
use windows::Win32::Foundation::{
    GetLastError, COLORREF, ERROR_ALREADY_EXISTS, HANDLE, HINSTANCE, HWND, LPARAM, LRESULT, POINT,
    RECT, WPARAM,
};
use windows::Win32::Graphics::Dwm::{
    DwmSetWindowAttribute, DWMWA_BORDER_COLOR, DWMWA_WINDOW_CORNER_PREFERENCE,
};
use windows::Win32::Graphics::Gdi::{
    CreateFontIndirectW, CreateRoundRectRgn, DeleteObject, SetWindowRgn, FONT_CHARSET,
    FONT_CLIP_PRECISION, FONT_OUTPUT_PRECISION, FONT_QUALITY, HFONT, LOGFONTW,
};
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};
use windows::Win32::System::Console::SetConsoleCtrlHandler;
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, OpenClipboard, SetClipboardData,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
use windows::Win32::System::Threading::{AttachThreadInput, CreateMutexW, GetCurrentThreadId};
use windows::Win32::UI::Controls::EM_SETSEL;
use windows::Win32::UI::HiDpi::GetDpiForSystem;
use windows::Win32::UI::Input::KeyboardAndMouse::{SetFocus, VK_ESCAPE, VK_RETURN};
use windows::Win32::UI::Shell::{
    DragQueryFileW, FileOpenDialog, IFileOpenDialog, IShellItem, IShellItemArray,
    FOS_ALLOWMULTISELECT, FOS_FORCEFILESYSTEM, FOS_PICKFOLDERS, HDROP, SIGDN_FILESYSPATH,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, BringWindowToTop, CallWindowProcW, CreatePopupMenu, CreateWindowExW,
    DefWindowProcW, DestroyMenu, DestroyWindow, GetCursorPos, GetWindowLongPtrW, GetWindowRect,
    GetWindowTextW, GetWindowThreadProcessId, MoveWindow, PostMessageW, SendMessageW,
    SetForegroundWindow, SetProcessDPIAware, SetWindowLongPtrW, ShowWindow, TrackPopupMenu,
    ES_AUTOHSCROLL, ES_AUTOVSCROLL, ES_LEFT, ES_MULTILINE, ES_WANTRETURN, GWLP_USERDATA,
    GWL_WNDPROC, HMENU, MA_ACTIVATE, MF_CHECKED, MF_POPUP, MF_SEPARATOR, MF_STRING, SW_HIDE,
    SW_SHOWNA, TPM_NONOTIFY, TPM_RETURNCMD, WINDOW_STYLE, WM_IME_ENDCOMPOSITION, WM_KEYDOWN,
    WM_KEYUP, WM_KILLFOCUS, WM_MOUSEACTIVATE, WM_SETFONT, WM_SETTEXT, WS_EX_TOOLWINDOW, WS_POPUP,
    WS_VISIBLE, WS_VSCROLL,
};

use sylva_core::config::ConfigStore;
use sylva_core::magnet::{settle_move, settle_resize, FreeSides, FENCE_GAP};
use sylva_core::model::{
    Desk, Fence, FenceAppearance, FenceLayout, FenceState, FenceStyle, Icon, PluginEntry,
    PluginKind, Rect, TodoItem, Vec2,
};
use sylva_render::{
    run_message_loop, Compositor, ConsoleHit, ConsoleTab, ConsoleZone, FenceHit, HitModel, IconHit,
    ListColumns, OverlayEvent, OverlayWindow, RectF, RenderDevice, ResizeZone, Scene, SceneConsole,
    SceneFence, SceneFenceDetail, SceneFenceRow, SceneIcon, SceneNotes, ScenePluginRow, SceneTab,
    SceneTodo, SceneTodoRow, Theme, GRIP_SIZE, WM_APP_QUIT, WM_SYLVA_INJECT,
};
use sylva_shell::icons::IconData;
use sylva_shell::items::DesktopItem;
use sylva_shell::takeover::DesktopHierarchy;

/// 应用数据目录名（位于 %APPDATA% 下）。
const APP_DIR: &str = "Sylva";

/// 栅栏最小宽度（缩放下限，物理像素）。
const MIN_FENCE_W: f32 = 200.0;

/// 栅栏最小高度（缩放下限，物理像素）。
const MIN_FENCE_H: f32 = 60.0;

/// 图标提取边长（物理像素）。取高于所有渲染尺寸的值（最大图标 64、列表 20），
/// 渲染时向下采样才清晰；若按渲染尺寸 32 提取再放大到 48，高 DPI 下发糊。
const ICON_EXTRACT_SIZE: u32 = 64;

// 右键菜单项 ID（分段避免冲突）。
const MENU_ICON_OPEN: usize = 1;
const MENU_ICON_REMOVE: usize = 2;
const MENU_LAYOUT: usize = 2000; // + 0=网格 1=列表
const MENU_ICON_SIZE: usize = 3000; // + 0/1/2 = 32/48/64
const MENU_STYLE: usize = 4000; // 背景风格子菜单：+0=玻璃 +1=透明 +2=颜色
const MENU_PASTE: usize = 2500; // 粘贴剪贴板文件
const MENU_DELETE_FENCE: usize = 5000;
const MENU_RENAME_FENCE: usize = 6000; // 重命名栅栏（栅栏内就地编辑）
const MENU_TINT: usize = 7000; // 背景色调子菜单项：+0=默认，+1..=N 对应预设
                               // 菜单动作的具名常量（match 中常量模式不能含算术）
const MENU_LAYOUT_GRID: usize = MENU_LAYOUT;
const MENU_LAYOUT_LIST: usize = MENU_LAYOUT + 1;
const MENU_ICON_SIZE_SMALL: usize = MENU_ICON_SIZE;
const MENU_ICON_SIZE_MID: usize = MENU_ICON_SIZE + 1;
const MENU_ICON_SIZE_LARGE: usize = MENU_ICON_SIZE + 2;
const MENU_STYLE_GLASS: usize = MENU_STYLE;
const MENU_STYLE_TRANSPARENT: usize = MENU_STYLE + 1;
const MENU_STYLE_COLOR: usize = MENU_STYLE + 2;
/// 右键菜单「添加…」（单一入口：能选中的直接添加，不做文件/文件夹区分）。
const MENU_ADD: usize = 1500;

/// 栅栏边框宽度（DIP）：中粗，按用户要求固定（× scale 变物理像素）。
const MEDIUM_BORDER_WIDTH: f32 = 2.0;

// ---- 控制中心（插件宿主 + 栅栏管理 + 插件管理）布局常量（DIP，× scale 变物理像素）----
/// 控制台面板默认宽度（用户拖边缘缩放后由 `desk.console_size` 覆盖）。
const CONSOLE_W: f32 = 320.0;
/// 控制台面板最小宽/高（缩放钳制，避免缩到无法交互）。
const CONSOLE_MIN_W: f32 = 260.0;
const CONSOLE_MIN_H: f32 = 170.0;
/// 控制台与屏幕右/上边距（未拖动时的默认摆放位置）。
const CONSOLE_MARGIN: f32 = 24.0;
/// 标题栏高度（拖动把手；关闭按钮位于其中）。
const CONSOLE_TITLE_H: f32 = 40.0;
/// 控制中心标签栏高度（待办 / 便签 / 栅栏 / 插件）。
const CONSOLE_TAB_H: f32 = 34.0;
/// 输入行高度（名称行「输入框 + 添加按钮」，详情行同高）。
const CONSOLE_INPUT_H: f32 = 32.0;
/// 名称行与详情行之间的间距。
const CONSOLE_INPUT_GAP: f32 = 8.0;
/// 待办单行高度（二级结构：名称 + 详细信息两行文字）。
const CONSOLE_ROW_H: f32 = 46.0;
/// 待办列表最多同时显示的行数（超出滚动）。
const CONSOLE_MAX_ROWS: usize = 12;
/// 关闭按钮边长。
const CONSOLE_CLOSE_W: f32 = 32.0;
/// 标题栏「切换桌面」按钮宽度。
const CONSOLE_TOGGLE_W: f32 = 86.0;
/// 「添加」按钮宽度。
const CONSOLE_ADD_W: f32 = 56.0;
/// 待办列表距面板左右的内边距。
const CONSOLE_PAD: f32 = 12.0;
/// 折叠胶囊高度（始终可见的「待办 + N」小条）。
const CONSOLE_PILL_H: f32 = 34.0;
/// 栅栏管理页：每行高度。
const CONSOLE_FENCE_ROW_H: f32 = 36.0;
/// 栅栏管理页：最多同时显示的行数（超出滚动）。
const CONSOLE_FENCE_MAX_ROWS: usize = 5;
/// 栅栏管理页：选中栅栏详情区高度。
const CONSOLE_FENCE_DETAIL_H: f32 = 158.0;
/// 插件页：每行高度。
const CONSOLE_PLUGIN_ROW_H: f32 = 48.0;
/// 插件页：最多同时显示的行数。
const CONSOLE_PLUGIN_MAX_ROWS: usize = 7;
/// 便签页：文本卡片高度。
const CONSOLE_NOTES_H: f32 = 168.0;
/// 控制台展开面板最大高度（DIP；内容再多也滚动）。
const CONSOLE_MAX_H: f32 = 640.0;

/// 背景色调预设（标签, RGB 0..1）：菜单项顺序即此处顺序（+1 起）。
const TINT_PRESETS: &[(&str, [f32; 3])] = &[
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
    /// 当前悬停的图标（栅栏下标, 图标下标）；None = 无悬停。
    hover: Option<(usize, usize)>,
    /// 当前选中的图标集合（框选 / Ctrl 单击多选，如资源管理器）。空 = 无选中。
    selected: Vec<(usize, usize)>,
    /// 框选橡皮筋矩形（所属栅栏下标 + 物理像素矩形）；None = 未在框选。
    select_band: Option<(usize, RectF)>,
    /// overlay 窗口句柄（右键菜单 owner / 就地编辑框父窗口）。
    hwnd: HWND,
    /// 就地重命名编辑会话（None = 未在编辑）。
    editing: Option<Editing>,
    /// 控制台（插件宿主）运行时状态：待办输入编辑框 + 待办列表滚动。
    console: ConsoleState,
    /// 控制台面板/待办行动画状态（`AnimTick` 推进；idle 时停止定时器保持 0% CPU）。
    console_anim: ConsoleAnim,
    /// 桌面切换时栅栏整体淡出/淡入补间（None = 无动画，按 `desk.desktop_mode` 取最终值）。
    desktop_fade: Option<PanelTween>,
    /// 控制中心当前标签页下标（与 `console_tab_order` 返回的顺序一致）。
    console_tab: usize,
    /// 栅栏管理页当前选中的栅栏下标。
    selected_fence: usize,
    /// 当前悬停的控制台控件（绘制高亮反馈用）。
    console_hover: Option<ConsoleZone>,
    /// 栅栏拖动/缩放补间（多个栅栏可同时动；结束自动移除）。
    fence_tweens: Vec<FenceTween>,
    /// 图标悬停放大补间（一次只有一个悬停图标）。
    icon_hover: Option<IconHoverAnim>,
    /// 桌面层级（保留句柄副本；「切换桌面」时反复隐藏/恢复真实图标）。
    hierarchy: DesktopHierarchy,
    /// overlay 窗口原始指针（动画定时器的启停需要访问它；与进程存活期一致）。
    overlay_ptr: *mut OverlayWindow,
    /// 内部库文件夹（软件目录下）：粘贴/拖入的文件先物理复制进来，栅栏索引库内副本。
    library: PathBuf,
    /// 插件目录（软件目录下）：可放入 `plugin.json` 清单自由增添外部插件。
    plugins_dir: PathBuf,
    /// 本次事件中新添加图标的位图（`handle_event` 末尾随场景一起上传）。
    pending_uploads: Vec<(u64, IconData)>,
    /// 最近一次用户交互时间（空闲时修剪工作集用；后台 `SyncLibrary` 不计）。
    last_activity: std::time::Instant,
    /// 最近一次工作集修剪时间（限频：空闲时最多每 60s 一次）。
    last_trim: std::time::Instant,
}

/// 就地重命名目标：栅栏内图标 / 栅栏标题本身。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EditTarget {
    Item { fence: usize, icon: usize },
    FenceTitle { fence: usize },
}

/// 就地重命名会话（编辑框已创建；`edit` 是独立置顶弹出框，非 overlay 子窗口——
/// DirectComposition 窗口不合成子 HWND，子控件会不可见）。
struct Editing {
    target: EditTarget,
    edit: HWND,
    /// 编辑框子类化数据（原始窗口过程），句柄存进编辑框 GWLP_USERDATA，
    /// 由子类回调链式转发未处理消息。保持存活到 `DestroyWindow` 之后，
    /// 子类回调在控件销毁期间仍会读取 `orig_proc`/`font`（字段本身不被直接读）。
    #[allow(dead_code)]
    session: Box<EditSession>,
}

/// 编辑框子类会话：原始窗口过程 + 字体（会话结束时释放）。
/// 就地重命名额外携带自动换宽参数：输入时编辑框随文本变宽，不超出 `max_right`。
struct EditSession {
    orig_proc: isize,
    font: HFONT,
    /// 编辑框最小宽度（物理像素，保底点击区）。
    min_w: f32,
    /// 编辑框右缘上限（物理像素，不越过栅栏右缘）。
    max_right: f32,
    /// 标签字号（物理像素，`label_width` 估算文本宽度用）。
    font_size: f32,
    /// 文本内边距（物理像素，与创建时一致）。
    pad: f32,
    /// 编辑框圆角半径（物理像素；区域圆角在尺寸变化后需重放）。
    radius: f32,
}

/// 待办输入编辑框会话（与就地重命名同机制：原始窗口过程 + 字体）。
struct TodoEditSession {
    orig_proc: isize,
    font: HFONT,
}

impl Drop for TodoEditSession {
    fn drop(&mut self) {
        if !self.font.0.is_null() {
            unsafe {
                let _ = DeleteObject(self.font.into());
            }
        }
    }
}

/// 待办输入编辑框（独立置顶弹出框，DirectComposition 不合成子 HWND）。
/// Drop 时销毁窗口（先于 session 释放——字段按声明序析构，窗口销毁期间
/// 子类回调仍需读取 `session.orig_proc`）。
struct TodoEdit {
    hwnd: HWND,
    /// 只保持存活到 Drop 释放字体；回调经 GWLP_USERDATA 访问（同 EditSession）。
    #[allow(dead_code)]
    session: Box<TodoEditSession>,
}

impl Drop for TodoEdit {
    fn drop(&mut self) {
        unsafe {
            let _ = DestroyWindow(self.hwnd);
        }
    }
}

/// 便签编辑会话（多行 EDIT：Enter 换行，失焦保存）。
struct NotesEditSession {
    orig_proc: isize,
    font: HFONT,
}

impl Drop for NotesEditSession {
    fn drop(&mut self) {
        if !self.font.0.is_null() {
            unsafe {
                let _ = DeleteObject(self.font.into());
            }
        }
    }
}

/// 便签编辑框（独立置顶弹出框，机制与待办输入框一致；多行文本）。
struct NotesEdit {
    hwnd: HWND,
    #[allow(dead_code)]
    session: Box<NotesEditSession>,
}

impl Drop for NotesEdit {
    fn drop(&mut self) {
        unsafe {
            let _ = DestroyWindow(self.hwnd);
        }
    }
}

/// 控制台（插件宿主）运行时状态。
struct ConsoleState {
    /// 待办「名称」输入编辑框（None = 尚未创建；面板折叠时仍保留句柄，仅隐藏）。
    name_edit: Option<TodoEdit>,
    /// 待办「详细信息」输入编辑框（二级行；None = 未创建，折叠时仅隐藏）。
    detail_edit: Option<TodoEdit>,
    /// 便签插件编辑框（多行文本；便签页打开时创建并摆放）。
    notes_edit: Option<NotesEdit>,
    /// 待办列表滚动偏移（物理像素；布局时在 [0, scroll_max] 内钳制）。
    scroll: f32,
    /// 栅栏管理页列表滚动偏移（物理像素；行数超出可视区时滚轮滚动）。
    fence_scroll: f32,
}

/// 面板展开/折叠补间（`from→to`，`dur` 秒，ease_out_cubic）。
#[derive(Debug, Clone, Copy)]
struct PanelTween {
    t0: Instant,
    dur: f32,
    from: f32,
    to: f32,
}

/// 待办行动画（入场 / 完成勾选交叉淡化）。
#[derive(Debug, Clone, Copy)]
struct RowEnter {
    t0: Instant,
    dur: f32,
}
#[derive(Debug, Clone, Copy)]
struct RowToggle {
    t0: Instant,
    dur: f32,
}

/// 待办行的动画状态（按行 id 索引）。
#[derive(Debug, Clone, Copy, Default)]
struct RowAnim {
    enter: Option<RowEnter>,
    toggle: Option<RowToggle>,
}

/// 删除中的行（幽灵）：保留原名称/详情/完成态与原始下标，在原槽位淡出，
/// 槽位高度随动画收缩，下方行平滑上滑补位（流体布局，无需坐标回推）。
struct ExitRow {
    id: u64,
    /// 删除前在 `desk.todos` 中的原始下标（恢复原始顺序用）。
    index: usize,
    name: String,
    detail: String,
    done: bool,
    t0: Instant,
    dur: f32,
}

/// 控制台面板动画状态（App 层驱动，overlay `AnimTick` 定时推进）。
struct ConsoleAnim {
    /// 面板展开进度 0..1（0=折叠胶囊，1=完整面板）。已 ease 插值。
    panel: f32,
    panel_tween: Option<PanelTween>,
    rows: HashMap<u64, RowAnim>,
    exiting: Vec<ExitRow>,
    /// 展开动画完成后聚焦输入框（用户点击胶囊/热键展开时置位，一次性）。
    focus_on_expand: bool,
}

impl ConsoleAnim {
    fn new(open: bool) -> Self {
        Self {
            panel: if open { 1.0 } else { 0.0 },
            panel_tween: None,
            rows: HashMap::new(),
            exiting: Vec::new(),
            focus_on_expand: false,
        }
    }

    /// 是否有动画在推进（决定动画定时器启停）。
    fn active(&self) -> bool {
        self.panel_tween.is_some() || !self.rows.is_empty() || !self.exiting.is_empty()
    }
}

/// 栅栏拖动/缩放补间：视觉矩形从 `from` 追赶到 `to`（模型已落到 `to`，
/// 场景渲染用补间值，形成丝滑跟随；补间结束视觉 = 模型）。
#[derive(Debug, Clone, Copy)]
struct FenceTween {
    fence: usize,
    from: Rect,
    to: Rect,
    t0: Instant,
    dur: f32,
}

/// 图标悬停缩放补间（0..1：1 = 完全放大）。
#[derive(Debug, Clone, Copy)]
struct IconHoverAnim {
    fence: usize,
    icon: usize,
    t0: Instant,
    dur: f32,
    /// 补间起点进度（0/1，或上一次动画的当前值——连续进出不跳变）。
    from: f32,
    /// 补间终点进度（1 = 放大，0 = 收回）。
    to: f32,
}

impl IconHoverAnim {
    fn progress(&self, now: Instant) -> f32 {
        match tween_progress(self.t0, self.dur, now) {
            Some(p) => self.from + (self.to - self.from) * ease_out_cubic(p),
            None => self.to,
        }
    }
}

// 编辑框子类回调访问 Runtime 的入口（主线程独占，仅消息处理期间借用）。
thread_local! {
    static EDIT_RUNTIME: RefCell<*const RefCell<Runtime>> = const { RefCell::new(std::ptr::null()) };
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

    // 全部使用物理像素，必须声明进程 DPI 感知
    unsafe {
        let _ = SetProcessDPIAware();
    }
    // COM：图标枚举/提取需要（APARTMENTTHREADED）
    let _com = sylva_shell::com::init();

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

fn run(data_dir: &std::path::Path) -> sylva_core::Result<()> {
    // M0 桌面状态：加载/校验/回写，确保配置目录就绪
    let store = ConfigStore::new(data_dir.to_path_buf());
    let mut desk = store.load()?;
    desk.validate();
    // 内部库文件夹（软件目录下）：粘贴/拖入的文件先物理复制进来，栅栏索引库内副本。
    // 库内文件被外部删除时，栅栏对应项同步移除（定时器 `SyncLibrary` 与启动时各清一次）。
    let library_dir = data_dir.join("library");
    let _ = std::fs::create_dir_all(&library_dir);
    let plugins_dir = data_dir.join("plugins");
    let _ = std::fs::create_dir_all(&plugins_dir);
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
    let dpi_scale = unsafe { GetDpiForSystem() } as f32 / 96.0;
    theme.scale = dpi_scale;
    theme.title.size *= dpi_scale;
    theme.label.size *= dpi_scale;
    theme.icon_size *= dpi_scale;
    theme.icon_gap *= dpi_scale;
    theme.icon_caption_gap *= dpi_scale;
    theme.fence_padding *= dpi_scale;
    theme.fence_corner_radius *= dpi_scale;
    theme.title_padding_bottom *= dpi_scale;
    theme.caption_max_width *= dpi_scale;
    theme.list_row_gap *= dpi_scale;
    theme.list_label_gap *= dpi_scale;
    tracing::info!(
        dpi = unsafe { GetDpiForSystem() },
        scale = dpi_scale,
        "主题按 DPI 缩放"
    );
    let compositor = Compositor::new(
        device,
        overlay.hwnd,
        overlay.x as f32,
        overlay.y as f32,
        vw,
        vh,
        theme.clone(),
    )
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
        hover: None,
        selected: Vec::new(),
        select_band: None,
        hwnd: overlay.hwnd,
        editing: None,
        console: ConsoleState {
            name_edit: None,
            detail_edit: None,
            notes_edit: None,
            scroll: 0.0,
            fence_scroll: 0.0,
        },
        console_anim: ConsoleAnim::new(console_open),
        desktop_fade: None,
        console_tab: 0,
        selected_fence: 0,
        console_hover: None,
        fence_tweens: Vec::new(),
        icon_hover: None,
        hierarchy,
        overlay_ptr,
        library: library_dir,
        plugins_dir,
        pending_uploads: Vec::new(),
        last_activity: std::time::Instant::now(),
        last_trim: std::time::Instant::now(),
    };
    // 插件目录扫描：外部 plugin.json 清单并入注册表（内置插件始终存在）。
    scan_plugins_dir(&mut rt);
    // 控制台（插件宿主）：若上次退出时面板为开，则重建待办输入编辑框并按布局摆放。
    // 只在「用户打开面板」时聚焦输入框（启动时从不抢焦点）。
    sync_console_edit(&mut rt);
    // 库同步：启动时清理一次——库文件夹里已在外部被删除的文件，对应栅栏项一并移除
    if reconcile_library(&mut rt) {
        let _ = rt.store.save(&rt.desk);
    }
    // 高度策略：`bounds.h == 0` 表示未手动缩放，按内容自适应（增删应用自动长高）。
    // 不在此冻结高度——用户拖边缘/角缩放后才落为具体值。
    let scene = build_scene(&mut rt, Instant::now());
    let upload_refs: Vec<(u64, &sylva_shell::icons::IconData)> =
        uploads.iter().map(|(id, d)| (*id, d)).collect();
    rt.compositor
        .present(&scene, &upload_refs)
        .map_err(|e| sylva_core::CoreError::Render(e.to_string()))?;
    let model = hit_model_from(&rt.theme, &scene, &rt.desk);
    memory::report("首帧呈现后");
    // 启动期一次性分配已就绪：把不再活跃的内存页换出工作集（D3D/场景构建等），
    // 降低常驻内存。GPU 侧资源由驱动管理不受影响，用到时自动换回。
    memory::trim();
    memory::report("工作集修剪后");

    // 6) 事件回路：App 处理交互 → 重绘 → 返回新命中模型（overlay 据此更新区域）
    let runtime = Rc::new(RefCell::new(rt));
    // 就地编辑框子类回调通过该指针借用 Runtime（Rc 使 RefCell 地址稳定，进程存活期间有效）
    EDIT_RUNTIME.with(|c| *c.borrow_mut() = &*runtime as *const RefCell<Runtime>);
    let runtime2 = runtime.clone();
    overlay.set_event_handler(Box::new(move |ev| {
        // 模态菜单 / 属性页 / Shell 动词执行期间，嵌套消息循环会派发定时器、悬停、
        // 注入等其它事件再入本回调。此时外层 `handle_event` 仍持有 `Runtime` 的可变
        // 借用，再入必然 RefCell 借用冲突崩溃——一律丢弃再入事件，保持当前命中模型。
        if HANDLING.with(|h| h.replace(true)) {
            return None;
        }
        let _reentry = ReentryGuard;
        Some(handle_event(&mut runtime2.borrow_mut(), ev))
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

    tracing::info!("Sylva 已就绪：拖边缘/角缩放、标题栏拖动、双击打开、文件拖入/粘贴添加；Ctrl+Alt+T 控制台（待办），Ctrl+Shift+F10 退出");
    run_message_loop();

    // 退出前把便签编辑框内容落盘（失焦已保存，这里兜底）。
    {
        let mut rt = runtime.borrow_mut();
        save_notes(&mut rt);
    }
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

/// 除 `skip` 外其它栅栏的当前边界（碰撞/吸附的锚点集合）。
fn other_bounds(rt: &Runtime, skip: usize) -> Vec<Rect> {
    rt.desk
        .fences
        .iter()
        .enumerate()
        .filter(|(i, _)| *i != skip)
        .map(|(_, f)| f.bounds)
        .collect()
}

/// 虚拟屏幕边界（物理像素；栅栏活动范围）。
fn screen_rect(rt: &Runtime) -> Rect {
    Rect::new(0.0, 0.0, rt.vw, rt.vh)
}

/// 该事件是否代表「用户真正点/拖了某处」（就地重命名编辑打开时据此提交）。
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

/// 处理一个用户交互事件：更新布局 → 重绘 → 生成新命中模型。
fn handle_event(rt: &mut Runtime, ev: OverlayEvent) -> HitModel {
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
    if rt.editing.is_some() && is_popup_dismiss_event(&ev) {
        finish_edit_window(rt, true);
    }
    match ev {
        OverlayEvent::FenceMove { fence, pos } => {
            // 拖动标题栏：不交叉（无重叠推挤）+ 磁吸吸附 + 限制在虚拟屏幕内
            let (x, y) = pos;
            let from = fence_visual_rect(rt, fence);
            let cur = rt.desk.fences.get(fence).map(|f| (f.bounds.w, f.bounds.h));
            if let Some((w, h)) = cur {
                let others = other_bounds(rt, fence);
                let screen = screen_rect(rt);
                let cand = Rect::new(x, y, w, h);
                let out = settle_move(&cand, &others, &screen, FENCE_GAP);
                if let Some(f) = rt.desk.fences.get_mut(fence) {
                    f.bounds.x = out.x;
                    f.bounds.y = out.y;
                }
                // 丝滑跟随：模型落到目标，视觉矩形从当前位置追过去
                let to = Rect::new(out.x, out.y, w, h);
                set_fence_tween(
                    rt,
                    FenceTween {
                        fence,
                        from,
                        to,
                        t0: Instant::now(),
                        dur: 0.12,
                    },
                );
            }
        }
        OverlayEvent::FenceResize { fence, zone, rect } => {
            // 拖边缘/角标：只动可动边，被其它栅栏挡住时停在交界边（不侵入），
            // 边接近时吸附对齐；锚定边不动，最小尺寸/屏幕边界约束在算法内完成。
            let (nx, ny, nw, nh) = rect;
            let from = fence_visual_rect(rt, fence);
            let others = other_bounds(rt, fence);
            let screen = screen_rect(rt);
            let free = match zone {
                ResizeZone::Right => FreeSides::Right,
                ResizeZone::Bottom => FreeSides::Bottom,
                ResizeZone::BottomRight => FreeSides::BottomRight,
                ResizeZone::Left => FreeSides::Left,
                ResizeZone::BottomLeft => FreeSides::BottomLeft,
                ResizeZone::TopRight => FreeSides::TopRight,
            };
            let cand = Rect::new(nx, ny, nw, nh);
            let out = settle_resize(
                &cand,
                &others,
                &screen,
                free,
                MIN_FENCE_W,
                MIN_FENCE_H,
                FENCE_GAP,
            );
            if let Some(f) = rt.desk.fences.get_mut(fence) {
                f.bounds = out;
            }
            set_fence_tween(
                rt,
                FenceTween {
                    fence,
                    from,
                    to: out,
                    t0: Instant::now(),
                    dur: 0.12,
                },
            );
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
                    FenceLayout::Grid => (f.appearance.icon_size + f.appearance.gap) * s,
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
                save_notes(rt);
                rt.desk.console_open = false;
                if let Err(e) = rt.store.save(&rt.desk) {
                    tracing::warn!("控制台状态持久化失败: {e}");
                }
                start_panel_tween(rt, 0.0);
                layout_console_edit(rt);
            }
            ConsoleZone::Expand => {
                // 点击胶囊展开面板，动画完成聚焦输入框（可直接输入）
                rt.desk.console_open = true;
                if let Err(e) = rt.store.save(&rt.desk) {
                    tracing::warn!("控制台状态持久化失败: {e}");
                }
                ensure_console_edit(rt);
                rt.console_anim.focus_on_expand = true;
                start_panel_tween(rt, 1.0);
                layout_console_edit(rt);
            }
            ConsoleZone::Input => {
                // 点击名称输入框：给编辑框键盘焦点（独立弹出框需先到前台）
                focus_console_edit(rt);
            }
            ConsoleZone::DetailInput => {
                // 点击详细信息输入框：聚焦详情编辑框
                focus_console_detail(rt);
            }
            ConsoleZone::Add => {
                // 点击「添加」按钮：提交当前输入文本为一条待办
                commit_todo_from_edit(rt);
            }
            ConsoleZone::Tab(i) => {
                // 切换标签页：先保存便签文本，再切页并摆放/隐藏对应编辑框
                save_notes(rt);
                let order = console_tab_order(rt);
                rt.console_tab = i.min(order.len().saturating_sub(1));
                layout_console_edit(rt);
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
            ConsoleZone::PluginToggle(i) => {
                if let Some(p) = rt.desk.plugins.get_mut(i) {
                    p.enabled = !p.enabled;
                }
                // 便签被禁用且正停留在便签页 → 切回待办页
                let order = console_tab_order(rt);
                if rt.console_tab >= order.len() {
                    rt.console_tab = 0;
                }
                save_notes(rt);
                layout_console_edit(rt);
                let _ = rt.store.save(&rt.desk);
            }
            ConsoleZone::OpenPluginDir => {
                let _ = std::fs::create_dir_all(&rt.plugins_dir);
                let _ = std::process::Command::new("explorer")
                    .arg(&rt.plugins_dir)
                    .spawn();
            }
            ConsoleZone::NotesEdit => {
                focus_notes_edit(rt);
            }
            ConsoleZone::Toggle(i) => {
                // 勾选/取消勾选：切换完成状态 + 勾选交叉淡化动画
                if let Some(t) = rt.desk.todos.get_mut(i) {
                    t.done = !t.done;
                    let id = t.id;
                    let ra = rt.console_anim.rows.entry(id).or_default();
                    ra.toggle = Some(RowToggle {
                        t0: Instant::now(),
                        dur: 0.16,
                    });
                    arm_anim_timer(rt);
                    if let Err(e) = rt.store.save(&rt.desk) {
                        tracing::warn!("待办持久化失败: {e}");
                    }
                }
            }
            ConsoleZone::Delete(i) => {
                // 删除该条待办：立即从模型移除，同时记录「幽灵」行做淡出+槽位塌缩动画
                if i < rt.desk.todos.len() {
                    let removed = rt.desk.todos.remove(i);
                    let orig = exit_original_index(&rt.console_anim.exiting, i);
                    rt.console_anim.exiting.push(ExitRow {
                        id: removed.id,
                        index: orig,
                        name: removed.name.clone(),
                        detail: removed.detail.clone(),
                        done: removed.done,
                        t0: Instant::now(),
                        dur: 0.2,
                    });
                    rt.console_anim.rows.remove(&removed.id);
                    arm_anim_timer(rt);
                    if let Err(e) = rt.store.save(&rt.desk) {
                        tracing::warn!("待办持久化失败: {e}");
                    }
                }
            }
        },
        OverlayEvent::ConsoleScroll { delta } => {
            // 滚轮按当前页路由：待办页滚待办列表，栅栏页滚栅栏列表
            let order = console_tab_order(rt);
            let active = order
                .get(rt.console_tab)
                .copied()
                .unwrap_or(ConsoleTab::Todo);
            if active == ConsoleTab::Fences {
                let s = rt.theme.scale;
                let step = CONSOLE_FENCE_ROW_H * s;
                let max = fence_scroll_max(rt);
                rt.console.fence_scroll =
                    (rt.console.fence_scroll - (delta as f32 / 120.0) * step).clamp(0.0, max);
            } else {
                // 滚轮滚动待办列表：正=向上，负=向下；上下界由 console_scroll_max 钳制
                let step = CONSOLE_ROW_H * rt.theme.scale;
                let max = console_scroll_max(rt);
                rt.console.scroll =
                    (rt.console.scroll - (delta as f32 / 120.0) * step).clamp(0.0, max);
            }
        }
        OverlayEvent::ConsoleHover { zone } => {
            // 控件悬停：存下供下一帧绘制高亮（仅展开面板内上报）
            rt.console_hover = zone;
        }
        OverlayEvent::ConsoleMove { pos } => {
            // 拖动标题栏移动面板：记录左上角（原始坐标 + 增量，避免粘连）
            rt.desk.console_pos = Some(Vec2 { x: pos.0, y: pos.1 });
            layout_console_edit(rt);
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
            layout_console_edit(rt);
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
                ensure_console_edit(rt);
                rt.console_anim.focus_on_expand = true;
                start_panel_tween(rt, 1.0);
            } else {
                start_panel_tween(rt, 0.0);
            }
            layout_console_edit(rt);
        }
        OverlayEvent::EditCommitted => {
            // 就地重命名提交后的注入事件：数据已改好，这里只需让尾部重建场景。
        }
        OverlayEvent::AnimTick => {
            // 动画帧：推进面板/行补间；全部结束后停用定时器，回到空闲 0% CPU。
            if !advance_anim(rt) {
                unsafe { (*rt.overlay_ptr).set_anim_active(false) };
            }
            layout_console_edit(rt); // 展开到一半才显示输入框，跟随 panel 进度
        }
        OverlayEvent::SyncLibrary => {
            // 库内文件被外部删除 → 栅栏对应项同步移除（有变化才持久化）
            if reconcile_library(rt) {
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
        }
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
    hit_model_from(&rt.theme, &scene, &rt.desk)
}

/// 双击/右键打开：按显式成员列表反查并启动。
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

/// 按 id 移出栅栏：内部库项直接删除（引用，库文件由「删除」动作负责），桌面图标移回未分组区。
fn remove_fence_icon_by_id(rt: &mut Runtime, id: &String) {
    if rt.desk.icons.get(id).map(|i| i.added).unwrap_or(false) {
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

/// 删除选中项：内部库项连同库内文件一并删除（库内删 → 栅栏项消失，与同步机制一致）；
/// 桌面图标移出栅栏（回未分组区，不碰源文件）。
fn delete_selected(rt: &mut Runtime) {
    let ids = selected_ids(rt);
    for id in &ids {
        let added = rt.desk.icons.get(id).map(|ic| ic.added).unwrap_or(false);
        if added {
            if let Some(p) = rt.desk.icons.get(id).and_then(|ic| ic.path.clone()) {
                let pp = Path::new(&p);
                if is_inside_library(rt, pp) {
                    if pp.is_dir() {
                        let _ = std::fs::remove_dir_all(pp);
                    } else {
                        let _ = std::fs::remove_file(pp);
                    }
                }
            }
            remove_icon_entirely(rt, id);
        } else {
            rt.desk.move_icon(id, None);
        }
    }
    let _ = rt.store.save(&rt.desk);
}

/// 开始就地重命名（Explorer 风格，无弹窗）：在图标标签 / 栅栏标题上创建编辑框。
/// 编辑框是独立置顶弹出框（定位用虚拟屏幕坐标 = overlay 客户端坐标），支持 IME 中文；
/// Enter 提交 / Esc 取消 / 失焦提交，提交后改磁盘真实文件名并重建元数据与位图。
fn start_inplace_rename(rt: &mut Runtime, target: EditTarget) {
    // 已有编辑会话先销毁（被新会话替换，不提交）
    if let Some(ed) = rt.editing.take() {
        unsafe {
            let _ = DestroyWindow(ed.edit);
        }
    }

    // 初始文本 + 定位矩形（物理像素）
    let (current, rect) = match target {
        EditTarget::Item { fence, icon } => {
            let Some(r) = item_label_rect(rt, fence, icon) else {
                tracing::warn!(fence, icon, "无法定位图标标签，跳过就地改名");
                return;
            };
            let Some(name) = item_name(rt, fence, icon) else {
                return;
            };
            (name, r)
        }
        EditTarget::FenceTitle { fence } => {
            let Some(name) = rt.desk.fences.get(fence).map(|f| {
                f.title
                    .clone()
                    .unwrap_or_else(|| format!("栅栏 {}", fence + 1))
            }) else {
                return;
            };
            (name, fence_title_rect(rt, fence))
        }
    };

    // 编辑框尺寸：至少容纳当前文本 + 内边距，不小于标签区；不超出栅栏右缘。
    // 高度**恰好等于标签区**——多出的几像素会被暗色背景填满，形成「底部黑条」。
    let s = rt.theme.scale;
    let text_w = label_width(&current, rt.theme.label.size) + 18.0 * s;
    let mut ew = rect.w.max(text_w).max(90.0 * s);
    let eh = rect.h;
    let max_right = match target {
        EditTarget::Item { fence, .. } => rt
            .desk
            .fences
            .get(fence)
            .map(|f| f.bounds.x + f.bounds.w - rt.theme.fence_padding)
            .unwrap_or(rect.x + ew),
        EditTarget::FenceTitle { .. } => rect.x + rect.w,
    };
    ew = ew.min((max_right - rect.x).max(40.0 * s));

    let hinst = match unsafe { GetModuleHandleW(None) } {
        Ok(m) => HINSTANCE(m.0),
        Err(e) => {
            tracing::warn!("获取模块句柄失败: {e}");
            return;
        }
    };
    let current_w: Vec<u16> = current.encode_utf16().chain(std::iter::once(0)).collect();
    // 用独立置顶弹出框（非 overlay 子窗口）承载编辑：DirectComposition 窗口
    // 不合成子 HWND，子控件会不可见；置顶弹出框按普通窗口渲染，IME/键盘焦点正常，
    // 定位用虚拟屏幕坐标（overlay 客户端坐标 = 虚拟屏幕坐标，直接可用）。
    let edit = match unsafe {
        CreateWindowExW(
            // 去 WS_EX_CLIENTEDGE（纯白凹陷底）——暗色背景由 overlay 的 WM_CTLCOLOREDIT 提供；
            // 去 WS_EX_TOPMOST（层级 bug）——改用 WS_EX_TOOLWINDOW，随 overlay 所在桌面层，
            // 不再浮到其它软件之上，也不进任务栏/Alt+Tab。
            WS_EX_TOOLWINDOW,
            PCWSTR(wide("EDIT").as_ptr()),
            PCWSTR(current_w.as_ptr()),
            WS_POPUP
                | WS_VISIBLE
                | WINDOW_STYLE(ES_AUTOHSCROLL as u32)
                | WINDOW_STYLE(ES_LEFT as u32),
            rect.x as i32,
            rect.y as i32,
            ew as i32,
            eh as i32,
            // owner = overlay 窗口：WM_CTLCOLOREDIT 到达 overlay，由它填暗色背景
            Some(rt.hwnd),
            None,
            Some(hinst),
            None,
        )
    } {
        Ok(h) => h,
        Err(e) => {
            tracing::warn!("创建重命名编辑框失败: {e}");
            return;
        }
    };

    // 子类化：捕获 Enter / Esc / 失焦 + 输入随文本换宽；原始过程 + 字体存进 EditSession
    let font = create_edit_font(rt.theme.label.size);
    let session = Box::new(EditSession {
        orig_proc: unsafe {
            SetWindowLongPtrW(edit, GWL_WNDPROC, edit_subclass_proc as *const () as isize)
        },
        font,
        min_w: 90.0 * s,
        max_right,
        font_size: rt.theme.label.size,
        pad: 18.0 * s,
        radius: 8.0 * s,
    });
    let raw = &*session as *const EditSession;
    unsafe {
        let _ = SetWindowLongPtrW(edit, GWLP_USERDATA, raw as isize);
        let _ = SendMessageW(
            edit,
            WM_SETFONT,
            Some(WPARAM(font.0 as usize)),
            Some(LPARAM(1)),
        );
        let _ = SendMessageW(edit, EM_SETSEL, Some(WPARAM(0)), Some(LPARAM(-1)));
    }
    // 编辑框视觉统一：无系统黑边 + 圆角；再稳定获取键盘焦点
    style_edit_window(edit, 8.0 * rt.theme.scale);
    focus_popup_edit(edit);
    rt.editing = Some(Editing {
        target,
        edit,
        session,
    });
    tracing::info!(target = ?target, "开始就地重命名");
}

/// 结束就地重命名会话：销毁编辑框，提交则应用改名；内容真变化时注入一次重绘。
fn finish_edit_window(rt: &mut Runtime, commit: bool) {
    let Some(ed) = rt.editing.take() else {
        return;
    };
    let target = ed.target;
    let text = if commit {
        edit_text(ed.edit)
    } else {
        String::new()
    };
    // 销毁编辑框：期间 WM_KILLFOCUS 触发子类回调，但 `ed.session` 仍存活（本作用域持有），
    // 且子类里 try_borrow 失败会自动忽略——幂等，不会二次进入。
    unsafe {
        let _ = DestroyWindow(ed.edit);
    }
    drop(ed);

    if commit && apply_rename(rt, target, &text) {
        inject_rebuild(rt);
    }
}

/// 应用重命名结果：返回内容是否真的变化（变化才触发重绘）。
fn apply_rename(rt: &mut Runtime, target: EditTarget, new_name: &str) -> bool {
    let new_name = new_name.trim();
    if new_name.is_empty() {
        return false;
    }
    match target {
        EditTarget::FenceTitle { fence } => {
            let Some(f) = rt.desk.fences.get_mut(fence) else {
                return false;
            };
            let old = f
                .title
                .clone()
                .unwrap_or_else(|| format!("栅栏 {}", fence + 1));
            if old == new_name {
                return false;
            }
            f.title = Some(new_name.to_string());
            let _ = rt.store.save(&rt.desk);
            tracing::info!(fence, name = new_name, "栅栏改名");
            true
        }
        EditTarget::Item { fence, icon } => commit_icon_rename(rt, fence, icon, new_name),
    }
}

/// 重命名栅栏内图标（编辑程序名字）：改的是磁盘上的真实文件名（快捷方式保持
/// `.lnk`/`.url`/`.appref-ms` 扩展名），改名后重建元数据、位图与引用。
/// 返回内容是否真的变化。
fn commit_icon_rename(rt: &mut Runtime, fence: usize, icon: usize, new_name: &str) -> bool {
    let (id, path, current) = match rt.desk.fences.get(fence).and_then(|f| f.icon_ids.get(icon)) {
        Some(id) => match rt.desk.icons.get(id) {
            Some(ic) => (id.clone(), ic.path.clone(), ic.display_name.clone()),
            None => return false,
        },
        None => return false,
    };
    if new_name == current {
        return false;
    }
    let Some(old_path) = path else {
        tracing::warn!(id = %id, "虚拟项（无路径）无法改名");
        return false;
    };
    let Some(new_path) = new_path_for_rename(&old_path, new_name) else {
        tracing::warn!(old_path, "无法计算新路径");
        return false;
    };
    let new_path_str = new_path.to_string_lossy().into_owned();
    if new_path_str == old_path {
        return false;
    }
    // 磁盘改名：与原文件夹名一致——Windows 资源管理器也是这样直接改文件
    if let Err(e) = std::fs::rename(&old_path, &new_path) {
        tracing::warn!(old_path, new_path = %new_path_str, "文件改名失败: {e}");
        return false;
    }
    // 重建 DesktopItem（新路径 → 新 id/显示名/类别），替换 items 池中的项
    let new_item = match sylva_shell::items::item_from_path(&new_path_str) {
        Ok(it) => it,
        Err(e) => {
            tracing::warn!(new_path = %new_path_str, "重建图标项失败（文件已改名，重启后重新识别）: {e}");
            return false;
        }
    };
    let new_display = new_item.display_name.clone();
    let new_id = new_item.id.clone();
    if let Some(idx) = rt.item_index.remove(&id) {
        rt.items[idx] = new_item;
        rt.item_index.insert(new_id.clone(), idx);
    }
    // 更新元数据：旧 id 换新 id，path/显示名/详情按新路径补齐
    if let Some(mut ic) = rt.desk.icons.remove(&id) {
        ic.id = new_id.clone();
        ic.display_name = new_display;
        ic.path = Some(new_path_str.clone());
        sylva_core::details::enrich(&mut ic, &new_path_str);
        rt.desk.icons.insert(new_id.clone(), ic);
    }
    // 替换栅栏成员与自由区引用
    for f in &mut rt.desk.fences {
        if let Some(pos) = f.icon_ids.iter().position(|x| x == &id) {
            f.icon_ids[pos] = new_id.clone();
        }
    }
    if let Some(pos) = rt.desk.free_icons.iter().position(|x| x == &id) {
        rt.desk.free_icons[pos] = new_id.clone();
    }
    // 新 id → 新位图槽（不复用旧槽，避免与既有槽冲突），重新提取图标
    rt.bitmap_ids.remove(&id);
    let slot = rt.bitmap_ids.values().copied().max().unwrap_or(0) + 1;
    if let Some(idx) = rt.item_index.get(&new_id).copied() {
        match sylva_shell::icons::extract_icon(&rt.items[idx], ICON_EXTRACT_SIZE) {
            Ok(data) => {
                rt.bitmap_ids.insert(new_id.clone(), slot);
                rt.pending_uploads.push((slot, data));
            }
            Err(e) => tracing::warn!(new_path = %new_path_str, "改名后图标提取失败: {e}"),
        }
    }
    let _ = rt.store.save(&rt.desk);
    tracing::info!(old_path, new_path = %new_path_str, "图标改名");
    true
}

/// 注入一次「仅重绘」事件：让 `handle_event` 尾部重建场景与命中模型。
fn inject_rebuild(rt: &mut Runtime) {
    let ev = Box::new(OverlayEvent::EditCommitted);
    unsafe {
        let _ = PostMessageW(
            Some(rt.hwnd),
            WM_SYLVA_INJECT,
            WPARAM(0),
            LPARAM(Box::into_raw(ev) as isize),
        );
    }
}

/// 图标标签文本（当前显示名）。
fn item_name(rt: &Runtime, fence: usize, icon: usize) -> Option<String> {
    rt.desk
        .fences
        .get(fence)
        .and_then(|f| f.icon_ids.get(icon))
        .and_then(|id| rt.desk.icons.get(id))
        .map(|ic| ic.display_name.clone())
}

/// 图标标签的绘制矩形（物理像素，虚拟屏幕坐标）：就地编辑框的定位基准。
/// 与 `layout_fence` / `grid_icons` / `list_icons` 的几何保持一致。
fn item_label_rect(rt: &Runtime, fence: usize, icon: usize) -> Option<RectF> {
    let f = rt.desk.fences.get(fence)?;
    f.icon_ids.get(icon)?;
    let s = rt.theme.scale;
    let pad = f.appearance.padding * s;
    let title_block_h = rt.theme.title.size * 1.6 + rt.theme.title_padding_bottom;
    let content_top = f.bounds.y + pad + title_block_h + pad;
    let content_left = f.bounds.x + pad;
    let inner_w = (f.bounds.w - 2.0 * pad).max(1.0);
    match f.appearance.layout {
        FenceLayout::Grid => {
            let icon_size = f.appearance.icon_size * s;
            let cell_w = icon_size + f.appearance.gap * s;
            let row_h = icon_size + rt.theme.icon_caption_gap + rt.theme.label.size * 1.6;
            let cols = ((inner_w / cell_w).floor() as usize).max(1);
            let ix = content_left + (icon % cols) as f32 * cell_w;
            let iy = content_top + (icon / cols) as f32 * row_h - f.scroll;
            Some(RectF {
                x: ix - 2.0,
                y: iy + icon_size + rt.theme.icon_caption_gap,
                w: icon_size + 4.0,
                h: rt.theme.label.size * 1.6,
            })
        }
        FenceLayout::List => {
            let label_h = rt.theme.label.size * 1.6;
            let list_icon = LIST_ICON_SIZE * s;
            let row_h = list_icon.max(label_h) + rt.theme.list_row_gap;
            let header_h = label_h + 8.0 * s;
            let type_w = LIST_TYPE_W * s;
            let mod_w = LIST_MOD_W * s;
            let size_w = LIST_SIZE_W * s;
            let col_gap = LIST_COL_GAP * s;
            let name_w = (inner_w - col_gap * 3.0 - type_w - mod_w - size_w).max(60.0 * s);
            let iy = content_top + header_h + icon as f32 * row_h - f.scroll;
            Some(RectF {
                x: content_left + list_icon + rt.theme.list_label_gap,
                y: iy + (list_icon - label_h) / 2.0,
                w: name_w,
                h: label_h,
            })
        }
    }
}

/// 栅栏标题文本矩形（就地编辑框的定位基准）。
fn fence_title_rect(rt: &Runtime, fence: usize) -> RectF {
    let f = &rt.desk.fences[fence];
    let pad = rt.theme.fence_padding;
    RectF {
        x: f.bounds.x + pad,
        y: f.bounds.y + pad,
        w: (f.bounds.w - 2.0 * pad).max(1.0),
        h: rt.theme.title.size * 1.6,
    }
}

/// 编辑框子类窗口过程：Enter 提交 / Esc 取消 / 失焦提交 + 输入随文本换宽，
/// 其余消息链式转发给原过程。
unsafe extern "system" fn edit_subclass_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let sess = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const EditSession;
    if sess.is_null() {
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    }
    let orig = (*sess).orig_proc;
    match msg {
        // 点击编辑框直接激活窗口：独立弹出框点击后立刻可输入，不依赖焦点抢注
        WM_MOUSEACTIVATE => return LRESULT(MA_ACTIVATE as isize),
        WM_KEYDOWN => match wparam.0 as u32 {
            v if v == VK_RETURN.0 as u32 => {
                finish_edit_subclass(hwnd, true);
                return LRESULT(0);
            }
            v if v == VK_ESCAPE.0 as u32 => {
                finish_edit_subclass(hwnd, false);
                return LRESULT(0);
            }
            _ => {}
        },
        // 键盘输入后随文本换宽（普通键盘字符）
        WM_KEYUP => {
            resize_edit_to_fit(hwnd, &*sess);
        }
        // 中文 IME 组词完成时同样换宽（拼音/候选上屏后文本变化）
        WM_IME_ENDCOMPOSITION => {
            resize_edit_to_fit(hwnd, &*sess);
        }
        // 失焦即提交（资源管理器行为）；Enter 提交销毁时产生的失焦幂等跳过
        WM_KILLFOCUS => {
            finish_edit_subclass(hwnd, true);
            return LRESULT(0);
        }
        _ => {}
    }
    let orig_fn: unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT =
        std::mem::transmute(orig);
    CallWindowProcW(Some(orig_fn), hwnd, msg, wparam, lparam)
}

/// 就地重命名编辑框随文本自动换宽：目标宽 = 文本估算宽 + 内边距，
/// 在 [min_w, max_right - left] 内钳制；左上角不动，只改宽。
fn resize_edit_to_fit(hwnd: HWND, sess: &EditSession) {
    let text = edit_text(hwnd);
    let want = label_width(text.trim(), sess.font_size) + sess.pad;
    let mut rc = RECT::default();
    unsafe {
        let _ = GetWindowRect(hwnd, &mut rc);
    }
    let left = rc.left as f32;
    let w = want
        .max(sess.min_w)
        .min((sess.max_right - left).max(sess.min_w));
    unsafe {
        let _ = MoveWindow(
            hwnd,
            rc.left,
            rc.top,
            w.max(1.0) as i32,
            (rc.bottom - rc.top).max(1),
            true,
        );
    }
    style_edit_window(hwnd, sess.radius);
}

/// 编辑框子类回调入口：借用 Runtime 结束编辑。正在被 `handle_event` 处理时跳过
/// （此时编辑会话已被 `finish_edit_window` 取走，幂等）。
fn finish_edit_subclass(hwnd: HWND, commit: bool) {
    let rtp = EDIT_RUNTIME.with(|c| *c.borrow());
    if rtp.is_null() {
        return;
    }
    let cell = unsafe { &*rtp };
    let Ok(mut rt) = cell.try_borrow_mut() else {
        return;
    };
    // 只结束指向本编辑框的会话
    let Some(ed) = rt.editing.as_ref() else {
        return;
    };
    if ed.edit != hwnd {
        return;
    }
    finish_edit_window(&mut rt, commit);
}

/// 读取编辑框文本。
fn edit_text(hwnd: HWND) -> String {
    let mut buf = vec![0u16; 1024];
    let n = unsafe { GetWindowTextW(hwnd, &mut buf) };
    String::from_utf16_lossy(&buf[..n as usize])
}

/// 创建与标签字号匹配的中文字体（Microsoft YaHei UI），供编辑框使用。
fn create_edit_font(font_size: f32) -> HFONT {
    let mut lf = LOGFONTW {
        lfHeight: -(font_size as i32 + 2),
        lfWidth: 0,
        lfEscapement: 0,
        lfOrientation: 0,
        lfWeight: 400,
        lfItalic: 0,
        lfUnderline: 0,
        lfStrikeOut: 0,
        lfCharSet: FONT_CHARSET(1), // DEFAULT_CHARSET
        lfOutPrecision: FONT_OUTPUT_PRECISION(0),
        lfClipPrecision: FONT_CLIP_PRECISION(0),
        lfQuality: FONT_QUALITY(5), // CLEARTYPE_QUALITY
        lfPitchAndFamily: 0,
        lfFaceName: [0u16; 32],
    };
    let face = "Microsoft YaHei UI".encode_utf16();
    for (i, c) in face.take(31).enumerate() {
        lf.lfFaceName[i] = c;
    }
    unsafe { CreateFontIndirectW(&lf) }
}

/// 给编辑框应用统一视觉（重命名 / 待办输入共用）：无系统黑边 + 圆角。
///
/// - Win11：DWM 圆角 + 边框色设为面板底色（系统边框因此不可见，消除「黑边」）；
/// - Win10：`DWM` 圆角不可用，用 `SetWindowRgn` 区域圆角兜底（两个版本一致圆角）；
/// - 编辑框底色由 overlay 的 `WM_CTLCOLOREDIT` 返回的画刷填充，与面板同色。
fn style_edit_window(hwnd: HWND, radius: f32) {
    const DWMWCP_ROUND: u32 = 2;
    // 面板填充色 [0.062,0.086,0.133] ≈ RGB(16,22,34)；DWM 边框用同色 → 视觉无边框
    let border = COLORREF(0x00_22_16_10);
    unsafe {
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_WINDOW_CORNER_PREFERENCE,
            &DWMWCP_ROUND as *const u32 as *const core::ffi::c_void,
            std::mem::size_of::<u32>() as u32,
        );
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_BORDER_COLOR,
            &border as *const COLORREF as *const core::ffi::c_void,
            std::mem::size_of::<COLORREF>() as u32,
        );
    }
    // 区域圆角（Win10/11 全生效）：窗口尺寸取当前矩形，SetWindowRgn 接管区域所有权。
    let mut rc = RECT::default();
    unsafe {
        let _ = GetWindowRect(hwnd, &mut rc);
    }
    let w = (rc.right - rc.left).max(1);
    let h = (rc.bottom - rc.top).max(1);
    let r = (radius.round().max(2.0) as i32).min(w.min(h) / 2);
    let rgn = unsafe { CreateRoundRectRgn(0, 0, w, h, r, r) };
    unsafe {
        let _ = SetWindowRgn(hwnd, Some(rgn), true);
    }
}

/// 给独立弹出编辑框稳定的键盘焦点：先确保窗口在前台，再 SetFocus。
/// 进程非前台时 `SetForegroundWindow` 会被系统拒绝，退回 AttachThreadInput 强制激活，
/// 保证点击输入行后可直接打字（修复「点了输入框没反应」的焦点链路）。
fn focus_popup_edit(hwnd: HWND) {
    unsafe {
        let cur = GetCurrentThreadId();
        let tgt = GetWindowThreadProcessId(hwnd, None);
        let attached = if cur != tgt && cur != 0 && tgt != 0 {
            let ok = AttachThreadInput(cur, tgt, true);
            if ok.0 != 0 {
                Some((cur, tgt))
            } else {
                None
            }
        } else {
            None
        };
        let _ = BringWindowToTop(hwnd);
        let _ = SetForegroundWindow(hwnd);
        let _ = SetFocus(Some(hwnd));
        if let Some((a, b)) = attached {
            let _ = AttachThreadInput(a, b, false);
        }
    }
}

impl Drop for EditSession {
    fn drop(&mut self) {
        if !self.font.0.is_null() {
            unsafe {
                let _ = DeleteObject(self.font.into());
            }
        }
    }
}

// ---- 控制台（插件宿主）待办输入编辑框 ----
//
// 与就地重命名同机制：DirectComposition 窗口不合成子 HWND，待办输入框必须是
// 独立弹出框（WS_POPUP + WS_EX_TOOLWINDOW，无任务栏图标，无 WS_EX_TOPMOST——
// 去掉置顶后随 overlay 所在桌面层，不再浮到其它软件之上），定位用虚拟屏幕坐标
// （overlay 客户端坐标 = 虚拟屏幕坐标）。输入框是常驻控件，面板关闭时隐藏、
// 打开时显示，Enter 提交一条待办、Esc 清空（不自动聚焦提交）。

/// 借用 Runtime（编辑框子类回调入口，主线程独占；`handle_event` 持有借用时跳过）。
fn with_runtime<F: FnOnce(&mut Runtime)>(f: F) {
    let rtp = EDIT_RUNTIME.with(|c| *c.borrow());
    if rtp.is_null() {
        return;
    }
    let cell = unsafe { &*rtp };
    let Ok(mut rt) = cell.try_borrow_mut() else {
        return;
    };
    f(&mut rt);
}

/// 待办输入框子类窗口过程：Enter 提交 / Esc 清空，其余消息链式转发给原过程。
/// 注意不做 WM_KILLFOCUS 提交——输入框常驻，失焦提交会把任何一次点击变成新待办。
unsafe extern "system" fn todo_edit_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let sess = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const TodoEditSession;
    if sess.is_null() {
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    }
    let orig = (*sess).orig_proc;
    match msg {
        // 点击输入框直接激活（与重命名编辑框一致，修复点击无响应的焦点链路）
        WM_MOUSEACTIVATE => return LRESULT(MA_ACTIVATE as isize),
        WM_KEYDOWN => match wparam.0 as u32 {
            v if v == VK_RETURN.0 as u32 => {
                todo_commit_subclass(hwnd);
                return LRESULT(0);
            }
            v if v == VK_ESCAPE.0 as u32 => {
                todo_clear_subclass(hwnd);
                return LRESULT(0);
            }
            _ => {}
        },
        _ => {}
    }
    let orig_fn: unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT =
        std::mem::transmute(orig);
    CallWindowProcW(Some(orig_fn), hwnd, msg, wparam, lparam)
}

/// 输入框子类回调：Enter → 提交当前文本为一条待办 + 注入重绘。
fn todo_commit_subclass(hwnd: HWND) {
    with_runtime(|rt| {
        let is_console_edit = rt
            .console
            .name_edit
            .as_ref()
            .map(|ed| ed.hwnd == hwnd)
            .unwrap_or(false)
            || rt
                .console
                .detail_edit
                .as_ref()
                .map(|ed| ed.hwnd == hwnd)
                .unwrap_or(false);
        if !is_console_edit {
            return;
        }
        commit_todo_from_edit(rt);
        inject_rebuild(rt);
    });
}

/// 输入框子类回调：Esc → 清空文本（不产生待办）。
fn todo_clear_subclass(hwnd: HWND) {
    with_runtime(|rt| {
        let is_console_edit = rt
            .console
            .name_edit
            .as_ref()
            .map(|ed| ed.hwnd == hwnd)
            .unwrap_or(false)
            || rt
                .console
                .detail_edit
                .as_ref()
                .map(|ed| ed.hwnd == hwnd)
                .unwrap_or(false);
        if !is_console_edit {
            return;
        }
        unsafe {
            let _ = SendMessageW(
                hwnd,
                WM_SETTEXT,
                Some(WPARAM(0)),
                Some(LPARAM(wide("").as_ptr() as isize)),
            );
        }
    });
}

/// ease_out_cubic：网页级缓动（起快收缓，配合 60fps 定时器丝滑自然）。
fn ease_out_cubic(t: f32) -> f32 {
    let u = t.clamp(0.0, 1.0);
    1.0 - (1.0 - u).powi(3)
}

/// ease_out_back：过冲回弹（面板展开/待办行入场用，目标方向先超出一点再收回）。
fn ease_out_back(t: f32) -> f32 {
    let u = t.clamp(0.0, 1.0);
    const C1: f32 = 1.70158;
    const C3: f32 = C1 + 1.0;
    1.0 + C3 * (u - 1.0).powi(3) + C1 * (u - 1.0).powi(2)
}

/// 补间进度：`t0` 起 `dur` 秒内返回 0..1，超时返回 None（补间结束）。
fn tween_progress(t0: Instant, dur: f32, now: Instant) -> Option<f32> {
    let el = now.duration_since(t0).as_secs_f32();
    if el >= dur {
        None
    } else {
        Some(el / dur)
    }
}

/// 启用动画定时器（overlay 每 16ms 触发一次 `AnimTick`）。
fn arm_anim_timer(rt: &mut Runtime) {
    if !rt.console_anim.active()
        && rt.desktop_fade.is_none()
        && rt.fence_tweens.is_empty()
        && !icon_hover_active(rt)
    {
        return;
    }
    unsafe { (*rt.overlay_ptr).set_anim_active(true) };
}

/// 推进一帧动画：面板补间 + 行入场/勾选补间 + 删除幽灵清理。
/// 返回是否仍有动画在推进（否则调用方停用定时器）。
fn advance_anim(rt: &mut Runtime) -> bool {
    let now = Instant::now();
    let anim = &mut rt.console_anim;
    // 桌面切换：栅栏整体淡入/淡出补间（结束即清除）
    if let Some(df) = rt.desktop_fade {
        if tween_progress(df.t0, df.dur, now).is_none() {
            rt.desktop_fade = None;
        }
    }
    // 栅栏拖动/缩放补间：结束即从表里摘除（视觉 = 模型）
    rt.fence_tweens
        .retain(|t| tween_progress(t.t0, t.dur, now).is_some());
    // 面板展开/折叠补间
    if let Some(pt) = anim.panel_tween {
        match tween_progress(pt.t0, pt.dur, now) {
            Some(p) => {
                let e = ease_out_back(p);
                anim.panel = pt.from + (pt.to - pt.from) * e;
            }
            None => {
                anim.panel = pt.to;
                anim.panel_tween = None;
            }
        }
    }
    // 行级补间；结束的行从表里摘除
    let mut done: Vec<u64> = Vec::new();
    for (&id, ra) in anim.rows.iter_mut() {
        if let Some(en) = ra.enter {
            if tween_progress(en.t0, en.dur, now).is_none() {
                ra.enter = None;
            }
        }
        if let Some(tg) = ra.toggle {
            if tween_progress(tg.t0, tg.dur, now).is_none() {
                ra.toggle = None;
            }
        }
        if ra.enter.is_none() && ra.toggle.is_none() {
            done.push(id);
        }
    }
    for id in done {
        anim.rows.remove(&id);
    }
    // 删除幽灵：动画结束即移除
    anim.exiting
        .retain(|e| tween_progress(e.t0, e.dur, now).is_some());
    // 展开完成后聚焦输入框（一次性，用户主动展开时置位）
    if anim.focus_on_expand && anim.panel >= 0.5 {
        anim.focus_on_expand = false;
        if let Some(ed) = rt.console.name_edit.as_ref() {
            focus_popup_edit(ed.hwnd);
        }
    }
    anim.active()
        || rt.desktop_fade.is_some()
        || !rt.fence_tweens.is_empty()
        || icon_hover_active(rt)
}

/// 开始面板展开/折叠补间（`to` 目标进度）。目标立即生效于命中模型，
/// 视觉高度由补间在 `AnimTick` 逐帧推进。
fn start_panel_tween(rt: &mut Runtime, to: f32) {
    let from = rt.console_anim.panel;
    rt.console_anim.panel_tween = Some(PanelTween {
        t0: Instant::now(),
        dur: 0.24,
        from,
        to,
    });
    arm_anim_timer(rt);
}

/// 把「当前列表中的下标」换算为「删除前的原始下标」：列表中已有若干删除幽灵
/// 正在淡出（`exiting` 按进入顺序排列），它们仍在原始槽位占据位置，所以原始下标
/// 是 `cur + 所有原始下标 < orig 的幽灵数` 的最大不动点（从高位向下迭代收敛）。
fn exit_original_index(exiting: &[ExitRow], cur: usize) -> usize {
    let mut orig = cur + exiting.len();
    loop {
        let cand = cur + exiting.iter().filter(|e| e.index < orig).count();
        if cand == orig {
            return orig;
        }
        orig = cand;
    }
}

/// 把两个输入框（名称 + 详细信息）当前文本提交为一条待办；两个都为空不提交。
/// 名称空但详情有内容时把详情提为名称。提交后清空两个输入框。
/// 不注入重绘：`handle_event` 尾部会重建场景；子类回调路径由调用方注入。
fn commit_todo_from_edit(rt: &mut Runtime) {
    let Some(name_hwnd) = rt.console.name_edit.as_ref().map(|ed| ed.hwnd) else {
        return;
    };
    let mut name = edit_text(name_hwnd).trim().to_string();
    let detail = rt
        .console
        .detail_edit
        .as_ref()
        .map(|ed| edit_text(ed.hwnd).trim().to_string())
        .unwrap_or_default();
    if name.is_empty() && !detail.is_empty() {
        // 用户只填了详情行：详情作为名称提交，详情行清空
        name = detail.clone();
    }
    let detail = if name == detail {
        String::new()
    } else {
        detail
    };
    if name.is_empty() {
        return;
    }
    let id = rt.desk.next_todo_id;
    rt.desk.next_todo_id = rt.desk.next_todo_id.wrapping_add(1);
    rt.desk.todos.push(TodoItem::new(id, name, detail));
    if let Err(e) = rt.store.save(&rt.desk) {
        tracing::warn!("待办持久化失败: {e}");
    }
    // 新行入场动画：淡入 + 从下方滑入
    rt.console_anim.rows.insert(
        id,
        RowAnim {
            enter: Some(RowEnter {
                t0: Instant::now(),
                dur: 0.22,
            }),
            toggle: None,
        },
    );
    arm_anim_timer(rt);
    // 清空名称 + 详情两个输入框
    let clear: [HWND; 2] = [
        name_hwnd,
        rt.console
            .detail_edit
            .as_ref()
            .map(|ed| ed.hwnd)
            .unwrap_or(name_hwnd),
    ];
    for hwnd in clear {
        unsafe {
            let _ = SendMessageW(
                hwnd,
                WM_SETTEXT,
                Some(WPARAM(0)),
                Some(LPARAM(wide("").as_ptr() as isize)),
            );
        }
    }
    tracing::debug!("已添加一条待办");
}

/// 创建一个待办输入编辑框（名称行或详情行）。owner = overlay 窗口：
/// WM_CTLCOLOREDIT 到达 overlay 填暗色背景。**去 WS_EX_TOPMOST**——置顶会让
/// 输入框浮到其它软件之上（层级 bug）；去掉后随 overlay 所在层级（桌面层，在
/// 正常窗口之下），WS_EX_TOOLWINDOW 隐藏任务栏/Alt+Tab 条目。
fn create_todo_edit(rt: &Runtime, detail: bool) -> Option<TodoEdit> {
    let hinst = match unsafe { GetModuleHandleW(None) } {
        Ok(m) => HINSTANCE(m.0),
        Err(e) => {
            tracing::warn!("获取模块句柄失败，无法创建待办输入框: {e}");
            return None;
        }
    };
    let font_size = if detail {
        rt.theme.label.size * 0.82
    } else {
        rt.theme.label.size
    };
    let font = create_edit_font(font_size);
    let hwnd = match unsafe {
        CreateWindowExW(
            WS_EX_TOOLWINDOW,
            PCWSTR(wide("EDIT").as_ptr()),
            PCWSTR(wide("").as_ptr()),
            WS_POPUP | WINDOW_STYLE(ES_AUTOHSCROLL as u32) | WINDOW_STYLE(ES_LEFT as u32),
            0,
            0,
            120,
            CONSOLE_INPUT_H as i32,
            Some(rt.hwnd),
            None,
            Some(hinst),
            None,
        )
    } {
        Ok(h) => h,
        Err(e) => {
            tracing::warn!("创建待办输入框失败: {e}");
            unsafe {
                let _ = DeleteObject(font.into());
            }
            return None;
        }
    };
    let session = Box::new(TodoEditSession {
        orig_proc: unsafe {
            SetWindowLongPtrW(hwnd, GWL_WNDPROC, todo_edit_proc as *const () as isize)
        },
        font,
    });
    let raw = &*session as *const TodoEditSession;
    unsafe {
        let _ = SetWindowLongPtrW(hwnd, GWLP_USERDATA, raw as isize);
        let _ = SendMessageW(
            hwnd,
            WM_SETFONT,
            Some(WPARAM(font.0 as usize)),
            Some(LPARAM(1)),
        );
    }
    style_edit_window(hwnd, 8.0 * rt.theme.scale);
    Some(TodoEdit { hwnd, session })
}

/// 确保两个待办输入编辑框存在（名称 + 详情，创建 + 子类化 + 字体）。重复调用为幂等。
fn ensure_console_edit(rt: &mut Runtime) {
    if rt.console.name_edit.is_none() {
        if let Some(ed) = create_todo_edit(rt, false) {
            rt.console.name_edit = Some(ed);
        }
    }
    if rt.console.detail_edit.is_none() {
        if let Some(ed) = create_todo_edit(rt, true) {
            rt.console.detail_edit = Some(ed);
        }
    }
}

/// 移动编辑框到指定矩形并显示（SW_SHOWNA 不抢焦点）；尺寸变化后重放圆角区域。
fn move_show_edit(hwnd: HWND, r: RectF, radius: f32) {
    unsafe {
        let _ = MoveWindow(
            hwnd,
            r.x as i32,
            r.y as i32,
            r.w.max(1.0) as i32,
            r.h.max(1.0) as i32,
            true,
        );
        let _ = ShowWindow(hwnd, SW_SHOWNA);
    }
    style_edit_window(hwnd, radius);
}

/// 按当前控制中心布局摆放/隐藏编辑框（待办名称/详情 + 便签，不抢焦点）。
/// 折叠胶囊（panel < 0.5）时全部隐藏；展开到一半（≥0.5）后只显示当前标签页的编辑框。
fn layout_console_edit(rt: &mut Runtime) {
    let panel = console_geometry(&rt.desk, &rt.theme, rt.vw, rt.vh, rt.console_anim.panel);
    let s = rt.theme.scale;
    let order = console_tab_order(rt);
    let active = order
        .get(rt.console_tab)
        .copied()
        .unwrap_or(ConsoleTab::Todo);
    let expanded = rt.console_anim.panel >= 0.5;
    let (show_todo, show_notes) = if expanded {
        (active == ConsoleTab::Todo, active == ConsoleTab::Notes)
    } else {
        (false, false)
    };
    if show_todo {
        ensure_console_edit(rt);
        if let Some(ed) = rt.console.name_edit.as_ref() {
            move_show_edit(ed.hwnd, console_input_rect(&panel, s), 8.0 * s);
        }
        if let Some(ed) = rt.console.detail_edit.as_ref() {
            move_show_edit(ed.hwnd, console_detail_rect(&panel, s), 8.0 * s);
        }
    } else {
        for ed in rt
            .console
            .name_edit
            .iter()
            .chain(rt.console.detail_edit.iter())
        {
            unsafe {
                let _ = ShowWindow(ed.hwnd, SW_HIDE);
            }
        }
    }
    if show_notes {
        ensure_notes_edit(rt);
        if let Some(ed) = rt.console.notes_edit.as_ref() {
            let r = console_notes_rect(&panel, s);
            move_show_edit(
                ed.hwnd,
                RectF {
                    x: r.x + 1.0,
                    y: r.y + 1.0,
                    w: r.w - 2.0,
                    h: r.h - 2.0,
                },
                8.0 * s,
            );
        }
    } else {
        if let Some(ed) = rt.console.notes_edit.as_ref() {
            unsafe {
                let _ = ShowWindow(ed.hwnd, SW_HIDE);
            }
        }
    }
}

/// 确保编辑框存在并按当前布局摆放/隐藏（启动与展开时调用，不抢焦点）。
fn sync_console_edit(rt: &mut Runtime) {
    if rt.console_anim.panel < 0.5 {
        layout_console_edit(rt);
        return;
    }
    layout_console_edit(rt);
}

/// 给「名称」输入框键盘焦点（独立弹出框需先到前台，规则与就地重命名一致）。
fn focus_console_edit(rt: &mut Runtime) {
    if let Some(ed) = rt.console.name_edit.as_ref() {
        if rt.console_anim.panel >= 0.5 {
            focus_popup_edit(ed.hwnd);
        }
    }
}

/// 给「详细信息」输入框键盘焦点（点击详情行时）。
fn focus_console_detail(rt: &mut Runtime) {
    if let Some(ed) = rt.console.detail_edit.as_ref() {
        if rt.console_anim.panel >= 0.5 {
            focus_popup_edit(ed.hwnd);
        }
    }
}

/// 创建便签编辑框（多行 EDIT：Enter 换行，失焦自动保存）。
fn create_notes_edit(rt: &Runtime) -> Option<NotesEdit> {
    let hinst = match unsafe { GetModuleHandleW(None) } {
        Ok(m) => HINSTANCE(m.0),
        Err(e) => {
            tracing::warn!("获取模块句柄失败，无法创建便签编辑框: {e}");
            return None;
        }
    };
    let font = create_edit_font(rt.theme.label.size * 0.95);
    let text = rt
        .desk
        .plugins
        .iter()
        .find(|p| p.kind == PluginKind::Notes)
        .map(|p| p.note_text.clone())
        .unwrap_or_default();
    let text_w: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    let hwnd = match unsafe {
        CreateWindowExW(
            WS_EX_TOOLWINDOW,
            PCWSTR(wide("EDIT").as_ptr()),
            PCWSTR(text_w.as_ptr()),
            WS_POPUP
                | WINDOW_STYLE(ES_MULTILINE as u32)
                | WINDOW_STYLE(ES_AUTOVSCROLL as u32)
                | WINDOW_STYLE(ES_WANTRETURN as u32)
                | WINDOW_STYLE(ES_LEFT as u32)
                | WS_VSCROLL,
            0,
            0,
            120,
            CONSOLE_NOTES_H as i32,
            Some(rt.hwnd),
            None,
            Some(hinst),
            None,
        )
    } {
        Ok(h) => h,
        Err(e) => {
            tracing::warn!("创建便签编辑框失败: {e}");
            unsafe {
                let _ = DeleteObject(font.into());
            }
            return None;
        }
    };
    let session = Box::new(NotesEditSession {
        orig_proc: unsafe {
            SetWindowLongPtrW(hwnd, GWL_WNDPROC, notes_edit_proc as *const () as isize)
        },
        font,
    });
    let raw = &*session as *const NotesEditSession;
    unsafe {
        let _ = SetWindowLongPtrW(hwnd, GWLP_USERDATA, raw as isize);
        let _ = SendMessageW(
            hwnd,
            WM_SETFONT,
            Some(WPARAM(font.0 as usize)),
            Some(LPARAM(1)),
        );
    }
    style_edit_window(hwnd, 8.0 * rt.theme.scale);
    Some(NotesEdit { hwnd, session })
}

/// 确保便签编辑框存在（幂等）。
fn ensure_notes_edit(rt: &mut Runtime) {
    if rt.console.notes_edit.is_none() {
        if let Some(ed) = create_notes_edit(rt) {
            rt.console.notes_edit = Some(ed);
        }
    }
}

/// 给便签编辑框键盘焦点。
fn focus_notes_edit(rt: &mut Runtime) {
    if let Some(ed) = rt.console.notes_edit.as_ref() {
        if rt.console_anim.panel >= 0.5 {
            focus_popup_edit(ed.hwnd);
        }
    }
}

/// 便签编辑框子类过程：失焦保存到模型并持久化；点击直接激活。
unsafe extern "system" fn notes_edit_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let sess = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const NotesEditSession;
    if sess.is_null() {
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    }
    let orig = (*sess).orig_proc;
    match msg {
        WM_MOUSEACTIVATE => return LRESULT(MA_ACTIVATE as isize),
        WM_KILLFOCUS => {
            save_notes_window(hwnd);
        }
        _ => {}
    }
    let orig_fn: unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT =
        std::mem::transmute(orig);
    CallWindowProcW(Some(orig_fn), hwnd, msg, wparam, lparam)
}

/// 把指定编辑框的文本保存进便签插件数据（按句柄确认是便签编辑框）。
fn save_notes_window(hwnd: HWND) {
    with_runtime(|rt| {
        let is_notes = rt
            .console
            .notes_edit
            .as_ref()
            .map(|ed| ed.hwnd == hwnd)
            .unwrap_or(false);
        if !is_notes {
            return;
        }
        let text = edit_text(hwnd);
        if let Some(p) = rt
            .desk
            .plugins
            .iter_mut()
            .find(|p| p.kind == PluginKind::Notes)
        {
            p.note_text = text;
            let _ = rt.store.save(&rt.desk);
        }
    });
}

/// 保存当前便签编辑框文本（切换标签页 / 关闭面板 / 退出前调用）。
fn save_notes(rt: &mut Runtime) {
    if let Some(ed) = rt.console.notes_edit.as_ref() {
        let text = edit_text(ed.hwnd);
        if let Some(p) = rt
            .desk
            .plugins
            .iter_mut()
            .find(|p| p.kind == PluginKind::Notes)
        {
            if p.note_text != text {
                p.note_text = text;
                let _ = rt.store.save(&rt.desk);
            }
        }
    }
}

/// 栅栏管理页列表最大可滚动量（物理像素；0 = 行数不超出可视区）。
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

/// 外部插件清单（plugins 目录下的 plugin.json）。
#[derive(serde::Deserialize)]
struct PluginManifest {
    id: String,
    name: String,
    #[serde(default)]
    version: String,
    #[serde(default)]
    desc: String,
}

/// 扫描插件目录：把新增的 `plugin.json` 清单并入注册表（幂等，按 id 去重）。
/// 已存在的清单更新 name/version/desc；内置插件（todo/notes）不会被外部清单覆盖。
fn scan_plugins_dir(rt: &mut Runtime) {
    let _ = std::fs::create_dir_all(&rt.plugins_dir);
    let mut changed = false;
    let Ok(entries) = std::fs::read_dir(&rt.plugins_dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let is_json = path
            .extension()
            .map(|e| e.to_string_lossy().eq_ignore_ascii_case("json"))
            .unwrap_or(false);
        if !is_json {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(mf) = serde_json::from_str::<PluginManifest>(&text) else {
            tracing::warn!(file = %path.display(), "plugin.json 解析失败，已跳过");
            continue;
        };
        match rt.desk.plugins.iter_mut().find(|p| p.id == mf.id) {
            Some(p) => {
                if p.name != mf.name || p.version != mf.version || p.desc != mf.desc {
                    p.name = mf.name;
                    p.version = mf.version;
                    p.desc = mf.desc;
                    changed = true;
                }
            }
            None => {
                rt.desk.plugins.push(PluginEntry {
                    id: mf.id,
                    name: mf.name,
                    kind: PluginKind::External,
                    enabled: false,
                    version: mf.version,
                    desc: mf.desc,
                    note_text: String::new(),
                });
                changed = true;
            }
        }
    }
    if changed {
        if let Err(e) = rt.store.save(&rt.desk) {
            tracing::warn!("插件注册表持久化失败: {e}");
        }
    }
}

/// 由旧路径 + 新显示名计算改名后的路径：快捷方式（.lnk/.url/.appref-ms）自动
/// 保留原扩展名（显示名不含扩展名），其余类型按用户输入原样拼接。
fn new_path_for_rename(old_path: &str, new_name: &str) -> Option<PathBuf> {
    let p = Path::new(old_path);
    let parent = p.parent()?;
    let old_file = p.file_name()?.to_string_lossy().into_owned();
    let lower = old_file.to_ascii_lowercase();
    let stripped_ext = ["lnk", "url", "appref-ms"].iter().copied().find(|ext| {
        let dot = format!(".{ext}");
        lower.ends_with(&dot) && old_file.len() > ext.len() + 1
    });
    let final_name = match stripped_ext {
        Some(ext) if !new_name.to_ascii_lowercase().ends_with(&format!(".{ext}")) => {
            format!("{new_name}.{ext}")
        }
        _ => new_name.to_string(),
    };
    if final_name.is_empty() {
        return None;
    }
    Some(parent.join(final_name))
}

/// 把任意文件/文件夹/快捷方式路径加入指定栅栏（拖入 / 粘贴共用）。
/// 首次出现的路径构建 `DesktopItem`、录入元数据、提取图标（进 `pending_uploads`）；
/// 已归属任一栅栏或自由区的路径跳过。位图随后在 `handle_event` 末尾随场景上传。
fn add_paths_to_fence(rt: &mut Runtime, fence: usize, paths: &[String]) {
    if rt.desk.fences.get(fence).is_none() {
        return;
    }
    for src in paths {
        let src_path = Path::new(src);
        // 先物理复制进内部库：栅栏索引的是「库内副本」，库内删除 → 栅栏项同步删。
        // 源已在库内（幂等粘贴/跨栅栏移动）直接复用，不再重复复制。
        let target: PathBuf = if is_inside_library(rt, src_path) {
            src_path.to_path_buf()
        } else {
            match copy_into_library(rt, src_path) {
                Ok(p) => p,
                Err(e) => {
                    tracing::warn!(src, "复制进库失败: {e}");
                    continue;
                }
            }
        };
        let target_str = target.to_string_lossy().into_owned();
        let item = match sylva_shell::items::item_from_path(&target_str) {
            Ok(it) => it,
            Err(e) => {
                tracing::warn!(target_str, "无法创建图标项: {e}");
                continue;
            }
        };
        let id = item.id.clone();
        // 已在本栅栏则跳过（防止同栅栏内重复）；在其它栅栏/自由区允许再添加——
        // 「复制→粘贴到另一个栅栏」就是让同一项出现在两个栅栏里（跨栅栏粘贴失效根因）。
        let already = rt
            .desk
            .fences
            .get(fence)
            .map(|f| f.icon_ids.contains(&id))
            .unwrap_or(false);
        if already {
            continue;
        }
        // 录入元数据：持久化库内副本路径（重启恢复图标/打开）；added=true 表示内部库项
        let mut ic = Icon::new(id.clone(), item.display_name.clone(), item.kind);
        ic.path = Some(target_str.clone());
        ic.added = true;
        sylva_core::details::enrich(&mut ic, &target_str);
        rt.desk.icons.insert(id.clone(), ic);
        let new_bitmap = rt.items.len() as u64;
        rt.items.push(item);
        rt.item_index.insert(id.clone(), rt.items.len() - 1);
        rt.bitmap_ids.insert(id.clone(), new_bitmap);
        if let Some(f) = rt.desk.fences.get_mut(fence) {
            f.icon_ids.push(id.clone());
        }
        // 提取图标（阻塞但命中系统图标缓存，通常很快）；失败不阻断添加
        let idx = rt.item_index[&id];
        match sylva_shell::icons::extract_icon(&rt.items[idx], ICON_EXTRACT_SIZE) {
            Ok(data) => rt.pending_uploads.push((new_bitmap, data)),
            Err(e) => tracing::warn!(target_str, "图标提取失败: {e}"),
        }
    }
}

/// 库同步：检查所有内部库项，`library` 里的文件已被外部删除时，栅栏对应项同步移除。
/// 返回是否有项被移除（调用方可据此决定持久化；重绘由事件尾部统一完成）。
fn reconcile_library(rt: &mut Runtime) -> bool {
    let missing: Vec<String> = rt
        .desk
        .icons
        .iter()
        .filter(|(_, ic)| ic.added)
        .filter(|(_, ic)| {
            ic.path
                .as_ref()
                .map(|p| is_inside_library(rt, Path::new(p)) && !Path::new(p).exists())
                .unwrap_or(false)
        })
        .map(|(id, _)| id.clone())
        .collect();
    let mut changed = false;
    for id in missing {
        changed = true;
        remove_icon_entirely(rt, &id);
    }
    if changed {
        tracing::info!(removed = changed, "库内文件被删，同步移除栅栏项");
    }
    changed
}

/// `path` 是否位于内部库文件夹内（组件级大小写不敏感前缀比较）。
fn is_inside_library(rt: &Runtime, path: &Path) -> bool {
    let lib: Vec<String> = rt
        .library
        .components()
        .map(|c| c.as_os_str().to_string_lossy().to_lowercase())
        .collect();
    if lib.is_empty() {
        return false;
    }
    let comps: Vec<String> = path
        .components()
        .map(|c| c.as_os_str().to_string_lossy().to_lowercase())
        .collect();
    comps.len() >= lib.len() && lib.iter().zip(comps.iter()).all(|(a, b)| a == b)
}

/// 把文件/文件夹复制进内部库；返回库内目标路径。同名自动改名 `name (1).ext`。
fn copy_into_library(rt: &Runtime, src: &Path) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(&rt.library)?;
    let name = src.file_name().and_then(|n| n.to_str()).unwrap_or("item");
    let dest = unique_library_path(&rt.library, name);
    if src.is_dir() {
        copy_dir_all(src, &dest)?;
    } else {
        std::fs::copy(src, &dest)?;
    }
    Ok(dest)
}

/// 库内不重名路径：存在则 `name (n).ext` 递增。
fn unique_library_path(lib: &Path, name: &str) -> PathBuf {
    let cand = lib.join(name);
    if !cand.exists() {
        return cand;
    }
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i..]),
        _ => (name, ""),
    };
    for i in 1..1000 {
        let cand = lib.join(format!("{stem} ({i}){ext}"));
        if !cand.exists() {
            return cand;
        }
    }
    cand
}

/// 递归复制目录（不跟符号链接，按常规文件处理）。
fn copy_dir_all(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        let target = dst.join(entry.file_name());
        if ty.is_dir() {
            copy_dir_all(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

/// 把一个图标从 Sylva 中整体移除（仅删引用，不碰磁盘文件）。
/// 用于「拖入/粘贴新增项」的移除——它们不属于真实桌面，移出栅栏即删除。
fn remove_icon_entirely(rt: &mut Runtime, id: &str) {
    rt.desk.icons.remove(id);
    rt.desk.free_icons.retain(|x| x != id);
    for f in &mut rt.desk.fences {
        f.icon_ids.retain(|x| x != id);
    }
    rt.bitmap_ids.remove(id);
    // 从 items 池移除对应 DesktopItem（持有 PIDL，Drop 时释放），并重建下标
    if let Some(i) = rt.items.iter().position(|it| it.id == *id) {
        rt.items.remove(i);
    }
    rt.item_index = rt
        .items
        .iter()
        .enumerate()
        .map(|(i, it)| (it.id.clone(), i))
        .collect();
}

/// 剪贴板格式：CF_HDROP（拖放文件列表）。windows-rs 0.62 将其定义在
/// `Win32_System_Ole` 里；这里用文档稳定值 15，避免引入整个 Ole 功能集。
const CF_HDROP: u32 = 15;

/// 读剪贴板里的文件列表（CF_HDROP）。
fn clipboard_file_paths() -> Vec<String> {
    let mut out = Vec::new();
    unsafe {
        if OpenClipboard(None).is_err() {
            return out;
        }
        if let Ok(handle) = GetClipboardData(CF_HDROP) {
            if !handle.is_invalid() {
                let hdrop = HDROP(handle.0);
                let n = DragQueryFileW(hdrop, u32::MAX, None);
                for i in 0..n {
                    let len = DragQueryFileW(hdrop, i, None);
                    let mut buf = vec![0u16; len as usize + 1];
                    DragQueryFileW(hdrop, i, Some(&mut buf));
                    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
                    out.push(String::from_utf16_lossy(&buf[..end]));
                }
            }
        }
        let _ = CloseClipboard();
    }
    out
}

/// 图标右键菜单动作。
enum IconMenuAction {
    Open,
    Remove,
}

/// 栅栏右键菜单动作。
enum FenceMenuAction {
    /// 打开原生选择器（文件夹多选模式——Windows 的系统对话框只能单一类型多选；
    /// 选中的文件夹直接添加进本栅栏，文件走拖拽/粘贴）。
    Add,
    /// 从剪贴板粘贴文件进本栅栏。
    Paste,
    /// 就地重命名栅栏标题。
    Rename,
    SetLayout(FenceLayout),
    SetIconSize(f32),
    /// 设置背景风格（玻璃 / 透明 / 颜色）。
    SetStyle(FenceStyle),
    /// 设置背景色调；None = 恢复默认玻璃底色。
    SetTint(Option<[f32; 3]>),
    Delete,
}

/// 处理右键：弹出上下文菜单并执行选中动作（菜单为模态，阻塞到关闭）。
fn handle_context_menu(rt: &mut Runtime, fence: usize, icon: Option<usize>, _pos: (f32, f32)) {
    let (sx, sy) = cursor_screen();
    if let Some(ii) = icon {
        // 多选集合：右键集合中的任意一项 → 集合操作（打开全部 / 复制 / 移出 / 删除）。
        // 右键未选中的项 → 先单选该项，再走单项逻辑（资源管理器行为）。
        let key = (fence, ii);
        let multi = rt.selected.len() > 1 && rt.selected.contains(&key);
        if multi {
            match multi_icon_context_menu(rt.hwnd, sx, sy) {
                Some(MultiMenuAction::Open) => open_selected(rt),
                Some(MultiMenuAction::Copy) => copy_selected(rt),
                Some(MultiMenuAction::Remove) => remove_selected(rt),
                Some(MultiMenuAction::Delete) => delete_selected(rt),
                None => {}
            }
            return;
        }
        if !rt.selected.contains(&key) {
            rt.selected = vec![key];
        }
        // 有文件路径的项走真实 Shell 右键菜单（等同桌面右键）；虚拟项（无路径）
        // 退回简版「打开 / 移出栅栏」。
        let path = rt
            .desk
            .fences
            .get(fence)
            .and_then(|f| f.icon_ids.get(ii))
            .and_then(|id| rt.desk.icons.get(id))
            .and_then(|ic| ic.path.clone());
        match path {
            Some(p) => match shell_menu::show(&p, rt.hwnd, sx, sy) {
                Some(shell_menu::ShellMenuAction::Remove) => remove_fence_icon(rt, fence, ii),
                Some(shell_menu::ShellMenuAction::Rename) => {
                    start_inplace_rename(rt, EditTarget::Item { fence, icon: ii })
                }
                Some(shell_menu::ShellMenuAction::Invoked) => {
                    // 原生动词执行后（如「删除」）：文件若已被移走/删除，立即清掉
                    // 栅栏里的死图标，等价于资源管理器删除后刷新视图。
                    if !std::path::Path::new(&p).exists() {
                        if let Some(id) = rt
                            .desk
                            .fences
                            .get(fence)
                            .and_then(|f| f.icon_ids.get(ii))
                            .cloned()
                        {
                            remove_icon_entirely(rt, &id);
                        }
                        let _ = rt.store.save(&rt.desk);
                    }
                }
                // 真实 Shell 菜单没弹出来（路径无效 / COM 异常等）：退回简版菜单，
                // 保证右键必有反馈，不让「Windows 右击列表」静默消失。
                None => {
                    tracing::warn!(path = %p, "Shell 右键菜单创建失败，退回简版菜单");
                    match icon_context_menu(rt.hwnd, sx, sy) {
                        Some(IconMenuAction::Open) => launch_fence_icon(rt, fence, ii),
                        Some(IconMenuAction::Remove) => remove_fence_icon(rt, fence, ii),
                        None => {}
                    }
                }
            },
            None => match icon_context_menu(rt.hwnd, sx, sy) {
                Some(IconMenuAction::Open) => launch_fence_icon(rt, fence, ii),
                Some(IconMenuAction::Remove) => remove_fence_icon(rt, fence, ii),
                None => {}
            },
        }
    } else if let Some(action) = fence_context_menu(rt, fence, sx, sy) {
        match action {
            FenceMenuAction::Add => {
                if let Some(paths) = pick_paths(rt.hwnd) {
                    add_paths_to_fence(rt, fence, &paths);
                    let _ = rt.store.save(&rt.desk);
                }
            }
            FenceMenuAction::Paste => {
                let paths = clipboard_file_paths();
                if !paths.is_empty() {
                    add_paths_to_fence(rt, fence, &paths);
                } else {
                    tracing::info!("剪贴板中没有文件，跳过粘贴");
                }
            }
            FenceMenuAction::Rename => {
                start_inplace_rename(rt, EditTarget::FenceTitle { fence });
            }
            FenceMenuAction::SetLayout(l) => {
                if l == FenceLayout::List {
                    let w = list_auto_width(rt, fence);
                    if let Some(f) = rt.desk.fences.get_mut(fence) {
                        f.appearance.layout = l;
                        f.bounds.w = w;
                    }
                } else if let Some(f) = rt.desk.fences.get_mut(fence) {
                    f.appearance.layout = l;
                }
                let _ = rt.store.save(&rt.desk);
            }
            FenceMenuAction::SetIconSize(s) => {
                if let Some(f) = rt.desk.fences.get_mut(fence) {
                    f.appearance.icon_size = s;
                }
                let _ = rt.store.save(&rt.desk);
            }
            FenceMenuAction::SetStyle(style) => {
                if let Some(f) = rt.desk.fences.get_mut(fence) {
                    f.appearance.bg_style = style;
                }
                let _ = rt.store.save(&rt.desk);
            }
            FenceMenuAction::SetTint(tint) => {
                if let Some(f) = rt.desk.fences.get_mut(fence) {
                    f.appearance.tint = tint;
                }
                let _ = rt.store.save(&rt.desk);
            }
            FenceMenuAction::Delete => {
                let ids: Vec<String> = rt
                    .desk
                    .fences
                    .get(fence)
                    .map(|f| f.icon_ids.clone())
                    .unwrap_or_default();
                for id in ids {
                    rt.desk.move_icon(&id, None);
                }
                if fence < rt.desk.fences.len() {
                    rt.desk.fences.remove(fence);
                }
                let _ = rt.store.save(&rt.desk);
            }
        }
    }
}

/// 图标右键菜单：打开 / 移出栅栏。返回选中的动作。
fn icon_context_menu(hwnd: HWND, sx: i32, sy: i32) -> Option<IconMenuAction> {
    let menu = popup_menu();
    if menu.is_invalid() {
        return None;
    }
    unsafe {
        let s = wide("打开");
        let _ = AppendMenuW(menu, MF_STRING, MENU_ICON_OPEN, PCWSTR(s.as_ptr()));
        let s2 = wide("移出栅栏");
        let _ = AppendMenuW(menu, MF_STRING, MENU_ICON_REMOVE, PCWSTR(s2.as_ptr()));
    }
    let cmd = unsafe {
        TrackPopupMenu(
            menu,
            TPM_RETURNCMD | TPM_NONOTIFY,
            sx,
            sy,
            Some(0),
            hwnd,
            None,
        )
        .0 as usize
    };
    unsafe {
        let _ = DestroyMenu(menu);
    }
    match cmd as usize {
        MENU_ICON_OPEN => Some(IconMenuAction::Open),
        MENU_ICON_REMOVE => Some(IconMenuAction::Remove),
        _ => None,
    }
}

/// 多选集合的右键菜单动作。
enum MultiMenuAction {
    Open,
    Copy,
    Remove,
    Delete,
}

/// 多选右键菜单：打开全部 / 复制 / 移出栅栏 / 删除。返回选中的动作。
fn multi_icon_context_menu(hwnd: HWND, sx: i32, sy: i32) -> Option<MultiMenuAction> {
    const M_OPEN: usize = 1;
    const M_COPY: usize = 2;
    const M_REMOVE: usize = 3;
    const M_DELETE: usize = 4;
    let menu = popup_menu();
    if menu.is_invalid() {
        return None;
    }
    unsafe {
        let _ = AppendMenuW(menu, MF_STRING, M_OPEN, PCWSTR(wide("打开").as_ptr()));
        let _ = AppendMenuW(menu, MF_STRING, M_COPY, PCWSTR(wide("复制").as_ptr()));
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        let _ = AppendMenuW(menu, MF_STRING, M_REMOVE, PCWSTR(wide("移出栅栏").as_ptr()));
        let _ = AppendMenuW(menu, MF_STRING, M_DELETE, PCWSTR(wide("删除").as_ptr()));
    }
    let cmd = unsafe {
        TrackPopupMenu(
            menu,
            TPM_RETURNCMD | TPM_NONOTIFY,
            sx,
            sy,
            Some(0),
            hwnd,
            None,
        )
        .0 as usize
    };
    unsafe {
        let _ = DestroyMenu(menu);
    }
    match cmd as usize {
        M_OPEN => Some(MultiMenuAction::Open),
        M_COPY => Some(MultiMenuAction::Copy),
        M_REMOVE => Some(MultiMenuAction::Remove),
        M_DELETE => Some(MultiMenuAction::Delete),
        _ => None,
    }
}

/// 把一组路径写入剪贴板（CF_HDROP，与资源管理器「复制」同一格式）。
fn set_clipboard_paths(paths: &[String]) {
    // DROPFILES 头（20 字节）：pFiles 偏移 + 坐标 + 标志；随后每个路径 UTF-16LE + \0，
    // 列表以额外 \0 结束（末路径的 \0 + 结尾 \0 = 双 \0 终止）。
    let header = 20usize;
    let bytes_len: usize = header
        + paths
            .iter()
            .map(|p| p.encode_utf16().count() * 2 + 2)
            .sum::<usize>()
        + 2;
    unsafe {
        if OpenClipboard(None).is_err() {
            return;
        }
        let _ = EmptyClipboard();
        if let Ok(hglobal) = GlobalAlloc(GMEM_MOVEABLE, bytes_len) {
            let ptr = GlobalLock(hglobal);
            if !ptr.is_null() {
                let buf = std::slice::from_raw_parts_mut(ptr as *mut u8, bytes_len);
                buf.fill(0);
                // DROPFILES 头（20 字节）：pFiles=偏移20、pt 坐标、fNC、fWide
                // 偏移：pFiles(0..4) | pt(4..12) | fNC(12..16) | fWide(16..20)
                buf[0..4].copy_from_slice(&(header as u32).to_le_bytes());
                buf[16..20].copy_from_slice(&1u32.to_le_bytes()); // fWide = TRUE（UTF-16）
                let mut off = header;
                for p in paths {
                    for u in p.encode_utf16() {
                        let bytes = u.to_le_bytes();
                        buf[off] = bytes[0];
                        buf[off + 1] = bytes[1];
                        off += 2;
                    }
                    off += 2; // 路径结尾 \0
                }
                // 列表结束双 \0 的第二个由上面的 buf.fill(0) 保证（bytes_len 已计入 +2）
                let _ = GlobalUnlock(hglobal);
                // fWide 位于偏移 16（BOOL），fNC 位于偏移 12
                let _ = SetClipboardData(CF_HDROP, Some(HANDLE(hglobal.0)));
            }
        }
        let _ = CloseClipboard();
    }
}

/// 打开原生选择器（`IFileOpenDialog` + `FOS_PICKFOLDERS`，多选文件夹），返回选中的
/// 绝对路径列表。用户取消（`HRESULT 0x800704C7`）或失败返回 None。
///
/// 单一「添加…」入口：Windows 系统对话框不能文件 + 文件夹混选（硬平台限制），
/// 这里用文件夹模式多选——能选中的（文件夹）直接添加进栅栏；单个/多个文件仍可
/// 拖拽或粘贴进栅栏。
///
/// COM 已在启动早期以 STA 初始化。对话框运行在自己模态消息循环里，期间到达的
/// overlay 事件会被重入守卫丢弃——与 `TrackPopupMenu` 同一套机制，不会破坏状态。
fn pick_paths(owner: HWND) -> Option<Vec<String>> {
    unsafe {
        let dialog: IFileOpenDialog =
            CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER).ok()?;
        let title = PCWSTR(wide("添加到栅栏").as_ptr());
        dialog.SetTitle(title).ok()?;
        let opts = FOS_PICKFOLDERS | FOS_ALLOWMULTISELECT | FOS_FORCEFILESYSTEM;
        dialog.SetOptions(opts).ok()?;
        if dialog.Show(Some(owner)).is_err() {
            return None; // 取消或失败：都不添加
        }
        let items: IShellItemArray = dialog.GetResults().ok()?;
        let count = items.GetCount().ok()?;
        let mut paths = Vec::with_capacity(count as usize);
        for i in 0..count {
            let item: IShellItem = items.GetItemAt(i).ok()?;
            let name: PWSTR = item.GetDisplayName(SIGDN_FILESYSPATH).ok()?;
            // FOS_PICKFOLDERS + FOS_FORCEFILESYSTEM：返回的必是文件系统路径
            let s = name.to_string().ok()?;
            paths.push(s);
        }
        Some(paths)
    }
}

/// 栅栏右键菜单：添加 / 布局 / 图标大小 / 透明度 / 背景色调 / 删除栅栏。
fn fence_context_menu(rt: &mut Runtime, fence: usize, sx: i32, sy: i32) -> Option<FenceMenuAction> {
    let hwnd = rt.hwnd;
    let app = &rt.desk.fences.get(fence)?.appearance;

    // 布局子菜单
    let layout_menu = popup_menu();
    if !layout_menu.is_invalid() {
        for (i, l) in [FenceLayout::Grid, FenceLayout::List].iter().enumerate() {
            let flag = if *l == app.layout {
                MF_STRING | MF_CHECKED
            } else {
                MF_STRING
            };
            let s = wide(l.label());
            unsafe {
                let _ = AppendMenuW(layout_menu, flag, MENU_LAYOUT + i, PCWSTR(s.as_ptr()));
            }
        }
    }

    // 图标大小子菜单：仅网格布局有图标大小概念，列表布局下不生成（隐藏该接口）
    let mut size_menu = HMENU::default();
    if app.layout == FenceLayout::Grid {
        size_menu = popup_menu();
        if !size_menu.is_invalid() {
            const SIZES: [(f32, &str); 3] =
                [(32.0, "小（32）"), (48.0, "中（48）"), (64.0, "大（64）")];
            for (i, (sz, lb)) in SIZES.iter().enumerate() {
                let flag = if (app.icon_size - sz).abs() < 0.5 {
                    MF_STRING | MF_CHECKED
                } else {
                    MF_STRING
                };
                let s = wide(lb);
                unsafe {
                    let _ = AppendMenuW(size_menu, flag, MENU_ICON_SIZE + i, PCWSTR(s.as_ptr()));
                }
            }
        }
    }

    // 背景色调子菜单：默认（玻璃底色）+ 预设色板
    let tint_menu = popup_menu();
    if !tint_menu.is_invalid() {
        let s = wide("默认（玻璃底色）");
        let flag_clear = if app.tint.is_none() {
            MF_STRING | MF_CHECKED
        } else {
            MF_STRING
        };
        let _ = unsafe { AppendMenuW(tint_menu, flag_clear, MENU_TINT, PCWSTR(s.as_ptr())) };
        for (i, (lb, c)) in TINT_PRESETS.iter().enumerate() {
            let s = wide(lb);
            let flag = if app.tint == Some(*c) {
                MF_STRING | MF_CHECKED
            } else {
                MF_STRING
            };
            let _ = unsafe { AppendMenuW(tint_menu, flag, MENU_TINT + 1 + i, PCWSTR(s.as_ptr())) };
        }
    }

    // 背景风格子菜单：玻璃 / 透明 / 颜色（三选一，当前风格打勾）
    let style_menu = popup_menu();
    if !style_menu.is_invalid() {
        let styles = [FenceStyle::Glass, FenceStyle::Outline, FenceStyle::Filled];
        for (i, st) in styles.iter().enumerate() {
            let flag = if *st == app.bg_style {
                MF_STRING | MF_CHECKED
            } else {
                MF_STRING
            };
            let s = wide(st.label());
            let _ = unsafe { AppendMenuW(style_menu, flag, MENU_STYLE + i, PCWSTR(s.as_ptr())) };
        }
    }

    // 主菜单
    let main = popup_menu();
    if main.is_invalid() {
        unsafe {
            let _ = DestroyMenu(layout_menu);
            let _ = DestroyMenu(size_menu);
            let _ = DestroyMenu(tint_menu);
            let _ = DestroyMenu(style_menu);
        }
        return None;
    }
    unsafe {
        // 单一「添加…」入口：系统对话框无法文件+文件夹混选（平台限制），用文件夹模式
        // 多选——能选中的（文件夹）直接添加，不区分「文件 / 文件夹」两个入口。
        let s = wide("添加…");
        let _ = AppendMenuW(main, MF_STRING, MENU_ADD, PCWSTR(s.as_ptr()));
        if !layout_menu.is_invalid() {
            let s = wide("布局");
            let _ = AppendMenuW(main, MF_POPUP, layout_menu.0 as usize, PCWSTR(s.as_ptr()));
        }
        if !size_menu.is_invalid() {
            let s = wide("图标大小");
            let _ = AppendMenuW(main, MF_POPUP, size_menu.0 as usize, PCWSTR(s.as_ptr()));
        }
        // 背景风格：玻璃 / 透明 / 颜色（三选一）
        if !style_menu.is_invalid() {
            let s = wide("背景风格");
            let _ = AppendMenuW(main, MF_POPUP, style_menu.0 as usize, PCWSTR(s.as_ptr()));
        }
        if !tint_menu.is_invalid() {
            let s = wide("背景色调");
            let _ = AppendMenuW(main, MF_POPUP, tint_menu.0 as usize, PCWSTR(s.as_ptr()));
        }
        let _ = AppendMenuW(main, MF_SEPARATOR, 0, PCWSTR::null());
        let s = wide("重命名栅栏");
        let _ = AppendMenuW(main, MF_STRING, MENU_RENAME_FENCE, PCWSTR(s.as_ptr()));
        let s = wide("粘贴文件（从剪贴板）");
        let _ = AppendMenuW(main, MF_STRING, MENU_PASTE, PCWSTR(s.as_ptr()));
        let _ = AppendMenuW(main, MF_SEPARATOR, 0, PCWSTR::null());
        let s = wide("删除栅栏");
        let _ = AppendMenuW(main, MF_STRING, MENU_DELETE_FENCE, PCWSTR(s.as_ptr()));
    }

    let cmd = unsafe {
        TrackPopupMenu(
            main,
            TPM_RETURNCMD | TPM_NONOTIFY,
            sx,
            sy,
            Some(0),
            hwnd,
            None,
        )
        .0 as usize
    };
    unsafe {
        let _ = DestroyMenu(layout_menu);
        let _ = DestroyMenu(size_menu);
        let _ = DestroyMenu(tint_menu);
        let _ = DestroyMenu(style_menu);
        let _ = DestroyMenu(main);
    }

    match cmd as usize {
        MENU_DELETE_FENCE => Some(FenceMenuAction::Delete),
        MENU_RENAME_FENCE => Some(FenceMenuAction::Rename),
        MENU_PASTE => Some(FenceMenuAction::Paste),
        MENU_ADD => Some(FenceMenuAction::Add),
        MENU_STYLE_GLASS => Some(FenceMenuAction::SetStyle(FenceStyle::Glass)),
        MENU_STYLE_TRANSPARENT => Some(FenceMenuAction::SetStyle(FenceStyle::Outline)),
        MENU_STYLE_COLOR => Some(FenceMenuAction::SetStyle(FenceStyle::Filled)),
        x if x >= MENU_TINT && x <= MENU_TINT + TINT_PRESETS.len() => {
            if x == MENU_TINT {
                Some(FenceMenuAction::SetTint(None))
            } else {
                TINT_PRESETS
                    .get(x - MENU_TINT - 1)
                    .map(|(_, c)| FenceMenuAction::SetTint(Some(*c)))
            }
        }
        MENU_LAYOUT_GRID => Some(FenceMenuAction::SetLayout(FenceLayout::Grid)),
        MENU_LAYOUT_LIST => Some(FenceMenuAction::SetLayout(FenceLayout::List)),
        MENU_ICON_SIZE_SMALL => Some(FenceMenuAction::SetIconSize(32.0)),
        MENU_ICON_SIZE_MID => Some(FenceMenuAction::SetIconSize(48.0)),
        MENU_ICON_SIZE_LARGE => Some(FenceMenuAction::SetIconSize(64.0)),
        _ => None,
    }
}

/// 创建一个弹出菜单句柄（失败返回无效句柄，后续用 `is_invalid` 判空）。
fn popup_menu() -> HMENU {
    unsafe { CreatePopupMenu().unwrap_or_default() }
}

/// 把字符串转成 UTF-16（含结尾 NUL），供 Win32 宽字符 API 使用。
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// 光标屏幕坐标（物理像素）。
fn cursor_screen() -> (i32, i32) {
    let mut pt = POINT::default();
    unsafe {
        let _ = GetCursorPos(&mut pt);
    }
    (pt.x, pt.y)
}

/// Windows 应用主题是否为深色模式（决定栅栏圆角矩形边框的黑白色）。
/// 读取 `HKCU\...\Themes\Personalize\AppsUseLightTheme`：0=深色应用，1=浅色应用。
/// 读取失败按浅色处理（Windows 默认）。
fn system_dark_mode() -> bool {
    use windows::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD};
    let subkey = wide(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize");
    let value = wide("AppsUseLightTheme");
    let mut data: u32 = 1;
    let mut size = std::mem::size_of::<u32>() as u32;
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            PCWSTR(subkey.as_ptr()),
            PCWSTR(value.as_ptr()),
            RRF_RT_REG_DWORD,
            None,
            Some(&mut data as *mut u32 as *mut core::ffi::c_void),
            Some(&mut size),
        )
    };
    status.is_ok() && data == 0
}

/// 列表布局下建议的栅栏宽度：名称列 + 类型/修改日期/大小三列 + 内边距。
fn list_auto_width(rt: &Runtime, fence: usize) -> f32 {
    let f = &rt.desk.fences[fence];
    let s = rt.theme.scale;
    let max_w: f32 = f
        .icon_ids
        .iter()
        .filter_map(|id| rt.desk.icons.get(id))
        .map(|ic| label_width(&ic.display_name, rt.theme.label.size))
        .fold(0.0, f32::max);
    let fixed = (LIST_TYPE_W + LIST_MOD_W + LIST_SIZE_W + LIST_COL_GAP * 3.0) * s;
    let base =
        f.appearance.padding * s * 2.0 + LIST_ICON_SIZE * s + rt.theme.list_label_gap + max_w;
    (base + fixed).max(420.0 * s)
}

/// 粗略估算标签文本宽度（CJK 按字号宽，ASCII 按 0.62 倍宽）。
fn label_width(text: &str, font_size: f32) -> f32 {
    let units: f32 = text
        .chars()
        .map(|c| if c.is_ascii() { 0.62 } else { 1.0 })
        .sum();
    units * font_size
}

/// 首次运行：把全部图标按稳定顺序平分成两个演示栅栏，并建立图标元数据。
/// 已有布局时不作任何改动（栅栏是用户显式成员列表）。
fn seed_fences(desk: &mut Desk, items: &[DesktopItem], theme: &Theme) {
    if !desk.fences.is_empty() || items.is_empty() {
        return;
    }
    // 元数据落库：图标按稳定 id 持久化，供渲染标签与成员引用
    for it in items {
        let mut ic = Icon::new(it.id.clone(), it.display_name.clone(), it.kind);
        ic.path = it.path.clone();
        if let Some(p) = it.path.as_deref() {
            sylva_core::details::enrich(&mut ic, p);
        }
        desk.icons.entry(it.id.clone()).or_insert(ic);
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
        scroll: 0.0,
    };
    // 用布局算出的高度定位第二个栅栏（首栅栏暂不在 desk 中，单独算一次几何）
    let sf1 = layout_fence(theme, &f1, desk, &empty, None, &[], None, 0);

    // 第二个栅栏用「透明」风格演示多种背景风格
    let app2 = FenceAppearance {
        bg_style: FenceStyle::Outline,
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
        scroll: 0.0,
    };

    desk.fences.push(f1);
    desk.fences.push(f2);
    tracing::info!(
        fences = desk.fences.len(),
        icons = desk.icons.len(),
        "首次运行：已创建演示栅栏布局"
    );
}

/// 把整个桌面状态排布成场景（每个栅栏按网格/列表排布；内容超出滚动）。
fn build_scene(rt: &mut Runtime, now: Instant) -> Scene {
    let alpha = fence_alpha(rt, now);
    let mut scene = Scene::new(rt.vw, rt.vh);
    for i in 0..rt.desk.fences.len() {
        // 视觉矩形：拖拽/缩放补间期间用插值（模型已是目标值，场景跟随动画）
        let mut f = rt.desk.fences[i].clone();
        f.bounds = fence_visual_rect(rt, i);
        let mut sf = layout_fence(
            &rt.theme,
            &f,
            &rt.desk,
            &rt.bitmap_ids,
            rt.hover,
            &rt.selected,
            rt.select_band,
            i,
        );
        // 桌面切换整体淡入淡出
        sf.alpha = alpha;
        // 悬停放大：只对当前悬停图标生效（列表行内小图标同样放大）
        if let Some((hf, hi)) = rt.hover {
            if hf == i {
                if let Some(ic) = sf.icons.get_mut(hi) {
                    ic.scale = icon_hover_scale(rt, hf, hi, now);
                }
            }
        }
        // 回写钳制后的滚动偏移（滚轮事件在 layout 内被限制在 [0, max_scroll]）
        rt.desk.fences[i].scroll = sf.scroll;
        scene.fences.push(sf);
    }
    // 控制中心：始终在场（折叠胶囊 ⇄ 展开面板），折叠时的命中区由 hit 模型裁剪为胶囊。
    scene.console = Some(build_console(rt, rt.console.scroll, &rt.console_anim, now));
    scene
}

/// 栅栏整体透明度（0=隐藏，1=完全显示）：桌面切换补间推进中取插值，否则按模式取终值。
fn fence_alpha(rt: &Runtime, now: Instant) -> f32 {
    if let Some(t) = rt.desktop_fade {
        match tween_progress(t.t0, t.dur, now) {
            Some(p) => t.from + (t.to - t.from) * ease_out_cubic(p),
            None => t.to,
        }
    } else if rt.desk.desktop_mode {
        0.0
    } else {
        1.0
    }
}

/// 栅栏当前视觉矩形：有补间时取插值（拖拽跟随），否则 = 模型矩形。
fn fence_visual_rect(rt: &Runtime, fence: usize) -> Rect {
    let now = Instant::now();
    if let Some(t) = rt.fence_tweens.iter().find(|t| t.fence == fence) {
        match tween_progress(t.t0, t.dur, now) {
            Some(p) => {
                let e = ease_out_cubic(p);
                Rect::new(
                    t.from.x + (t.to.x - t.from.x) * e,
                    t.from.y + (t.to.y - t.from.y) * e,
                    t.from.w + (t.to.w - t.from.w) * e,
                    t.from.h + (t.to.h - t.from.h) * e,
                )
            }
            None => t.to,
        }
    } else {
        rt.desk
            .fences
            .get(fence)
            .map(|f| f.bounds)
            .unwrap_or_default()
    }
}

/// 记录/替换栅栏补间（同一栅栏重复拖动时从当前视觉位置接续，不跳变）并启动动画定时器。
fn set_fence_tween(rt: &mut Runtime, tween: FenceTween) {
    rt.fence_tweens.retain(|t| t.fence != tween.fence);
    rt.fence_tweens.push(tween);
    arm_anim_timer(rt);
}

/// 图标悬停当前缩放（1.0 = 常态，1.06 = 完全放大）。
fn icon_hover_scale(rt: &Runtime, fence: usize, icon: usize, now: Instant) -> f32 {
    match rt.icon_hover {
        Some(h) if h.fence == fence && h.icon == icon => 1.0 + 0.06 * h.progress(now),
        _ => 1.0,
    }
}

/// 图标悬停补间是否仍在推进（决定动画定时器是否需要跑）。
fn icon_hover_active(rt: &Runtime) -> bool {
    rt.icon_hover
        .map(|h| tween_progress(h.t0, h.dur, Instant::now()).is_some())
        .unwrap_or(false)
}

/// 控制中心展开后的完整高度（DIP）：取待办 / 便签 / 栅栏管理 / 插件 各页需要高度的
/// 最大值（所有标签页共享同一面板高度，切换标签不会跳变），并钳制在最大高度内。
fn console_full_height(desk: &Desk, s: f32) -> f32 {
    let title_tab = (CONSOLE_TITLE_H + CONSOLE_TAB_H) * s;
    let todo_h = title_tab
        + CONSOLE_INPUT_H * s
        + CONSOLE_INPUT_GAP * s
        + CONSOLE_INPUT_H * s
        + 10.0 * s
        + desk.todos.len().clamp(1, CONSOLE_MAX_ROWS) as f32 * CONSOLE_ROW_H * s
        + 12.0 * s;
    let fences_h = title_tab
        + 8.0 * s
        + desk.fences.len().min(CONSOLE_FENCE_MAX_ROWS) as f32 * CONSOLE_FENCE_ROW_H * s
        + 8.0 * s
        + CONSOLE_FENCE_DETAIL_H * s
        + 12.0 * s;
    let plugins_h = title_tab
        + 8.0 * s
        + desk.plugins.len().clamp(1, CONSOLE_PLUGIN_MAX_ROWS) as f32 * CONSOLE_PLUGIN_ROW_H * s
        + 8.0 * s
        + 34.0 * s
        + 12.0 * s;
    let notes_h = title_tab + 8.0 * s + CONSOLE_NOTES_H * s + 12.0 * s;
    todo_h
        .max(fences_h)
        .max(plugins_h)
        .max(notes_h)
        .clamp(CONSOLE_MIN_H * s, CONSOLE_MAX_H * s)
}

/// 控制中心面板矩形（物理像素，虚拟屏幕坐标）。
///
/// 未拖动过（`console_pos == None`）时默认摆右上角；拖动后记住 `console_pos`
/// 左上角。高度按 `panel` 进度（0..1）在「折叠胶囊」与「完整面板」间插值——
/// 胶囊始终可见（控制中心入口不会再找不到）。
///
/// 尺寸策略：未手动缩放过（`console_size == None`）时宽取 `CONSOLE_W`、高取
/// `console_full_height`（标签页/内容自适应）；用户拖边缘/角缩放后
/// `console_size` 落为具体宽高（钳制在最小尺寸之上），之后高度固定、超出滚动。
fn console_geometry(desk: &Desk, theme: &Theme, vw: f32, _vh: f32, panel: f32) -> RectF {
    let s = theme.scale;
    let auto_full_h = console_full_height(desk, s);
    let (w, full_h) = match desk.console_size {
        Some((w, h)) => (w.max(CONSOLE_MIN_W * s), h.max(CONSOLE_MIN_H * s)),
        None => (CONSOLE_W * s, auto_full_h),
    };
    let pill_h = CONSOLE_PILL_H * s;
    // 允许小幅过冲（展开回弹），1.12 上限避免面板瞬时过高
    let h = pill_h + (full_h - pill_h) * panel.clamp(0.0, 1.12);
    let (x, y) = match desk.console_pos {
        Some(p) => (p.x, p.y),
        None => (
            (vw - w - CONSOLE_MARGIN * s).max(8.0 * s),
            (CONSOLE_MARGIN * s).max(8.0 * s),
        ),
    };
    RectF { x, y, w, h }
}

/// 当前标签页顺序（待办 → 便签（启用时）→ 栅栏 → 插件）。
fn console_tab_order(rt: &Runtime) -> Vec<ConsoleTab> {
    let mut order = vec![ConsoleTab::Todo];
    if rt
        .desk
        .plugins
        .iter()
        .any(|p| p.kind == PluginKind::Notes && p.enabled)
    {
        order.push(ConsoleTab::Notes);
    }
    order.push(ConsoleTab::Fences);
    order.push(ConsoleTab::Plugins);
    order
}

/// 待办页内容区顶边（标题栏 + 标签栏之下）。
fn console_content_top(panel: &RectF, s: f32) -> f32 {
    panel.y + CONSOLE_TITLE_H * s + CONSOLE_TAB_H * s
}

/// 待办「名称」输入框矩形（面板内）。
fn console_input_rect(panel: &RectF, s: f32) -> RectF {
    let top = console_content_top(panel, s) + 8.0 * s;
    RectF {
        x: panel.x + CONSOLE_PAD * s,
        y: top,
        w: (panel.w - 2.0 * CONSOLE_PAD * s - CONSOLE_ADD_W * s - 8.0 * s).max(40.0),
        h: CONSOLE_INPUT_H * s,
    }
}

/// 待办「详细信息」输入框矩形（名称行之下，全宽）。
fn console_detail_rect(panel: &RectF, s: f32) -> RectF {
    let name = console_input_rect(panel, s);
    RectF {
        x: name.x,
        y: name.y + name.h + CONSOLE_INPUT_GAP * s,
        w: (panel.w - 2.0 * CONSOLE_PAD * s).max(40.0),
        h: CONSOLE_INPUT_H * s,
    }
}

/// 便签页文本卡片矩形（面板内）。
fn console_notes_rect(panel: &RectF, s: f32) -> RectF {
    RectF {
        x: panel.x + CONSOLE_PAD * s,
        y: console_content_top(panel, s) + 8.0 * s,
        w: (panel.w - 2.0 * CONSOLE_PAD * s).max(40.0),
        h: CONSOLE_NOTES_H * s,
    }
}

/// 待办列表最大可滚动量（物理像素；0 = 条目不超出可视区，无滚动）。
///
/// 可视行数由面板实际高度决定：自动高度时恰好装下全部（≤MAX_ROWS），手动
/// 缩放后高度固定、超出滚动——两种模式统一按「面板高 - 头部区」推算行数。
fn console_scroll_max(rt: &Runtime) -> f32 {
    let n = rt.desk.todos.len();
    if n == 0 {
        return 0.0;
    }
    let s = rt.theme.scale;
    let panel = console_geometry(&rt.desk, &rt.theme, rt.vw, rt.vh, 1.0);
    let header_h = (CONSOLE_TITLE_H + CONSOLE_TAB_H) * s
        + CONSOLE_INPUT_H * s
        + CONSOLE_INPUT_GAP * s
        + CONSOLE_INPUT_H * s
        + 22.0 * s;
    let view_h = (panel.h - header_h).max(0.0);
    let row_h = CONSOLE_ROW_H * s;
    let shown = (view_h / row_h).floor().max(0.0) as usize;
    if n <= shown {
        return 0.0;
    }
    (n - shown) as f32 * row_h
}

/// 构建控制中心面板场景（待办 / 便签 / 栅栏管理 / 插件管理）。面板始终在场：
/// 折叠胶囊（panel<0.5）⇄ 展开面板（panel≥0.5），高度按 `anim.panel` 插值。
///
/// 待办行采用流体布局：删除中的幽灵行按「原始下标」插回 live 列表（恢复删除前顺序），
/// 其槽位高度随动画收缩、原位淡出，下方行平滑上滑补位——无需坐标回推，多次并发
/// 删除也正确。行透明度/勾选淡化/入场下滑由 `SceneTodoRow` 动画字段承载。
fn build_console(
    rt: &Runtime,
    console_scroll: f32,
    anim: &ConsoleAnim,
    now: Instant,
) -> SceneConsole {
    let desk = &rt.desk;
    let theme = &rt.theme;
    let s = theme.scale;
    let panel = console_geometry(desk, theme, rt.vw, rt.vh, anim.panel);
    let title_h = CONSOLE_TITLE_H * s;
    let tab_h = CONSOLE_TAB_H * s;
    let input_h = CONSOLE_INPUT_H * s;
    let row_h = CONSOLE_ROW_H * s;
    let content_top = console_content_top(&panel, s);

    // —— 标签栏（待办 → 便签（启用时）→ 栅栏 → 插件）——
    let order = console_tab_order(rt);
    let active_tab = rt.console_tab.min(order.len().saturating_sub(1));
    let active_kind = order[active_tab];
    let tab_w = (panel.w - 2.0 * CONSOLE_PAD * s) / order.len().max(1) as f32;
    let tabs: Vec<SceneTab> = order
        .iter()
        .enumerate()
        .map(|(i, t)| SceneTab {
            rect: RectF {
                x: panel.x + CONSOLE_PAD * s + i as f32 * tab_w,
                y: panel.y + title_h,
                w: tab_w,
                h: tab_h,
            },
            label: t.label().to_string(),
            active: i == active_tab,
        })
        .collect();

    // —— 标题栏：关闭 + 切换桌面 ——
    let close = RectF {
        x: panel.x + panel.w - CONSOLE_CLOSE_W * s - 8.0 * s,
        y: panel.y + 8.0 * s,
        w: CONSOLE_CLOSE_W * s,
        h: CONSOLE_CLOSE_W * s,
    };
    let toggle_h = 26.0 * s;
    let desktop_toggle = RectF {
        x: close.x - CONSOLE_TOGGLE_W * s - 6.0 * s,
        y: panel.y + (title_h - toggle_h) / 2.0,
        w: CONSOLE_TOGGLE_W * s,
        h: toggle_h,
    };

    // —— 待办页 ——
    let input = console_input_rect(&panel, s);
    let detail_input = console_detail_rect(&panel, s);
    let add = RectF {
        x: input.x + input.w + 8.0 * s,
        y: input.y,
        w: CONSOLE_ADD_W * s,
        h: input_h,
    };
    let rows_top = detail_input.y + detail_input.h + 10.0 * s;
    let n = desk.todos.len();
    let shown = n.clamp(1, CONSOLE_MAX_ROWS);
    let scroll_max = if n > shown {
        (n - shown) as f32 * row_h
    } else {
        0.0
    };
    let scroll = console_scroll.clamp(0.0, scroll_max);
    let box_s = (16.0 * s).min(input_h);

    // 合并渲染行：live 待办在前，删除幽灵按原始下标插回（恢复删除前的顺序）。
    let mut render: Vec<(u64, Option<&ExitRow>, &str, &str, bool)> = Vec::with_capacity(n);
    for t in &desk.todos {
        render.push((t.id, None, t.name.as_str(), t.detail.as_str(), t.done));
    }
    let mut exits: Vec<&ExitRow> = anim.exiting.iter().collect();
    exits.sort_by_key(|e| e.index);
    for e in exits {
        let pos = e.index.min(render.len());
        render.insert(
            pos,
            (e.id, Some(e), e.name.as_str(), e.detail.as_str(), e.done),
        );
    }
    let mut rows = Vec::with_capacity(render.len());
    let mut checkbox = Vec::with_capacity(render.len());
    let mut del = Vec::with_capacity(render.len());
    let mut hit_index = Vec::with_capacity(render.len());
    let mut cursor = rows_top - scroll;
    let mut live_i = 0usize;
    for (id, exit, name, detail, done) in render {
        let (factor, mut alpha) = match exit {
            Some(e) => {
                let p = tween_progress(e.t0, e.dur, now)
                    .map(ease_out_cubic)
                    .unwrap_or(0.0);
                (1.0 - p, 1.0 - p)
            }
            None => (1.0, 1.0),
        };
        let mut slide = 0.0f32;
        let mut done_progress = 1.0f32;
        if let Some(ra) = anim.rows.get(&id) {
            if let Some(en) = ra.enter {
                if let Some(p) = tween_progress(en.t0, en.dur, now) {
                    let e = ease_out_back(p);
                    alpha *= e;
                    slide = (1.0 - e) * 8.0 * s;
                }
            }
            if let Some(tg) = ra.toggle {
                if let Some(p) = tween_progress(tg.t0, tg.dur, now) {
                    done_progress = ease_out_cubic(p);
                }
            }
        }
        let ry = cursor + slide;
        checkbox.push(RectF {
            x: panel.x + CONSOLE_PAD * s,
            y: ry + (row_h - box_s) / 2.0,
            w: box_s,
            h: box_s,
        });
        del.push(RectF {
            x: panel.x + panel.w - CONSOLE_PAD * s - 22.0 * s,
            y: ry + (row_h - 22.0 * s) / 2.0,
            w: 22.0 * s,
            h: 22.0 * s,
        });
        hit_index.push(if exit.is_none() {
            let i = live_i;
            live_i += 1;
            Some(i)
        } else {
            None
        });
        rows.push(SceneTodoRow {
            name: name.to_string(),
            detail: detail.to_string(),
            done,
            alpha: alpha.clamp(0.0, 1.0),
            done_progress,
        });
        cursor += row_h * factor;
    }

    // —— 便签页 ——
    let notes_enabled = desk
        .plugins
        .iter()
        .any(|p| p.kind == PluginKind::Notes && p.enabled);
    let notes_text = desk
        .plugins
        .iter()
        .find(|p| p.kind == PluginKind::Notes)
        .map(|p| p.note_text.clone())
        .unwrap_or_default();
    let notes = if notes_enabled {
        Some(SceneNotes {
            rect: console_notes_rect(&panel, s),
            text: notes_text,
        })
    } else {
        None
    };

    // —— 栅栏管理页 ——
    let list_top = content_top + 8.0 * s;
    let row_h_f = CONSOLE_FENCE_ROW_H * s;
    let fence_n = desk.fences.len();
    let fence_shown = fence_n.min(CONSOLE_FENCE_MAX_ROWS);
    let fence_scroll_max = if fence_n > fence_shown {
        (fence_n - fence_shown) as f32 * row_h_f
    } else {
        0.0
    };
    let fence_scroll = rt.console.fence_scroll.clamp(0.0, fence_scroll_max);
    let fence_list_view = RectF {
        x: panel.x + CONSOLE_PAD * s,
        y: list_top,
        w: panel.w - 2.0 * CONSOLE_PAD * s,
        h: fence_shown as f32 * row_h_f,
    };
    let sel = rt.selected_fence.min(fence_n.saturating_sub(1));
    let fence_rows: Vec<SceneFenceRow> = desk
        .fences
        .iter()
        .enumerate()
        .map(|(i, f)| SceneFenceRow {
            rect: RectF {
                x: panel.x + CONSOLE_PAD * s,
                y: list_top + i as f32 * row_h_f - fence_scroll,
                w: panel.w - 2.0 * CONSOLE_PAD * s,
                h: row_h_f,
            },
            title: f.title.clone().unwrap_or_else(|| format!("栅栏 {}", i + 1)),
            selected: i == sel,
        })
        .collect();
    let fence_detail = if fence_n > 0 {
        let d = RectF {
            x: panel.x + CONSOLE_PAD * s,
            y: list_top + fence_shown as f32 * row_h_f + 8.0 * s,
            w: panel.w - 2.0 * CONSOLE_PAD * s,
            h: CONSOLE_FENCE_DETAIL_H * s,
        };
        let app = &desk.fences[sel].appearance;
        let btn_h = 24.0 * s;
        let label_w = 40.0 * s;
        let row_y = |i: usize| d.y + 24.0 * s + i as f32 * 30.0 * s;
        // 布局：网格 / 列表
        let layout_grid = RectF {
            x: d.x + label_w,
            y: row_y(0),
            w: 52.0 * s,
            h: btn_h,
        };
        let layout_list = RectF {
            x: layout_grid.x + layout_grid.w + 6.0 * s,
            y: row_y(0),
            w: 52.0 * s,
            h: btn_h,
        };
        // 图标大小：小 / 中 / 大
        let size_s = RectF {
            x: d.x + label_w,
            y: row_y(1),
            w: 40.0 * s,
            h: btn_h,
        };
        let size_m = RectF {
            x: size_s.x + size_s.w + 6.0 * s,
            y: row_y(1),
            w: 40.0 * s,
            h: btn_h,
        };
        let size_l = RectF {
            x: size_m.x + size_m.w + 6.0 * s,
            y: row_y(1),
            w: 40.0 * s,
            h: btn_h,
        };
        // 背景风格：玻璃 / 描边 / 颜色
        let style_glass = RectF {
            x: d.x + label_w,
            y: row_y(2),
            w: 48.0 * s,
            h: btn_h,
        };
        let style_outline = RectF {
            x: style_glass.x + style_glass.w + 6.0 * s,
            y: row_y(2),
            w: 48.0 * s,
            h: btn_h,
        };
        let style_filled = RectF {
            x: style_outline.x + style_outline.w + 6.0 * s,
            y: row_y(2),
            w: 48.0 * s,
            h: btn_h,
        };
        // 色调：默认 + 预设色板
        let sw = 18.0 * s;
        let gap = 6.0 * s;
        let tint_y = row_y(3) + (btn_h - sw) / 2.0;
        let tint_default = RectF {
            x: d.x + label_w,
            y: tint_y,
            w: sw,
            h: sw,
        };
        let mut tints = Vec::with_capacity(TINT_PRESETS.len());
        let mut x = tint_default.x + sw + gap;
        for _ in TINT_PRESETS {
            tints.push(RectF {
                x,
                y: tint_y,
                w: sw,
                h: sw,
            });
            x += sw + gap;
        }
        Some(SceneFenceDetail {
            rect: d,
            title: desk.fences[sel]
                .title
                .clone()
                .unwrap_or_else(|| format!("栅栏 {}", sel + 1)),
            layout: app.layout,
            icon_size: app.icon_size,
            style: app.bg_style,
            tint: app.tint,
            layout_grid,
            layout_list,
            size_s,
            size_m,
            size_l,
            style_glass,
            style_outline,
            style_filled,
            tint_default,
            tints,
        })
    } else {
        None
    };

    // —— 插件页 ——
    let plugin_rows: Vec<ScenePluginRow> = desk
        .plugins
        .iter()
        .take(CONSOLE_PLUGIN_MAX_ROWS)
        .enumerate()
        .map(|(i, p)| {
            let rect = RectF {
                x: panel.x + CONSOLE_PAD * s,
                y: content_top + 8.0 * s + i as f32 * CONSOLE_PLUGIN_ROW_H * s,
                w: panel.w - 2.0 * CONSOLE_PAD * s,
                h: CONSOLE_PLUGIN_ROW_H * s,
            };
            let tw = 40.0 * s;
            let th = 22.0 * s;
            ScenePluginRow {
                rect,
                name: p.name.clone(),
                desc: p.desc.clone(),
                version: p.version.clone(),
                enabled: p.enabled,
                toggle: RectF {
                    x: rect.x + rect.w - tw - 4.0 * s,
                    y: rect.y + (rect.h - th) / 2.0,
                    w: tw,
                    h: th,
                },
            }
        })
        .collect();
    let open_plugins = RectF {
        x: panel.x + CONSOLE_PAD * s,
        y: content_top + 8.0 * s + CONSOLE_PLUGIN_MAX_ROWS as f32 * CONSOLE_PLUGIN_ROW_H * s,
        w: panel.w - 2.0 * CONSOLE_PAD * s,
        h: 30.0 * s,
    };

    SceneConsole {
        x: panel.x,
        y: panel.y,
        width: panel.w,
        height: panel.h,
        title_h,
        tab_h,
        tabs,
        active_tab,
        active_kind,
        close,
        desktop_toggle,
        input,
        detail_input,
        add,
        checkbox,
        del,
        todo: SceneTodo {
            rows,
            scroll,
            scroll_max,
            rows_top,
            row_h,
        },
        notes,
        fence_rows,
        fence_list_view,
        fence_detail,
        plugin_rows,
        open_plugins,
        fill_color: [0.062, 0.086, 0.133, 0.92],
        border_color: [1.0, 1.0, 1.0, 0.18],
        panel: anim.panel,
        count: n,
        hit_index,
        hover_zone: if anim.panel >= 0.5 {
            rt.console_hover
        } else {
            None
        },
        desktop_mode: desk.desktop_mode,
    }
}

/// 列表布局详情列的固定宽度与间距（物理像素）。
const LIST_TYPE_W: f32 = 90.0;
const LIST_MOD_W: f32 = 140.0;
const LIST_SIZE_W: f32 = 80.0;
const LIST_COL_GAP: f32 = 16.0;
/// 列表行内的小图标尺寸（详情列表风格，与「图标大小」无关）。
const LIST_ICON_SIZE: f32 = 20.0;
/// 列表未手动缩放时的最大可见行数（超出滚动）。
const LIST_AUTO_ROWS: usize = 8;

/// 单个栅栏的排布（网格 / 列表）。
///
/// - 宽度：用户控制的 `fence.bounds.w`（列表切过去时按最长标签自动收窄）；
/// - 高度：未手动缩放过（`bounds.h <= 0`）时按内容自适应——网格长满、列表最多
///   8 行（超出滚动）；手动缩放过则固定，内容超出后用滚轮滚动（右缘有指示条）；
/// - 网格：自左向右、自上而下排布，标签在图标下方；
/// - 列表：单列纵向，名称/类型/修改日期/大小四列 + 固定列头，滚轮滚动。
/// - 滚动：所有图标都在场景里（位置按 `scroll` 平移），绘制时用内容区裁剪，
///   命中模型跳过滚出可视区的项。返回的 `SceneFence.scroll` 已被钳制，
///   `build_scene` 据此回写 `desk.fences[i].scroll`。
#[allow(clippy::too_many_arguments)]
fn layout_fence(
    theme: &Theme,
    fence: &Fence,
    desk: &Desk,
    bitmap_ids: &HashMap<String, u64>,
    hover: Option<(usize, usize)>,
    selected: &[(usize, usize)],
    select_band: Option<(usize, RectF)>,
    fence_idx: usize,
) -> SceneFence {
    let app = &fence.appearance;
    // 模型里的外观是 DIP 逻辑值（持久化，跨 DPI 稳定），布局时 × scale 变物理像素。
    let scale = theme.scale;
    let pad = app.padding * scale;
    let n = fence.icon_ids.len();
    let title_block_h = theme.title.size * 1.6 + theme.title_padding_bottom;
    let content_top = fence.bounds.y + pad + title_block_h + pad;
    let content_left = fence.bounds.x + pad;
    let inner_w = (fence.bounds.w - 2.0 * pad).max(1.0);

    // 预取名称 + 位图 + 详情列文本（布局与绘制共用）。
    let rows: Vec<(String, u64, String, String, String)> = fence
        .icon_ids
        .iter()
        .map(|id| {
            let ic = desk.icons.get(id);
            let label = ic.map(|i| i.display_name.clone()).unwrap_or_default();
            let bitmap = bitmap_ids.get(id).copied().unwrap_or(u64::MAX);
            match ic {
                Some(i) => (
                    label,
                    bitmap,
                    i.type_label.clone(),
                    sylva_shell::time::format_modified(i.modified_secs),
                    sylva_core::details::format_size(i.size_bytes),
                ),
                None => (label, bitmap, String::new(), String::new(), String::new()),
            }
        })
        .collect();

    let hover_icon = hover.filter(|&(fi, _)| fi == fence_idx).map(|(_, ii)| ii);
    let selected: Vec<usize> = selected
        .iter()
        .filter(|&&(fi, _)| fi == fence_idx)
        .map(|&(_, ii)| ii)
        .collect();
    let select_band = select_band
        .filter(|&(fi, _)| fi == fence_idx)
        .map(|(_, r)| r);

    let (icons, scroll, scroll_max, scroll_view, list_cols, height) = match app.layout {
        FenceLayout::Grid => {
            let icon_size = app.icon_size * scale;
            let gap = app.gap * scale;
            // 横向步距：图标 + 图标间距；纵向步距：图标 + 标签间距 + 标签高。
            // 二者必须分开——共用一个 cell 会让标签叠进下一行图标（行列重叠根因）。
            let cell_w = icon_size + gap;
            let row_h = icon_size + theme.icon_caption_gap + theme.label.size * 1.6;
            let cols = ((inner_w / cell_w).floor() as usize).max(1);
            let rows_n = n.div_ceil(cols).max(1);
            // 内容总高 = 每行（图标 + 标签）累加，末行标签也有高度
            let content_full = rows_n as f32 * row_h;
            let auto_h = pad + title_block_h + pad + content_full + pad;
            let h = if fence.bounds.h > 0.0 {
                fence.bounds.h.max(MIN_FENCE_H)
            } else {
                auto_h
            };
            let view = (h - pad - title_block_h - pad - pad).max(0.0);
            let scroll_max = (content_full - view).max(0.0);
            let scroll = fence.scroll.clamp(0.0, scroll_max);
            let icons = grid_icons(
                theme,
                fence,
                &rows,
                cols,
                cell_w,
                row_h,
                content_top,
                content_left,
                scroll,
                icon_size,
            );
            (icons, scroll, scroll_max, view, None, h)
        }
        FenceLayout::List => {
            let label_h = theme.label.size * 1.6;
            let list_icon = LIST_ICON_SIZE * scale;
            // 行高至少容纳标签高（或图标高），再加行距；否则 24px 文字叠进下一行
            let row_h = list_icon.max(label_h) + theme.list_row_gap;
            let header_h = label_h + 8.0 * scale;
            // 内容总高（所有行）
            let content_full = if n > 0 {
                n as f32 * row_h - theme.list_row_gap
            } else {
                0.0
            };
            // 未手动缩放：最多显示 LIST_AUTO_ROWS 行，超出滚动
            let auto_rows = n.clamp(1, LIST_AUTO_ROWS);
            let auto_rows_h = (auto_rows as f32 * row_h - theme.list_row_gap).max(0.0);
            let auto_h = pad + title_block_h + pad + header_h + auto_rows_h + pad;
            let h = if fence.bounds.h > 0.0 {
                fence.bounds.h.max(MIN_FENCE_H)
            } else {
                auto_h
            };
            let view = (h - pad - title_block_h - pad - header_h - pad).max(0.0);
            let scroll_max = (content_full - view).max(0.0);
            let scroll = fence.scroll.clamp(0.0, scroll_max);
            // 四列：名称列吃剩余宽度，其余固定（列宽同样 × scale）
            let type_w = LIST_TYPE_W * scale;
            let mod_w = LIST_MOD_W * scale;
            let size_w = LIST_SIZE_W * scale;
            let col_gap = LIST_COL_GAP * scale;
            let name_w = (inner_w - col_gap * 3.0 - type_w - mod_w - size_w).max(60.0 * scale);
            let type_x = content_left + name_w + col_gap;
            let modified_x = type_x + type_w + col_gap;
            let size_x = modified_x + mod_w + col_gap;
            let cols = ListColumns {
                type_x,
                modified_x,
                size_x,
                header_h,
            };
            let icons = list_icons(
                theme,
                fence,
                &rows,
                content_top,
                content_left,
                header_h,
                row_h,
                scroll,
                list_icon,
            );
            (icons, scroll, scroll_max, view, Some(cols), h)
        }
    };

    // 背景填充按「背景风格」决定（玻璃 / 透明 / 颜色），与旧的透明度滑块无关：
    // - 颜色（Filled）：不透明纯色，颜色 = 背景色调（未选时用默认背景色）；
    // - 玻璃（Glass）：半透明玻璃底，默认底色并向「背景色调」靠拢 45%（保留暗底质感）；
    // - 透明（Outline）：完全透明，只留圆角描边（fill_color = None，边框照常绘制）。
    let bg = app.bg_color;
    let tint_rgb = match app.tint {
        Some(t) => [t[0], t[1], t[2]],
        None => [bg[0], bg[1], bg[2]],
    };
    let glass_rgb = match app.tint {
        Some(t) => [
            bg[0] + (t[0] - bg[0]) * 0.45,
            bg[1] + (t[1] - bg[1]) * 0.45,
            bg[2] + (t[2] - bg[2]) * 0.45,
        ],
        None => [bg[0], bg[1], bg[2]],
    };
    let fill_color = match app.bg_style {
        FenceStyle::Filled => Some([tint_rgb[0], tint_rgb[1], tint_rgb[2], 1.0]),
        FenceStyle::Glass => Some([glass_rgb[0], glass_rgb[1], glass_rgb[2], 0.55]),
        FenceStyle::Outline => None,
    };
    // 圆角矩形边框跟随 Windows 主题（深色=白、浅色=黑），中粗固定宽度；
    // 透明度保持清晰可见（暗色 42% / 浅色 45%）。
    let border_color = if system_dark_mode() {
        [1.0, 1.0, 1.0, 0.42]
    } else {
        [0.0, 0.0, 0.0, 0.45]
    };
    let border_width = MEDIUM_BORDER_WIDTH * scale;

    SceneFence {
        x: fence.bounds.x,
        y: fence.bounds.y,
        width: fence.bounds.w,
        height,
        title: fence.title.clone().unwrap_or_default(),
        icons,
        layout: app.layout,
        list_cols,
        scroll,
        scroll_max,
        scroll_view,
        content_top,
        content_left,
        hover_icon,
        selected,
        select_band,
        border_width,
        border_color,
        fill_color,
        alpha: 1.0,
    }
}

/// 网格排布全部图标位置（不裁剪；滚动用 `scroll` 平移，绘制时裁剪）。
#[allow(clippy::too_many_arguments)]
fn grid_icons(
    theme: &Theme,
    _fence: &Fence,
    rows: &[(String, u64, String, String, String)],
    cols: usize,
    cell_w: f32,
    row_h: f32,
    content_top: f32,
    content_left: f32,
    scroll: f32,
    icon_size: f32,
) -> Vec<SceneIcon> {
    let _ = theme;
    rows.iter()
        .enumerate()
        .map(|(i, (label, bitmap, ct, cm, cs))| SceneIcon {
            label: label.clone(),
            bitmap_id: *bitmap,
            x: content_left + (i % cols) as f32 * cell_w,
            y: content_top + (i / cols) as f32 * row_h - scroll,
            size: icon_size,
            col_type: ct.clone(),
            col_modified: cm.clone(),
            col_size: cs.clone(),
            scale: 1.0,
        })
        .collect()
}

/// 列表排布全部图标位置（单列纵向；滚动用 `scroll` 平移，绘制时裁剪）。
#[allow(clippy::too_many_arguments)]
fn list_icons(
    theme: &Theme,
    fence: &Fence,
    rows: &[(String, u64, String, String, String)],
    content_top: f32,
    content_left: f32,
    header_h: f32,
    row_h: f32,
    scroll: f32,
    size: f32,
) -> Vec<SceneIcon> {
    let _ = (theme, fence);
    rows.iter()
        .enumerate()
        .map(|(i, (label, bitmap, ct, cm, cs))| SceneIcon {
            label: label.clone(),
            bitmap_id: *bitmap,
            x: content_left,
            y: content_top + header_h + i as f32 * row_h - scroll,
            size,
            col_type: ct.clone(),
            col_modified: cm.clone(),
            col_size: cs.clone(),
            scale: 1.0,
        })
        .collect()
}

/// 由场景几何生成命中模型：栅栏（标题移动把手 + 右下角缩放把手 + 整体区域）与
/// 图标（双击打开）。`fence`/`icon` 下标与 `desk.fences` / `icon_ids` 对应。
fn hit_model_from(theme: &Theme, scene: &Scene, _desk: &Desk) -> HitModel {
    let mut fences = Vec::with_capacity(scene.fences.len());
    let mut icons = Vec::new();
    for (fi, f) in scene.fences.iter().enumerate() {
        // 桌面切换淡出中的栅栏不参与命中（区域随之收缩，点击穿透到桌面）
        if f.alpha <= 0.01 {
            continue;
        }
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
        // 可视区（列头之下）：滚出可视区的图标不参与命中，避免误点。
        let view_top = f.content_top + f.list_cols.map(|c| c.header_h).unwrap_or(0.0);
        let view_bottom = view_top + f.scroll_view;
        for (ii, icon) in f.icons.iter().enumerate() {
            if icon.y + icon.size < view_top || icon.y > view_bottom {
                continue;
            }
            // 列表布局整行可点（图标 + 名称 + 详情列），双击/右键/悬停都对整行生效，
            // 不再只有那一小块图标有反应（列表栅栏「点了没反应」根因）。
            let rect = if f.layout == FenceLayout::List {
                let pad = f.content_left - f.x;
                let row_extent = icon.size.max(theme.label.size * 1.6) + theme.list_row_gap;
                RectF {
                    x: f.content_left,
                    y: icon.y,
                    w: (f.width - 2.0 * pad).max(1.0),
                    h: row_extent,
                }
            } else {
                RectF {
                    x: icon.x,
                    y: icon.y,
                    w: icon.size,
                    h: icon.size,
                }
            };
            icons.push(IconHit {
                rect,
                fence: fi,
                icon: ii,
            });
        }
    }
    // 控制中心命中（与绘制几何同源）。折叠胶囊（panel<0.5）：整块胶囊 = 展开按钮；
    // 展开面板：标签页 / 关闭 / 切换桌面 / 各页控件（幽灵行不可点，经 hit_index 映射）。
    let mut console = None;
    if let Some(c) = &scene.console {
        let mut zones = Vec::new();
        let body = RectF {
            x: c.x,
            y: c.y,
            w: c.width,
            h: c.height,
        };
        if c.panel < 0.5 {
            // 折叠：点击胶囊任意处展开（标题栏拖动在展开形态才生效）
            zones.push((ConsoleZone::Expand, body));
        } else {
            for (i, t) in c.tabs.iter().enumerate() {
                zones.push((ConsoleZone::Tab(i), t.rect));
            }
            zones.push((ConsoleZone::Close, c.close));
            zones.push((ConsoleZone::DesktopToggle, c.desktop_toggle));
            match c.active_kind {
                ConsoleTab::Todo => {
                    zones.push((ConsoleZone::Input, c.input));
                    zones.push((ConsoleZone::DetailInput, c.detail_input));
                    zones.push((ConsoleZone::Add, c.add));
                    for (i, r) in c.checkbox.iter().enumerate() {
                        if let Some(live) = c.hit_index.get(i).copied().flatten() {
                            zones.push((ConsoleZone::Toggle(live), *r));
                        }
                    }
                    for (i, r) in c.del.iter().enumerate() {
                        if let Some(live) = c.hit_index.get(i).copied().flatten() {
                            zones.push((ConsoleZone::Delete(live), *r));
                        }
                    }
                }
                ConsoleTab::Notes => {
                    if let Some(n) = &c.notes {
                        zones.push((ConsoleZone::NotesEdit, n.rect));
                    }
                }
                ConsoleTab::Fences => {
                    for (i, r) in c.fence_rows.iter().enumerate() {
                        // 滚出可视区的行不参与命中（避免点到详情区时误中隐藏行）
                        if r.rect.y + r.rect.h < c.fence_list_view.y
                            || r.rect.y > c.fence_list_view.y + c.fence_list_view.h
                        {
                            continue;
                        }
                        zones.push((ConsoleZone::FenceSelect(i), r.rect));
                    }
                    if let Some(d) = &c.fence_detail {
                        zones.push((ConsoleZone::FenceLayout(FenceLayout::Grid), d.layout_grid));
                        zones.push((ConsoleZone::FenceLayout(FenceLayout::List), d.layout_list));
                        zones.push((ConsoleZone::FenceIconSize(32.0), d.size_s));
                        zones.push((ConsoleZone::FenceIconSize(48.0), d.size_m));
                        zones.push((ConsoleZone::FenceIconSize(64.0), d.size_l));
                        zones.push((ConsoleZone::FenceStyle(FenceStyle::Glass), d.style_glass));
                        zones.push((
                            ConsoleZone::FenceStyle(FenceStyle::Outline),
                            d.style_outline,
                        ));
                        zones.push((ConsoleZone::FenceStyle(FenceStyle::Filled), d.style_filled));
                        zones.push((ConsoleZone::FenceTint(None), d.tint_default));
                        for (i, r) in d.tints.iter().enumerate() {
                            if let Some((_, c)) = TINT_PRESETS.get(i) {
                                zones.push((ConsoleZone::FenceTint(Some(*c)), *r));
                            }
                        }
                    }
                }
                ConsoleTab::Plugins => {
                    for (i, r) in c.plugin_rows.iter().enumerate() {
                        zones.push((ConsoleZone::PluginToggle(i), r.toggle));
                    }
                    zones.push((ConsoleZone::OpenPluginDir, c.open_plugins));
                }
            }
        }
        console = Some(ConsoleHit {
            rect: body,
            title: RectF {
                x: c.x,
                y: c.y,
                w: c.width,
                h: c.title_h,
            },
            zones,
        });
    }
    HitModel {
        fences,
        icons,
        console,
    }
}
