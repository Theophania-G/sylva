//! COM 初始化（单线程单元，STA）。
//!
//! Shell 相关的 COM 接口（IShellFolder / IShellItem 等）要求调用线程
//! 先完成 CoInitializeEx。在应用启动早期调用一次。

use windows::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};

/// 初始化当前线程的 COM。已在 STA 初始化时幂等（S_FALSE 视为成功）。
pub fn init() -> windows::core::Result<()> {
    // CoInitializeEx 返回裸 HRESULT（S_OK/S_FALSE 都算成功），用 .ok()? 传播真错误
    unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }.ok()?;
    Ok(())
}
