//! 磁吸对齐算法。纯数学，可单测。
//!
//! 拖动栅栏时，若其某条边与屏幕边缘或其它栅栏边距离小于阈值，
//! 则把该边吸附过去。返回需要施加的位移，由调用方决定如何应用
//! （配合动画做平滑吸附）。

use crate::model::Rect;

/// 磁吸阈值（逻辑 px）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MagnetConfig {
    pub threshold: f32,
}

impl Default for MagnetConfig {
    fn default() -> Self {
        Self { threshold: 24.0 }
    }
}

/// 需要施加的位移量。
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct SnapDelta {
    pub dx: f32,
    pub dy: f32,
}

/// 计算 `bounds` 对 `screen` 与 `anchors` 的吸附位移。
///
/// 规则：只取绝对值最小且落在阈值内的那个候选位移。
/// `anchors` 通常为其它栅栏的边界矩形。
pub fn snap_to(bounds: &Rect, screen: &Rect, anchors: &[Rect], cfg: &MagnetConfig) -> SnapDelta {
    let t = cfg.threshold;

    // 用 Option 表达「尚未吸附」状态，避免把 0 误当作已吸附的结果。
    let mut dx: Option<f32> = None;
    let mut dy: Option<f32> = None;

    let consider = |current: &mut Option<f32>, candidate: f32| {
        if candidate.abs() <= t && current.is_none_or(|c| candidate.abs() < c.abs()) {
            *current = Some(candidate);
        }
    };

    // 屏幕四边
    consider(&mut dx, screen.x - bounds.x);
    consider(&mut dx, screen.right() - bounds.right());
    consider(&mut dy, screen.y - bounds.y);
    consider(&mut dy, screen.bottom() - bounds.bottom());

    // 其它栅栏的四条边
    for a in anchors {
        consider(&mut dx, a.x - bounds.x);
        consider(&mut dx, a.right() - bounds.right());
        consider(&mut dy, a.y - bounds.y);
        consider(&mut dy, a.bottom() - bounds.bottom());
    }

    SnapDelta {
        dx: dx.unwrap_or(0.0),
        dy: dy.unwrap_or(0.0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> MagnetConfig {
        MagnetConfig { threshold: 24.0 }
    }

    #[test]
    fn snaps_to_screen_edge() {
        let screen = Rect::new(0.0, 0.0, 1920.0, 1080.0);
        let b = Rect::new(10.0, 100.0, 200.0, 100.0);
        let d = snap_to(&b, &screen, &[], &cfg());
        // 左边缘差 10px < 24 -> dx = -10 吸附到 0
        assert_eq!(d.dx, -10.0);
        assert_eq!(d.dy, 0.0);
    }

    #[test]
    fn snaps_to_right_edge() {
        let screen = Rect::new(0.0, 0.0, 1920.0, 1080.0);
        let b = Rect::new(1700.0, 100.0, 200.0, 100.0);
        // 右边缘差 = 1920 - 1900 = 20 < 24
        let d = snap_to(&b, &screen, &[], &cfg());
        assert_eq!(d.dx, 20.0);
    }

    #[test]
    fn ignores_when_outside_threshold() {
        let screen = Rect::new(0.0, 0.0, 1920.0, 1080.0);
        let b = Rect::new(100.0, 100.0, 200.0, 100.0);
        let d = snap_to(&b, &screen, &[], &cfg());
        assert_eq!(d, SnapDelta { dx: 0.0, dy: 0.0 });
    }

    #[test]
    fn prefers_nearest_anchor() {
        let screen = Rect::new(0.0, 0.0, 1920.0, 1080.0);
        let b = Rect::new(500.0, 100.0, 200.0, 100.0);
        let anchors = [Rect::new(510.0, 100.0, 200.0, 100.0)];
        // 本栅栏左边缘(500) 与锚点左边缘(510) 差 10px，向右吸附 +10
        let d = snap_to(&b, &screen, &anchors, &cfg());
        assert_eq!(d.dx, 10.0);
    }
}
