//! 领域模型：桌面、栅栏、图标等核心类型。
//!
//! 本模块只包含纯数据定义与默认值，不含任何平台相关逻辑，
//! 以便 `fence-core` 保持零 Win32 依赖、可完全单元测试。

use serde::{Deserialize, Serialize};

use std::collections::HashMap;

/// 图标的稳定标识符。
///
/// 由 Shell 层的项指纹（PIDL / 路径哈希）生成，跨重启稳定，
/// 用于持久化成员关系与排序。
pub type ItemId = String;

/// 二维点。
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct Vec2 {
    pub x: f32,
    pub y: f32,
}

/// 矩形（逻辑坐标，与 DPI 无关；渲染时由 Render 层按 DPI 换算像素）。
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }

    /// 右边界。
    pub fn right(&self) -> f32 {
        self.x + self.w
    }

    /// 下边界。
    pub fn bottom(&self) -> f32 {
        self.y + self.h
    }

    /// 点是否在矩形内（含边界）。
    pub fn contains(&self, p: Vec2) -> bool {
        p.x >= self.x && p.x <= self.right() && p.y >= self.y && p.y <= self.bottom()
    }

    /// 向内收缩 `d`（负值向外扩张）。不会缩到负宽高。
    pub fn inset(&self, d: f32) -> Self {
        Self {
            x: self.x + d,
            y: self.y + d,
            w: (self.w - 2.0 * d).max(0.0),
            h: (self.h - 2.0 * d).max(0.0),
        }
    }
}

/// 图标的类别。仅用于渲染表现（如快捷方式角标）与右键菜单，
/// **不参与任何自动归类**——归属完全由用户拖拽决定。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ItemKind {
    /// 应用程序。
    App,
    /// 文件夹。
    Folder,
    /// 文档。
    Doc,
    /// 驱动器/卷。
    Drive,
    /// 快捷方式。
    Link,
    /// 未知。
    Unknown,
}

/// 栅栏的折叠状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum FenceState {
    /// 展开：显示栅栏区域与全部图标。
    #[default]
    Expanded,
    /// 折叠：仅显示标题栏。
    Folded,
}

/// 栅栏外观配置。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FenceAppearance {
    /// 背景色 RGBA（0.0..=1.0）。
    pub bg_color: [f32; 4],
    /// 圆角半径（逻辑 px）。
    pub corner_radius: f32,
    /// 是否启用亚克力毛玻璃（Win11 生效，Win10 降级半透明）。
    pub acrylic: bool,
    /// 标题栏高度（逻辑 px）。
    pub title_bar_height: f32,
    /// 栅栏内边距。
    pub padding: f32,
    /// 图标尺寸（逻辑 px）。
    pub icon_size: f32,
    /// 图标间距（逻辑 px）。
    pub gap: f32,
}

impl Default for FenceAppearance {
    fn default() -> Self {
        Self {
            bg_color: [0.08, 0.08, 0.12, 0.55],
            corner_radius: 12.0,
            acrylic: true,
            title_bar_height: 32.0,
            padding: 12.0,
            icon_size: 48.0,
            gap: 10.0,
        }
    }
}

/// 栅栏。绑定「显式图标成员列表」，无分类语义。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Fence {
    pub id: u64,
    /// 用户自拟标题，可留空。
    pub title: Option<String>,
    /// 所属显示器（逻辑坐标基准）。
    pub monitor_id: u32,
    /// 栅栏几何（逻辑坐标）。
    pub bounds: Rect,
    pub state: FenceState,
    /// 成员顺序即布局顺序。
    pub icon_ids: Vec<ItemId>,
    pub appearance: FenceAppearance,
}

/// 图标元数据。核心层只关心标识与展示信息。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Icon {
    pub id: ItemId,
    pub display_name: String,
    pub kind: ItemKind,
}

/// 图标当前的归属位置。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IconLocation {
    /// 未分组图标区。
    Free,
    /// 位于指定栅栏内。
    Fence(u64),
}

/// 桌面全局状态，唯一的持久化根。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Desk {
    /// 配置版本，用于结构迁移。
    pub version: u32,
    pub settings: crate::config::AppSettings,
    pub fences: Vec<Fence>,
    /// 未分组图标区：不属于任何栅栏的图标，顺序即布局顺序。
    pub free_icons: Vec<ItemId>,
    pub icons: HashMap<ItemId, Icon>,
}

impl Desk {
    pub fn new(settings: crate::config::AppSettings) -> Self {
        Self {
            version: 1,
            settings,
            fences: Vec::new(),
            free_icons: Vec::new(),
            icons: HashMap::new(),
        }
    }

    pub fn fence(&self, id: u64) -> Option<&Fence> {
        self.fences.iter().find(|f| f.id == id)
    }

    pub fn fence_mut(&mut self, id: u64) -> Option<&mut Fence> {
        self.fences.iter_mut().find(|f| f.id == id)
    }

    /// 下一个可用的栅栏 id（简单自增，稳定即可）。
    pub fn next_fence_id(&self) -> u64 {
        self.fences.iter().map(|f| f.id).max().unwrap_or(0) + 1
    }

    /// 查询图标的归属位置。
    pub fn icon_location(&self, id: &ItemId) -> Option<IconLocation> {
        for f in &self.fences {
            if f.icon_ids.contains(id) {
                return Some(IconLocation::Fence(f.id));
            }
        }
        if self.free_icons.contains(id) {
            Some(IconLocation::Free)
        } else {
            None
        }
    }

    /// 把一个图标从当前归属移到目标位置（None 表示未分组区）。
    /// 幂等：目标位置已包含则原样返回。图标不存在时不做任何事。
    pub fn move_icon(&mut self, id: &ItemId, to: Option<u64>) -> Option<IconLocation> {
        let from = self.icon_location(id)?;
        // 从旧位置移除
        if let Some(fid) = from.fence_id() {
            if let Some(f) = self.fence_mut(fid) {
                f.icon_ids.retain(|x| x != id);
            }
        }
        self.free_icons.retain(|x| x != id);
        // 加入新位置
        match to {
            None => {
                if !self.free_icons.contains(id) {
                    self.free_icons.push(id.clone());
                }
            }
            Some(fid) => {
                if let Some(f) = self.fence_mut(fid) {
                    if !f.icon_ids.contains(id) {
                        f.icon_ids.push(id.clone());
                    }
                } else {
                    // 目标栅栏不存在：退回未分组区
                    if !self.free_icons.contains(id) {
                        self.free_icons.push(id.clone());
                    }
                }
            }
        }
        Some(from)
    }

    /// 校验栅栏成员引用的完整性（存在但无元数据的图标会被剔除）。
    /// 用于加载配置后的防御性清理。
    pub fn validate(&mut self) {
        for f in &mut self.fences {
            f.icon_ids.retain(|id| self.icons.contains_key(id));
        }
        self.free_icons.retain(|id| self.icons.contains_key(id));
    }
}

impl IconLocation {
    /// 栅栏 id；未分组区返回 None。
    pub fn fence_id(&self) -> Option<u64> {
        match self {
            IconLocation::Free => None,
            IconLocation::Fence(id) => Some(*id),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn desk() -> Desk {
        Desk::new(crate::config::AppSettings::default())
    }

    fn icon(id: &str) -> Icon {
        Icon {
            id: id.to_string(),
            display_name: id.to_string(),
            kind: ItemKind::Unknown,
        }
    }

    #[test]
    fn move_icon_between_locations() {
        let mut d = desk();
        d.icons.insert("a".into(), icon("a"));
        d.icons.insert("b".into(), icon("b"));
        d.free_icons = vec!["a".into(), "b".into()];

        let f1 = d.next_fence_id();
        d.fences.push(Fence {
            id: f1,
            title: Some("工作".into()),
            monitor_id: 0,
            bounds: Rect::new(0.0, 0.0, 300.0, 200.0),
            state: FenceState::Expanded,
            icon_ids: Vec::new(),
            appearance: FenceAppearance::default(),
        });

        // 移到栅栏
        let from = d.move_icon(&"a".into(), Some(f1));
        assert_eq!(from, Some(IconLocation::Free));
        assert_eq!(d.icon_location(&"a".into()), Some(IconLocation::Fence(f1)));

        // 移回未分组区
        let from = d.move_icon(&"a".into(), None);
        assert_eq!(from, Some(IconLocation::Fence(f1)));
        assert_eq!(d.icon_location(&"a".into()), Some(IconLocation::Free));
    }

    #[test]
    fn move_icon_to_missing_fence_falls_back_to_free() {
        let mut d = desk();
        d.icons.insert("a".into(), icon("a"));
        d.free_icons = vec!["a".into()];
        let from = d.move_icon(&"a".into(), Some(999));
        assert_eq!(from, Some(IconLocation::Free));
        assert_eq!(d.icon_location(&"a".into()), Some(IconLocation::Free));
    }

    #[test]
    fn validate_drops_dangling_members() {
        let mut d = desk();
        d.icons.insert("a".into(), icon("a"));
        d.fences.push(Fence {
            id: 1,
            title: None,
            monitor_id: 0,
            bounds: Rect::default(),
            state: FenceState::Expanded,
            icon_ids: vec!["a".into(), "ghost".into()],
            appearance: FenceAppearance::default(),
        });
        d.free_icons = vec!["ghost2".into()];
        d.validate();
        assert_eq!(d.fences[0].icon_ids, vec!["a".to_string()]);
        assert!(d.free_icons.is_empty());
    }

    #[test]
    fn rect_contains_and_inset() {
        let r = Rect::new(10.0, 10.0, 100.0, 50.0);
        assert!(r.contains(Vec2 { x: 10.0, y: 10.0 }));
        assert!(!r.contains(Vec2 { x: 111.0, y: 10.0 }));
        let inner = r.inset(5.0);
        assert_eq!(inner, Rect::new(15.0, 15.0, 90.0, 40.0));
    }
}
