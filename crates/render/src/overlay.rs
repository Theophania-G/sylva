//! overlay 窗口：挂在桌面壳层下的透明子窗口，承载 WinRT 合成视觉树。
//!
//! - 覆盖整个虚拟屏幕（多显示器），位置随父窗口屏幕坐标实时计算；
//! - `WS_EX_NOACTIVATE` 不抢焦点；窗口内容完全由 WinRT `Windows.UI.Composition`
//!   视觉树提供（`WS_EX_NOREDIRECTIONBITMAP` 无重定向位图——合成器直连窗口，
//!   `CreateDesktopWindowTarget` 才可用，且 BackdropBrush 才能采样到窗口背后
//!   真实的桌面，这是真·实时模糊的前置要求）；
//! - **点击穿透**：窗口区域被 `SetWindowRgn` 裁剪为全部栅栏矩形的并集
//!   （`CombineRgn(RGN_OR)`）。区域外的鼠标命中直接落到下方窗口（桌面/其他应用）。
//!   不用 `WM_NCHITTEST` 返回 `HTTRANSPARENT`——它只能把点击转发给**同一进程**
//!   的窗口，而 Explorer 桌面是不同进程，全屏 overlay 会变成点击死区；
//! - 交互：标题栏拖动栅栏、右下角手柄缩放、双击图标打开（窗口类需 `CS_DBLCLKS`）。
//!   事件以 `OverlayEvent` 交给 App 层回调处理；回调返回新的命中模型，窗口据此
//!   重建区域并更新命中数据（拖动过程每次移动都重建区域，栅栏随之移动/缩放）；
//! - 窗口状态（命中模型 / 拖拽状态 / 事件回调）保存在 `GWLP_USERDATA`，
//!   由 `OverlayWindow` 独占生命周期，同一线程读写，无需跨线程同步。

use std::sync::OnceLock;

use windows::core::{Result, PCWSTR};
use windows::Win32::Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, POINT, WPARAM, TRUE};
use windows::Win32::Graphics::Gdi::{
    CombineRgn, CreateRectRgn, CreateSolidBrush, DeleteObject, EqualRgn, SetBkColor, SetTextColor,
    SetWindowRgn, HBRUSH, HDC, HRGN, RGN_OR,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
use windows::Win32::UI::Controls::WM_MOUSELEAVE;
use windows::Win32::UI::Input::Ime::{
    ImmGetCompositionStringW, ImmGetContext, ImmReleaseContext, GCS_COMPSTR, GCS_RESULTSTR,
    IME_COMPOSITION_STRING,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, RegisterHotKey, ReleaseCapture, SetCapture, SetFocus, TrackMouseEvent, MOD_ALT,
    MOD_CONTROL, MOD_SHIFT, TME_LEAVE, TRACKMOUSEEVENT, VK_CONTROL, VK_F10,
};
use windows::Win32::UI::Shell::{
    DragAcceptFiles, DragFinish, DragQueryFileW, DragQueryPoint, Shell_NotifyIconW, HDROP,
    NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NOTIFYICONDATAW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetCursorPos,
    GetForegroundWindow, GetMessageW, GetSystemMetrics, GetWindowLongPtrW,
    GetWindowThreadProcessId, KillTimer, LoadCursorW, LoadIconW, PostQuitMessage, RegisterClassW,
    SendMessageW, SetCursor, SetForegroundWindow, SetTimer, SetWindowLongPtrW, SetWindowPos,
    ShowWindow, TranslateMessage, CS_DBLCLKS, GWLP_USERDATA, HCURSOR, HICON, HTCLIENT,
    HTTRANSPARENT, ICON_BIG, ICON_SMALL, IDC_ARROW, IDC_SIZEALL, IDC_SIZENESW, IDC_SIZENS,
    IDC_SIZENWSE, IDC_SIZEWE, MSG, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN,
    SM_YVIRTUALSCREEN, SWP_NOACTIVATE, SWP_NOREDRAW, SWP_NOOWNERZORDER, SWP_NOZORDER, SW_SHOWNA,
    SW_SHOWNOACTIVATE, WM_CHAR, WM_CLOSE, WM_CTLCOLOREDIT, WM_DROPFILES, WM_ERASEBKGND, WM_HOTKEY,
    WM_IME_COMPOSITION, WM_IME_ENDCOMPOSITION, WM_IME_SETCONTEXT, WM_IME_STARTCOMPOSITION,
    WM_KEYDOWN, WM_KILLFOCUS, WM_LBUTTONDBLCLK, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE,
    WM_MOUSEWHEEL, WM_NCHITTEST, WM_RBUTTONDOWN, WM_RBUTTONUP, WM_SETCURSOR, WM_SETICON, WM_TIMER,
    WM_DISPLAYCHANGE, WM_DPICHANGED,
    WNDCLASSW, WS_EX_NOACTIVATE, WS_EX_NOREDIRECTIONBITMAP, WS_EX_TOOLWINDOW, WS_POPUP,
};

use sylva_core::model::{FenceLayout, FenceStyle, SidebarPosition};

/// 窗口类名（全局唯一，单实例）。
const CLASS_NAME: &str = "SylvaOverlay";
/// 隐藏焦点代理窗口类名（独立顶层，离屏 1×1，DefWindowProc 处理即可）。
const PROXY_CLASS: &str = "SylvaFocusProxy";

/// 外部通知主循环退出的消息（WM_APP + 1）。
/// 由 `run_message_loop` 的调用方决定在退出前恢复现场（如恢复真实桌面图标）。
pub const WM_APP_QUIT: u32 = 0x8000 + 1;

/// App 层注入一个 `OverlayEvent`（WM_APP + 2）。`lParam` 指向一个
/// `Box<OverlayEvent>`（发送方 `Box::into_raw`，接收方 `Box::from_raw` 释放）。
/// 用途：就地重命名提交后，绕过鼠标消息直接触发一次完整重绘 + 命中模型重建。
pub const WM_SYLVA_INJECT: u32 = 0x8000 + 2;

/// 托盘图标回调消息（WM_APP + 3）：`lParam` 为托盘鼠标消息（WM_RBUTTONUP 等）。
pub const WM_TRAY: u32 = 0x8000 + 3;

/// 托盘图标 ID（进程内唯一）。
const TRAY_ID: u32 = 1;

/// 全局退出热键：Ctrl+Shift+F10。GUI 版没有控制台，Ctrl+C 不可用，
/// 必须有一个干净退出的入口（否则只能杀进程，桌面图标无法恢复）。
const QUIT_HOTKEY_ID: i32 = 1;

/// 控制台（插件面板）开关热键：Ctrl+Alt+T。
const CONSOLE_HOTKEY_ID: i32 = 2;

/// 库同步定时器 ID：周期触发 `SyncLibrary`，App 检查库文件夹中已被外部删除的
/// 文件，同步移除栅栏对应项（「库内删除 → 栅栏项消失」）。
const SYNC_LIBRARY_TIMER: usize = 0x5311;
/// 库同步间隔（毫秒）。
const SYNC_LIBRARY_MS: u32 = 4000;

/// 动画定时器 ID：周期触发 `AnimTick`，App 推进控制台面板/待办行动画并重绘。
/// 无动画进行时由 App 通知停止（保持空闲 0% CPU）。
const ANIM_TIMER: usize = 0x5312;
/// 动画帧间隔（毫秒，≈60fps）。
const ANIM_MS: u32 = 16;

/// 右下角缩放手柄尺寸（物理像素）。App 层用同样数值生成手柄命中区域。
pub const GRIP_SIZE: f32 = 26.0;
/// 左/右/下边缘的缩放松动距离（物理像素），拖边缘即可改宽/改高。
pub const EDGE_RESIZE: f32 = 9.0;
/// 非手柄角的缩放松动距离（物理像素，左下/右上角）。
pub const CORNER_RESIZE: f32 = 12.0;
/// 按下与松开间鼠标位移小于该值即视为「单击」（选中图标，如资源管理器）；
/// 超过则视为拖动（移动栅栏）。
pub const CLICK_DRAG_THRESHOLD: f32 = 5.0;
/// 侧边栏图标拖动排序的启动阈值（像素）：按下后移动超过此距离才进入 reorder 模式。
const REORDER_THRESHOLD: f32 = 8.0;

/// 类只注册一次（同一 HINSTANCE）。
static CLASS_REGISTERED: OnceLock<()> = OnceLock::new();
static PROXY_CLASS_REGISTERED: OnceLock<()> = OnceLock::new();

/// 矩形（虚拟屏幕坐标，物理像素）。
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct RectF {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl RectF {
    /// 点是否在矩形内（含边界）。
    pub fn contains(&self, px: f32, py: f32) -> bool {
        px >= self.x && px <= self.x + self.w && py >= self.y && py <= self.y + self.h
    }
}

/// 一个栅栏的命中数据：整体矩形（区域 + 兜底命中）、标题栏（移动把手）、
/// 右下角手柄（缩放把手）。`id` 是 App 层 `desk.fences` 的下标。
#[derive(Debug, Clone, Copy)]
pub struct FenceHit {
    pub body: RectF,
    pub title: RectF,
    pub grip: RectF,
    pub id: usize,
    /// 侧边栏悬停工具提示矩形（可延伸到栅栏之外）；None = 无。用于把工具提示
    /// 区域并入窗口区域，否则区域外的绘制不可见。
    pub tooltip: Option<RectF>,
    /// 是否为侧边栏布局（用于拖动排序判定）。
    pub is_sidebar: bool,
}

/// 一个图标的命中数据。`fence` / `icon` 分别是 App 层 `desk.fences` 下标
/// 与该栅栏 `icon_ids` 下标（与场景中图标的排列一一对应）。
#[derive(Debug, Clone, Copy)]
pub struct IconHit {
    pub rect: RectF,
    pub fence: usize,
    pub icon: usize,
}

/// 控制台面板内可点击的控件。
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ConsoleZone {
    /// 关闭控制台面板（折叠为胶囊，不销毁）。
    Close,
    /// 点击折叠胶囊 / 空白：展开面板。
    Expand,
    /// 切换到第 `index` 个标签页。
    Tab(usize),
    /// 标题栏「切换桌面」按钮：栅栏 ⇄ 原始桌面。
    DesktopToggle,
    /// 栅栏管理页：选中第 `index` 个栅栏显示详情控制。
    FenceSelect(usize),
    FenceLayout(FenceLayout),
    FenceIconSize(f32),
    FenceStyle(FenceStyle),
    /// 栅栏管理页：设置侧边栏停靠位置（仅 Sidebar 布局有效）。
    FenceSidebarPos(SidebarPosition),
    FenceTint(Option<[f32; 3]>),
    /// 栅栏管理页：「添加栅栏」按钮（新建一个空白栅栏并选中）。
    AddFence,
    /// 栅栏管理页：移出当前选中的栅栏。
    RemoveFence,
    /// 栅栏管理页：更改选中栅栏的存储位置（打开文件夹选择器）。
    ChangeStoragePath,
}

/// 控制台（插件面板）的命中数据：整体矩形（窗口区域 + 命中判定范围）、
/// 标题栏（拖动把手）、各控件矩形。
#[derive(Debug, Clone)]
pub struct ConsoleHit {
    /// 整个面板矩形（区域并集 + 命中范围；点在此矩形外不视为控制台点击）。
    pub rect: RectF,
    /// 标题栏（拖动控制台移动）。
    pub title: RectF,
    /// 控件矩形列表（与命中测试顺序一致，先命中的生效）。
    pub zones: Vec<(ConsoleZone, RectF)>,
}

/// 命中模型：窗口区域 + 交互命中测试的数据源。
#[derive(Debug, Clone, Default)]
pub struct HitModel {
    pub fences: Vec<FenceHit>,
    pub icons: Vec<IconHit>,
    /// 控制台面板；None = 未打开。
    pub console: Option<ConsoleHit>,
    /// 当前内联编辑框矩形（浮于栅栏之上）：点击内部用于定位光标，不再落到
    /// 下面的栅栏/图标（否则会误触发「点击别处提交编辑」）。
    pub edit_rect: Option<RectF>,
}

/// 缩放拖拽所作用的栅栏区域（决定改宽 / 改高 / 是否随动左上角）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResizeZone {
    /// 右边缘：只改宽度。
    Right,
    /// 下边缘：只改高度。
    Bottom,
    /// 右下角：宽高一起改。
    BottomRight,
    /// 左边缘：改宽度并随动 x。
    Left,
    /// 左下角：宽高一起改并随动 x。
    BottomLeft,
    /// 右上角：宽高一起改并随动 y。
    TopRight,
}

/// 用户交互事件（坐标全部为虚拟屏幕物理像素）。
#[derive(Debug, Clone)]
pub enum OverlayEvent {
    /// 拖动标题栏移动栅栏：目标位置（左上角）。
    FenceMove { fence: usize, pos: (f32, f32) },
    /// 拖边缘/角标缩放：目标矩形（左上角 x, y 与新宽高 w, h）与作用的边。
    FenceResize {
        fence: usize,
        zone: ResizeZone,
        rect: (f32, f32, f32, f32),
    },
    /// 一次拖动结束（App 在此持久化布局）。
    FenceDragEnd { fence: usize },
    /// 双击栅栏内的图标。
    IconDoubleClicked { fence: usize, icon: usize },
    /// 单击栅栏内的图标（按下+松开且位移很小）：用于选中高亮，如资源管理器。
    /// `ctrl` = 是否按住 Ctrl（按住时切换该图标的选择状态，实现不连续多选）。
    IconClicked {
        fence: usize,
        icon: usize,
        ctrl: bool,
    },
    /// 框选拖拽（橡皮筋）：`rect` 是当前框选矩形，`selected` 是框中的图标（拖动中逐帧上报）。
    SelectDrag {
        fence: usize,
        rect: (f32, f32, f32, f32),
        selected: Vec<(usize, usize)>,
    },
    /// 框选拖拽结束：清除橡皮筋显示（选择结果已在最后一次 `SelectDrag` 中生效）。
    SelectEnd,
    /// 就地重命名提交后由 App 注入：仅用于触发一次完整重绘 + 命中模型重建
    /// （场景数据已在注入前改好）。
    EditCommitted,
    /// 鼠标点在编辑框内：`x` 为虚拟屏幕物理坐标，App 据此把光标定位到对应字符。
    /// 点击编辑框不再穿透到下面的栅栏/图标（避免误触发「点击别处提交」）。
    EditCaret { x: f32 },
    /// 右键按下：`icon` 为 Some 表示点在图标的图标上，None 表示点在栅栏空白/标题上。
    /// `pos` 为虚拟屏幕坐标（App 层据此弹上下文菜单）。
    ContextMenu {
        fence: usize,
        icon: Option<usize>,
        pos: (f32, f32),
    },
    /// 鼠标悬停到某个图标（用于高亮反馈）。
    HoverEnter { fence: usize, icon: usize },
    /// 鼠标移出所有图标（清除高亮）。
    HoverLeave,
    /// 光标位置变化（虚拟屏幕物理坐标）：App 层据此做连续 Dock 放大，
    /// 位置未变时不上报（避免无谓重绘）。
    CursorMove { x: f32, y: f32 },
    /// 光标离开窗口（`WM_MOUSELEAVE`）：App 层清除 Dock 放大（恢复 1.0）。
    CursorLeave,
    /// 文件被拖入某个栅栏（`WM_DROPFILES`）：App 把这些路径加进该栅栏。
    FilesDropped { fence: usize, paths: Vec<String> },
    /// 鼠标滚轮滚动某栅栏：`delta` 是滚轮原始刻度（正=向上/远离，负=向下）。
    FenceScroll { fence: usize, delta: i32 },
    /// 托盘图标：右键（显示控制中心/退出菜单）。
    TrayMenu,
    /// 托盘图标：双击（切换控制中心开合）。
    TrayToggle,
    /// 控制台控件悬停变化（None = 移出所有控件；仅展开面板内上报）。
    ConsoleHover { zone: Option<ConsoleZone> },
    /// 控制台（插件面板）内点击某个控件。
    ConsoleClick { zone: ConsoleZone },
    /// 在控制台面板上滚动滚轮（待办列表滚动）：`delta` 是滚轮原始刻度。
    ConsoleScroll { delta: i32 },
    /// 拖动控制台标题栏：目标左上角（虚拟屏幕坐标）。
    ConsoleMove { pos: (f32, f32) },
    /// 控制台拖动结束（App 在此持久化位置）。
    ConsoleDragEnd,
    /// 拖控制台右/下边缘或右下角：目标矩形（宽高随动，左上角锚定不变）。
    ConsoleResize { rect: (f32, f32, f32, f32) },
    /// 控制台缩放拖拽结束（App 在此持久化尺寸）。
    ConsoleResizeEnd,
    /// 全局热键 Ctrl+Alt+T：切换控制台开关。
    ConsoleToggle,
    /// 键盘按下（overlay 获得焦点时）：`vk` 虚拟键码，`ctrl` 是否按住 Ctrl。
    /// 文本字符走 `Char`（经 TranslateMessage 转换）。
    KeyDown { vk: u32, ctrl: bool },
    /// 文本字符（非 IME 合成路径的普通输入）。
    Char { ch: u16 },
    /// IME 开始合成。
    ImeStart,
    /// IME 合成更新：`text` 为当前合成串，`caret` 为合成串内光标。
    ImeCompose { text: String, caret: usize },
    /// IME 合成结果上屏（最终提交的文本）。
    ImeResult { text: String },
    /// IME 合成结束（上屏）。
    ImeEnd,
    /// overlay 失去键盘焦点（内联编辑应取消聚焦但不丢文本）。
    OverlayFocusLost,
    /// 定时器周期触发：App 检查内部库文件夹，删除的库文件对应栅栏项同步移除。
    SyncLibrary,
    /// 动画定时器触发（ANIM_TIMER，16ms）：App 推进控制台面板/待办行动画。
    /// 无动画进行时 App 调用 `OverlayWindow::set_anim_active(false)` 停止本定时器。
    AnimTick,
    /// 所在显示器 DPI 变化（`WM_DPICHANGED`）：`dpi` 为新 x-DPI。
    /// App 据此重算主题缩放并重新夹屏 + 重绘（Per-Monitor v2 下的实时跟随）。
    DpiChanged { dpi: u32 },
    /// 显示拓扑/分辨率变化（`WM_DISPLAYCHANGE`）：虚拟屏宽高可能已变。
    /// App 据此重算虚拟屏尺寸并重新夹屏 + 重绘（找回超屏栅栏）。
    DisplayChange,
    /// 侧边栏图标拖动中（重排序）：`icon` 是被拖动的图标在 `icon_ids` 中的下标，
    /// `(mx, my)` 为当前光标位置（虚拟屏幕物理像素）。
    SidebarReorderDrag { fence: usize, icon: usize, mx: f32, my: f32 },
    /// 侧边栏图标拖动结束：`from` 是原始下标，`to` 是目标插入位置。
    SidebarReorderEnd { fence: usize, from: usize, to: usize },
}

/// 拖拽会话（按下到松开之间持续有效）。
#[derive(Debug, Clone, Copy)]
struct DragState {
    kind: DragKind,
    fence: usize,
    /// 按下时的鼠标位置（虚拟屏幕坐标）；原始目标 = `orig + (鼠标 - start)`。
    start: (f32, f32),
    /// 按下时栅栏的整体矩形（吸附/碰撞的基准原点）。
    orig: RectF,
    /// 按下点是否落在某图标上（该栅栏内图标下标）；用于单击选中判定。
    pressed_icon: Option<usize>,
    /// 按下时是否按住 Ctrl（单击时决定「切换选择」还是「单选」）。
    ctrl: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum DragKind {
    /// 标题栏/图标拖动 → 移动栅栏。
    Move,
    /// 空白处拖动 → 框选（橡皮筋多选，资源管理器行为）。
    Select,
    /// 边缘/角标拖动 → 缩放（记录作用于哪个区域）。
    Resize(ResizeZone),
    /// 控制台标题栏拖动 → 移动控制台面板。
    ConsoleMove,
    /// 控制台右/下边缘或右下角拖动 → 缩放面板（锚定左上角）。
    ConsoleResize(ResizeZone),
    /// 侧边栏图标拖动 → 重新排序。
    SidebarReorder,
}

struct WindowState {
    model: HitModel,
    drag: Option<DragState>,
    /// 当前悬停目标（栅栏下标, 图标下标）；仅在变化时上报 Hover 事件。
    hovered: Option<(usize, Option<usize>)>,
    /// 当前悬停的控制台控件（仅在变化时上报 ConsoleHover 事件）。
    console_hovered: Option<ConsoleZone>,
    /// 上次上报的光标位置（仅位置变化才发 CursorMove，避免无谓重绘）。
    last_cursor: Option<(f32, f32)>,
    /// 上次 SetWindowRgn 的区域句柄：区域几何未变时跳过 SetWindowRgn。
    ///
    /// SetWindowRgn 会使窗口无效并触发整窗重绘/重合成（即使区域完全相同），
    /// Dock 存在时光标移动每次都会 set_model 到这里——反复调用造成无谓 CPU 与
    /// 边缘重绘抖动。区域句柄所有权归窗口（系统释放），此处仅保存引用用于 EqualRgn。
    last_region: Option<HRGN>,
    /// App 层事件处理器；返回新的命中模型以同步区域与命中数据。
    ///
    /// 返回 `None` 表示丢弃本事件、保持当前命中模型：App 在模态菜单 / 属性页等
    /// 嵌套消息循环期间会再入本回调（定时器、悬停、注入事件），此时 `&mut Runtime`
    /// 借用已被外层占用，再入必然借用冲突崩溃，必须让事件静默丢弃。
    handler: Option<Box<dyn FnMut(OverlayEvent) -> Option<HitModel>>>,
    /// 编辑框（重命名/待办输入）背景画刷：`WM_CTLCOLOREDIT` 返回它让 EDIT 用深色底。
    edit_brush: HBRUSH,
}

/// overlay 窗口。
pub struct OverlayWindow {
    pub hwnd: HWND,
    /// 隐藏的焦点代理窗口（独立顶层，离屏 1×1）：内联编辑需要键盘焦点，
    /// 但 overlay 必须保持 `WS_EX_NOACTIVATE`（激活它会把桌面壳层提到应用之上）。
    /// 聚焦代理只负责「成为前台进程」，键盘焦点仍给 overlay。
    proxy: HWND,
    /// 窗口在虚拟屏幕上的左上角（物理像素；可含负值，如副屏在主屏左/上方时）。
    pub x: i32,
    pub y: i32,
    /// 覆盖的虚拟屏幕尺寸（物理像素）。
    pub width: u32,
    pub height: u32,
    state: *mut WindowState,
}

impl OverlayWindow {
    /// 让 overlay 获得键盘输入（前台交给隐藏代理，overlay 本体不被激活/提层）。
    pub fn focus_for_input(&self) {
        unsafe {
            // 前台锁：非前台进程的 SetForegroundWindow 会被系统拒绝。经典解法是把
            // 本线程暂时挂到当前前台线程，再设前台，最后解挂——无需真正激活 overlay，
            // 桌面壳层仍保持在应用之下。
            let cur = GetCurrentThreadId();
            let fg_hwnd = GetForegroundWindow();
            let fg = GetWindowThreadProcessId(fg_hwnd, None);
            let attached = cur != fg && fg != 0 && AttachThreadInput(cur, fg, true).0 != 0;
            let _ = SetForegroundWindow(self.proxy);
            let _ = SetFocus(Some(self.hwnd));
            if attached {
                let _ = AttachThreadInput(cur, fg, false);
            }
        }
    }

    /// 在桌面壳层下创建覆盖整个虚拟屏幕的 overlay 窗口。
    pub fn create(parent: HWND) -> Result<Self> {
        let hmodule = unsafe { GetModuleHandleW(None)? };
        let hinstance = HINSTANCE(hmodule.0);
        ensure_class(hinstance);

        let (vx, vy, vw, vh) = virtual_screen();

        // 窗口直接定位在虚拟屏原点 (vx, vy)：客户端 (0,0) = 虚拟屏 (0,0)，栅栏的
        // 虚拟屏幕坐标即可直接作为客户端坐标绘制/命中，无需任何换算。
        // 旧版减去 `parent_rect.left/top` 会把窗口挪偏——单屏下 (0,0) 减了无感，
        // 副屏在主屏左/上（vx/vy≠0）时窗口与合成器坐标同时错位，正是多屏栅栏位置
        // 错乱、模糊栅栏对不上边框的根因之一。这里只保留父窗口的 z 序/所有权关系。

        // 编辑框暗色背景画刷（重命名/待办输入共用），与玻璃卡片面板填充色完全一致
        // （面板填充 [0.062,0.086,0.133]≈RGB(16,22,34)），输入框与面板无缝一体，
        // 不再是一块突兀的深色方块；失败时退回空刷，编辑框用系统默认白底。
        let edit_brush = unsafe { CreateSolidBrush(COLORREF(0x00_22_16_10)) }; // RGB(16,22,34)
        let state = Box::new(WindowState {
            model: HitModel::default(),
            drag: None,
            hovered: None,
            console_hovered: None,
            last_cursor: None,
            last_region: None,
            handler: None,
            edit_brush,
        });
        let state_ptr = Box::into_raw(state);

        let hwnd = unsafe {
            CreateWindowExW(
                // `WS_EX_NOACTIVATE`：点击栅栏/小组件不激活窗口、不把桌面壳层提到
                // 应用之上（桌面层级永远在正常应用下面）。键盘输入走隐藏焦点代理。
                // `WS_EX_TOOLWINDOW`：不进任务栏/Alt+Tab——日常入口是托盘图标
                // （默认折叠在通知区隐藏图标里），右键托盘即「Sylva 控制中心」。
                // `WS_EX_NOREDIRECTIONBITMAP`：无重定向位图——WinRT 合成器直连窗口
                // （CreateDesktopWindowTarget 要求），且 BackdropBrush 才能采样到
                // 窗口背后真实的桌面（真·实时模糊的前置）。
                WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW | WS_EX_NOREDIRECTIONBITMAP,
                PCWSTR(wide(CLASS_NAME).as_ptr()),
                // 窗口标题：任务栏按钮/悬停提示显示「Sylva」（留空会退回进程名 sylva.exe）
                PCWSTR(wide("Sylva").as_ptr()),
                WS_POPUP,
                vx,
                vy,
                vw,
                vh,
                Some(parent),
                None,
                Some(hinstance),
                Some(state_ptr as *const core::ffi::c_void),
            )?
        };
        // 创建完成、消息泵启动前写入状态，wnd_proc 从此刻起可安全读取
        unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, state_ptr as isize) };
        // 显式设置任务栏/Alt+Tab 图标（大+小），确保运行中的任务栏按钮显示 Sylva 图标。
        // LoadIconW 返回共享句柄，无需释放；资源缺失时返回空图标，WM_SETICON 接受空值
        // 并退回默认图标，不会出错。
        let hicon = app_icon(hinstance);
        unsafe {
            let _ = SendMessageW(
                hwnd,
                WM_SETICON,
                Some(WPARAM(ICON_BIG as usize)),
                Some(LPARAM(hicon.0 as isize)),
            );
            let _ = SendMessageW(
                hwnd,
                WM_SETICON,
                Some(WPARAM(ICON_SMALL as usize)),
                Some(LPARAM(hicon.0 as isize)),
            );
        }

        // 托盘图标（通知区，默认折叠在「隐藏的图标」里）：右键 = 控制中心菜单，
        // 双击 = 切换控制中心开合。日常入口不再占任务栏按钮。
        let mut nid: NOTIFYICONDATAW = unsafe { std::mem::zeroed() };
        nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
        nid.hWnd = hwnd;
        nid.uID = TRAY_ID;
        nid.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
        nid.uCallbackMessage = WM_TRAY;
        nid.hIcon = hicon;
        let tip = wide("Sylva 桌面栅栏");
        for (i, c) in tip.iter().take(127).enumerate() {
            nid.szTip[i] = *c;
        }
        let _ = unsafe { Shell_NotifyIconW(NIM_ADD, &nid) };

        // 立即用一个空区域：首个栅栏命中模型到来之前，整个窗口对鼠标不可见，
        // 不会出现「全屏死区」。首个模型到达后区域会被替换为栅栏并集。
        let empty = unsafe { CreateRectRgn(0, 0, 0, 0) };
        unsafe { SetWindowRgn(hwnd, Some(empty), false) };

        // 全局退出热键（Ctrl+Shift+F10）：绑定到 overlay 窗口，主线程处理 WM_HOTKEY
        let _ = unsafe {
            RegisterHotKey(
                Some(hwnd),
                QUIT_HOTKEY_ID,
                MOD_CONTROL | MOD_SHIFT,
                VK_F10.0 as u32, // VIRTUAL_KEY 是 u16 newtype，转 u32
            )
        };
        // 控制台开关热键（Ctrl+Alt+T）：切换插件面板
        let _ = unsafe {
            RegisterHotKey(
                Some(hwnd),
                CONSOLE_HOTKEY_ID,
                MOD_CONTROL | MOD_ALT,
                b'T' as u32,
            )
        };
        let _shown = unsafe { ShowWindow(hwnd, SW_SHOWNA) };

        // 隐藏焦点代理：独立顶层窗口（离屏 1×1，不进任务栏），只负责让进程成为前台，
        // 键盘焦点仍给 overlay（NOACTIVATE 本体不会因点击被激活/提层）。
        ensure_proxy_class(hinstance);
        let proxy = unsafe {
            CreateWindowExW(
                WS_EX_TOOLWINDOW,
                PCWSTR(wide(PROXY_CLASS).as_ptr()),
                PCWSTR(wide("SylvaFocusProxy").as_ptr()),
                WS_POPUP,
                -32000,
                -32000,
                1,
                1,
                None,
                None,
                Some(hinstance),
                None,
            )?
        };
        unsafe {
            let _ = ShowWindow(proxy, SW_SHOWNOACTIVATE);
        }

        // 接受文件拖放（WM_DROPFILES）：把任意文件/文件夹/快捷方式拖进栅栏。
        // 区域裁剪不拦截拖放——系统按窗口可见区域（region）决定拖放目标，
        // 拖到栅栏并集内命中本窗口，拖到区域外仍落到桌面。
        unsafe { DragAcceptFiles(hwnd, true) };

        // 库同步定时器：周期触发 `SyncLibrary`（见 `WM_TIMER` 处理）
        let _ = unsafe { SetTimer(Some(hwnd), SYNC_LIBRARY_TIMER, SYNC_LIBRARY_MS, None) };

        Ok(Self {
            hwnd,
            proxy,
            x: vx,
            y: vy,
            width: vw as u32,
            height: vh as u32,
            state: state_ptr,
        })
    }

    /// 应用命中模型：更新命中数据并把窗口区域裁剪为栅栏并集（区域外点击穿透）。
    pub fn set_model(&self, model: HitModel) {
        let state = unsafe { &mut *self.state };
        state.model = model;
        apply_region(self.hwnd, &state.model, &mut state.last_region);
    }

    /// 设置用户交互事件处理器。回调返回新的命中模型（由 App 根据新布局生成），
    /// overlay 随即更新命中数据与窗口区域。
    pub fn set_event_handler(&self, handler: Box<dyn FnMut(OverlayEvent) -> Option<HitModel>>) {
        unsafe { &mut *self.state }.handler = Some(handler);
    }

    /// 启用/停用动画定时器（16ms 触发一次 `AnimTick`）。
    /// App 在启动动画时启用、动画全部结束后停用，保证空闲时 0% CPU。
    pub fn set_anim_active(&self, active: bool) {
        unsafe {
            if active {
                let _ = SetTimer(Some(self.hwnd), ANIM_TIMER, ANIM_MS, None);
            } else {
                let _ = KillTimer(Some(self.hwnd), ANIM_TIMER);
            }
        }
    }

    /// 显示拓扑变化后把 overlay 重设到新的虚拟屏幕（移动 + 缩放）。
    /// `(vx, vy)` 为新虚拟屏原点（副屏在左/上时可负），`w×h` 为虚拟屏尺寸。
    /// 客户端坐标始终 = 虚拟屏幕坐标，无需换算（见 `create`）。
    pub fn resize(&mut self, vx: i32, vy: i32, w: u32, h: u32) {
        self.x = vx;
        self.y = vy;
        self.width = w;
        self.height = h;
        unsafe {
            let _ = SetWindowPos(
                self.hwnd,
                None,
                vx,
                vy,
                w as i32,
                h as i32,
                SWP_NOACTIVATE | SWP_NOZORDER | SWP_NOOWNERZORDER | SWP_NOREDRAW,
            );
        }
    }
}

impl Drop for OverlayWindow {
    fn drop(&mut self) {
        // 先销毁窗口（同步触发 WM_DESTROY 及后续消息），再回收状态，
        // 避免窗口销毁期间 wnd_proc 引用已释放的内存。
        unsafe {
            let _ = KillTimer(Some(self.hwnd), SYNC_LIBRARY_TIMER);
            let _ = KillTimer(Some(self.hwnd), ANIM_TIMER);
        }
        let _ = unsafe { DestroyWindow(self.hwnd) };
        let _ = unsafe { DestroyWindow(self.proxy) };
        // 移除托盘图标
        let mut nid: NOTIFYICONDATAW = unsafe { std::mem::zeroed() };
        nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
        nid.hWnd = self.hwnd;
        nid.uID = TRAY_ID;
        let _ = unsafe { Shell_NotifyIconW(NIM_DELETE, &nid) };
        unsafe {
            let st = Box::from_raw(self.state);
            if !st.edit_brush.0.is_null() {
                let _ = DeleteObject(st.edit_brush.into());
            }
        }
    }
}

/// 虚拟屏幕范围（屏幕坐标，物理像素）。
fn virtual_screen() -> (i32, i32, i32, i32) {
    unsafe {
        (
            GetSystemMetrics(SM_XVIRTUALSCREEN),
            GetSystemMetrics(SM_YVIRTUALSCREEN),
            GetSystemMetrics(SM_CXVIRTUALSCREEN),
            GetSystemMetrics(SM_CYVIRTUALSCREEN),
        )
    }
}

/// 加载 exe 内嵌主图标（资源 ID 1 = sylva.ico）。`LoadIconW` 返回系统共享图标句柄，
/// 不需要（也不能）手动 `DestroyIcon`；资源缺失时返回空图标，调用方回落默认图标。
fn app_icon(hinstance: HINSTANCE) -> HICON {
    // MAKEINTRESOURCE(1)：资源 ID 1 = sylva.ico（不是真正的指针，clippy 误报时放行）
    #[allow(clippy::manual_dangling_ptr)]
    unsafe { LoadIconW(Some(hinstance), PCWSTR(1 as *const u16)) }.unwrap_or_default()
}

fn ensure_class(hinstance: HINSTANCE) {
    CLASS_REGISTERED.get_or_init(|| {
        let class_name = wide(CLASS_NAME);
        let wc = WNDCLASSW {
            // CS_DBLCLKS：没有它双击会被拆成两次 WM_LBUTTONDOWN，
            // WM_LBUTTONDBLCLK 永远不会到达。
            style: CS_DBLCLKS,
            lpfnWndProc: Some(wnd_proc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: hinstance,
            // 主图标 = exe 内嵌的 sylva.ico（资源 ID 1）。WNDCLASSW 无 hIconSm 字段，
            // 小图标在窗口创建后经 WM_SETICON(ICON_SMALL) 显式设置（任务栏按钮优先取它）。
            hIcon: app_icon(hinstance),
            hCursor: Default::default(),
            hbrBackground: Default::default(),
            lpszMenuName: PCWSTR::null(),
            lpszClassName: PCWSTR(class_name.as_ptr()),
        };
        let _atom = unsafe { RegisterClassW(&wc) };
        // 类重名时返回 0（唯一实例，忽略）
    });
}

/// 注册焦点代理窗口类（纯 DefWindowProc，不接收任何特殊消息）。
fn ensure_proxy_class(hinstance: HINSTANCE) {
    PROXY_CLASS_REGISTERED.get_or_init(|| {
        let class_name = wide(PROXY_CLASS);
        let wc = WNDCLASSW {
            style: Default::default(),
            lpfnWndProc: Some(proxy_proc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: hinstance,
            hIcon: Default::default(),
            hCursor: Default::default(),
            hbrBackground: Default::default(),
            lpszMenuName: PCWSTR::null(),
            lpszClassName: PCWSTR(class_name.as_ptr()),
        };
        let _atom = unsafe { RegisterClassW(&wc) };
    });
}

/// 焦点代理窗口过程：全部消息交给默认处理（只是前台占位，不处理交互）。
unsafe extern "system" fn proxy_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    DefWindowProcW(hwnd, msg, wparam, lparam)
}

/// 把命中模型的栅栏矩形并集设为窗口区域。区域外：
/// - 点击/命中直接落到下方窗口（点击穿透的关键）；
/// - 该区域同时限制窗口的可视范围——场景恰好只画这些栅栏，无可见裁剪。
///
/// `SetWindowRgn` 接管区域所有权：旧区域由系统自动释放，新建的区域交给窗口后
/// 不可再手动删除；中间产生的单矩形区域在合并进结果后立即删除。
///
/// `cached` 保存上次交给窗口的区域句柄（所有权在窗口）。区域几何与上次完全相同时
/// 直接跳过 SetWindowRgn——该调用会让窗口失效、强制重绘/重合成，悬停/光标移动期间
/// 区域几乎总是不变，反复调用只会增加 CPU 与重绘抖动（Dock 场景尤其明显）。
fn apply_region(hwnd: HWND, model: &HitModel, cached: &mut Option<HRGN>) {
    let Some(rgn) = build_region(model) else {
        return;
    };
    if let Some(prev) = cached {
        unsafe {
            if EqualRgn(*prev, rgn) == TRUE {
                // 区域没变：丢弃新句柄，保留旧句柄（仍归窗口所有，系统管理释放）。
                let _ = DeleteObject(rgn.into());
                return;
            }
        }
    }
    unsafe { SetWindowRgn(hwnd, Some(rgn), true) };
    *cached = Some(rgn);
}

/// 把全部栅栏矩形 + 侧边栏工具提示 + 内联编辑框 + 控制台面板合并成一个区域
/// （RGN_OR 并集）。无有效矩形时返回 None。
fn build_region(model: &HitModel) -> Option<HRGN> {
    let mut acc: Option<HRGN> = None;
    for f in &model.fences {
        add_rect(&mut acc, f.body);
        if let Some(tt) = f.tooltip {
            add_rect(&mut acc, tt);
        }
    }
    // 编辑框可能伸出栅栏（如侧边栏贴边时的旁侧编辑框）：并入区域，
    // 否则框内点击穿透到桌面、光标定位收不到。
    if let Some(er) = model.edit_rect {
        add_rect(&mut acc, er);
    }
    if let Some(c) = &model.console {
        add_rect(&mut acc, c.rect);
    }
    acc
}

/// 把一个矩形并入区域（并集）。零尺寸矩形跳过；中间单矩形区域用完即删。
fn add_rect(acc: &mut Option<HRGN>, r: RectF) {
    let x1 = r.x as i32;
    let y1 = r.y as i32;
    let x2 = (r.x + r.w) as i32;
    let y2 = (r.y + r.h) as i32;
    if x2 <= x1 || y2 <= y1 {
        return;
    }
    let rect = unsafe { CreateRectRgn(x1, y1, x2, y2) };
    match acc {
        None => *acc = Some(rect),
        Some(dst) => {
            unsafe { CombineRgn(Some(*dst), Some(*dst), Some(rect), RGN_OR) };
            unsafe {
                let _ = DeleteObject(rect.into());
            }
        }
    }
}

unsafe extern "system" fn wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_NCHITTEST => {
            // 区域外的点根本不会进入本窗口（区域已裁剪为栅栏并集），
            // 能到达这里的一定在栅栏内 → HTCLIENT。状态未就绪时穿透兜底。
            let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut WindowState;
            if ptr.is_null() {
                return LRESULT(HTTRANSPARENT as isize);
            }
            LRESULT(HTCLIENT as isize)
        }
        // 合成器接管绘制，擦除由合成器完成
        WM_ERASEBKGND => LRESULT(1),
        // 边缘/角标缩放光标：像普通窗口一样给拖拽手势反馈
        WM_SETCURSOR => {
            let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut WindowState;
            if ptr.is_null() {
                return LRESULT(0);
            }
            let state = unsafe { &mut *ptr };
            let mut pt = POINT::default();
            unsafe {
                let _ = GetCursorPos(&mut pt);
            }
            let cur = cursor_at(&state.model, pt.x as f32, pt.y as f32);
            unsafe {
                let _ = SetCursor(Some(cur));
            }
            LRESULT(1)
        }
        // 文件拖放：把路径交给 App 加入命中栅栏
        WM_DROPFILES => {
            let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut WindowState;
            if ptr.is_null() {
                return LRESULT(0);
            }
            let state = unsafe { &mut *ptr };
            let hdrop = HDROP(wparam.0 as *mut core::ffi::c_void);
            let mut pt = POINT::default();
            unsafe {
                let _ = DragQueryPoint(hdrop, &mut pt);
            }
            let paths = drop_paths(hdrop);
            unsafe { DragFinish(hdrop) };
            if !paths.is_empty() {
                // 用整栅栏矩形（标题/主体/手柄，含边缘）判定落点——拖到标题栏也应进该栅栏
                let px = pt.x as f32;
                let py = pt.y as f32;
                let fence = state
                    .model
                    .fences
                    .iter()
                    .find(|f| {
                        f.body.contains(px, py)
                            || f.title.contains(px, py)
                            || f.grip.contains(px, py)
                    })
                    .map(|f| f.id)
                    .or_else(|| state.model.fences.first().map(|f| f.id));
                if let Some(fence) = fence {
                    emit_event(hwnd, state, OverlayEvent::FilesDropped { fence, paths });
                }
            }
            LRESULT(0)
        }
        // 滚轮滚动：消息发给光标下的窗口（焦点窗口在别的线程时系统自动重定向）。
        // 高字是有符号滚轮刻度（WHEEL_DELTA=120 一格）。滚到栅栏/滚动条上才滚动。
        WM_MOUSEWHEEL => {
            let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut WindowState;
            if ptr.is_null() {
                return LRESULT(0);
            }
            let state = unsafe { &mut *ptr };
            let mut pt = POINT::default();
            unsafe {
                let _ = GetCursorPos(&mut pt);
            }
            let delta = ((wparam.0 >> 16) & 0xFFFF) as u16 as i16 as i32;
            if delta != 0 {
                let px = pt.x as f32;
                let py = pt.y as f32;
                // 控制台面板优先：滚轮滚动待办列表
                if let Some(c) = &state.model.console {
                    if c.rect.contains(px, py) {
                        emit_event(hwnd, state, OverlayEvent::ConsoleScroll { delta });
                        return LRESULT(0);
                    }
                }
                if let Some(f) = state.model.fences.iter().find(|f| f.body.contains(px, py)) {
                    emit_event(
                        hwnd,
                        state,
                        OverlayEvent::FenceScroll { fence: f.id, delta },
                    );
                }
            }
            LRESULT(0)
        }
        WM_LBUTTONDOWN | WM_LBUTTONUP | WM_LBUTTONDBLCLK => {
            let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut WindowState;
            if ptr.is_null() {
                return LRESULT(0);
            }
            let state = unsafe { &mut *ptr };
            // 鼠标消息 lParam 是客户端坐标；overlay 恰好覆盖虚拟屏幕且无边框，
            // 客户端坐标即虚拟屏幕坐标，可直接命中测试。
            let (mx, my) = client_point(lparam);
            match msg {
                WM_LBUTTONDOWN => on_button_down(hwnd, state, mx, my),
                WM_LBUTTONUP => on_button_up(hwnd, state, mx, my),
                WM_LBUTTONDBLCLK => on_double_click(hwnd, state, mx, my),
                _ => unreachable!(),
            }
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut WindowState;
            if ptr.is_null() {
                return LRESULT(0);
            }
            let state = unsafe { &mut *ptr };
            let (mx, my) = client_point(lparam);
            // 请求 WM_MOUSELEAVE：光标离开窗口时清除 Dock 放大（避免放大「粘」住）
            unsafe {
                let mut tme = TRACKMOUSEEVENT {
                    cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                    dwFlags: TME_LEAVE,
                    hwndTrack: hwnd,
                    dwHoverTime: 0,
                };
                let _ = TrackMouseEvent(&mut tme);
            }
            // 非拖拽期间跟踪悬停：目标变化才上报（避免每帧重绘）。
            if state.drag.is_none() {
                let key = hover_key(&state.model, mx, my);
                if key != state.hovered {
                    state.hovered = key;
                    match key {
                        Some((f, Some(i))) => {
                            tracing::trace!(fence = f, icon = i, "悬停进入图标");
                            emit_event(hwnd, state, OverlayEvent::HoverEnter { fence: f, icon: i });
                        }
                        _ => emit_event(hwnd, state, OverlayEvent::HoverLeave),
                    }
                }
                // 控制台控件悬停（展开面板内才生效；折叠胶囊 / 空白处 = None）
                let cz = state
                    .model
                    .console
                    .as_ref()
                    .filter(|c| c.rect.contains(mx, my))
                    .and_then(|c| console_zone_at(c, mx, my));
                if cz != state.console_hovered {
                    state.console_hovered = cz;
                    emit_event(hwnd, state, OverlayEvent::ConsoleHover { zone: cz });
                }
            }
            // 光标位置变化 → 连续 Dock 放大（位置未变不上报，避免无谓重绘）
            if state.last_cursor != Some((mx, my)) {
                state.last_cursor = Some((mx, my));
                emit_event(hwnd, state, OverlayEvent::CursorMove { x: mx, y: my });
            }
            on_mouse_move(hwnd, state, mx, my);
            LRESULT(0)
        }
        WM_MOUSELEAVE => {
            let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut WindowState;
            if ptr.is_null() {
                return LRESULT(0);
            }
            let state = unsafe { &mut *ptr };
            // 清空悬停 + 光标（清除 Dock 放大）
            state.hovered = None;
            state.console_hovered = None;
            state.last_cursor = None;
            emit_event(hwnd, state, OverlayEvent::HoverLeave);
            emit_event(hwnd, state, OverlayEvent::CursorLeave);
            LRESULT(0)
        }
        WM_RBUTTONDOWN => {
            let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut WindowState;
            if ptr.is_null() {
                return LRESULT(0);
            }
            let state = unsafe { &mut *ptr };
            let (mx, my) = client_point(lparam);
            // 命中：控制台 → 图标 → 栅栏；未命中任何交互目标时吞掉。
            if let Some(c) = &state.model.console {
                if c.rect.contains(mx, my) {
                    return LRESULT(0);
                }
            }
            if let Some(icon) = state.model.icons.iter().find(|i| i.rect.contains(mx, my)) {
                emit_event(
                    hwnd,
                    state,
                    OverlayEvent::ContextMenu {
                        fence: icon.fence,
                        icon: Some(icon.icon),
                        pos: (mx, my),
                    },
                );
            } else if let Some(f) = state.model.fences.iter().find(|f| f.body.contains(mx, my)) {
                emit_event(
                    hwnd,
                    state,
                    OverlayEvent::ContextMenu {
                        fence: f.id,
                        icon: None,
                        pos: (mx, my),
                    },
                );
            }
            LRESULT(0)
        }
        // 全局退出热键触发：与 WM_APP_QUIT 相同，走干净退出
        WM_HOTKEY if wparam.0 as i32 == QUIT_HOTKEY_ID => {
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        // 控制台开关热键（Ctrl+Alt+T）：交给 App 切换插件面板
        WM_HOTKEY if wparam.0 as i32 == CONSOLE_HOTKEY_ID => {
            let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut WindowState;
            if !ptr.is_null() {
                let state = unsafe { &mut *ptr };
                emit_event(hwnd, state, OverlayEvent::ConsoleToggle);
            }
            LRESULT(0)
        }
        // 托盘图标回调：右键 → 控制中心菜单；双击 → 切换控制中心开合
        WM_TRAY if lparam.0 as u32 == WM_RBUTTONUP => {
            let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut WindowState;
            if !ptr.is_null() {
                let state = unsafe { &mut *ptr };
                emit_event(hwnd, state, OverlayEvent::TrayMenu);
            }
            LRESULT(0)
        }
        WM_TRAY if lparam.0 as u32 == WM_LBUTTONDBLCLK => {
            let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut WindowState;
            if !ptr.is_null() {
                let state = unsafe { &mut *ptr };
                emit_event(hwnd, state, OverlayEvent::TrayToggle);
            }
            LRESULT(0)
        }
        // 库同步定时器：周期触发，App 同步移除已被删除的库文件对应栅栏项
        WM_TIMER if wparam.0 == SYNC_LIBRARY_TIMER => {
            let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut WindowState;
            if !ptr.is_null() {
                let state = unsafe { &mut *ptr };
                emit_event(hwnd, state, OverlayEvent::SyncLibrary);
            }
            LRESULT(0)
        }
        // 动画定时器：App 推进控制台/待办行动画并重绘（无动画时 App 自行停表）
        WM_TIMER if wparam.0 == ANIM_TIMER => {
            let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut WindowState;
            if !ptr.is_null() {
                let state = unsafe { &mut *ptr };
                emit_event(hwnd, state, OverlayEvent::AnimTick);
            }
            LRESULT(0)
        }
        // 所在显示器 DPI 变化（Per-Monitor v2）：`wParam` 低位 = 新 x-DPI。
        // App 重算主题缩放 + 重新夹屏 + 重绘，栅栏/文字实时跟随缩放（不再等 DWM 拉伸）。
        WM_DPICHANGED => {
            let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut WindowState;
            if !ptr.is_null() {
                let state = unsafe { &mut *ptr };
                emit_event(
                    hwnd,
                    state,
                    OverlayEvent::DpiChanged {
                        dpi: (wparam.0 & 0xFFFF) as u32,
                    },
                );
            }
            LRESULT(0)
        }
        // 显示拓扑/分辨率变化：虚拟屏宽高可能已变，App 重算并夹回超屏栅栏。
        WM_DISPLAYCHANGE => {
            let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut WindowState;
            if !ptr.is_null() {
                let state = unsafe { &mut *ptr };
                emit_event(hwnd, state, OverlayEvent::DisplayChange);
            }
            LRESULT(0)
        }
        // —— 内联文本编辑：键盘与 IME 直达 App（overlay 获得焦点时）——
        WM_KEYDOWN => {
            let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut WindowState;
            if !ptr.is_null() {
                let state = unsafe { &mut *ptr };
                emit_event(
                    hwnd,
                    state,
                    OverlayEvent::KeyDown {
                        vk: wparam.0 as u32,
                        ctrl: is_ctrl_down(),
                    },
                );
            }
            LRESULT(0)
        }
        WM_CHAR => {
            let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut WindowState;
            if !ptr.is_null() {
                let state = unsafe { &mut *ptr };
                emit_event(
                    hwnd,
                    state,
                    OverlayEvent::Char {
                        ch: wparam.0 as u16,
                    },
                );
            }
            LRESULT(0)
        }
        WM_IME_STARTCOMPOSITION => {
            let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut WindowState;
            if !ptr.is_null() {
                let state = unsafe { &mut *ptr };
                emit_event(hwnd, state, OverlayEvent::ImeStart);
            }
            LRESULT(0)
        }
        WM_IME_COMPOSITION => {
            let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut WindowState;
            if !ptr.is_null() {
                let state = unsafe { &mut *ptr };
                let flags = lparam.0 as u32;
                if flags & GCS_RESULTSTR.0 != 0 {
                    let text = ime_string(hwnd, GCS_RESULTSTR);
                    emit_event(hwnd, state, OverlayEvent::ImeEnd);
                    emit_event(hwnd, state, OverlayEvent::ImeResult { text });
                } else if flags & GCS_COMPSTR.0 != 0 {
                    let text = ime_string(hwnd, GCS_COMPSTR);
                    emit_event(hwnd, state, OverlayEvent::ImeCompose { text, caret: 0 });
                }
            }
            LRESULT(0)
        }
        WM_IME_ENDCOMPOSITION => {
            let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut WindowState;
            if !ptr.is_null() {
                let state = unsafe { &mut *ptr };
                emit_event(hwnd, state, OverlayEvent::ImeEnd);
            }
            LRESULT(0)
        }
        WM_IME_SETCONTEXT => {
            // 关闭系统默认合成框（文本由 D2D 内联编辑自己绘制），候选窗口仍跟随光标
            let lp = lparam.0 & !(0x8000_0000isize);
            unsafe { DefWindowProcW(hwnd, msg, wparam, LPARAM(lp)) }
        }
        WM_KILLFOCUS => {
            let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut WindowState;
            if !ptr.is_null() {
                let state = unsafe { &mut *ptr };
                emit_event(hwnd, state, OverlayEvent::OverlayFocusLost);
            }
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
        }
        // 编辑框（重命名/待办输入，owner = 本窗口）绘制背景/文字色。
        // 返回暗色画刷 + 设置文字色，消除默认纯白底；底色与面板填充色一致。
        WM_CTLCOLOREDIT => {
            let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut WindowState;
            if ptr.is_null() {
                return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
            }
            let state = unsafe { &mut *ptr };
            let hdc = HDC(lparam.0 as *mut core::ffi::c_void);
            unsafe {
                let _ = SetTextColor(hdc, COLORREF(0x00_FA_F2_EE)); // RGB(238,242,250)
                let _ = SetBkColor(hdc, COLORREF(0x00_22_16_10)); // RGB(16,22,34) 面板底色
            }
            LRESULT(state.edit_brush.0 as isize)
        }
        // 外部信号：干净退出消息循环（wnd_proc 跑在主线程，PostQuitMessage 投递到主队列）
        WM_APP_QUIT => {
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        // 外部关闭请求（用户点关闭按钮 / Alt+F4 / 任务管理器「结束任务」发 WM_CLOSE）：
        // 走干净退出，让 RAII 恢复被隐藏的真实桌面图标——硬杀会跳过 Drop，桌面图标
        // 永久消失（卸载程序不再强杀，用户手动关闭即走到这里）。
        WM_CLOSE => {
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        // App 注入事件：就地重命名提交后触发一次完整重绘 + 命中模型重建。
        // `lParam` 是一个 `Box<OverlayEvent>`（发送方 into_raw，这里 from_raw 并释放）。
        WM_SYLVA_INJECT if lparam.0 != 0 => {
            let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut WindowState;
            if !ptr.is_null() {
                let state = unsafe { &mut *ptr };
                let ev = unsafe { Box::from_raw(lparam.0 as *mut OverlayEvent) };
                emit_event(hwnd, state, *ev);
            }
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

/// 控制台面板内命中的控件（先匹配的生效；控件矩形已含「仅面板内」的前提）。
fn console_zone_at(c: &ConsoleHit, mx: f32, my: f32) -> Option<ConsoleZone> {
    c.zones
        .iter()
        .find(|(_, r)| r.contains(mx, my))
        .map(|(z, _)| *z)
}

/// 控制台面板右/下边缘与右下角的缩放区（面板锚定左上角，只缩放不位移）。
/// 缩放区判定在标题栏移动之前，保证抓右/下边即缩放（标准窗口行为）。
fn console_resize_zone_at(c: &ConsoleHit, mx: f32, my: f32) -> Option<ResizeZone> {
    let right = c.rect.x + c.rect.w;
    let bottom = c.rect.y + c.rect.h;
    if mx >= right - CORNER_RESIZE && my >= bottom - CORNER_RESIZE {
        return Some(ResizeZone::BottomRight);
    }
    if mx >= right - EDGE_RESIZE {
        return Some(ResizeZone::Right);
    }
    if my >= bottom - EDGE_RESIZE {
        return Some(ResizeZone::Bottom);
    }
    None
}

/// 按下：命中控制台控件/标题栏、边缘/角标开始缩放；命中栅栏标题栏/空白开始移动。
/// 拖拽类动作都捕获鼠标以跟踪拖出窗口的移动。
fn on_button_down(hwnd: HWND, state: &mut WindowState, mx: f32, my: f32) {
    // 内联编辑框优先：点编辑框内部 = 把光标定位到点击处，不落到下面的栅栏/图标
    // （否则会触发「点击别处提交编辑」，用户根本无法点击文本）。
    if let Some(er) = state.model.edit_rect {
        if er.contains(mx, my) {
            emit_event(hwnd, state, OverlayEvent::EditCaret { x: mx });
            return;
        }
    }
    // 控制台优先：面板上的点击不落到栅栏/图标。控件（关闭/勾选/删除/添加）优先于
    // 标题栏拖动——关闭按钮就在标题栏内，先判控件才能「点 × 即关」而非开始拖动。
    if let Some(c) = &state.model.console {
        if c.rect.contains(mx, my) {
            if let Some(zone) = console_zone_at(c, mx, my) {
                emit_event(hwnd, state, OverlayEvent::ConsoleClick { zone });
                return;
            }
            // 右/下边缘与右下角 → 缩放（抓边即缩放，标准窗口行为）
            if let Some(zone) = console_resize_zone_at(c, mx, my) {
                state.drag = Some(DragState {
                    kind: DragKind::ConsoleResize(zone),
                    fence: usize::MAX,
                    start: (mx, my),
                    orig: c.rect,
                    pressed_icon: None,
                    ctrl: false,
                });
                unsafe {
                    let _ = SetCursor(Some(zone_cursor(zone)));
                }
                unsafe { SetCapture(hwnd) };
                return;
            }
            if c.title.contains(mx, my) {
                state.drag = Some(DragState {
                    kind: DragKind::ConsoleMove,
                    fence: usize::MAX,
                    start: (mx, my),
                    orig: c.rect,
                    pressed_icon: None,
                    ctrl: false,
                });
                unsafe {
                    let cur = LoadCursorW(None, IDC_SIZEALL).unwrap_or_default();
                    let _ = SetCursor(Some(cur));
                }
                unsafe { SetCapture(hwnd) };
                return;
            }
            return; // 面板空白：吞掉点击
        }
    }
    // 优先级：缩放区域 > 标题栏 > 空白。
    for f in &state.model.fences {
        if f.body.contains(mx, my) {
            if let Some(zone) = resize_zone_at(f, mx, my) {
                state.drag = Some(DragState {
                    kind: DragKind::Resize(zone),
                    fence: f.id,
                    start: (mx, my),
                    orig: f.body,
                    pressed_icon: None,
                    ctrl: false,
                });
                // 拖拽期间 SetCapture 不再发 WM_SETCURSOR，这里锁定一次缩放光标
                unsafe {
                    let _ = SetCursor(Some(zone_cursor(zone)));
                }
                unsafe { SetCapture(hwnd) };
                return;
            }
            break;
        }
    }
    for f in &state.model.fences {
        if f.title.contains(mx, my) {
            // 侧边栏图标上点击不启动拖动（留给 mouseup 作为单击打开文件）
            let on_icon = state
                .model
                .icons
                .iter()
                .any(|i| i.fence == f.id && i.rect.contains(mx, my));
            if on_icon {
                break; // 跳到下方 body+图标处理
            }
            state.drag = Some(DragState {
                kind: DragKind::Move,
                fence: f.id,
                start: (mx, my),
                orig: f.body,
                pressed_icon: None,
                ctrl: false,
            });
            unsafe {
                let cur = LoadCursorW(None, IDC_SIZEALL).unwrap_or_default();
                let _ = SetCursor(Some(cur));
            }
            unsafe { SetCapture(hwnd) };
            return;
        }
    }
    // 兜底：按下栅栏主体。
    // - 空白处 → 框选（橡皮筋多选，资源管理器行为；拖动过程中移动栅栏改走标题栏/图标）。
    // - 图标上 → 拖动栅栏；位移小时作为「单击选中」，位移大时仍是拖动。
    for f in &state.model.fences {
        if f.body.contains(mx, my) {
            let pressed_icon = state
                .model
                .icons
                .iter()
                .find(|i| i.fence == f.id && i.rect.contains(mx, my))
                .map(|i| i.icon);
            if pressed_icon.is_none() {
                let ctrl = is_ctrl_down();
                state.drag = Some(DragState {
                    kind: DragKind::Select,
                    fence: f.id,
                    start: (mx, my),
                    orig: f.body,
                    pressed_icon: None,
                    ctrl,
                });
                // 空白按下即清空选择（Explorer 行为）；拖拽中再逐帧上报框选结果
                emit_event(
                    hwnd,
                    state,
                    OverlayEvent::SelectDrag {
                        fence: f.id,
                        rect: (mx, my, 0.0, 0.0),
                        selected: Vec::new(),
                    },
                );
                unsafe { SetCapture(hwnd) };
                return;
            }
            state.drag = Some(DragState {
                kind: DragKind::Move,
                fence: f.id,
                start: (mx, my),
                orig: f.body,
                pressed_icon,
                ctrl: is_ctrl_down(),
            });
            unsafe {
                let cur = LoadCursorW(None, IDC_SIZEALL).unwrap_or_default();
                let _ = SetCursor(Some(cur));
            }
            unsafe { SetCapture(hwnd) };
            return;
        }
    }
}

/// Ctrl 是否处于按下状态（框选/多选判定）。
fn is_ctrl_down() -> bool {
    unsafe { GetKeyState(VK_CONTROL.0 as i32) < 0 }
}

/// 两个矩形是否相交（含边接触）——框选命中判定。
fn rect_overlap(a: RectF, b: RectF) -> bool {
    a.x < b.x + b.w && b.x < a.x + a.w && a.y < b.y + b.h && b.y < a.y + a.h
}

/// 计算框选矩形命中的图标：只在本栅栏内取与矩形相交的图标（布局下标）。
fn band_selection(model: &HitModel, fence: usize, band: RectF) -> Vec<(usize, usize)> {
    model
        .icons
        .iter()
        .filter(|i| i.fence == fence && rect_overlap(i.rect, band))
        .map(|i| (i.fence, i.icon))
        .collect()
}

/// 点所在栅栏的缩放区域（角优先、边其次）。不在任何缩放区返回 None。
fn resize_zone_at(f: &FenceHit, mx: f32, my: f32) -> Option<ResizeZone> {
    let left = f.body.x;
    let right = f.body.x + f.body.w;
    let bottom = f.body.y + f.body.h;
    // 角（优先级最高）
    if mx >= right - CORNER_RESIZE && my >= bottom - CORNER_RESIZE {
        return Some(ResizeZone::BottomRight);
    }
    if mx <= left + CORNER_RESIZE && my >= bottom - CORNER_RESIZE {
        return Some(ResizeZone::BottomLeft);
    }
    if mx >= right - CORNER_RESIZE && my <= f.body.y + CORNER_RESIZE {
        return Some(ResizeZone::TopRight);
    }
    // 边
    if mx >= right - EDGE_RESIZE {
        return Some(ResizeZone::Right);
    }
    if mx <= left + EDGE_RESIZE {
        return Some(ResizeZone::Left);
    }
    if my >= bottom - EDGE_RESIZE {
        return Some(ResizeZone::Bottom);
    }
    None
}

/// 缩放区域对应的系统光标。
fn zone_cursor(zone: ResizeZone) -> HCURSOR {
    let id = match zone {
        ResizeZone::Left | ResizeZone::Right => IDC_SIZEWE,
        ResizeZone::Bottom => IDC_SIZENS,
        ResizeZone::BottomRight => IDC_SIZENWSE,
        ResizeZone::BottomLeft | ResizeZone::TopRight => IDC_SIZENESW,
    };
    unsafe { LoadCursorW(None, id).unwrap_or_default() }
}

/// 光标处的形状（参考 Windows 资源管理器：只有标题栏/缩放区有特殊光标）：
/// 边缘/角 → 对应 resize 光标；标题栏 → 移动光标；图标/空白一律普通箭头。
/// 侧边栏：图标上保持箭头（点击打开），空白处才显示移动把手。
fn cursor_at(model: &HitModel, mx: f32, my: f32) -> HCURSOR {
    // 控制台面板：边缘/角 = resize 光标，控件/空白 = 箭头，标题栏 = 移动光标
    if let Some(c) = &model.console {
        if c.rect.contains(mx, my) {
            if let Some(zone) = console_resize_zone_at(c, mx, my) {
                return zone_cursor(zone);
            }
            if c.title.contains(mx, my) && console_zone_at(c, mx, my).is_none() {
                return unsafe { LoadCursorW(None, IDC_SIZEALL).unwrap_or_default() };
            }
            return unsafe { LoadCursorW(None, IDC_ARROW).unwrap_or_default() };
        }
    }
    let Some(f) = model
        .fences
        .iter()
        .find(|f| f.body.contains(mx, my) || f.title.contains(mx, my))
    else {
        return unsafe { LoadCursorW(None, IDC_ARROW).unwrap_or_default() };
    };
    if let Some(zone) = resize_zone_at(f, mx, my) {
        return zone_cursor(zone);
    }
    // 侧边栏：图标上保持箭头（单击打开），只有空白处显示移动光标
    let on_icon = model
        .icons
        .iter()
        .any(|i| i.fence == f.id && i.rect.contains(mx, my));
    if on_icon {
        return unsafe { LoadCursorW(None, IDC_ARROW).unwrap_or_default() };
    }
    // 非缩放区：标题栏是移动把手，正文/图标保持箭头（像资源管理器）。
    if f.title.contains(mx, my) {
        return unsafe { LoadCursorW(None, IDC_SIZEALL).unwrap_or_default() };
    }
    unsafe { LoadCursorW(None, IDC_ARROW).unwrap_or_default() }
}

/// 悬停目标：先图标后栅栏（图标优先）。未命中任何栅栏时返回 None。
fn hover_key(model: &HitModel, mx: f32, my: f32) -> Option<(usize, Option<usize>)> {
    // 鼠标在控制台面板上时不悬停任何栅栏/图标（面板浮于栅栏之上）
    if let Some(c) = &model.console {
        if c.rect.contains(mx, my) {
            return None;
        }
    }
    for icon in &model.icons {
        if icon.rect.contains(mx, my) {
            return Some((icon.fence, Some(icon.icon)));
        }
    }
    for f in &model.fences {
        if f.body.contains(mx, my) {
            return Some((f.id, None));
        }
    }
    None
}

/// 移动：拖动中连续上报新位置/新尺寸。目标一律用**原始矩形 + 鼠标总位移**
/// （`drag.orig + (鼠标 - 按下点)`），即「原始目标」——App 层对每个原始目标
/// 独立做碰撞/吸附。若用上一帧已生效矩形做增量，吸附是粘性的：被吸住后
/// 每帧的小增量（几像素 < 吸附阈值）都会再次触发吸附，栅栏永远跳不出去。
/// 原始目标保证：鼠标一旦移出吸附阈值（超过 24px），吸附自然解除。
fn on_mouse_move(hwnd: HWND, state: &mut WindowState, mx: f32, my: f32) {
    let Some(drag) = state.drag else {
        return;
    };
    let orig = drag.orig;
    match drag.kind {
        DragKind::Move => {
            // 侧边栏图标上按下 + 移动超阈值 → 切换为拖动排序模式
            if let Some(icon_idx) = drag.pressed_icon {
                let is_sidebar = state
                    .model
                    .fences
                    .iter()
                    .find(|f| f.id == drag.fence)
                    .map(|f| f.is_sidebar)
                    .unwrap_or(false);
                let dx = mx - drag.start.0;
                let dy = my - drag.start.1;
                if is_sidebar && (dx * dx + dy * dy) > REORDER_THRESHOLD * REORDER_THRESHOLD {
                    state.drag = Some(DragState {
                        kind: DragKind::SidebarReorder,
                        fence: drag.fence,
                        start: drag.start,
                        orig: drag.orig,
                        pressed_icon: Some(icon_idx),
                        ctrl: false,
                    });
                    emit_event(
                        hwnd,
                        state,
                        OverlayEvent::SidebarReorderDrag {
                            fence: drag.fence,
                            icon: icon_idx,
                            mx,
                            my,
                        },
                    );
                    return;
                }
            }
            // 原始目标左上角 = 按下时矩形左上角 + 鼠标相对按下点的位移
            let raw_x = orig.x + (mx - drag.start.0);
            let raw_y = orig.y + (my - drag.start.1);
            let event = OverlayEvent::FenceMove {
                fence: drag.fence,
                pos: (raw_x, raw_y),
            };
            emit_event(hwnd, state, event);
        }
        DragKind::SidebarReorder => {
            if let Some(icon_idx) = drag.pressed_icon {
                emit_event(
                    hwnd,
                    state,
                    OverlayEvent::SidebarReorderDrag {
                        fence: drag.fence,
                        icon: icon_idx,
                        mx,
                        my,
                    },
                );
            }
        }
        DragKind::Select => {
            // 橡皮筋框选：从按下点到当前点围成矩形，框中的本栅栏图标全部选中
            let (x1, y1) = drag.start;
            let band = RectF {
                x: x1.min(mx),
                y: y1.min(my),
                w: (mx - x1).abs(),
                h: (my - y1).abs(),
            };
            let selected = band_selection(&state.model, drag.fence, band);
            let event = OverlayEvent::SelectDrag {
                fence: drag.fence,
                rect: (band.x, band.y, band.w, band.h),
                selected,
            };
            emit_event(hwnd, state, event);
        }
        DragKind::ConsoleMove => {
            // 目标左上角 = 按下时面板左上角 + 鼠标相对按下点的位移
            let raw_x = orig.x + (mx - drag.start.0);
            let raw_y = orig.y + (my - drag.start.1);
            emit_event(
                hwnd,
                state,
                OverlayEvent::ConsoleMove {
                    pos: (raw_x, raw_y),
                },
            );
        }
        DragKind::ConsoleResize(zone) => {
            // 面板锚定左上角：只调宽高（宽/高可随动，左上角不动）
            let (dx, dy) = (mx - drag.start.0, my - drag.start.1);
            let mut r = orig;
            match zone {
                ResizeZone::Right => r.w += dx,
                ResizeZone::Bottom => r.h += dy,
                ResizeZone::BottomRight => {
                    r.w += dx;
                    r.h += dy;
                }
                _ => {}
            }
            emit_event(
                hwnd,
                state,
                OverlayEvent::ConsoleResize {
                    rect: (r.x, r.y, r.w, r.h),
                },
            );
        }
        DragKind::Resize(zone) => {
            let (dx, dy) = (mx - drag.start.0, my - drag.start.1);
            // 原始目标矩形：被拖的边跟随鼠标，锚定边保持按下时位置
            let mut r = orig;
            match zone {
                ResizeZone::Right => r.w += dx,
                ResizeZone::Bottom => r.h += dy,
                ResizeZone::BottomRight => {
                    r.w += dx;
                    r.h += dy;
                }
                ResizeZone::Left => {
                    r.x += dx;
                    r.w -= dx;
                }
                ResizeZone::BottomLeft => {
                    r.x += dx;
                    r.w -= dx;
                    r.h += dy;
                }
                ResizeZone::TopRight => {
                    r.y += dy;
                    r.h -= dy;
                }
            }
            let event = OverlayEvent::FenceResize {
                fence: drag.fence,
                zone,
                rect: (r.x, r.y, r.w, r.h),
            };
            emit_event(hwnd, state, event);
        }
    }
}

/// 枚举 `WM_DROPFILES` 携带的文件路径。
fn drop_paths(hdrop: HDROP) -> Vec<String> {
    let mut out = Vec::new();
    unsafe {
        let n = DragQueryFileW(hdrop, u32::MAX, None);
        for i in 0..n {
            let len = DragQueryFileW(hdrop, i, None);
            let mut buf = vec![0u16; len as usize + 1];
            DragQueryFileW(hdrop, i, Some(&mut buf));
            let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
            out.push(String::from_utf16_lossy(&buf[..end]));
        }
    }
    out
}

/// 松开：结束拖拽会话。位移小于阈值且按在图标上 → 视为「单击选中」；
/// 否则是拖动（无论是否移动，都通知 App 持久化布局）。
fn on_button_up(hwnd: HWND, state: &mut WindowState, mx: f32, my: f32) {
    if let Some(drag) = state.drag.take() {
        if drag.kind == DragKind::Move {
            let dx = mx - drag.start.0;
            let dy = my - drag.start.1;
            let moved = dx * dx + dy * dy;
            let t = CLICK_DRAG_THRESHOLD;
            if moved < t * t {
                if let Some(icon) = drag.pressed_icon {
                    emit_event(
                        hwnd,
                        state,
                        OverlayEvent::IconClicked {
                            fence: drag.fence,
                            icon,
                            ctrl: drag.ctrl,
                        },
                    );
                }
            }
        } else if drag.kind == DragKind::Select {
            // 框选结束：清除橡皮筋显示（选择结果已在最后一次 SelectDrag 中生效）
            emit_event(hwnd, state, OverlayEvent::SelectEnd);
        } else if drag.kind == DragKind::ConsoleMove {
            // 控制台拖动结束：App 持久化位置
            emit_event(hwnd, state, OverlayEvent::ConsoleDragEnd);
        } else if let DragKind::ConsoleResize(_) = drag.kind {
            // 控制台缩放结束：App 持久化尺寸
            emit_event(hwnd, state, OverlayEvent::ConsoleResizeEnd);
        } else if drag.kind == DragKind::SidebarReorder {
            // 侧边栏拖动排序结束：通知 App 执行重排
            if let Some(icon_idx) = drag.pressed_icon {
                // 计算目标位置：根据光标位置在侧边栏中的相对偏移推算插入下标
                let to = compute_reorder_target(&state.model, drag.fence, icon_idx, mx, my);
                emit_event(
                    hwnd,
                    state,
                    OverlayEvent::SidebarReorderEnd {
                        fence: drag.fence,
                        from: icon_idx,
                        to,
                    },
                );
            }
        }
        // 框选/控制台拖拽不是栅栏布局变化，不触发栅栏持久化
        if matches!(
            drag.kind,
            DragKind::Move | DragKind::Resize(_) | DragKind::SidebarReorder
        ) {
            emit_event(
                hwnd,
                state,
                OverlayEvent::FenceDragEnd { fence: drag.fence },
            );
        }
        unsafe {
            let _ = ReleaseCapture();
        }
    }
}

/// 根据光标位置计算侧边栏图标重排的目标插入位置。
/// 返回值为插入下标（0..=图标总数）：图标将被插入到该下标之前。
fn compute_reorder_target(model: &HitModel, fence_id: usize, _from: usize, mx: f32, my: f32) -> usize {
    // 收集该栅栏的所有图标，按 x 坐标排序（横向侧边栏）或 y 坐标排序（纵向）
    let mut icons: Vec<_> = model
        .icons
        .iter()
        .filter(|i| i.fence == fence_id)
        .collect();
    if icons.is_empty() {
        return 0;
    }
    // 判断侧边栏方向：如果图标 x 变化大于 y 变化，为横向（top），否则纵向（left/right）
    let first = &icons[0];
    let is_horizontal = if icons.len() > 1 {
        let second = &icons[1];
        (second.rect.x - first.rect.x).abs() > (second.rect.y - first.rect.y).abs()
    } else {
        true // 单图标默认横向
    };
    if is_horizontal {
        // 横向：按 x 排序，找光标 x 落在哪两个图标之间
        icons.sort_by(|a, b| a.rect.x.partial_cmp(&b.rect.x).unwrap());
        for (i, icon) in icons.iter().enumerate() {
            let mid = icon.rect.x + icon.rect.w / 2.0;
            if mx < mid {
                return i;
            }
        }
        icons.len()
    } else {
        // 纵向：按 y 排序，找光标 y 落在哪两个图标之间
        icons.sort_by(|a, b| a.rect.y.partial_cmp(&b.rect.y).unwrap());
        for (i, icon) in icons.iter().enumerate() {
            let mid = icon.rect.y + icon.rect.h / 2.0;
            if my < mid {
                return i;
            }
        }
        icons.len()
    }
}

/// 双击图标：把下标交给 App 打开对应项。
fn on_double_click(hwnd: HWND, state: &mut WindowState, mx: f32, my: f32) {
    // 编辑框内双击同样不穿透（WM_LBUTTONDBLCLK 独立到达，需单独守卫）
    if let Some(er) = state.model.edit_rect {
        if er.contains(mx, my) {
            return;
        }
    }
    for icon in &state.model.icons {
        if icon.rect.contains(mx, my) {
            emit_event(
                hwnd,
                state,
                OverlayEvent::IconDoubleClicked {
                    fence: icon.fence,
                    icon: icon.icon,
                },
            );
            return;
        }
    }
}

/// 把事件交给 App 回调，并用回调返回的新命中模型更新区域与命中数据。
fn emit_event(hwnd: HWND, state: &mut WindowState, event: OverlayEvent) {
    if let Some(handler) = &mut state.handler {
        // None = 模态期间再入被丢弃，保持当前命中模型（见 `set_event_handler` 注释）
        if let Some(model) = handler(event) {
            state.model = model;
            apply_region(hwnd, &state.model, &mut state.last_region);
        }
    }
}

/// 读取 IME 合成/结果字符串（GCS_COMPSTR / GCS_RESULTSTR）。
fn ime_string(hwnd: HWND, index: IME_COMPOSITION_STRING) -> String {
    let ctx = unsafe { ImmGetContext(hwnd) };
    if ctx.0.is_null() {
        return String::new();
    }
    let len = unsafe { ImmGetCompositionStringW(ctx, index, None, 0) };
    let mut out = String::new();
    if len > 0 {
        let mut buf = vec![0u16; (len as usize) / 2];
        unsafe {
            ImmGetCompositionStringW(
                ctx,
                index,
                Some(buf.as_mut_ptr() as *mut _ as *mut core::ffi::c_void),
                len as u32,
            );
        }
        out = String::from_utf16_lossy(&buf);
    }
    unsafe {
        let _ = ImmReleaseContext(hwnd, ctx);
    }
    out
}

/// 鼠标消息 lParam → 客户端坐标（低/高 16 位有符号）。
fn client_point(lparam: LPARAM) -> (f32, f32) {
    let x = (lparam.0 as u16) as i16 as i32;
    let y = ((lparam.0 >> 16) as u16) as i16 as i32;
    (x as f32, y as f32)
}

/// 处理消息直到收到 `WM_QUIT`（`PostQuitMessage`）。返回后线程退出。
pub fn run_message_loop() {
    unsafe {
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).0 != 0 {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rectf_contains_checks_rectangle() {
        let r = RectF {
            x: 100.0,
            y: 200.0,
            w: 300.0,
            h: 400.0,
        };
        assert!(r.contains(100.0, 200.0));
        assert!(r.contains(400.0, 600.0));
        assert!(!r.contains(99.0, 200.0));
        assert!(!r.contains(100.0, 601.0));
    }

    #[test]
    fn client_point_sign_extension() {
        // 负坐标（主屏左侧的副屏）：低/高 16 位带符号
        let lp = LPARAM(0xFF0C_FE0Cu32 as i32 as isize); // 低 16 位 x=-500，高 16 位 y=-244
        let (x, y) = client_point(lp);
        assert_eq!(x, -500.0);
        assert_eq!(y, -244.0);
    }

    #[test]
    fn drag_resolve_delta_from_orig() {
        // 模拟移动拖拽的坐标运算：始终相对按下时的原始矩形，避免累计漂移。
        let orig = RectF {
            x: 120.0,
            y: 90.0,
            w: 400.0,
            h: 240.0,
        };
        let start = (300.0, 150.0);
        let now = (360.0, 220.0);
        let pos = (orig.x + (now.0 - start.0), orig.y + (now.1 - start.1));
        assert_eq!(pos, (180.0, 160.0));

        let size = (orig.w + (now.0 - start.0), orig.h + (now.1 - start.1));
        assert_eq!(size, (460.0, 310.0));
    }

    #[test]
    fn build_region_unions_without_panic() {
        // 纯 GDI 区域运算（无需 GPU/窗口）：验证合并逻辑可跑且能正确释放中间资源。
        let model = HitModel {
            fences: vec![
                FenceHit {
                    body: RectF {
                        x: 10.0,
                        y: 10.0,
                        w: 100.0,
                        h: 80.0,
                    },
                    title: RectF {
                        x: 10.0,
                        y: 10.0,
                        w: 100.0,
                        h: 40.0,
                    },
                    grip: RectF {
                        x: 84.0,
                        y: 64.0,
                        w: 26.0,
                        h: 26.0,
                    },
                    id: 0,
                    tooltip: None,
                    is_sidebar: false,
                },
                FenceHit {
                    body: RectF {
                        x: 60.0,
                        y: 50.0,
                        w: 120.0,
                        h: 90.0,
                    },
                    title: RectF {
                        x: 60.0,
                        y: 50.0,
                        w: 120.0,
                        h: 40.0,
                    },
                    grip: RectF {
                        x: 154.0,
                        y: 114.0,
                        w: 26.0,
                        h: 26.0,
                    },
                    id: 1,
                    tooltip: None,
                    is_sidebar: false,
                },
            ],
            icons: vec![],
            console: None,
            edit_rect: None,
        };
        let rgn = build_region(&model).expect("有栅栏就应有区域");
        unsafe {
            let _ = DeleteObject(rgn.into());
        }
    }
}
