//! overlay 窗口：挂在桌面壳层下的透明子窗口，承载 DComp 视觉树。
//!
//! - 覆盖整个虚拟屏幕（多显示器），位置随父窗口屏幕坐标实时计算；
//! - `WS_EX_NOACTIVATE` 不抢焦点；窗口内容完全由 DComp 视觉树提供（不设置
//!   `WS_EX_NOREDIRECTIONBITMAP`——红方向免窗口无法与 DComp target 关联，
//!   `CreateTargetForHwnd` 会返回 `E_INVALIDARG`）；
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
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    CombineRgn, CreateRectRgn, DeleteObject, SetWindowRgn, HRGN, RGN_OR,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    RegisterHotKey, ReleaseCapture, SetCapture, MOD_CONTROL, MOD_SHIFT, VK_F10,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW,
    GetSystemMetrics, GetWindowLongPtrW, GetWindowRect, PostQuitMessage, RegisterClassW,
    SetWindowLongPtrW, ShowWindow, TranslateMessage, CS_DBLCLKS, GWLP_USERDATA, HTCLIENT,
    HTTRANSPARENT, MSG, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN,
    SM_YVIRTUALSCREEN, SW_SHOWNA, WM_ERASEBKGND, WM_HOTKEY, WM_LBUTTONDBLCLK, WM_LBUTTONDOWN,
    WM_LBUTTONUP, WM_MOUSEMOVE, WM_NCHITTEST, WNDCLASSW, WS_EX_NOACTIVATE, WS_POPUP,
};

/// 窗口类名（全局唯一，单实例）。
const CLASS_NAME: &str = "SylvaOverlay";

/// 外部通知主循环退出的消息（WM_APP + 1）。
/// 由 `run_message_loop` 的调用方决定在退出前恢复现场（如恢复真实桌面图标）。
pub const WM_APP_QUIT: u32 = 0x8000 + 1;

/// 全局退出热键：Ctrl+Shift+F10。GUI 版没有控制台，Ctrl+C 不可用，
/// 必须有一个干净退出的入口（否则只能杀进程，桌面图标无法恢复）。
const QUIT_HOTKEY_ID: i32 = 1;

/// 右下角缩放手柄尺寸（物理像素）。App 层用同样数值生成手柄命中区域。
pub const GRIP_SIZE: f32 = 26.0;

/// 类只注册一次（同一 HINSTANCE）。
static CLASS_REGISTERED: OnceLock<()> = OnceLock::new();

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
}

/// 一个图标的命中数据。`fence` / `icon` 分别是 App 层 `desk.fences` 下标
/// 与该栅栏 `icon_ids` 下标（与场景中图标的排列一一对应）。
#[derive(Debug, Clone, Copy)]
pub struct IconHit {
    pub rect: RectF,
    pub fence: usize,
    pub icon: usize,
}

/// 控制台按钮动作（App 层执行）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConsoleAction {
    /// 新建一个栅栏。
    NewFence,
    /// 切换某个栅栏的窗口模式（填充 / 描边 / 玻璃）。
    CycleStyle { fence: usize },
}

/// 控制台按钮的命中数据。
#[derive(Debug, Clone, Copy)]
pub struct ConsoleHit {
    pub rect: RectF,
    pub action: ConsoleAction,
}

/// 命中模型：窗口区域 + 交互命中测试的数据源。
#[derive(Debug, Clone, Default)]
pub struct HitModel {
    pub fences: Vec<FenceHit>,
    pub icons: Vec<IconHit>,
    /// 控制台面板整体矩形（加入窗口区域，使按钮可点击；面板本身不拖拽）。
    pub console_panel: Option<RectF>,
    /// 控制台按钮（点击立即触发，优先级高于栅栏拖动）。
    pub console: Vec<ConsoleHit>,
}

/// 用户交互事件（坐标全部为虚拟屏幕物理像素）。
#[derive(Debug, Clone, Copy)]
pub enum OverlayEvent {
    /// 拖动标题栏移动栅栏：目标位置（左上角）。
    FenceMove { fence: usize, pos: (f32, f32) },
    /// 拖动角标缩放：目标尺寸（宽度；高度由 App 按内容自适应）。
    FenceResize { fence: usize, size: (f32, f32) },
    /// 一次拖动结束（App 在此持久化布局）。
    FenceDragEnd { fence: usize },
    /// 双击栅栏内的图标。
    IconDoubleClicked { fence: usize, icon: usize },
    /// 点击控制台按钮。
    ConsoleClick { action: ConsoleAction },
}

/// 拖拽会话（按下到松开之间持续有效）。
#[derive(Debug, Clone, Copy)]
struct DragState {
    mode: DragMode,
    fence: usize,
    /// 按下时的鼠标位置（虚拟屏幕坐标）。
    start: (f32, f32),
    /// 按下时栅栏的整体矩形（增量都从它出发，避免累计漂移）。
    orig: RectF,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum DragMode {
    /// 标题栏拖动 → 移动。
    Move,
    /// 角标拖动 → 缩放。
    Resize,
}

struct WindowState {
    model: HitModel,
    drag: Option<DragState>,
    /// App 层事件处理器；返回新的命中模型以同步区域与命中数据。
    handler: Option<Box<dyn FnMut(OverlayEvent) -> HitModel>>,
}

/// overlay 窗口。
pub struct OverlayWindow {
    pub hwnd: HWND,
    /// 覆盖的虚拟屏幕尺寸（物理像素）。
    pub width: u32,
    pub height: u32,
    state: *mut WindowState,
}

impl OverlayWindow {
    /// 在桌面壳层下创建覆盖整个虚拟屏幕的 overlay 窗口。
    pub fn create(parent: HWND) -> Result<Self> {
        let hmodule = unsafe { GetModuleHandleW(None)? };
        let hinstance = HINSTANCE(hmodule.0);
        ensure_class(hinstance);

        let (vx, vy, vw, vh) = virtual_screen();

        // 父窗口屏幕左上角 → 子窗口定位（WorkerW 无边框无标题，outer≈client）
        let mut parent_rect = RECT::default();
        unsafe { GetWindowRect(parent, &mut parent_rect)? };

        let state = Box::new(WindowState {
            model: HitModel::default(),
            drag: None,
            handler: None,
        });
        let state_ptr = Box::into_raw(state);

        let hwnd = unsafe {
            CreateWindowExW(
                WS_EX_NOACTIVATE,
                PCWSTR(wide(CLASS_NAME).as_ptr()),
                PCWSTR::null(),
                WS_POPUP,
                vx - parent_rect.left,
                vy - parent_rect.top,
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
        let _shown = unsafe { ShowWindow(hwnd, SW_SHOWNA) };

        Ok(Self {
            hwnd,
            width: vw as u32,
            height: vh as u32,
            state: state_ptr,
        })
    }

    /// 应用命中模型：更新命中数据并把窗口区域裁剪为栅栏并集（区域外点击穿透）。
    pub fn set_model(&self, model: HitModel) {
        let state = unsafe { &mut *self.state };
        state.model = model;
        apply_region(self.hwnd, &state.model);
    }

    /// 设置用户交互事件处理器。回调返回新的命中模型（由 App 根据新布局生成），
    /// overlay 随即更新命中数据与窗口区域。
    pub fn set_event_handler(&self, handler: Box<dyn FnMut(OverlayEvent) -> HitModel>) {
        unsafe { &mut *self.state }.handler = Some(handler);
    }
}

impl Drop for OverlayWindow {
    fn drop(&mut self) {
        // 先销毁窗口（同步触发 WM_DESTROY 及后续消息），再回收状态，
        // 避免窗口销毁期间 wnd_proc 引用已释放的内存。
        let _ = unsafe { DestroyWindow(self.hwnd) };
        unsafe { drop(Box::from_raw(self.state)) };
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
            hIcon: Default::default(),
            hCursor: Default::default(),
            hbrBackground: Default::default(),
            lpszMenuName: PCWSTR::null(),
            lpszClassName: PCWSTR(class_name.as_ptr()),
        };
        let _atom = unsafe { RegisterClassW(&wc) };
        // 类重名时返回 0（唯一实例，忽略）
    });
}

/// 把命中模型的栅栏矩形并集设为窗口区域。区域外：
/// - 点击/命中直接落到下方窗口（点击穿透的关键）；
/// - 该区域同时限制窗口的可视范围——场景恰好只画这些栅栏，无可见裁剪。
///
/// `SetWindowRgn` 接管区域所有权：旧区域由系统自动释放，新建的区域交给窗口后
/// 不可再手动删除；中间产生的单矩形区域在合并进结果后立即删除。
fn apply_region(hwnd: HWND, model: &HitModel) {
    if let Some(rgn) = build_region(model) {
        unsafe { SetWindowRgn(hwnd, Some(rgn), true) };
    }
}

/// 把全部栅栏矩形 + 控制台面板合并成一个区域（RGN_OR 并集）。无有效矩形时返回 None。
fn build_region(model: &HitModel) -> Option<HRGN> {
    let mut acc: Option<HRGN> = None;
    for f in &model.fences {
        add_rect(&mut acc, f.body);
    }
    if let Some(panel) = model.console_panel {
        add_rect(&mut acc, panel);
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
        // DComp 接管合成，擦除由合成器完成
        WM_ERASEBKGND => LRESULT(1),
        WM_LBUTTONDOWN | WM_MOUSEMOVE | WM_LBUTTONUP | WM_LBUTTONDBLCLK => {
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
                WM_MOUSEMOVE => on_mouse_move(hwnd, state, mx, my),
                WM_LBUTTONUP => on_button_up(hwnd, state),
                WM_LBUTTONDBLCLK => on_double_click(hwnd, state, mx, my),
                _ => unreachable!(),
            }
            LRESULT(0)
        }
        // 全局退出热键触发：与 WM_APP_QUIT 相同，走干净退出
        WM_HOTKEY if wparam.0 as i32 == QUIT_HOTKEY_ID => {
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        // 外部信号：干净退出消息循环（wnd_proc 跑在主线程，PostQuitMessage 投递到主队列）
        WM_APP_QUIT => {
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

/// 按下：命中控制台按钮立即触发；命中角标开始缩放；命中标题栏开始移动。
/// 拖拽类动作都捕获鼠标以跟踪拖出窗口的移动。
fn on_button_down(hwnd: HWND, state: &mut WindowState, mx: f32, my: f32) {
    // 优先级：控制台按钮（即时点击）> 角标（缩放）> 标题栏（移动）。
    for btn in &state.model.console {
        if btn.rect.contains(mx, my) {
            emit_event(
                hwnd,
                state,
                OverlayEvent::ConsoleClick { action: btn.action },
            );
            return;
        }
    }
    for f in &state.model.fences {
        if f.grip.contains(mx, my) {
            state.drag = Some(DragState {
                mode: DragMode::Resize,
                fence: f.id,
                start: (mx, my),
                orig: f.body,
            });
            unsafe { SetCapture(hwnd) };
            return;
        }
    }
    for f in &state.model.fences {
        if f.title.contains(mx, my) {
            state.drag = Some(DragState {
                mode: DragMode::Move,
                fence: f.id,
                start: (mx, my),
                orig: f.body,
            });
            unsafe { SetCapture(hwnd) };
            return;
        }
    }
}

/// 移动：拖动中连续上报新位置/新尺寸（增量一律相对按下时的原始矩形）。
fn on_mouse_move(hwnd: HWND, state: &mut WindowState, mx: f32, my: f32) {
    let Some(drag) = state.drag else {
        return;
    };
    let (dx, dy) = (mx - drag.start.0, my - drag.start.1);
    match drag.mode {
        DragMode::Move => {
            let event = OverlayEvent::FenceMove {
                fence: drag.fence,
                pos: (drag.orig.x + dx, drag.orig.y + dy),
            };
            emit_event(hwnd, state, event);
        }
        DragMode::Resize => {
            let event = OverlayEvent::FenceResize {
                fence: drag.fence,
                size: (drag.orig.w + dx, drag.orig.h + dy),
            };
            emit_event(hwnd, state, event);
        }
    }
}

/// 松开：结束拖拽会话，通知 App 持久化。
fn on_button_up(hwnd: HWND, state: &mut WindowState) {
    if let Some(drag) = state.drag.take() {
        emit_event(
            hwnd,
            state,
            OverlayEvent::FenceDragEnd { fence: drag.fence },
        );
        unsafe {
            let _ = ReleaseCapture();
        }
    }
}

/// 双击图标：把下标交给 App 打开对应项。
fn on_double_click(hwnd: HWND, state: &mut WindowState, mx: f32, my: f32) {
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
        let model = handler(event);
        state.model = model;
        apply_region(hwnd, &state.model);
    }
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
                },
            ],
            icons: vec![],
            console_panel: Some(RectF {
                x: 20.0,
                y: 20.0,
                w: 300.0,
                h: 120.0,
            }),
            console: vec![],
        };
        let rgn = build_region(&model).expect("有栅栏就应有区域");
        unsafe {
            let _ = DeleteObject(rgn.into());
        }
    }
}
