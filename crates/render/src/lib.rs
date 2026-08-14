//! `fence-render`：DirectComposition / Direct2D / DirectWrite 渲染层。
//!
//! - DirectComposition：驱动覆盖层窗口的视觉树，每个栅栏一个 Visual，硬件合成动画
//! - Direct2D：栅栏背景（圆角、亚克力降级）、图标位图
//! - DirectWrite：图标标签文字（DPI 感知）
//!
//! M1 起填充；当前为占位，保证 workspace 可编译。

#![allow(dead_code)]

pub fn placeholder() -> &'static str {
    "render"
}
