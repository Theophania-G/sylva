//! 进程内事件总线。
//!
//! 单进程模型下，用一条有界 channel 承载应用级事件即可。
//! 各层（shell / render / ui）持有 `Sender`，主循环消费 `Receiver`。
//!
//! `tx`/`rx`/`Shutdown` 等是 M1 事件循环的预留 API，届时移除本标注。

#![allow(dead_code)] // M1 事件循环预留 API

use crossbeam_channel::{bounded, Receiver, Sender};

use fence_core::event::CoreEvent;

/// 应用级事件。后续里程碑加入 Shell/Render/UI 事件变体。
#[derive(Debug, Clone, PartialEq)]
pub enum AppEvent {
    Core(CoreEvent),
    Shutdown,
}

pub struct EventBus {
    tx: Sender<AppEvent>,
    rx: Receiver<AppEvent>,
}

impl EventBus {
    pub fn new() -> Self {
        let (tx, rx) = bounded(256);
        Self { tx, rx }
    }

    pub fn tx(&self) -> Sender<AppEvent> {
        self.tx.clone()
    }

    pub fn rx(&self) -> &Receiver<AppEvent> {
        &self.rx
    }

    pub fn publish(&self, ev: AppEvent) {
        // 有界 channel；消费者短暂繁忙时丢弃日志并继续（桌面工具可接受）。
        let _ = self.tx.send(ev);
    }
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bus_publishes_and_receives() {
        let bus = EventBus::new();
        bus.publish(AppEvent::Shutdown);
        assert_eq!(bus.rx().recv(), Ok(AppEvent::Shutdown));
    }
}
