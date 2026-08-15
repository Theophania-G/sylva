//! 栅栏间距算法。纯数学，可单测。
//!
//! 目标（按用户要求重构，替代旧版「磁吸吸附」）：
//! 1. **栅栏之间永不重叠**（硬约束）；
//! 2. **不同栅栏的边之间默认保持最小间距**（`FENCE_GAP`）；
//! 3. 不再吸附对齐——旧版会因「边与边距离小于阈值」把上边吸到其它栅栏的上边，
//!    造成奇怪的贴边/重叠行为，新版一律只保证「间隙」，不做任何对齐。
//!
//! 移动：把栅栏沿「侵入最小的轴向」推离所有其它栅栏，直到每条边与其他栅栏
//! 的空隙 ≥ `gap`；随后夹进屏幕。缩放：只约束可动边，锚定边不动。

use crate::model::Rect;

/// 不同栅栏边之间的最小间距（逻辑 px，与 DPI 无关）。
///
/// 拖动/缩放栅栏时强制保持；屏幕上放不下（两个栅栏中间只有不足 gap 的空间）时
/// 屏幕边界优先，间距让位于屏幕——间距是默认要求，不是越狱级硬约束。
pub const FENCE_GAP: f32 = 12.0;

/// 缩放时可动的边。锚定边不动，可动边会被推挤 / 钳制。
/// 与渲染层 `ResizeZone` 一一对应（拖动整体用 `Move`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FreeSides {
    Move,
    Right,
    Bottom,
    BottomRight,
    Left,
    BottomLeft,
    TopRight,
}

impl FreeSides {
    pub fn left_free(&self) -> bool {
        matches!(self, Self::Move | Self::Left | Self::BottomLeft)
    }
    pub fn right_free(&self) -> bool {
        matches!(
            self,
            Self::Move | Self::Right | Self::BottomRight | Self::TopRight
        )
    }
    pub fn top_free(&self) -> bool {
        matches!(self, Self::Move | Self::TopRight)
    }
    pub fn bottom_free(&self) -> bool {
        matches!(
            self,
            Self::Move | Self::Bottom | Self::BottomRight | Self::BottomLeft
        )
    }
}

/// 水平方向的重叠量（0 = 不重叠）。
fn overlap_x(a: &Rect, b: &Rect) -> f32 {
    (a.right().min(b.right()) - a.x.max(b.x)).max(0.0)
}

/// 垂直方向的重叠量（0 = 不重叠）。
fn overlap_y(a: &Rect, b: &Rect) -> f32 {
    (a.bottom().min(b.bottom()) - a.y.max(b.y)).max(0.0)
}

/// 两矩形在水平方向的净空隙：正 = 在水平方向分开这么多；负 = 水平重叠量。
fn clearance_x(a: &Rect, b: &Rect) -> f32 {
    if a.right() <= b.x {
        b.x - a.right()
    } else if b.right() <= a.x {
        a.x - b.right()
    } else {
        -overlap_x(a, b)
    }
}

/// 两矩形在垂直方向的净空隙：正 = 在垂直方向分开这么多；负 = 垂直重叠量。
fn clearance_y(a: &Rect, b: &Rect) -> f32 {
    if a.bottom() <= b.y {
        b.y - a.bottom()
    } else if b.bottom() <= a.y {
        a.y - b.bottom()
    } else {
        -overlap_y(a, b)
    }
}

/// 两矩形是否已按最小间距 `gap` 彼此隔开：任一轴的空隙 ≥ gap 即满足。
fn gap_separated(a: &Rect, b: &Rect, gap: f32) -> bool {
    clearance_x(a, b) >= gap || clearance_y(a, b) >= gap
}

/// 为让水平空隙达到 `gap`，沿 x 需要施加的位移（仅当推离方向明确时返回非 0）。
fn gap_push_x(r: &Rect, o: &Rect, gap: f32) -> f32 {
    if r.right() <= o.x {
        // r 在 o 左边：左移使 r.right() = o.x - gap
        (o.x - gap - r.right()).min(0.0)
    } else if o.right() <= r.x {
        // r 在 o 右边：右移使 r.x = o.right() + gap
        (o.right() + gap - r.x).max(0.0)
    } else {
        // 水平重叠：朝远离 o 中心的方向推出（重叠量 + gap）
        let dir = if (r.x + r.w / 2.0) < (o.x + o.w / 2.0) {
            -1.0
        } else {
            1.0
        };
        dir * (overlap_x(r, o) + gap)
    }
}

/// 为让垂直空隙达到 `gap`，沿 y 需要施加的位移。
fn gap_push_y(r: &Rect, o: &Rect, gap: f32) -> f32 {
    if r.bottom() <= o.y {
        (o.y - gap - r.bottom()).min(0.0)
    } else if o.bottom() <= r.y {
        (o.bottom() + gap - r.y).max(0.0)
    } else {
        let dir = if (r.y + r.h / 2.0) < (o.y + o.h / 2.0) {
            -1.0
        } else {
            1.0
        };
        dir * (overlap_y(r, o) + gap)
    }
}

/// 沿侵入最小的轴向把 `r` 推离 `o`，使与 `o` 的空隙达到 `gap`。
fn push_away_gap(r: &mut Rect, o: &Rect, gap: f32) {
    let dx = gap_push_x(r, o, gap);
    let dy = gap_push_y(r, o, gap);
    if dx.abs() <= dy.abs() {
        r.x += dx;
    } else {
        r.y += dy;
    }
}

/// 移动时的间距强制：迭代把 `r` 推出所有未隔开的栅栏，最多 4 轮（覆盖多个栅栏同时挤压）。
fn resolve_gap_move(r: &mut Rect, others: &[Rect], gap: f32) {
    for _ in 0..4 {
        let mut pushed = false;
        for o in others {
            if gap_separated(r, o, gap) {
                continue;
            }
            push_away_gap(r, o, gap);
            pushed = true;
        }
        if !pushed {
            break;
        }
    }
}

fn clamp_screen(r: &mut Rect, screen: &Rect) {
    r.x = r.x.clamp(screen.x, (screen.right() - r.w).max(screen.x));
    r.y = r.y.clamp(screen.y, (screen.bottom() - r.h).max(screen.y));
}

/// 可动右缘的上限（right 不得超过此值才能与 `o` 保持 gap；`o` 帮不上忙时返回 +∞）。
fn right_limit(r: &Rect, o: &Rect, gap: f32) -> f32 {
    if gap_separated(r, o, gap) {
        return f32::INFINITY;
    }
    // 仅当 o 的左缘位于 r 左缘右侧（o 在 r 的右半侧挡路）时，右缘收缩才能避开
    if o.right() > r.x {
        (o.x - gap).max(r.x)
    } else {
        f32::INFINITY
    }
}

/// 可动左缘的下限（x 不得小于此值才能与 `o` 保持 gap；帮不上忙时返回 -∞）。
fn left_limit(r: &Rect, o: &Rect, gap: f32) -> f32 {
    if gap_separated(r, o, gap) {
        return f32::NEG_INFINITY;
    }
    if o.x < r.right() {
        (o.right() + gap).min(r.right())
    } else {
        f32::NEG_INFINITY
    }
}

/// 可动下缘的上限（bottom 不得超过此值才能与 `o` 保持 gap；帮不上忙时返回 +∞）。
fn bottom_limit(r: &Rect, o: &Rect, gap: f32) -> f32 {
    if gap_separated(r, o, gap) {
        return f32::INFINITY;
    }
    if o.bottom() > r.y {
        (o.y - gap).max(r.y)
    } else {
        f32::INFINITY
    }
}

/// 可动上缘的下限（y 不得小于此值才能与 `o` 保持 gap；帮不上忙时返回 -∞）。
fn top_limit(r: &Rect, o: &Rect, gap: f32) -> f32 {
    if gap_separated(r, o, gap) {
        return f32::NEG_INFINITY;
    }
    if o.y < r.bottom() {
        (o.bottom() + gap).min(r.bottom())
    } else {
        f32::NEG_INFINITY
    }
}

/// 可动边种类（`resolve_gap_resize` 贪心选最小侵入时用）。
#[derive(Debug, Clone, Copy, PartialEq)]
enum Edge {
    Left,
    Right,
    Top,
    Bottom,
}

/// 缩放时的间距强制：只约束可动边，锚定边保持不动。
///
/// 贪心「最小侵入」：每轮对所有可动边算出「为达到 gap 需移动的位移」，只夹最小的
/// 那个，再重算——避免固定顺序（如先右后下）导致角缩放时把本不该动的边也夹了。
/// 结果等价于「锚定角不变时面积最大的合法矩形」：下缘先撞上的先停，另一条边继续。
fn resolve_gap_resize(r: &mut Rect, others: &[Rect], gap: f32, free: FreeSides) {
    for _ in 0..4 {
        let mut best: Option<(f32, Edge)> = None;
        for o in others {
            if gap_separated(r, o, gap) {
                continue;
            }
            // 各可动边为达到 gap 需收缩的位移（正 = 需要收缩这么多）
            if free.right_free() {
                let d = r.right() - right_limit(r, o, gap);
                if d > 0.0 && best.is_none_or(|(bd, _)| d < bd) {
                    best = Some((d, Edge::Right));
                }
            }
            if free.bottom_free() {
                let d = r.bottom() - bottom_limit(r, o, gap);
                if d > 0.0 && best.is_none_or(|(bd, _)| d < bd) {
                    best = Some((d, Edge::Bottom));
                }
            }
            if free.left_free() {
                let d = left_limit(r, o, gap) - r.x;
                if d > 0.0 && best.is_none_or(|(bd, _)| d < bd) {
                    best = Some((d, Edge::Left));
                }
            }
            if free.top_free() {
                let d = top_limit(r, o, gap) - r.y;
                if d > 0.0 && best.is_none_or(|(bd, _)| d < bd) {
                    best = Some((d, Edge::Top));
                }
            }
        }
        match best {
            Some((d, Edge::Right)) => r.w = (r.w - d).max(0.0),
            Some((d, Edge::Bottom)) => r.h = (r.h - d).max(0.0),
            Some((d, Edge::Left)) => {
                r.x += d;
                r.w = (r.w - d).max(0.0);
            }
            Some((d, Edge::Top)) => {
                r.y += d;
                r.h = (r.h - d).max(0.0);
            }
            None => break,
        }
    }
}

fn apply_min(r: &mut Rect, free: FreeSides, min_w: f32, min_h: f32) {
    if free.right_free() && r.w < min_w {
        r.w = min_w;
    }
    if free.left_free() && r.w < min_w {
        r.x -= min_w - r.w;
        r.w = min_w;
    }
    if free.bottom_free() && r.h < min_h {
        r.h = min_h;
    }
    if free.top_free() && r.h < min_h {
        r.y -= min_h - r.h;
        r.h = min_h;
    }
}

fn clamp_resize(r: &mut Rect, screen: &Rect, free: FreeSides) {
    if free.left_free() {
        r.x = r.x.max(screen.x);
    }
    if free.right_free() {
        let m = screen.right() - r.x;
        if r.w > m {
            r.w = m.max(0.0);
        }
    }
    if free.top_free() {
        r.y = r.y.max(screen.y);
    }
    if free.bottom_free() {
        let m = screen.bottom() - r.y;
        if r.h > m {
            r.h = m.max(0.0);
        }
    }
}

/// 拖动后的最终矩形：与所有栅栏保持最小间距（永不重叠）→ 夹进屏幕 → 再校验一次 → 再夹屏。
pub fn settle_move(bounds: &Rect, others: &[Rect], screen: &Rect, gap: f32) -> Rect {
    let mut r = *bounds;
    resolve_gap_move(&mut r, others, gap);
    clamp_screen(&mut r, screen);
    // 夹屏可能把 r 重新贴到其它栅栏（贴近屏幕边缘时），再推一次；最后仍以屏幕为界。
    resolve_gap_move(&mut r, others, gap);
    clamp_screen(&mut r, screen);
    r
}

/// 缩放后的最终矩形：可动边不与其它栅栏重叠且保持最小间距 → 最小尺寸 → 屏幕内 → 再校验。
pub fn settle_resize(
    bounds: &Rect,
    others: &[Rect],
    screen: &Rect,
    free: FreeSides,
    min_w: f32,
    min_h: f32,
    gap: f32,
) -> Rect {
    let mut r = *bounds;
    resolve_gap_resize(&mut r, others, gap, free);
    apply_min(&mut r, free, min_w, min_h);
    clamp_resize(&mut r, screen, free);
    // 取最小尺寸 / 夹屏可能让可动边重新贴近或越过其它栅栏 → 再约束一次可动边
    //（间距/不重叠为硬约束，窄空间里最小尺寸让位于间距）。
    resolve_gap_resize(&mut r, others, gap, free);
    clamp_resize(&mut r, screen, free);
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gap() -> f32 {
        FENCE_GAP
    }

    #[test]
    fn move_enforces_min_gap_with_neighbor() {
        let screen = Rect::new(0.0, 0.0, 1920.0, 1080.0);
        // 目标位置压进邻居 100px → 沿侵入最小的轴向（水平）推出，保持至少 gap
        let b = Rect::new(400.0, 200.0, 200.0, 100.0);
        let neighbor = Rect::new(500.0, 200.0, 200.0, 100.0);
        let out = settle_move(&b, &[neighbor], &screen, gap());
        assert!(clearance_x(&out, &neighbor) >= gap());
        assert!(overlap_x(&out, &neighbor) == 0.0);
        assert_eq!(out.y, b.y); // 推出沿水平轴，y 不变
    }

    #[test]
    fn move_untouched_when_already_separated() {
        let screen = Rect::new(0.0, 0.0, 1920.0, 1080.0);
        let b = Rect::new(100.0, 100.0, 200.0, 100.0);
        let neighbor = Rect::new(500.0, 200.0, 200.0, 100.0); // 水平分开 200 > gap
        let out = settle_move(&b, &[neighbor], &screen, gap());
        assert_eq!(out, b);
    }

    #[test]
    fn move_clears_multiple_neighbors() {
        let screen = Rect::new(0.0, 0.0, 1920.0, 1080.0);
        let g = gap();
        // 右下方两个长条把 r 夹住，任何单次推出都会撞到另一个 → 迭代后全部保持 gap
        let b = Rect::new(400.0, 400.0, 100.0, 100.0);
        let n1 = Rect::new(450.0, 300.0, 100.0, 250.0); // 右侧长条
        let n2 = Rect::new(300.0, 450.0, 250.0, 100.0); // 下方长条
        let out = settle_move(&b, &[n1, n2], &screen, g);
        assert!(gap_separated(&out, &n1, g));
        assert!(gap_separated(&out, &n2, g));
    }

    #[test]
    fn move_no_longer_aligns_edges_without_gap_contact() {
        // 回归：旧版会把 r 的上边吸附到邻居的上边（dy=-10 对齐）；
        // 新版只保证「间隙」，不吸附对齐——已隔开的栅栏必须原样不动。
        let screen = Rect::new(0.0, 0.0, 1920.0, 1080.0);
        let b = Rect::new(600.0, 110.0, 200.0, 100.0); // 上边比邻居低 10px
        let neighbor = Rect::new(300.0, 100.0, 200.0, 100.0); // 水平分开 100 > gap
        let out = settle_move(&b, &[neighbor], &screen, gap());
        assert_eq!(out, b);
    }

    #[test]
    fn move_clamps_to_screen() {
        let screen = Rect::new(0.0, 0.0, 1920.0, 1080.0);
        let b = Rect::new(-50.0, -30.0, 200.0, 100.0);
        let out = settle_move(&b, &[], &screen, gap());
        assert_eq!(out.x, 0.0);
        assert_eq!(out.y, 0.0);
    }

    #[test]
    fn resize_right_edge_stops_at_gap_from_neighbor() {
        let screen = Rect::new(0.0, 0.0, 1920.0, 1080.0);
        let g = gap();
        // 右缘想伸到 700，邻居在 500..700 → 右缘停在 500 - gap，左缘锚定
        let b = Rect::new(300.0, 200.0, 400.0, 100.0);
        let neighbor = Rect::new(500.0, 200.0, 200.0, 100.0);
        let out = settle_resize(&b, &[neighbor], &screen, FreeSides::Right, 80.0, 60.0, g);
        assert_eq!(out.right(), 500.0 - g);
        assert_eq!(out.x, 300.0);
        assert!(overlap_x(&out, &neighbor) == 0.0);
    }

    #[test]
    fn resize_left_edge_keeps_right_anchored() {
        let screen = Rect::new(0.0, 0.0, 1920.0, 1080.0);
        let g = gap();
        // 左缘拖进邻居(200..300) 范围内 → 左缘停在 300 + gap，右缘锚定
        let b = Rect::new(250.0, 200.0, 200.0, 100.0); // 右缘 = 450
        let neighbor = Rect::new(200.0, 200.0, 100.0, 100.0);
        let out = settle_resize(&b, &[neighbor], &screen, FreeSides::Left, 80.0, 60.0, g);
        assert_eq!(out.x, 300.0 + g);
        assert_eq!(out.right(), 450.0);
    }

    #[test]
    fn resize_corner_stops_min_intrusion_edge_first() {
        let screen = Rect::new(0.0, 0.0, 1920.0, 1080.0);
        let g = gap();
        // 右下角缩放，右下两缘都伸进邻居 → 贪心只夹「侵入最小」的边，其余边保持提案位置，左上锚定。
        // 布局 A：下缘侵入更小（212 < 412）→ 下缘先停到邻居上缘 - gap。
        let a = Rect::new(300.0, 200.0, 500.0, 300.0); // right=800, bottom=500
        let oa = Rect::new(400.0, 300.0, 500.0, 400.0); // 400..900, 300..700
        let out_a = settle_resize(&a, &[oa], &screen, FreeSides::BottomRight, 80.0, 60.0, g);
        assert!(gap_separated(&out_a, &oa, g));
        assert_eq!(out_a.x, 300.0);
        assert_eq!(out_a.y, 200.0);
        assert_eq!(out_a.bottom(), 300.0 - g); // 下缘停在邻居上缘 - gap
        assert_eq!(out_a.right(), 800.0); // 夹完已隔开，右缘保持提案位置

        // 布局 B：右缘侵入更小（62 < 212）→ 右缘先停到邻居左缘 - gap。
        let b = Rect::new(300.0, 200.0, 500.0, 300.0); // right=800, bottom=500
        let ob = Rect::new(750.0, 300.0, 100.0, 200.0); // 750..850, 300..500
        let out_b = settle_resize(&b, &[ob], &screen, FreeSides::BottomRight, 80.0, 60.0, g);
        assert!(gap_separated(&out_b, &ob, g));
        assert_eq!(out_b.x, 300.0);
        assert_eq!(out_b.y, 200.0);
        assert_eq!(out_b.right(), 750.0 - g); // 右缘停在邻居左缘 - gap
        assert_eq!(out_b.bottom(), 500.0); // 夹完已隔开，下缘保持提案位置
    }

    #[test]
    fn resize_respects_min_size_and_screen() {
        let screen = Rect::new(0.0, 0.0, 1920.0, 1080.0);
        let g = gap();
        // 缩到比最小还小 → 取最小尺寸
        let b = Rect::new(100.0, 100.0, 30.0, 30.0);
        let out = settle_resize(&b, &[], &screen, FreeSides::BottomRight, 80.0, 60.0, g);
        assert!(out.w >= 80.0);
        assert!(out.h >= 60.0);
        // 超屏幕底部/右侧 → 钳制
        let b2 = Rect::new(1900.0, 1050.0, 400.0, 400.0);
        let out2 = settle_resize(&b2, &[], &screen, FreeSides::BottomRight, 80.0, 60.0, g);
        assert!(out2.right() <= 1920.0);
        assert!(out2.bottom() <= 1080.0);
    }

    #[test]
    fn free_sides_flags_are_consistent() {
        assert!(FreeSides::Move.left_free() && FreeSides::Move.right_free());
        assert!(FreeSides::Move.top_free() && FreeSides::Move.bottom_free());
        assert!(FreeSides::Right.right_free() && !FreeSides::Right.left_free());
        assert!(FreeSides::Left.left_free() && !FreeSides::Left.right_free());
        assert!(FreeSides::BottomLeft.left_free() && FreeSides::BottomLeft.bottom_free());
        assert!(!FreeSides::BottomLeft.top_free() && !FreeSides::BottomLeft.right_free());
    }
}
