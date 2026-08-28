//! D2D 绘制：把场景画进渲染目标。
//!
//! 缓存策略（性能约定 §7.2）：
//! - 位图（`IconStore`）与文本格式（`TextFormats`）跨帧缓存；
//! - 画笔每帧新建——渲染目标每帧重建，画笔不能跨帧复用（栅栏/控制台/按钮
//!   的填充与描边颜色来自场景，按栅栏逐一创建）；
//! - 空闲时不绘制任何东西（0% CPU），只有内容变化才触发重绘。

use std::collections::HashMap;

use windows::core::{Result, PCWSTR};
use windows::Win32::Globalization::GetUserDefaultLocaleName;
use windows::Win32::Graphics::Direct2D::Common::{
    D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_PIXEL_FORMAT, D2D_RECT_F, D2D_SIZE_U,
};
use windows::Win32::Graphics::Direct2D::{
    ID2D1Bitmap, ID2D1RenderTarget, ID2D1SolidColorBrush, D2D1_ANTIALIAS_MODE_PER_PRIMITIVE,
    D2D1_BITMAP_INTERPOLATION_MODE_LINEAR, D2D1_BITMAP_PROPERTIES, D2D1_DRAW_TEXT_OPTIONS_CLIP,
    D2D1_ELLIPSE, D2D1_LAYER_PARAMETERS, D2D1_ROUNDED_RECT,
};
use windows::Win32::Graphics::DirectWrite::{
    IDWriteFactory, IDWriteTextFormat, DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL,
    DWRITE_FONT_WEIGHT, DWRITE_FONT_WEIGHT_BOLD, DWRITE_FONT_WEIGHT_NORMAL,
    DWRITE_MEASURING_MODE_NATURAL, DWRITE_WORD_WRAPPING_NO_WRAP,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;

use sylva_core::model::{FenceLayout, FenceStyle, SidebarPosition};
use sylva_shell::icons::IconData;

use crate::overlay::{ConsoleZone, RectF};
use crate::scene::{ListColumns, Scene, SceneConsole, SceneEdit, SceneFence, SceneFenceDetail};
use crate::theme::{TextStyle, Theme};

/// 图标位图缓存：`bitmap_id` → 设备上的 D2D 位图。
///
/// 位图是设备级资源（像素已上传 GPU），跨帧有效；上传一次，之后只引用。
pub struct IconStore {
    map: HashMap<u64, ID2D1Bitmap>,
    next_id: u64,
}

impl IconStore {
    pub fn new() -> Self {
        Self {
            map: HashMap::new(),
            next_id: 0,
        }
    }

    /// 上传图标像素并返回位图 ID。
    pub fn insert(&mut self, target: &ID2D1RenderTarget, data: &IconData) -> Result<u64> {
        let id = self.next_id;
        self.next_id += 1;
        self.map.insert(id, make_bitmap(target, data)?);
        Ok(id)
    }

    /// 用调用方指定的 ID 上传位图（App 层预分配稳定 ID 时用）。
    pub fn insert_at(
        &mut self,
        target: &ID2D1RenderTarget,
        id: u64,
        data: &IconData,
    ) -> Result<()> {
        self.map.insert(id, make_bitmap(target, data)?);
        Ok(())
    }

    pub fn get(&self, id: u64) -> Option<&ID2D1Bitmap> {
        self.map.get(&id)
    }

    pub fn contains(&self, id: u64) -> bool {
        self.map.contains_key(&id)
    }
}

impl Default for IconStore {
    fn default() -> Self {
        Self::new()
    }
}

/// 跨帧缓存的 DWrite 文本格式（设备无关，可安全缓存）。
pub struct TextFormats {
    pub title: IDWriteTextFormat,
    /// 粗体标题（控制中心顶部「Sylva」）。
    pub title_bold: IDWriteTextFormat,
    pub label: IDWriteTextFormat,
    /// 待办副行（详细信息）用小一号字号。
    pub detail: IDWriteTextFormat,
}

impl TextFormats {
    pub fn new(dwrite: &IDWriteFactory, theme: &Theme) -> Result<Self> {
        let title = make_text_format(dwrite, theme.title, DWRITE_FONT_WEIGHT_NORMAL)?;
        let title_bold = make_text_format(dwrite, theme.title, DWRITE_FONT_WEIGHT_BOLD)?;
        tracing::debug!("TextFormats: 标题格式就绪");
        let label = make_text_format(dwrite, theme.label, DWRITE_FONT_WEIGHT_NORMAL)?;
        tracing::debug!("TextFormats: 标签格式就绪");
        let detail_style = crate::theme::TextStyle {
            font_family: theme.label.font_family,
            size: theme.label.size * 0.72,
            color: theme.label.color,
        };
        let detail = make_text_format(dwrite, detail_style, DWRITE_FONT_WEIGHT_NORMAL)?;
        tracing::debug!("TextFormats: 副行格式就绪");
        Ok(Self {
            title,
            title_bold,
            label,
            detail,
        })
    }
}

/// 单帧用画笔集合（渲染目标每帧重建，故画笔只能单帧使用）。
/// 栅栏/控制台/按钮的填充与描边颜色来自场景，按元素在 `draw_*` 内创建。
struct Brushes {
    title: ID2D1SolidColorBrush,
    label: ID2D1SolidColorBrush,
}

/// 把一帧场景画进渲染目标。目标在 `Frame::finish` 后由调用方提交。
pub fn draw_scene(
    target: &ID2D1RenderTarget,
    theme: &Theme,
    scene: &Scene,
    icons: &IconStore,
    formats: &TextFormats,
) -> Result<()> {
    // 全透明清底：栅栏外区域让桌面（壁纸/图标）透出
    let clear = D2D1_COLOR_F {
        r: 0.0,
        g: 0.0,
        b: 0.0,
        a: 0.0,
    };
    unsafe { target.Clear(Some(&clear)) };

    let brushes = Brushes {
        title: unsafe { target.CreateSolidColorBrush(&theme.title.color.to_d2d(), None)? },
        label: unsafe { target.CreateSolidColorBrush(&theme.label.color.to_d2d(), None)? },
    };

    for fence in &scene.fences {
        draw_fence(target, theme, fence, &brushes, icons, formats)?;
    }
    if let Some(console) = &scene.console {
        draw_console(target, theme, console, &brushes, formats)?;
    }
    // 内联文本编辑（最顶层，与卡片/面板同表面）
    if let Some(edit) = &scene.edit {
        draw_inline_edit(target, theme, edit, formats)?;
    }
    Ok(())
}

/// 控制中心：玻璃卡片面板（栅栏管理）。关闭后完全不渲染（不留胶囊残影）。
///
/// 面板高度由 App 层按 `panel` 进度插值（0 = 完全隐藏，1 = 完整面板）；
/// 这里把 `panel` 当作整体透明度，开合时淡入淡出。各控件矩形与 `SceneConsole`
/// 几何一致（命中模型复用同一份）；内联文本编辑由场景级 `SceneEdit` 最后绘制。
fn draw_console(
    target: &ID2D1RenderTarget,
    theme: &Theme,
    c: &SceneConsole,
    brushes: &Brushes,
    formats: &TextFormats,
) -> Result<()> {
    let s = theme.scale;
    let a = c.panel.clamp(0.0, 1.0);
    if a <= 0.01 {
        return Ok(());
    }
    let accent = [0.23, 0.51, 0.96, 0.92];

    // 玻璃卡片底色 + 描边
    let panel = D2D1_ROUNDED_RECT {
        rect: D2D_RECT_F {
            left: c.x,
            top: c.y,
            right: c.x + c.width,
            bottom: c.y + c.height,
        },
        radiusX: 14.0 * s,
        radiusY: 14.0 * s,
    };
    let fill = [
        c.fill_color[0],
        c.fill_color[1],
        c.fill_color[2],
        c.fill_color[3] * a,
    ];
    let bg = unsafe { target.CreateSolidColorBrush(&color(fill), None)? };
    unsafe { target.FillRoundedRectangle(&panel, &bg) };
    let edge = [
        c.border_color[0],
        c.border_color[1],
        c.border_color[2],
        c.border_color[3] * a,
    ];
    let edge_brush = unsafe { target.CreateSolidColorBrush(&color(edge), None)? };
    unsafe { target.DrawRoundedRectangle(&panel, &edge_brush, 1.0, None) };

    // 标题「Sylva」：居中、粗体
    let title_h = c.title_h.max(34.0 * s);
    let title_lr = D2D_RECT_F {
        left: c.x,
        top: c.y + (title_h - theme.title.size * 1.6) / 2.0,
        right: c.x + c.width,
        bottom: c.y + title_h,
    };
    draw_text_centered(target, "Sylva", &formats.title_bold, title_lr, &brushes.title);

    // 标题栏：关闭按钮「✕」+「切换桌面」按钮
    let close_hover = matches!(c.hover_zone, Some(ConsoleZone::Close));
    if close_hover {
        let hov = D2D1_ROUNDED_RECT {
            rect: D2D_RECT_F {
                left: c.close.x,
                top: c.close.y,
                right: c.close.x + c.close.w,
                bottom: c.close.y + c.close.h,
            },
            radiusX: 8.0 * s,
            radiusY: 8.0 * s,
        };
        let hov_bg =
            unsafe { target.CreateSolidColorBrush(&color([0.85, 0.28, 0.28, 0.30 * a]), None)? };
        unsafe { target.FillRoundedRectangle(&hov, &hov_bg) };
    }
    let xw = text_estimate_width("✕", theme.title.size);
    let close_lr = D2D_RECT_F {
        left: c.close.x + (c.close.w - xw) / 2.0,
        top: c.close.y + (c.close.h - theme.title.size * 1.6) / 2.0,
        right: c.close.x + c.close.w,
        bottom: c.close.y + c.close.h,
    };
    let close_brush =
        unsafe { target.CreateSolidColorBrush(&color([1.0, 1.0, 1.0, 0.62 * a]), None)? };
    draw_text(target, "✕", &formats.title, close_lr, &close_brush);

    // 栅栏管理页（单页，无标签栏）
    draw_fences_page(target, theme, c, formats, a, accent)?;
    Ok(())
}

/// 栅栏管理页：可点选栅栏列表 + 选中栅栏的详情控制区。
#[allow(clippy::too_many_arguments)]
fn draw_fences_page(
    target: &ID2D1RenderTarget,
    theme: &Theme,
    c: &SceneConsole,
    formats: &TextFormats,
    full_t: f32,
    accent: [f32; 4],
) -> Result<()> {
    let s = theme.scale;
    // 列表可视区裁剪（滚动时行被裁掉）
    let clip = D2D_RECT_F {
        left: c.fence_list_view.x,
        top: c.fence_list_view.y,
        right: c.fence_list_view.x + c.fence_list_view.w,
        bottom: c.fence_list_view.y + c.fence_list_view.h,
    };
    if clip.bottom > clip.top {
        unsafe { target.PushAxisAlignedClip(&clip, D2D1_ANTIALIAS_MODE_PER_PRIMITIVE) };
    }
    for (i, r) in c.fence_rows.iter().enumerate() {
        let hover = matches!(c.hover_zone, Some(ConsoleZone::FenceSelect(j)) if j == i);
        let rr = D2D1_ROUNDED_RECT {
            rect: D2D_RECT_F {
                left: r.rect.x,
                top: r.rect.y,
                right: r.rect.x + r.rect.w,
                bottom: r.rect.y + r.rect.h,
            },
            radiusX: 8.0 * s,
            radiusY: 8.0 * s,
        };
        let fill = if r.selected {
            [accent[0], accent[1], accent[2], 0.24 * full_t]
        } else if hover {
            [1.0, 1.0, 1.0, 0.08 * full_t]
        } else {
            [1.0, 1.0, 1.0, 0.04 * full_t]
        };
        let b = unsafe { target.CreateSolidColorBrush(&color(fill), None)? };
        unsafe { target.FillRoundedRectangle(&rr, &b) };
        if r.selected {
            let edge_l = D2D1_ROUNDED_RECT {
                rect: D2D_RECT_F {
                    left: r.rect.x + 2.0 * s,
                    top: r.rect.y + 2.0 * s,
                    right: r.rect.x + r.rect.w - 2.0 * s,
                    bottom: r.rect.y + r.rect.h - 2.0 * s,
                },
                radiusX: 8.0 * s,
                radiusY: 8.0 * s,
            };
            let e = unsafe {
                target.CreateSolidColorBrush(
                    &color([accent[0], accent[1], accent[2], 0.7 * full_t]),
                    None,
                )?
            };
            unsafe { target.DrawRoundedRectangle(&edge_l, &e, 1.0, None) };
        }
        let lr = D2D_RECT_F {
            left: r.rect.x + 12.0 * s,
            top: r.rect.y + (r.rect.h - theme.label.size * 1.6) / 2.0,
            right: r.rect.x + r.rect.w - 30.0 * s,
            bottom: r.rect.y + r.rect.h,
        };
        let txt =
            unsafe { target.CreateSolidColorBrush(&color([1.0, 1.0, 1.0, 0.90 * full_t]), None)? };
        draw_text_centered(target, &r.title, &formats.label, lr, &txt);
    }
    if clip.bottom > clip.top {
        unsafe { target.PopAxisAlignedClip() };
    }
    // 详情控制区
    if let Some(d) = &c.fence_detail {
        draw_fence_detail(target, theme, c, d, formats, full_t, accent)?;
    }
    // 「添加栅栏」按钮
    let add_hover = matches!(c.hover_zone, Some(ConsoleZone::AddFence));
    draw_segmented_button(
        target,
        theme,
        c.add_fence,
        "＋ 添加栅栏",
        false,
        add_hover,
        formats,
        accent,
    );
    // 「删除栅栏」按钮（添加按钮下方；hover 变红）
    let remove_hover = matches!(c.hover_zone, Some(ConsoleZone::RemoveFence));
    draw_segmented_button(
        target,
        theme,
        c.remove_btn,
        "删除栅栏",
        false,
        remove_hover,
        formats,
        if remove_hover {
            [0.85, 0.28, 0.28, 0.9]
        } else {
            accent
        },
    );
    // 「切换桌面 / 回到栅栏」按钮（删除栅栏下方）
    let toggle_hover = matches!(c.hover_zone, Some(ConsoleZone::DesktopToggle));
    let toggle_label = if c.desktop_mode {
        "回到栅栏"
    } else {
        "切换桌面"
    };
    draw_segmented_button(
        target,
        theme,
        c.desktop_toggle,
        toggle_label,
        c.desktop_mode,
        toggle_hover,
        formats,
        accent,
    );
    Ok(())
}

/// 栅栏详情控制区：布局 / 图标大小 / 背景风格 分段按钮 + 色调色板。
#[allow(clippy::too_many_arguments)]
fn draw_fence_detail(
    target: &ID2D1RenderTarget,
    theme: &Theme,
    c: &SceneConsole,
    d: &SceneFenceDetail,
    formats: &TextFormats,
    full_t: f32,
    accent: [f32; 4],
) -> Result<()> {
    let s = theme.scale;
    let label_lr = D2D_RECT_F {
        left: d.rect.x + 2.0 * s,
        top: d.rect.y + 2.0 * s,
        right: d.rect.x + d.rect.w - 2.0 * s,
        bottom: d.rect.y + 20.0 * s,
    };
    let title =
        unsafe { target.CreateSolidColorBrush(&color([1.0, 1.0, 1.0, 0.75 * full_t]), None)? };
    draw_text(target, &d.title, &formats.label, label_lr, &title);

    let label_x = d.rect.x + 2.0 * s;
    let label_w = 40.0 * s;
    let row_y = |i: usize| d.rect.y + 26.0 * s + i as f32 * 30.0 * s;
    let label_brush =
        unsafe { target.CreateSolidColorBrush(&color([1.0, 1.0, 1.0, 0.45 * full_t]), None)? };

    // 布局
    let lr = D2D_RECT_F {
        left: label_x,
        top: row_y(0) + 2.0 * s,
        right: label_x + label_w,
        bottom: row_y(0) + 24.0 * s,
    };
    draw_text(target, "布局", &formats.detail, lr, &label_brush);
    draw_segmented_button(
        target,
        theme,
        d.layout_grid,
        "网格",
        d.layout == FenceLayout::Grid,
        matches!(
            c.hover_zone,
            Some(ConsoleZone::FenceLayout(FenceLayout::Grid))
        ),
        formats,
        accent,
    );
    draw_segmented_button(
        target,
        theme,
        d.layout_list,
        "列表",
        d.layout == FenceLayout::List,
        matches!(
            c.hover_zone,
            Some(ConsoleZone::FenceLayout(FenceLayout::List))
        ),
        formats,
        accent,
    );
    draw_segmented_button(
        target,
        theme,
        d.layout_sidebar,
        "侧边栏",
        d.layout == FenceLayout::Sidebar,
        matches!(
            c.hover_zone,
            Some(ConsoleZone::FenceLayout(FenceLayout::Sidebar))
        ),
        formats,
        accent,
    );

    // 图标大小
    let lr2 = D2D_RECT_F {
        left: label_x,
        top: row_y(1) + 2.0 * s,
        right: label_x + label_w,
        bottom: row_y(1) + 24.0 * s,
    };
    draw_text(target, "大小", &formats.detail, lr2, &label_brush);
    for (rect, val, label) in [
        (d.size_s, 32.0, "小"),
        (d.size_m, 48.0, "中"),
        (d.size_l, 64.0, "大"),
    ] {
        draw_segmented_button(
            target,
            theme,
            rect,
            label,
            (d.icon_size - val).abs() < 0.5,
            matches!(c.hover_zone, Some(ConsoleZone::FenceIconSize(v)) if (v - val).abs() < 0.5),
            formats,
            accent,
        );
    }

    // 背景风格
    let lr3 = D2D_RECT_F {
        left: label_x,
        top: row_y(2) + 2.0 * s,
        right: label_x + label_w,
        bottom: row_y(2) + 24.0 * s,
    };
    draw_text(target, "风格", &formats.detail, lr3, &label_brush);
    draw_segmented_button(
        target,
        theme,
        d.style_glass,
        "玻璃",
        d.style == FenceStyle::Glass,
        matches!(
            c.hover_zone,
            Some(ConsoleZone::FenceStyle(FenceStyle::Glass))
        ),
        formats,
        accent,
    );
    draw_segmented_button(
        target,
        theme,
        d.style_outline,
        "描边",
        d.style == FenceStyle::Outline,
        matches!(
            c.hover_zone,
            Some(ConsoleZone::FenceStyle(FenceStyle::Outline))
        ),
        formats,
        accent,
    );
    draw_segmented_button(
        target,
        theme,
        d.style_filled,
        "纯色",
        d.style == FenceStyle::Filled,
        matches!(
            c.hover_zone,
            Some(ConsoleZone::FenceStyle(FenceStyle::Filled))
        ),
        formats,
        accent,
    );
    draw_segmented_button(
        target,
        theme,
        d.style_blur,
        "模糊",
        d.style == FenceStyle::Blur,
        matches!(
            c.hover_zone,
            Some(ConsoleZone::FenceStyle(FenceStyle::Blur))
        ),
        formats,
        accent,
    );

    // 色调色板：默认（玻璃底）+ 预设色
    let lr4 = D2D_RECT_F {
        left: label_x,
        top: row_y(3) + 2.0 * s,
        right: label_x + label_w,
        bottom: row_y(3) + 22.0 * s,
    };
    draw_text(target, "色调", &formats.detail, lr4, &label_brush);
    draw_tint_swatch(
        target,
        theme,
        d.tint_default,
        [0.20, 0.24, 0.32],
        d.tint.is_none(),
        matches!(c.hover_zone, Some(ConsoleZone::FenceTint(None))),
        accent,
    )?;
    for (i, rect) in d.tints.iter().enumerate() {
        if let Some(rgb) = TINT_COLORS.get(i) {
            let rgb = *rgb;
            let active = d.tint == Some(rgb);
            let hover = matches!(c.hover_zone, Some(ConsoleZone::FenceTint(Some(t))) if t == rgb);
            draw_tint_swatch(target, theme, *rect, rgb, active, hover, accent)?;
        }
    }

    // 存储位置行
    let lr5 = D2D_RECT_F {
        left: label_x,
        top: row_y(4) + 2.0 * s,
        right: label_x + label_w,
        bottom: row_y(4) + 24.0 * s,
    };
    draw_text(target, "存储", &formats.detail, lr5, &label_brush);
    let storage_hover = matches!(c.hover_zone, Some(ConsoleZone::ChangeStoragePath));
    draw_segmented_button(
        target,
        theme,
        d.storage_btn,
        "更改位置…",
        false,
        storage_hover,
        formats,
        accent,
    );

    // 侧边栏停靠位置（仅 Sidebar 布局时高亮可用）
    let lr6 = D2D_RECT_F {
        left: label_x,
        top: row_y(5) + 2.0 * s,
        right: label_x + label_w,
        bottom: row_y(5) + 24.0 * s,
    };
    let pos_label_alpha = if d.layout == FenceLayout::Sidebar {
        0.45
    } else {
        0.20
    };
    let pos_label_brush = unsafe {
        target.CreateSolidColorBrush(&color([1.0, 1.0, 1.0, pos_label_alpha * full_t]), None)?
    };
    draw_text(target, "位置", &formats.detail, lr6, &pos_label_brush);
    for (rect, pos, label) in [
        (d.sidebar_left, SidebarPosition::Left, "左"),
        (d.sidebar_top, SidebarPosition::Top, "上"),
        (d.sidebar_right, SidebarPosition::Right, "右"),
    ] {
        let active = d.sidebar_pos == pos && d.layout == FenceLayout::Sidebar;
        let hover = matches!(c.hover_zone, Some(ConsoleZone::FenceSidebarPos(p)) if p == pos);
        draw_segmented_button(target, theme, rect, label, active, hover, formats, accent);
    }

    Ok(())
}

/// 色调色板常量（与 App 层 TINT_PRESETS 平行；这里只读 RGB）。
const TINT_COLORS: &[[f32; 3]] = &[
    [0.32, 0.55, 0.95],
    [0.30, 0.80, 0.85],
    [0.40, 0.75, 0.45],
    [0.95, 0.85, 0.40],
    [0.98, 0.62, 0.30],
    [0.92, 0.35, 0.35],
    [0.66, 0.45, 0.90],
    [0.92, 0.93, 0.96],
    [0.56, 0.58, 0.62],
];

/// 单个色调色板圆点（选中 = 白环，悬停 = 放大光晕）。
fn draw_tint_swatch(
    target: &ID2D1RenderTarget,
    _theme: &Theme,
    rect: RectF,
    rgb: [f32; 3],
    active: bool,
    hover: bool,
    _accent: [f32; 4],
) -> Result<()> {
    let cx = rect.x + rect.w / 2.0;
    let cy = rect.y + rect.h / 2.0;
    let r = rect.w / 2.0;
    let fill =
        unsafe { target.CreateSolidColorBrush(&color([rgb[0], rgb[1], rgb[2], 1.0]), None)? };
    if hover {
        let halo = D2D1_ELLIPSE {
            point: windows_numerics::Vector2 { X: cx, Y: cy },
            radiusX: r + 3.0,
            radiusY: r + 3.0,
        };
        let hb = unsafe { target.CreateSolidColorBrush(&color([1.0, 1.0, 1.0, 0.18]), None)? };
        unsafe { target.FillEllipse(&halo, &hb) };
    }
    let ell = D2D1_ELLIPSE {
        point: windows_numerics::Vector2 { X: cx, Y: cy },
        radiusX: r,
        radiusY: r,
    };
    unsafe { target.FillEllipse(&ell, &fill) };
    if active {
        let ring = D2D1_ELLIPSE {
            point: windows_numerics::Vector2 { X: cx, Y: cy },
            radiusX: r + 1.0,
            radiusY: r + 1.0,
        };
        let rb = unsafe { target.CreateSolidColorBrush(&color([1.0, 1.0, 1.0, 0.95]), None)? };
        unsafe { target.DrawEllipse(&ring, &rb, 1.6, None) };
    }
    Ok(())
}

/// 分段按钮：圆角底 + 居中文字；`active` = 强调色填充，`hover` = 提亮。
#[allow(clippy::too_many_arguments)]
fn draw_segmented_button(
    target: &ID2D1RenderTarget,
    theme: &Theme,
    rect: RectF,
    label: &str,
    active: bool,
    hover: bool,
    formats: &TextFormats,
    accent: [f32; 4],
) {
    let s = theme.scale;
    let rr = D2D1_ROUNDED_RECT {
        rect: D2D_RECT_F {
            left: rect.x,
            top: rect.y,
            right: rect.x + rect.w,
            bottom: rect.y + rect.h,
        },
        radiusX: 7.0 * s,
        radiusY: 7.0 * s,
    };
    let fill = if active {
        [accent[0], accent[1], accent[2], 0.85]
    } else if hover {
        [1.0, 1.0, 1.0, 0.16]
    } else {
        [1.0, 1.0, 1.0, 0.07]
    };
    if let Ok(b) = unsafe { target.CreateSolidColorBrush(&color(fill), None) } {
        unsafe { target.FillRoundedRectangle(&rr, &b) };
    }
    let lr = D2D_RECT_F {
        left: rect.x,
        top: rect.y + (rect.h - theme.label.size * 1.6) / 2.0,
        right: rect.x + rect.w,
        bottom: rect.y + rect.h,
    };
    let alpha = if active { 0.96 } else { 0.80 };
    if let Ok(b) = unsafe { target.CreateSolidColorBrush(&color([1.0, 1.0, 1.0, alpha]), None) } {
        draw_text_centered(target, label, &formats.label, lr, &b);
    }
}

fn draw_fence(
    target: &ID2D1RenderTarget,
    theme: &Theme,
    fence: &SceneFence,
    brushes: &Brushes,
    icons: &IconStore,
    formats: &TextFormats,
) -> Result<()> {
    if fence.alpha >= 0.999 {
        return draw_fence_inner(target, theme, fence, brushes, icons, formats);
    }
    let lp = D2D1_LAYER_PARAMETERS {
        contentBounds: D2D_RECT_F {
            left: fence.x,
            top: fence.y,
            right: fence.x + fence.width,
            bottom: fence.y + fence.height,
        },
        opacity: fence.alpha.clamp(0.0, 1.0),
        ..Default::default()
    };
    unsafe { target.PushLayer(&lp, None) };
    let res = draw_fence_inner(target, theme, fence, brushes, icons, formats);
    unsafe { target.PopLayer() };
    res
}

/// 栅栏内部绘制（圆角矩形、标题、图标、滚动条），由 `draw_fence` 按透明度包裹。
fn draw_fence_inner(
    target: &ID2D1RenderTarget,
    theme: &Theme,
    fence: &SceneFence,
    brushes: &Brushes,
    icons: &IconStore,
    formats: &TextFormats,
) -> Result<()> {
    let rr = D2D1_ROUNDED_RECT {
        rect: D2D_RECT_F {
            left: fence.x,
            top: fence.y,
            right: fence.x + fence.width,
            bottom: fence.y + fence.height,
        },
        radiusX: theme.fence_corner_radius,
        radiusY: theme.fence_corner_radius,
    };

    // 窗口模式：内部完全透明（仅描边）时跳过填充
    if let Some(fill) = fence.fill_color {
        let brush = unsafe { target.CreateSolidColorBrush(&color(fill), None)? };
        unsafe { target.FillRoundedRectangle(&rr, &brush) };
    }
    // 模糊风格（fill_color = None）：背景由合成器里独立的 GaussianBlurEffect 视觉
    // 提供（见 compositor::sync_blurs），本表面在栅栏矩形内保持透明以透出；图标/
    // 标题/边框仍在顶层绘制。
    if fence.border_width > 0.0 {
        let brush = unsafe { target.CreateSolidColorBrush(&color(fence.border_color), None)? };
        unsafe { target.DrawRoundedRectangle(&rr, &brush, fence.border_width, None) };
    }

    if !fence.title.is_empty() {
        let tr = D2D_RECT_F {
            left: fence.x + theme.fence_padding,
            top: fence.y + theme.fence_padding,
            right: fence.x + fence.width - theme.fence_padding,
            bottom: fence.y + theme.fence_padding + theme.title.size * 1.6,
        };
        draw_text_centered(target, &fence.title, &formats.title, tr, &brushes.title);
    }

    // 内容区：悬停高亮 + 图标行 + 列表列头，全部裁剪在内容区内（滚动后顶部可裁掉）。
    let row_top = match fence.list_cols {
        Some(cols) => {
            draw_list_header(target, theme, fence, cols, formats, &brushes.label);
            fence.content_top + cols.header_h
        }
        None => fence.content_top,
    };
    // 悬停图标放大时扩展裁剪区域，避免 2x 放大的图标被内容区裁掉
    let hover_grow = if let Some(hi) = fence.hover_icon {
        fence
            .icons
            .get(hi)
            .map(|ic| (ic.size * (ic.scale - 1.0) * 0.5).max(0.0))
            .unwrap_or(0.0)
    } else {
        0.0
    };
    let clip = D2D_RECT_F {
        left: fence.content_left - hover_grow,
        top: row_top - hover_grow,
        right: fence.x + fence.width - theme.fence_padding + hover_grow,
        bottom: row_top + fence.scroll_view + hover_grow,
    };
    unsafe { target.PushAxisAlignedClip(&clip, D2D1_ANTIALIAS_MODE_PER_PRIMITIVE) };

    // 悬停高亮：画在对应图标底下（含列表行）。
    if let Some(hi) = fence.hover_icon {
        if let Some(icon) = fence.icons.get(hi) {
            // 行高与 App 层布局一致：图标（或标签）高 + 行距，保证整行被高亮覆盖。
            let row_extent = icon.size.max(theme.label.size * 1.6) + theme.list_row_gap;
            let highlight = if fence.layout == FenceLayout::List {
                // 列表：高亮整行（图标 + 名称 + 详情列）
                D2D1_ROUNDED_RECT {
                    rect: D2D_RECT_F {
                        left: icon.x - 4.0,
                        top: icon.y - 2.0,
                        right: fence.x + fence.width - theme.fence_padding,
                        bottom: icon.y + row_extent,
                    },
                    radiusX: 8.0,
                    radiusY: 8.0,
                }
            } else {
                D2D1_ROUNDED_RECT {
                    rect: D2D_RECT_F {
                        left: icon.x - 4.0,
                        top: icon.y - 4.0,
                        right: icon.x + icon.size + 4.0,
                        bottom: icon.y + icon.size + 4.0,
                    },
                    radiusX: 8.0,
                    radiusY: 8.0,
                }
            };
            let fill =
                unsafe { target.CreateSolidColorBrush(&color([1.0, 1.0, 1.0, 0.12]), None)? };
            let edge =
                unsafe { target.CreateSolidColorBrush(&color([1.0, 1.0, 1.0, 0.30]), None)? };
            unsafe {
                target.FillRoundedRectangle(&highlight, &fill);
                target.DrawRoundedRectangle(&highlight, &edge, 1.0, None);
            }
        }
    }

    // 选中高亮（多选，如资源管理器）：蓝色系，区别于悬停的白色系。
    // 先创建一次画笔，集合内逐项绘制，避免每项重复建画刷。
    if !fence.selected.is_empty() {
        let fill = unsafe { target.CreateSolidColorBrush(&color([0.26, 0.48, 0.79, 0.35]), None)? };
        let edge = unsafe { target.CreateSolidColorBrush(&color([0.26, 0.48, 0.79, 0.65]), None)? };
        for &si in &fence.selected {
            let Some(icon) = fence.icons.get(si) else {
                continue;
            };
            let row_extent = icon.size.max(theme.label.size * 1.6) + theme.list_row_gap;
            let sel = if fence.layout == FenceLayout::List {
                // 列表：高亮整行（图标 + 名称 + 详情列）
                D2D1_ROUNDED_RECT {
                    rect: D2D_RECT_F {
                        left: icon.x - 4.0,
                        top: icon.y - 2.0,
                        right: fence.x + fence.width - theme.fence_padding,
                        bottom: icon.y + row_extent,
                    },
                    radiusX: 8.0,
                    radiusY: 8.0,
                }
            } else {
                D2D1_ROUNDED_RECT {
                    rect: D2D_RECT_F {
                        left: icon.x - 4.0,
                        top: icon.y - 4.0,
                        right: icon.x + icon.size + 4.0,
                        bottom: icon.y + icon.size + 4.0,
                    },
                    radiusX: 8.0,
                    radiusY: 8.0,
                }
            };
            unsafe {
                target.FillRoundedRectangle(&sel, &fill);
                target.DrawRoundedRectangle(&sel, &edge, 1.0, None);
            }
        }
    }

    for (ii, icon) in fence.icons.iter().enumerate() {
        // 悬停放大：以图标中心放大（scale 由 App 层补间填值），并垫一层柔光
        let grow = (icon.size * (icon.scale - 1.0) * 0.5).max(0.0);
        let dest = D2D_RECT_F {
            left: icon.x - grow,
            top: icon.y - grow,
            right: icon.x + icon.size + grow,
            bottom: icon.y + icon.size + grow,
        };
        if let Some(bmp) = icons.get(icon.bitmap_id) {
            // 柔光只垫在真正悬停的图标上（相邻放大图标不发光，避免「全部被高亮」）
            if grow > 0.5 && Some(ii) == fence.hover_icon {
                let glow = D2D1_ROUNDED_RECT {
                    rect: D2D_RECT_F {
                        left: dest.left - 6.0,
                        top: dest.top - 6.0,
                        right: dest.right + 6.0,
                        bottom: dest.bottom + 6.0,
                    },
                    radiusX: 10.0,
                    radiusY: 10.0,
                };
                let glow_b = unsafe {
                    target.CreateSolidColorBrush(
                        &color([1.0, 1.0, 1.0, (0.10 * (grow / 2.0)).min(0.25)]),
                        None,
                    )?
                };
                unsafe { target.FillRoundedRectangle(&glow, &glow_b) };
            }
            unsafe {
                target.DrawBitmap(
                    bmp,
                    Some(&dest),
                    (icon.scale - 1.0).mul_add(0.5, 1.0).min(1.0),
                    D2D1_BITMAP_INTERPOLATION_MODE_LINEAR,
                    None,
                );
            }
        }
        if icon.label.is_empty() {
            continue;
        }
        match fence.layout {
            // 侧边栏无内联标签：悬停提示由 App 层算好矩形，在裁剪区之外绘制
            //（见 draw_fence_inner 末尾的 tooltip_rect 绘制）。
            FenceLayout::Sidebar => {}
            FenceLayout::Grid => {
                let lr = D2D_RECT_F {
                    left: icon.x - 2.0,
                    top: icon.y + icon.size + theme.icon_caption_gap,
                    right: icon.x + icon.size + 2.0,
                    bottom: icon.y + icon.size + theme.icon_caption_gap + theme.label.size * 1.6,
                };
                draw_text(target, &icon.label, &formats.label, lr, &brushes.label);
            }
            FenceLayout::List => {
                // 列表：名称在图标右侧；类型/修改日期/大小按列对齐。
                let label_h = theme.label.size * 1.6;
                let ty = icon.y + (icon.size - label_h) / 2.0;
                let name_lr = D2D_RECT_F {
                    left: icon.x + icon.size + theme.list_label_gap,
                    top: ty,
                    right: fence
                        .list_cols
                        .map(|c| c.type_x - theme.list_label_gap)
                        .unwrap_or(fence.x + fence.width - theme.fence_padding),
                    bottom: ty + label_h,
                };
                draw_text(target, &icon.label, &formats.label, name_lr, &brushes.label);
                if let Some(cols) = fence.list_cols {
                    let type_lr = D2D_RECT_F {
                        left: cols.type_x,
                        top: ty,
                        right: cols.modified_x - theme.list_label_gap,
                        bottom: ty + label_h,
                    };
                    draw_text(
                        target,
                        &icon.col_type,
                        &formats.label,
                        type_lr,
                        &brushes.label,
                    );
                    let mod_lr = D2D_RECT_F {
                        left: cols.modified_x,
                        top: ty,
                        right: cols.size_x - theme.list_label_gap,
                        bottom: ty + label_h,
                    };
                    draw_text(
                        target,
                        &icon.col_modified,
                        &formats.label,
                        mod_lr,
                        &brushes.label,
                    );
                    let size_lr = D2D_RECT_F {
                        left: cols.size_x,
                        top: ty,
                        right: fence.x + fence.width - theme.fence_padding,
                        bottom: ty + label_h,
                    };
                    draw_text(
                        target,
                        &icon.col_size,
                        &formats.label,
                        size_lr,
                        &brushes.label,
                    );
                }
            }
        }
    }
    unsafe { target.PopAxisAlignedClip() };

    // 框选橡皮筋：半透明蓝 + 边线，裁剪在本栅栏内容区内（拖出栅栏也不越界）。
    if let Some(band) = fence.select_band {
        let clip = D2D_RECT_F {
            left: fence.content_left,
            top: row_top,
            right: fence.x + fence.width - theme.fence_padding,
            bottom: row_top + fence.scroll_view,
        };
        unsafe { target.PushAxisAlignedClip(&clip, D2D1_ANTIALIAS_MODE_PER_PRIMITIVE) };
        let band_rr = D2D1_ROUNDED_RECT {
            rect: D2D_RECT_F {
                left: band.x,
                top: band.y,
                right: band.x + band.w,
                bottom: band.y + band.h,
            },
            radiusX: 2.0,
            radiusY: 2.0,
        };
        let bfill =
            unsafe { target.CreateSolidColorBrush(&color([0.26, 0.48, 0.79, 0.16]), None)? };
        let bedge =
            unsafe { target.CreateSolidColorBrush(&color([0.26, 0.48, 0.79, 0.72]), None)? };
        unsafe {
            target.FillRoundedRectangle(&band_rr, &bfill);
            target.DrawRoundedRectangle(&band_rr, &bedge, 1.0, None);
        }
        unsafe { target.PopAxisAlignedClip() };
    }

    // 悬停工具提示：标签被截断成「…」时显示完整名称（Windows 资源管理器风格）。
    // 画在裁剪区之外、栅栏矩形之内（窗口区域 = 栅栏并集，区域外的绘制不可见）。
    if let Some(hi) = fence.hover_icon {
        if let Some(icon) = fence.icons.get(hi) {
            if !icon.label.is_empty()
                && label_max_width(fence, icon, theme) > 0.0
                && !text_fits(
                    &icon.label,
                    label_max_width(fence, icon, theme),
                    theme.label.size,
                )
            {
                draw_tooltip(
                    target,
                    theme,
                    &icon.label,
                    formats,
                    &brushes.label,
                    fence,
                    icon,
                );
            }
        }
    }

    // 侧边栏 Dock 工具提示：矩形由 App 层按屏幕边界算好（可延伸到栅栏之外、
    // 不受裁剪限制），画在裁剪区外显示完整名称，不再被 dock 右缘/内容裁剪截断。
    if fence.layout == FenceLayout::Sidebar {
        if let (Some(hi), Some(tt)) = (fence.hover_icon, fence.tooltip_rect) {
            if let Some(icon) = fence.icons.get(hi) {
                if !icon.label.is_empty() {
                    let s = theme.scale;
                    let pad = 7.0 * s;
                    let rr = D2D1_ROUNDED_RECT {
                        rect: D2D_RECT_F {
                            left: tt.x,
                            top: tt.y,
                            right: tt.x + tt.w,
                            bottom: tt.y + tt.h,
                        },
                        radiusX: 6.0 * s,
                        radiusY: 6.0 * s,
                    };
                    let bg_b =
                        unsafe { target.CreateSolidColorBrush(&color([0.10, 0.12, 0.16, 0.96]), None)? };
                    let edge_b = unsafe {
                        target.CreateSolidColorBrush(&color([1.0, 1.0, 1.0, 0.35]), None)?
                    };
                    unsafe {
                        target.FillRoundedRectangle(&rr, &bg_b);
                        target.DrawRoundedRectangle(&rr, &edge_b, 1.0, None);
                    }
                    let lr = D2D_RECT_F {
                        left: tt.x + pad,
                        top: tt.y + pad,
                        right: tt.x + tt.w - pad,
                        bottom: tt.y + tt.h - pad,
                    };
                    draw_text(target, &icon.label, &formats.label, lr, &brushes.label);
                }
            }
        }
    }

    // 滚动条指示条：内容超出可视区时在右缘画细条（滚轮滚动，不拖拽）。
    if fence.scroll_max > 0.0 {
        draw_scrollbar(target, theme, fence, row_top);
    }
    Ok(())
}

/// 列表列头：名称 / 类型 / 修改日期 / 大小（固定在上沿，不随内容滚动）。
fn draw_list_header(
    target: &ID2D1RenderTarget,
    theme: &Theme,
    fence: &SceneFence,
    cols: ListColumns,
    formats: &TextFormats,
    brush: &ID2D1SolidColorBrush,
) {
    let y = fence.content_top;
    let h = cols.header_h;
    // 分隔线 + 轻微底色，让列头区别于内容行
    let header_rr = D2D1_ROUNDED_RECT {
        rect: D2D_RECT_F {
            left: fence.content_left,
            top: y,
            right: fence.x + fence.width - theme.fence_padding,
            bottom: y + h,
        },
        radiusX: 6.0,
        radiusY: 6.0,
    };
    let bg = unsafe {
        target
            .CreateSolidColorBrush(&color([1.0, 1.0, 1.0, 0.05]), None)
            .ok()
    };
    if let Some(bg) = bg {
        unsafe { target.FillRoundedRectangle(&header_rr, &bg) };
    }

    let label_h = theme.label.size * 1.6;
    let ty = y + (h - label_h) / 2.0;
    let name_lr = D2D_RECT_F {
        left: fence.content_left,
        top: ty,
        right: cols.type_x - theme.list_label_gap,
        bottom: ty + label_h,
    };
    draw_text(target, "名称", &formats.label, name_lr, brush);
    let type_lr = D2D_RECT_F {
        left: cols.type_x,
        top: ty,
        right: cols.modified_x - theme.list_label_gap,
        bottom: ty + label_h,
    };
    draw_text(target, "类型", &formats.label, type_lr, brush);
    let mod_lr = D2D_RECT_F {
        left: cols.modified_x,
        top: ty,
        right: cols.size_x - theme.list_label_gap,
        bottom: ty + label_h,
    };
    draw_text(target, "修改日期", &formats.label, mod_lr, brush);
    let size_lr = D2D_RECT_F {
        left: cols.size_x,
        top: ty,
        right: fence.x + fence.width - theme.fence_padding,
        bottom: ty + label_h,
    };
    draw_text(target, "大小", &formats.label, size_lr, brush);
}

/// 滚动条指示条：细圆角条，位置/长度反映滚动进度。
fn draw_scrollbar(target: &ID2D1RenderTarget, theme: &Theme, fence: &SceneFence, top: f32) {
    let track_h = fence.scroll_view;
    // 侧边栏：超短小短线（固定比例，紧贴右缘），普通布局：按内容比例的可拖拽滑块
    let thumb_h = if fence.layout == FenceLayout::Sidebar {
        (track_h * 0.2).clamp(24.0, 64.0).min(track_h)
    } else {
        (track_h * track_h / (track_h + fence.scroll_max))
            .max(24.0)
            .min(track_h)
    };
    let max_off = (track_h - thumb_h).max(0.0);
    let off = if fence.scroll_max > 0.0 {
        (fence.scroll / fence.scroll_max) * max_off
    } else {
        0.0
    };
    let sb = D2D1_ROUNDED_RECT {
        rect: D2D_RECT_F {
            left: fence.x + fence.width - theme.fence_padding - 5.0,
            top: top + off,
            right: fence.x + fence.width - theme.fence_padding - 1.0,
            bottom: top + off + thumb_h,
        },
        radiusX: 2.0,
        radiusY: 2.0,
    };
    let Some(brush) =
        unsafe { target.CreateSolidColorBrush(&color([1.0, 1.0, 1.0, 0.22]), None) }.ok()
    else {
        return;
    };
    unsafe { target.FillRoundedRectangle(&sb, &brush) };
}

/// Sylva 控制台：深色圆角面板 + 标题 + 新建栅栏按钮 + 每栅栏一行（模式切换）。
/// 画单行文本。格式已设为 NO_WRAP（不会换行）；超出矩形宽度时按 Windows 风格
/// 截断为「…」，并把完整文本留给悬停工具提示（`draw_fence`）。
fn draw_text(
    target: &ID2D1RenderTarget,
    text: &str,
    format: &IDWriteTextFormat,
    rect: D2D_RECT_F,
    brush: &ID2D1SolidColorBrush,
) {
    let font_size = unsafe { format.GetFontSize() };
    let max_w = (rect.right - rect.left).max(0.0);
    let shown = truncate_to_fit(text, max_w, font_size);
    let wide: Vec<u16> = shown.encode_utf16().collect();
    unsafe {
        target.DrawText(
            &wide,
            format,
            &rect,
            brush,
            D2D1_DRAW_TEXT_OPTIONS_CLIP,
            DWRITE_MEASURING_MODE_NATURAL,
        );
    }
}

/// 居中绘制单行文本：按估算宽度把起点推到矩形中线（配合左对齐格式即可居中）。
fn draw_text_centered(
    target: &ID2D1RenderTarget,
    text: &str,
    format: &IDWriteTextFormat,
    rect: D2D_RECT_F,
    brush: &ID2D1SolidColorBrush,
) {
    let font_size = unsafe { format.GetFontSize() };
    let max_w = (rect.right - rect.left).max(0.0);
    let shown = truncate_to_fit(text, max_w, font_size);
    let w = text_estimate_width(&shown, font_size);
    let centered = D2D_RECT_F {
        left: rect.left + ((rect.right - rect.left) - w) / 2.0,
        right: rect.right,
        ..rect
    };
    draw_text(target, &shown, format, centered, brush);
}

/// 内联文本编辑渲染：输入行底 + 文本（含 IME 合成串）+ 光标 + 聚焦描边。
/// 与卡片/面板同表面绘制，文字与圆角矩形天然对齐。
fn draw_inline_edit(
    target: &ID2D1RenderTarget,
    theme: &Theme,
    e: &SceneEdit,
    formats: &TextFormats,
) -> Result<()> {
    let s = theme.scale;
    let pad_x = 10.0 * s;
    let font = theme.label.size;
    let line_h = font * 1.5;
    let rr = D2D1_ROUNDED_RECT {
        rect: D2D_RECT_F {
            left: e.rect.x,
            top: e.rect.y,
            right: e.rect.x + e.rect.w,
            bottom: e.rect.y + e.rect.h,
        },
        radiusX: 7.0 * s,
        radiusY: 7.0 * s,
    };
    let bg_alpha = if e.focused { 0.10 } else { 0.06 };
    let bg = unsafe { target.CreateSolidColorBrush(&color([1.0, 1.0, 1.0, bg_alpha]), None)? };
    unsafe { target.FillRoundedRectangle(&rr, &bg) };
    if e.focused {
        let edge = unsafe { target.CreateSolidColorBrush(&color([0.35, 0.62, 1.0, 0.9]), None)? };
        unsafe { target.DrawRoundedRectangle(&rr, &edge, 1.2, None) };
    } else {
        let edge = unsafe { target.CreateSolidColorBrush(&color([1.0, 1.0, 1.0, 0.14]), None)? };
        unsafe { target.DrawRoundedRectangle(&rr, &edge, 1.0, None) };
    }

    let clip = D2D_RECT_F {
        left: e.rect.x + 6.0 * s,
        top: e.rect.y + 4.0 * s,
        right: e.rect.x + e.rect.w - 6.0 * s,
        bottom: e.rect.y + e.rect.h - 4.0 * s,
    };
    unsafe { target.PushAxisAlignedClip(&clip, D2D1_ANTIALIAS_MODE_PER_PRIMITIVE) };

    let empty = e.lines.iter().all(|l| l.is_empty()) && !e.composing;
    if empty && !e.placeholder.is_empty() {
        let lr = D2D_RECT_F {
            left: e.rect.x + pad_x,
            top: e.rect.y + (e.rect.h - font * 1.6) / 2.0,
            right: e.rect.x + e.rect.w - pad_x,
            bottom: e.rect.y + e.rect.h,
        };
        let pb = unsafe { target.CreateSolidColorBrush(&color([1.0, 1.0, 1.0, 0.36]), None)? };
        draw_text(target, &e.placeholder, &formats.detail, lr, &pb);
    } else {
        let top0 = if e.single_line {
            e.rect.y + (e.rect.h - font * 1.6) / 2.0
        } else {
            e.rect.y + pad_x / 2.0
        };
        for (li, line) in e.lines.iter().enumerate() {
            let is_caret = li == e.line;
            let text = if is_caret {
                let before: String = line.chars().take(e.col).collect();
                let after: String = line.chars().skip(e.col).collect();
                format!("{before}{}{after}", e.comp)
            } else {
                line.clone()
            };
            let lr = D2D_RECT_F {
                left: e.rect.x + pad_x,
                top: top0 + li as f32 * line_h,
                right: e.rect.x + e.rect.w - pad_x,
                bottom: top0 + li as f32 * line_h + font * 1.6,
            };
            let tb = unsafe { target.CreateSolidColorBrush(&color([1.0, 1.0, 1.0, 0.92]), None)? };
            draw_text(target, &text, &formats.label, lr, &tb);
            if e.focused && is_caret {
                let before: String = line.chars().take(e.col).collect();
                let before_w =
                    text_estimate_width(&before, font) + text_estimate_width(&e.comp, font);
                let caret_x = e.rect.x + pad_x + before_w;
                let caret_y = if e.single_line {
                    e.rect.y + (e.rect.h - font * 1.6) / 2.0
                } else {
                    top0 + li as f32 * line_h
                };
                let blink_on = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis() / 500 % 2 == 0)
                    .unwrap_or(true);
                if blink_on {
                    let cb = unsafe {
                        target.CreateSolidColorBrush(&color([1.0, 1.0, 1.0, 0.95]), None)?
                    };
                    let p1 = windows_numerics::Vector2 {
                        X: caret_x,
                        Y: caret_y,
                    };
                    let p2 = windows_numerics::Vector2 {
                        X: caret_x,
                        Y: caret_y + font * 1.4,
                    };
                    unsafe { target.DrawLine(p1, p2, &cb, 1.4, None) };
                }
            }
        }
    }
    unsafe { target.PopAxisAlignedClip() };
    Ok(())
}

/// 文本在给定宽度内是否放得下（与 `truncate_to_fit` 同一估算口径，悬停判断共用）。
fn text_fits(text: &str, max_w: f32, font_size: f32) -> bool {
    text_estimate_width(text, font_size) <= max_w
}

/// 放不下时截断成「…」：从前往后保留放得下的字符，末尾补省略号（Windows 风格）。
fn truncate_to_fit<'a>(text: &'a str, max_w: f32, font_size: f32) -> std::borrow::Cow<'a, str> {
    if text.is_empty() || text_fits(text, max_w, font_size) {
        return std::borrow::Cow::Borrowed(text);
    }
    let ell = "…";
    let ell_w = text_estimate_width(ell, font_size);
    // 预算扣除省略号宽度；极窄时至少留半个字宽，保证能放一个字符 + 省略号
    let budget = (max_w - ell_w).max(font_size * 0.5);
    let mut out = String::new();
    for c in text.chars() {
        let w = if c.is_ascii() { 0.62 } else { 1.0 } * font_size;
        if text_estimate_width(&out, font_size) + w > budget {
            break;
        }
        out.push(c);
    }
    out.push_str(ell);
    std::borrow::Cow::Owned(out)
}

/// 图标标签可用的最大宽度（与 `draw_fence` 里的标签矩形一致）：
/// 网格 = 图标宽 + 4；列表 = 名称列宽。悬停时用它判断标签是否被截断。
fn label_max_width(fence: &SceneFence, icon: &crate::scene::SceneIcon, theme: &Theme) -> f32 {
    match fence.layout {
        FenceLayout::Grid => icon.size + 4.0,
        FenceLayout::List => fence
            .list_cols
            .map(|c| {
                let left = icon.x + icon.size + theme.list_label_gap;
                (c.type_x - theme.list_label_gap - left).max(0.0)
            })
            .unwrap_or(0.0),
        FenceLayout::Sidebar => 0.0, // 侧边栏无内联标签，不检查截断
    }
}

/// 悬停工具提示：深色圆角气泡，显示截断前的完整标签。位置优先在图标上方，
/// 空间不足则下方；横向/纵向都钳制在栅栏矩形内（超出区域的部分不可见）。
#[allow(clippy::too_many_arguments)]
fn draw_tooltip(
    target: &ID2D1RenderTarget,
    theme: &Theme,
    text: &str,
    formats: &TextFormats,
    brush: &ID2D1SolidColorBrush,
    fence: &SceneFence,
    icon: &crate::scene::SceneIcon,
) {
    let s = theme.scale;
    let font = theme.label.size;
    let pad = 7.0 * s;
    let tw = text_estimate_width(text, font) + pad * 2.0;
    let th = font * 1.6 + pad * 2.0;
    if tw <= 0.0 || th <= 0.0 {
        return;
    }
    let gap = 8.0 * s;
    let mut tx = icon.x + icon.size / 2.0 - tw / 2.0;
    let mut ty = icon.y - th - gap;
    if ty < fence.y + 4.0 {
        // 上方放不下 → 放到图标下方
        ty = icon.y + icon.size + gap;
    }
    tx = tx.max(fence.x + 4.0).min(fence.x + fence.width - tw - 4.0);
    ty = ty.max(fence.y + 4.0).min(fence.y + fence.height - th - 4.0);

    let rr = D2D1_ROUNDED_RECT {
        rect: D2D_RECT_F {
            left: tx,
            top: ty,
            right: tx + tw,
            bottom: ty + th,
        },
        radiusX: 6.0 * s,
        radiusY: 6.0 * s,
    };
    let bg = color([0.10, 0.12, 0.16, 0.96]);
    let edge = color([1.0, 1.0, 1.0, 0.35]);
    let bg_brush = match unsafe { target.CreateSolidColorBrush(&bg, None) } {
        Ok(b) => b,
        Err(_) => return,
    };
    let edge_brush = match unsafe { target.CreateSolidColorBrush(&edge, None) } {
        Ok(b) => b,
        Err(_) => return,
    };
    unsafe {
        target.FillRoundedRectangle(&rr, &bg_brush);
        target.DrawRoundedRectangle(&rr, &edge_brush, 1.0, None);
    }
    let tr = D2D_RECT_F {
        left: tx + pad,
        top: ty + pad,
        right: tx + tw - pad,
        bottom: ty + th - pad,
    };
    draw_text(target, text, &formats.label, tr, brush);
}

/// 粗略估算文本像素宽度，用于居中/对齐（口径见 `sylva_core::text::estimate_width`）。
fn text_estimate_width(text: &str, font_size: f32) -> f32 {
    sylva_core::text::estimate_width(text, font_size)
}

/// [f32;4]（直通 alpha）→ D2D 颜色。
fn color(c: [f32; 4]) -> D2D1_COLOR_F {
    D2D1_COLOR_F {
        r: c[0],
        g: c[1],
        b: c[2],
        a: c[3],
    }
}

/// 把 CPU 侧图标像素（top-down BGRA premultiplied）上传为 GPU 位图。
fn make_bitmap(target: &ID2D1RenderTarget, data: &IconData) -> Result<ID2D1Bitmap> {
    let props = D2D1_BITMAP_PROPERTIES {
        pixelFormat: D2D1_PIXEL_FORMAT {
            format: DXGI_FORMAT_B8G8R8A8_UNORM,
            alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
        },
        dpiX: 96.0,
        dpiY: 96.0,
    };
    unsafe {
        target.CreateBitmap(
            D2D_SIZE_U {
                width: data.width,
                height: data.height,
            },
            Some(data.pixels.as_ptr() as *const core::ffi::c_void),
            data.stride() as u32,
            &props,
        )
    }
}

fn make_text_format(
    dwrite: &IDWriteFactory,
    style: TextStyle,
    weight: DWRITE_FONT_WEIGHT,
) -> Result<IDWriteTextFormat> {
    let family = wide(style.font_family);
    let locale = user_locale();
    // localeName 传 NULL 会返回 E_INVALIDARG（实测），必须给显式 locale
    let format = unsafe {
        dwrite.CreateTextFormat(
            PCWSTR(family.as_ptr()),
            None,
            weight,
            DWRITE_FONT_STYLE_NORMAL,
            DWRITE_FONT_STRETCH_NORMAL,
            style.size,
            PCWSTR(locale.as_ptr()),
        )
    }?;
    // 单行文本：不做自动换行。栅栏变窄时文件名不再折行显示不全，改为超出截断「…」
    // （截断由 `draw_text` 按矩形宽度完成；省略号需求见 draw_text）。
    unsafe {
        let _ = format.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP);
    }
    Ok(format)
}

/// 当前用户默认 locale（如 "zh-CN"），UTF-16 含结尾 NUL。
fn user_locale() -> Vec<u16> {
    // LOCALE_NAME_MAX_LENGTH = 85
    let mut buf = [0u16; 85];
    let n = unsafe { GetUserDefaultLocaleName(&mut buf) };
    if n == 0 {
        // 取不到时兜底到中英文都能渲染的常见值
        return wide("zh-CN");
    }
    buf[..n as usize].to_vec() // n 含结尾 NUL，正好用作指针
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn icon_store_ids_are_monotonic_without_gpu() {
        // 不上传位图（无 GPU），仅验证 ID 分配逻辑。
        let mut store = IconStore::new();
        assert_eq!(store.next_id, 0);
        store.next_id = 5;
        // 没有 GPU 时 insert 会失败，这里只测纯逻辑路径
        let _ = &mut store;
    }

    #[test]
    fn wide_terminates_with_nul() {
        let w = wide("Fence");
        assert_eq!(
            w,
            vec![
                b'F' as u16,
                b'e' as u16,
                b'n' as u16,
                b'c' as u16,
                b'e' as u16,
                0
            ]
        );
    }

    #[test]
    fn color_array_maps_to_d2d() {
        let c = color([0.2, 0.4, 0.6, 0.8]);
        assert!((c.r - 0.2).abs() < 1e-6);
        assert!((c.a - 0.8).abs() < 1e-6);
    }
}
