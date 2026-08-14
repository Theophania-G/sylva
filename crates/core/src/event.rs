//! 领域事件。核心引擎状态变化时发出，由 app 层的事件总线分发。
//!
//! 事件只携带「发生了什么」的纯数据，不携带平台句柄或渲染对象，
//! 保证事件可在 crate 边界自由传递。

use crate::model::{ItemId, Rect};

/// 核心引擎发出的领域事件。
#[derive(Debug, Clone, PartialEq)]
pub enum CoreEvent {
    /// 桌面状态已加载。
    DeskLoaded { version: u32 },
    /// 桌面状态已保存。
    DeskSaved,
    /// 新建栅栏。
    FenceCreated { id: u64 },
    /// 删除栅栏（成员图标移入未分组区）。
    FenceRemoved { id: u64 },
    /// 栅栏几何变化。
    FenceBoundsChanged { id: u64, bounds: Rect },
    /// 栅栏折叠状态变化。
    FenceStateChanged {
        id: u64,
        state: crate::model::FenceState,
    },
    /// 图标加入桌面（新增图标进入未分组区）。
    IconAdded { id: ItemId },
    /// 图标从桌面移除。
    IconRemoved { id: ItemId },
    /// 图标移动归属。`from`/`to` 为 `None` 表示未分组区。
    IconMoved {
        id: ItemId,
        from: Option<u64>,
        to: Option<u64>,
    },
    /// 图标在栅栏内重排。
    IconReordered { fence_id: u64 },
}
