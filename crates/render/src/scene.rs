//! 场景模型：一次绘制所需的全部数据（应用无关）。
//!
//! 坐标全部为**物理像素**（虚拟屏幕坐标），与 overlay 窗口客户端坐标一致。

use crate::overlay::RectF;

/// 栅栏内的一个图标（位置由 App 层按主题网格排布后填入）。
#[derive(Debug, Clone)]
pub struct SceneIcon {
    /// 图标下方文字。
    pub label: String,
    /// 对应 `IconStore` 中的位图 ID（`IconStore::insert` 返回）。
    pub bitmap_id: u64,
    /// 图标左上角（物理像素，虚拟屏幕坐标）。
    pub x: f32,
    pub y: f32,
    pub size: f32,
}

/// 一个栅栏。
#[derive(Debug, Clone)]
pub struct SceneFence {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub title: String,
    pub icons: Vec<SceneIcon>,
    /// 边框描边宽度（物理像素）；0 = 不描边。
    pub border_width: f32,
    /// 边框颜色（直通 alpha）。
    pub border_color: [f32; 4],
    /// 背景填充颜色（直通 alpha）；None = 内部完全透明（仅描边）。
    pub fill_color: Option<[f32; 4]>,
}

impl SceneFence {
    /// 点是否落在栅栏矩形内（命中测试用）。
    pub fn contains(&self, px: f32, py: f32) -> bool {
        px >= self.x && px <= self.x + self.width && py >= self.y && py <= self.y + self.height
    }
}

/// 控制台里的一个按钮（几何 + 文案）。
#[derive(Debug, Clone)]
pub struct ConsoleButton {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub label: String,
}

/// 控制台里的一个栅栏行（名字 + 当前模式 + 模式切换按钮）。
#[derive(Debug, Clone)]
pub struct ConsoleRow {
    pub label: String,
    pub label_rect: RectF,
    pub mode_label: String,
    pub mode_rect: RectF,
    pub mode_btn: ConsoleButton,
}

/// Sylva 控制台面板：集成所有功能的入口（新建栅栏 / 切换窗口模式 / 操作提示）。
#[derive(Debug, Clone)]
pub struct SceneConsole {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub title: String,
    pub add_btn: ConsoleButton,
    pub rows: Vec<ConsoleRow>,
}

/// 整屏场景（虚拟屏幕）。
#[derive(Debug, Default)]
pub struct Scene {
    /// 虚拟屏幕尺寸（物理像素）。
    pub width: f32,
    pub height: f32,
    pub fences: Vec<SceneFence>,
    /// 控制台面板；None = 本帧不画。
    pub console: Option<SceneConsole>,
}

impl Scene {
    pub fn new(width: f32, height: f32) -> Self {
        Self {
            width,
            height,
            fences: Vec::new(),
            console: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_fence() -> SceneFence {
        SceneFence {
            x: 10.0,
            y: 20.0,
            width: 100.0,
            height: 80.0,
            title: "测试".into(),
            icons: vec![],
            border_width: 1.0,
            border_color: [1.0, 1.0, 1.0, 0.1],
            fill_color: Some([0.08, 0.08, 0.12, 0.55]),
        }
    }

    #[test]
    fn fence_contains_checks_rectangle() {
        let f = test_fence();
        assert!(f.contains(10.0, 20.0));
        assert!(f.contains(110.0, 100.0)); // 右下角（含边界）
        assert!(!f.contains(9.0, 20.0));
        assert!(!f.contains(10.0, 101.0));
    }

    #[test]
    fn fence_style_fields_are_plumbed() {
        let f = test_fence();
        assert_eq!(f.border_width, 1.0);
        assert!(f.fill_color.is_some());
        // 描边模式：无填充
        let outline = SceneFence {
            fill_color: None,
            border_width: 2.5,
            ..test_fence()
        };
        assert!(outline.fill_color.is_none());
        assert_eq!(outline.border_width, 2.5);
    }

    #[test]
    fn scene_defaults_empty() {
        let s = Scene::new(1920.0, 1080.0);
        assert!(s.fences.is_empty());
        assert!(s.console.is_none());
        assert_eq!(s.width, 1920.0);
    }
}
