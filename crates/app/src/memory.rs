//! 内存自检：报告当前进程的工作集与私有提交字节（用于验证内存优化，不参与业务逻辑）。

use windows::Win32::System::ProcessStatus::{
    EmptyWorkingSet, GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS,
};
use windows::Win32::System::Threading::GetCurrentProcess;

/// 读取并记录一段内存快照：工作集（RAM）与私有提交（页文件用量）。
///
/// 数据来自 `GetProcessMemoryInfo`，与任务管理器「内存」列口径一致。
/// 在初始化完成、图标上传后等关键节点调用，便于对比优化前后。
pub fn report(tag: &str) {
    let mut pmc: PROCESS_MEMORY_COUNTERS = unsafe { std::mem::zeroed() };
    let cb = std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32;
    let proc_handle = unsafe { GetCurrentProcess() };
    match unsafe { GetProcessMemoryInfo(proc_handle, &mut pmc, cb) } {
        Ok(()) => {
            tracing::info!(
                tag,
                working_set_mb = pmc.WorkingSetSize as f64 / 1048576.0,
                pagefile_mb = pmc.PagefileUsage as f64 / 1048576.0,
                "内存快照"
            );
        }
        Err(e) => tracing::warn!(tag, "读取进程内存失败: {e}"),
    }
}

/// 把已不再活跃的启动期内存页换出工作集，降低常驻内存（任务管理器「内存」列）。
///
/// 首次呈现后调用一次：D3D/D2D/场景构建等一次性分配的内存在随后的空闲期基本不触碰，
/// 留在工作集里白白占用 RAM。`EmptyWorkingSet` 让系统把这些页换出（用到时再换回），
/// 不释放提交量，只降常驻内存；GPU 侧资源由驱动管理，不受影响。
pub fn trim() {
    match unsafe { EmptyWorkingSet(GetCurrentProcess()) } {
        Ok(()) => tracing::info!("启动期工作集已修剪"),
        Err(e) => tracing::warn!("修剪工作集失败: {e}"),
    }
}
