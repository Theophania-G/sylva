//! 动画状态机与缓动函数。纯计算，可单测。
//!
//! 渲染层只负责把每个动画的当前值应用到合成器 Visual，
//! 动画的推进与缓动完全在本模块完成，保证可测试、可回放。

/// 缓动类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ease {
    Linear,
    QuadInOut,
    CubicOut,
    /// 过冲回弹（easeOutBack）：目标方向超出一点再收回到终点。
    BackOut,
    /// 阻尼弹簧：快速起步、轻微震荡、自然收敛（比 BackOut 更有“物理感”）。
    Spring,
}

/// 把归一化进度 `t`（0..=1）映射为缓动后的进度。
pub fn ease(t: f32, kind: Ease) -> f32 {
    let t = t.clamp(0.0, 1.0);
    match kind {
        Ease::Linear => t,
        Ease::QuadInOut => {
            if t < 0.5 {
                2.0 * t * t
            } else {
                1.0 - (-2.0 * t + 2.0).powi(2) / 2.0
            }
        }
        Ease::CubicOut => 1.0 - (1.0 - t).powi(3),
        Ease::BackOut => {
            const C1: f32 = 1.70158;
            const C3: f32 = C1 + 1.0;
            1.0 + C3 * (t - 1.0).powi(3) + C1 * (t - 1.0).powi(2)
        }
        Ease::Spring => {
            // 归一化阻尼弹簧：t=0 → 0，轻微过冲后收敛到 1（终值精确钳到 1）。
            let v = 1.0 - (-5.0 * t).exp() * (7.0 * t).cos();
            v.clamp(0.0, 1.08)
        }
    }
}

/// 单个标量的补间动画。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Tween {
    pub start: f32,
    pub end: f32,
    /// 时长（秒）。
    pub duration: f32,
    pub elapsed: f32,
    pub ease: Ease,
}

impl Tween {
    pub fn new(start: f32, end: f32, duration: f32) -> Self {
        Self {
            start,
            end,
            duration,
            elapsed: 0.0,
            ease: Ease::CubicOut,
        }
    }

    pub fn with_ease(mut self, ease: Ease) -> Self {
        self.ease = ease;
        self
    }

    /// 归一化进度（0..=1）。
    pub fn progress(&self) -> f32 {
        if self.duration <= 0.0 {
            1.0
        } else {
            (self.elapsed / self.duration).clamp(0.0, 1.0)
        }
    }

    /// 当前值（缓动后）。
    pub fn value(&self) -> f32 {
        self.start + (self.end - self.start) * ease(self.progress(), self.ease)
    }

    pub fn done(&self) -> bool {
        self.elapsed >= self.duration
    }

    /// 推进 `dt` 秒，返回当前值。
    pub fn update(&mut self, dt: f32) -> f32 {
        self.elapsed += dt;
        self.value()
    }
}

/// 二维向量的补间（用于栅栏折叠的高度动画、磁吸的位移动画）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Vec2Tween {
    pub start: crate::model::Vec2,
    pub end: crate::model::Vec2,
    pub tween: Tween,
}

impl Vec2Tween {
    pub fn new(start: crate::model::Vec2, end: crate::model::Vec2, duration: f32) -> Self {
        Self {
            start,
            end,
            tween: Tween::new(0.0, 1.0, duration),
        }
    }

    pub fn value(&self) -> crate::model::Vec2 {
        let t = ease(self.tween.progress(), self.tween.ease);
        crate::model::Vec2 {
            x: self.start.x + (self.end.x - self.start.x) * t,
            y: self.start.y + (self.end.y - self.start.y) * t,
        }
    }

    pub fn update(&mut self, dt: f32) -> crate::model::Vec2 {
        self.tween.update(dt);
        self.value()
    }

    pub fn done(&self) -> bool {
        self.tween.done()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ease_maps_endpoints() {
        assert_eq!(ease(0.0, Ease::Linear), 0.0);
        assert_eq!(ease(1.0, Ease::Linear), 1.0);
        assert_eq!(ease(0.0, Ease::CubicOut), 0.0);
        assert_eq!(ease(1.0, Ease::CubicOut), 1.0);
        assert_eq!(ease(0.0, Ease::BackOut), 0.0);
        assert!((ease(1.0, Ease::BackOut) - 1.0).abs() < 1e-5);
        assert_eq!(ease(0.0, Ease::Spring), 0.0);
        // 弹簧中段应有轻微过冲（>1），体现“丝滑回弹”
        assert!(ease(0.4, Ease::Spring) > 1.0);
        // 阻尼弹簧终值收敛到 1（允许小幅残差，视觉上已稳定）
        assert!((ease(1.0, Ease::Spring) - 1.0).abs() < 1e-2);
    }

    #[test]
    fn tween_reaches_end() {
        let mut t = Tween::new(0.0, 100.0, 1.0);
        for _ in 0..10 {
            t.update(0.2);
        }
        assert!(t.done());
        assert!((t.value() - 100.0).abs() < f32::EPSILON);
    }

    #[test]
    fn tween_zero_duration_immediately_done() {
        let mut t = Tween::new(5.0, 10.0, 0.0);
        assert_eq!(t.update(0.0), 10.0);
        assert!(t.done());
    }

    #[test]
    fn vec2_tween_interpolates() {
        let t = Vec2Tween::new(
            crate::model::Vec2 { x: 0.0, y: 0.0 },
            crate::model::Vec2 { x: 10.0, y: 20.0 },
            1.0,
        );
        assert_eq!(t.value(), crate::model::Vec2 { x: 0.0, y: 0.0 });
    }
}
