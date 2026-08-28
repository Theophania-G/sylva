//! 动画补间：面板展开/栅栏拖动/图标悬停的补间类型与推进（纯状态，无窗口访问）。

use crate::*;

/// 面板展开/折叠补间（`from→to`，`dur` 秒，ease_out_cubic）。
#[derive(Debug, Clone, Copy)]
pub(crate) struct PanelTween {
    pub(crate) t0: Instant,
    pub(crate) dur: f32,
    pub(crate) from: f32,
    pub(crate) to: f32,
}

/// 控制台面板动画状态（App 层驱动，overlay `AnimTick` 定时推进）。
pub(crate) struct ConsoleAnim {
    /// 面板展开进度 0..1（0=完全隐藏，1=完整面板）。已 ease 插值。
    pub(crate) panel: f32,
    pub(crate) panel_tween: Option<PanelTween>,
}

impl ConsoleAnim {
    pub(crate) fn new(open: bool) -> Self {
        Self {
            panel: if open { 1.0 } else { 0.0 },
            panel_tween: None,
        }
    }

    /// 是否有动画在推进（决定动画定时器启停）。
    fn active(&self) -> bool {
        self.panel_tween.is_some()
    }
}

/// 栅栏拖动/缩放补间：视觉矩形从 `from` 追赶到 `to`（模型已落到 `to`，
/// 场景渲染用补间值，形成丝滑跟随；补间结束视觉 = 模型）。
#[derive(Debug, Clone, Copy)]
pub(crate) struct FenceTween {
    pub(crate) fence: usize,
    pub(crate) from: Rect,
    pub(crate) to: Rect,
    pub(crate) t0: Instant,
    pub(crate) dur: f32,
}

/// 图标悬停缩放补间（0..1：1 = 完全放大）。
#[derive(Debug, Clone, Copy)]
pub(crate) struct IconHoverAnim {
    pub(crate) fence: usize,
    pub(crate) icon: usize,
    pub(crate) t0: Instant,
    pub(crate) dur: f32,
    /// 补间起点进度（0/1，或上一次动画的当前值——连续进出不跳变）。
    pub(crate) from: f32,
    /// 补间终点进度（1 = 放大，0 = 收回）。
    pub(crate) to: f32,
}

impl IconHoverAnim {
    pub(crate) fn progress(&self, now: Instant) -> f32 {
        match tween_progress(self.t0, self.dur, now) {
            Some(p) => self.from + (self.to - self.from) * ease_out_cubic(p),
            None => self.to,
        }
    }
}

pub(crate) fn ease_out_cubic(t: f32) -> f32 {
    let u = t.clamp(0.0, 1.0);
    1.0 - (1.0 - u).powi(3)
}

/// ease_out_back：过冲回弹（面板展开/待办行入场用，目标方向先超出一点再收回）。
pub(crate) fn ease_out_back(t: f32) -> f32 {
    let u = t.clamp(0.0, 1.0);
    const C1: f32 = 1.70158;
    const C3: f32 = C1 + 1.0;
    1.0 + C3 * (u - 1.0).powi(3) + C1 * (u - 1.0).powi(2)
}

/// 补间进度：`t0` 起 `dur` 秒内返回 0..1，超时返回 None（补间结束）。
pub(crate) fn tween_progress(t0: Instant, dur: f32, now: Instant) -> Option<f32> {
    let el = now.duration_since(t0).as_secs_f32();
    if el >= dur {
        None
    } else {
        Some(el / dur)
    }
}

/// 启用动画定时器（overlay 每 16ms 触发一次 `AnimTick`）。
pub(crate) fn arm_anim_timer(rt: &mut Runtime) {
    if !rt.console_anim.active()
        && rt.desktop_fade.is_none()
        && rt.fence_tweens.is_empty()
        && !icon_hover_active(rt)
    {
        return;
    }
    unsafe { (*rt.overlay_ptr).set_anim_active(true) };
}

/// 推进一帧动画：面板补间 + 栅栏补间 + 图标悬停。
/// 返回是否仍有动画在推进（否则调用方停用定时器）。
pub(crate) fn advance_anim(rt: &mut Runtime) -> bool {
    let now = Instant::now();
    let anim = &mut rt.console_anim;
    // 桌面切换：栅栏整体淡入/淡出补间（结束即清除）
    if let Some(df) = rt.desktop_fade {
        if tween_progress(df.t0, df.dur, now).is_none() {
            rt.desktop_fade = None;
        }
    }
    // 栅栏拖动/缩放补间：结束即从表里摘除（视觉 = 模型）
    rt.fence_tweens
        .retain(|t| tween_progress(t.t0, t.dur, now).is_some());
    // 面板展开/折叠补间
    if let Some(pt) = anim.panel_tween {
        match tween_progress(pt.t0, pt.dur, now) {
            Some(p) => {
                let e = ease_out_back(p);
                anim.panel = pt.from + (pt.to - pt.from) * e;
            }
            None => {
                anim.panel = pt.to;
                anim.panel_tween = None;
            }
        }
    }
    anim.active()
        || rt.desktop_fade.is_some()
        || !rt.fence_tweens.is_empty()
        || icon_hover_active(rt)
}

/// 开始面板展开/折叠补间（`to` 目标进度）。目标立即生效于命中模型，
/// 视觉高度由补间在 `AnimTick` 逐帧推进。
pub(crate) fn start_panel_tween(rt: &mut Runtime, to: f32) {
    let from = rt.console_anim.panel;
    rt.console_anim.panel_tween = Some(PanelTween {
        t0: Instant::now(),
        dur: 0.24,
        from,
        to,
    });
    arm_anim_timer(rt);
}

/// 栅栏管理页列表最大可滚动量（物理像素；0 = 行数不超出可视区）。
pub(crate) fn fence_alpha(rt: &Runtime, now: Instant) -> f32 {
    if let Some(t) = rt.desktop_fade {
        match tween_progress(t.t0, t.dur, now) {
            Some(p) => t.from + (t.to - t.from) * ease_out_cubic(p),
            None => t.to,
        }
    } else if rt.desk.desktop_mode {
        0.0
    } else {
        1.0
    }
}

/// 栅栏当前视觉矩形：有补间时取插值（拖拽跟随），否则 = 模型矩形。
pub(crate) fn fence_visual_rect(rt: &Runtime, fence: usize) -> Rect {
    let now = Instant::now();
    if let Some(t) = rt.fence_tweens.iter().find(|t| t.fence == fence) {
        match tween_progress(t.t0, t.dur, now) {
            Some(p) => {
                let e = ease_out_cubic(p);
                Rect::new(
                    t.from.x + (t.to.x - t.from.x) * e,
                    t.from.y + (t.to.y - t.from.y) * e,
                    t.from.w + (t.to.w - t.from.w) * e,
                    t.from.h + (t.to.h - t.from.h) * e,
                )
            }
            None => t.to,
        }
    } else {
        rt.desk
            .fences
            .get(fence)
            .map(|f| f.bounds)
            .unwrap_or_default()
    }
}

/// 记录/替换栅栏补间（同一栅栏重复拖动时从当前视觉位置接续，不跳变）并启动动画定时器。
pub(crate) fn set_fence_tween(rt: &mut Runtime, tween: FenceTween) {
    rt.fence_tweens.retain(|t| t.fence != tween.fence);
    rt.fence_tweens.push(tween);
    arm_anim_timer(rt);
}

/// 图标悬停当前缩放（1.0 = 常态，1.3 = 完全放大，约 1.3 倍原图标大小）。
pub(crate) fn icon_hover_scale(rt: &Runtime, fence: usize, icon: usize, now: Instant) -> f32 {
    match rt.icon_hover {
        Some(h) if h.fence == fence && h.icon == icon => 1.0 + 0.3 * h.progress(now),
        _ => 1.0,
    }
}

/// 图标悬停补间是否仍在推进（决定动画定时器是否需要跑）。
pub(crate) fn icon_hover_active(rt: &Runtime) -> bool {
    rt.icon_hover
        .map(|h| tween_progress(h.t0, h.dur, Instant::now()).is_some())
        .unwrap_or(false)
}

