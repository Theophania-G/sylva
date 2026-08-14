//! 桌面栅栏整理器 —— 入口。
//!
//! M0 阶段：初始化日志、加载桌面状态、验证事件总线与持久化闭环。
//! M1 起：进入 Win32 消息循环，接管桌面 Shell 并驱动渲染。

mod event_bus;
mod logging;

use std::path::PathBuf;

use event_bus::{AppEvent, EventBus};
use fence_core::config::ConfigStore;
use fence_core::event::CoreEvent;

/// 应用数据目录名（位于 %APPDATA% 下）。
const APP_DIR: &str = "FenceOrganizer";

fn main() {
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
        eprintln!("启动失败: {e:?}");
    }
}

fn run(data_dir: &std::path::Path) -> fence_core::Result<()> {
    // 加载（或创建）桌面状态
    let store = ConfigStore::new(data_dir.to_path_buf());
    let mut desk = store.load()?;
    desk.validate();

    let bus = EventBus::new();
    bus.publish(AppEvent::Core(CoreEvent::DeskLoaded {
        version: desk.version,
    }));

    tracing::info!(
        fences = desk.fences.len(),
        icons = desk.icons.len(),
        free = desk.free_icons.len(),
        "桌面状态已加载"
    );

    // 校验成功后回写一次，保证配置目录就绪
    store.save(&desk)?;
    bus.publish(AppEvent::Core(CoreEvent::DeskSaved));

    tracing::info!(
        progman = fence_shell::probe_progman().is_some(),
        "M0 骨架就绪；M1 将接管桌面 Shell"
    );

    Ok(())
}
