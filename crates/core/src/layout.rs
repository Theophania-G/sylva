//! 栅栏内网格布局计算。纯函数，可单测。

use crate::model::{Rect, Vec2};

/// 网格布局参数。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GridLayout {
    pub icon_size: f32,
    pub gap: f32,
    pub padding: f32,
}

impl Default for GridLayout {
    fn default() -> Self {
        Self {
            icon_size: 48.0,
            gap: 10.0,
            padding: 12.0,
        }
    }
}

impl GridLayout {
    /// 计算指定内容宽度下能容纳的列数（至少 1 列）。
    pub fn cols_for_width(&self, inner_w: f32) -> usize {
        if inner_w <= self.icon_size {
            return 1;
        }
        ((inner_w + self.gap) / (self.icon_size + self.gap)).floor() as usize
    }

    /// 图标占满指定列数所需的行数。
    pub fn rows_for_count(&self, count: usize, cols: usize) -> usize {
        if count == 0 {
            0
        } else {
            count.div_ceil(cols.max(1))
        }
    }

    /// 为 `count` 个图标排布网格，返回各图标**中心点**（基于 `area` 内边距）。
    ///
    /// 从左到右、从上到下填充；行随内容宽度自适应。
    pub fn arrange(&self, count: usize, area: &Rect) -> Vec<Vec2> {
        let inner = area.inset(self.padding);
        let cols = self.cols_for_width(inner.w).max(1);
        let step = self.icon_size + self.gap;

        let mut out = Vec::with_capacity(count);
        for i in 0..count {
            let row = (i / cols) as f32;
            let col = (i % cols) as f32;
            let x = inner.x + col * step + self.icon_size / 2.0;
            let y = inner.y + row * step + self.icon_size / 2.0;
            out.push(Vec2 { x, y });
        }
        out
    }

    /// 内容占用的最小高度（供折叠动画推算目标高度）。
    pub fn content_height(&self, count: usize, cols: usize) -> f32 {
        let rows = self.rows_for_count(count, cols);
        if rows == 0 {
            0.0
        } else {
            rows as f32 * self.icon_size + (rows - 1) as f32 * self.gap
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn g() -> GridLayout {
        GridLayout {
            icon_size: 48.0,
            gap: 10.0,
            padding: 10.0,
        }
    }

    #[test]
    fn arrange_fills_left_to_right_top_to_bottom() {
        let area = Rect::new(0.0, 0.0, 200.0, 200.0);
        // 内宽 = 180，列数 = floor((180+10)/58) = 3
        let pos = g().arrange(4, &area);
        assert_eq!(pos.len(), 4);
        // 第一行: x = 10 + 0*58 + 24 = 34 ; 第二行: y = 10 + 1*58 + 24 = 92
        assert_eq!(pos[0], Vec2 { x: 34.0, y: 34.0 });
        assert_eq!(pos[1], Vec2 { x: 92.0, y: 34.0 });
        assert_eq!(pos[2], Vec2 { x: 150.0, y: 34.0 });
        assert_eq!(pos[3], Vec2 { x: 34.0, y: 92.0 });
    }

    #[test]
    fn cols_never_zero() {
        assert_eq!(g().cols_for_width(5.0), 1);
        assert_eq!(g().cols_for_width(48.0), 1);
    }

    #[test]
    fn content_height_matches_rows() {
        assert_eq!(g().content_height(0, 3), 0.0);
        assert_eq!(g().content_height(1, 3), 48.0);
        assert_eq!(g().content_height(4, 3), 48.0 * 2.0 + 10.0);
    }
}
