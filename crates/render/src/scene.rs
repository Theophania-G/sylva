//! 场景模型：一次绘制所需的全部数据（应用无关）。
//!
//! 坐标全部为**物理像素**（虚拟屏幕坐标），与 overlay 窗口客户端坐标一致。

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
}

impl SceneFence {
    /// 点是否落在栅栏矩形内（命中测试用）。
    pub fn contains(&self, px: f32, py: f32) -> bool {
        px >= self.x && px <= self.x + self.width && py >= self.y && py <= self.y + self.height
    }
}

/// 整屏场景（虚拟屏幕）。
#[derive(Debug, Default)]
pub struct Scene {
    /// 虚拟屏幕尺寸（物理像素）。
    pub width: f32,
    pub height: f32,
    pub fences: Vec<SceneFence>,
}

impl Scene {
    pub fn new(width: f32, height: f32) -> Self {
        Self {
            width,
            height,
            fences: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fence_contains_checks_rectangle() {
        let f = SceneFence {
            x: 10.0,
            y: 20.0,
            width: 100.0,
            height: 80.0,
            title: "测试".into(),
            icons: vec![],
        };
        assert!(f.contains(10.0, 20.0));
        assert!(f.contains(110.0, 100.0)); // 右下角（含边界）
        assert!(!f.contains(9.0, 20.0));
        assert!(!f.contains(10.0, 101.0));
    }

    #[test]
    fn scene_defaults_empty() {
        let s = Scene::new(1920.0, 1080.0);
        assert!(s.fences.is_empty());
        assert_eq!(s.width, 1920.0);
    }
}
