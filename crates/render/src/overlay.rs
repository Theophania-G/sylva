//! overlay 窗口：挂在桌面壳层下的透明子窗口，承载 DComp 视觉树。
//!
//! - 覆盖整个虚拟屏幕（多显示器），位置随父窗口屏幕坐标实时计算；
//! - `WS_EX_NOACTIVATE` 不抢焦点；`WS_EX_NOREDIRECTIONBITMAP` 走 DComp 合成
//!   （无 GDI 重定向，消除闪烁与额外内存）；
//! - 命中测试：栅栏矩形内 → `HTCLIENT`（可交互），其余 → `HTTRANSPARENT`
//!   （点击穿透到桌面/壁纸）；
//! - 窗口状态（命中矩形）保存在 `GWLP_USERDATA`，由 `OverlayWindow` 独占生命周期，
//!   同一线程读写，无需跨线程同步。

use std::sync::OnceLock;

use windows::core::{Result, PCWSTR};
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW,
    GetSystemMetrics, GetWindowLongPtrW, GetWindowRect, RegisterClassW, SetWindowLongPtrW,
    ShowWindow, TranslateMessage, GWLP_USERDATA, HTCLIENT, HTTRANSPARENT, MSG, SM_CXVIRTUALSCREEN,
    SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN, SW_SHOWNA, WM_ERASEBKGND,
    WM_NCHITTEST, WNDCLASSW, WS_EX_NOACTIVATE, WS_EX_NOREDIRECTIONBITMAP, WS_POPUP,
};

/// 窗口类名（全局唯一，单实例）。
const CLASS_NAME: &str = "FenceDesktopOverlay";

/// 类只注册一次（同一 HINSTANCE）。
static CLASS_REGISTERED: OnceLock<()> = OnceLock::new();

/// 可交互区域（虚拟屏幕坐标，物理像素）。
#[derive(Debug, Clone, Copy, Default)]
pub struct HitRect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

struct WindowState {
    hit_rects: Vec<HitRect>,
}

/// overlay 窗口。
pub struct OverlayWindow {
    pub hwnd: HWND,
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
            hit_rects: Vec::new(),
        });
        let state_ptr = Box::into_raw(state);

        let hwnd = unsafe {
            CreateWindowExW(
                WS_EX_NOACTIVATE | WS_EX_NOREDIRECTIONBITMAP,
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
        let _shown = unsafe { ShowWindow(hwnd, SW_SHOWNA) };

        Ok(Self {
            hwnd,
            state: state_ptr,
        })
    }

    /// 更新可交互（命中）区域。
    pub fn set_hit_rects(&self, rects: &[HitRect]) {
        unsafe { &mut *self.state }.hit_rects = rects.to_vec();
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
            style: Default::default(),
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

unsafe extern "system" fn wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_NCHITTEST => {
            // 状态未就绪（创建早期）时一律穿透
            let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut WindowState;
            if ptr.is_null() {
                return LRESULT(HTTRANSPARENT as isize);
            }
            let state = unsafe { &*ptr };
            // lParam：低/高 16 位为屏幕坐标（有符号，覆盖主屏左侧的负坐标）
            let sx = (lparam.0 as u16) as i16 as i32;
            let sy = ((lparam.0 >> 16) as u16) as i16 as i32;
            let (vx, vy, _, _) = virtual_screen();
            let cx = (sx - vx) as f32;
            let cy = (sy - vy) as f32;
            let hit = state
                .hit_rects
                .iter()
                .any(|r| cx >= r.x && cx <= r.x + r.width && cy >= r.y && cy <= r.y + r.height);
            if hit {
                LRESULT(HTCLIENT as isize)
            } else {
                LRESULT(HTTRANSPARENT as isize)
            }
        }
        // DComp 接管合成，擦除由合成器完成
        WM_ERASEBKGND => LRESULT(1),
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
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
    fn hit_rect_math_used_by_wnd_proc_is_correct() {
        // 模拟 wnd_proc 的命中判断：屏幕坐标 → overlay 客户端坐标 → 命中测试。
        let rects = [HitRect {
            x: 100.0,
            y: 200.0,
            width: 300.0,
            height: 400.0,
        }];
        let (vx, vy) = (0, 0); // 主屏：虚拟原点即屏幕原点
        let hit = |sx: i32, sy: i32| {
            let cx = (sx - vx) as f32;
            let cy = (sy - vy) as f32;
            rects
                .iter()
                .any(|r| cx >= r.x && cx <= r.x + r.width && cy >= r.y && cy <= r.y + r.height)
        };
        assert!(hit(100, 200));
        assert!(hit(400, 600));
        assert!(!hit(99, 200));
        assert!(!hit(100, 601));
    }

    #[test]
    fn lparam_screen_coords_sign_extension() {
        // 负坐标（主屏左侧的副屏）：lParam 高 16 位带符号
        let lp = LPARAM(0xFF0C_FE0Cu32 as i32 as isize); // 低 16 位 x=-500，高 16 位 y=-244
        let sx = (lp.0 as u16) as i16 as i32;
        let sy = ((lp.0 >> 16) as u16) as i16 as i32;
        assert_eq!(sx, -500);
        assert_eq!(sy, -244);
    }
}
