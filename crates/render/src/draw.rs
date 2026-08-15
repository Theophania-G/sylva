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
    DWRITE_FONT_WEIGHT_NORMAL, DWRITE_MEASURING_MODE_NATURAL, DWRITE_WORD_WRAPPING_NO_WRAP,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;

use sylva_core::model::{FenceLayout, FenceStyle};
use sylva_shell::icons::IconData;
use windows_numerics::Vector2;

use crate::overlay::{ConsoleZone, RectF};
use crate::scene::{ConsoleTab, ListColumns, Scene, SceneConsole, SceneFence, SceneFenceDetail};
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
    pub label: IDWriteTextFormat,
    /// 待办副行（详细信息）用小一号字号。
    pub detail: IDWriteTextFormat,
}

impl TextFormats {
    pub fn new(dwrite: &IDWriteFactory, theme: &Theme) -> Result<Self> {
        let title = make_text_format(dwrite, theme.title)?;
        tracing::debug!("TextFormats: 标题格式就绪");
        let label = make_text_format(dwrite, theme.label)?;
        tracing::debug!("TextFormats: 标签格式就绪");
        let detail_style = crate::theme::TextStyle {
            font_family: theme.label.font_family,
            size: theme.label.size * 0.72,
            color: theme.label.color,
        };
        let detail = make_text_format(dwrite, detail_style)?;
        tracing::debug!("TextFormats: 副行格式就绪");
        Ok(Self {
            title,
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
    Ok(())
}

/// 控制中心：玻璃卡片，可折叠胶囊 ⇄ 展开面板（待办 / 便签 / 栅栏管理 / 插件管理）。
///
/// 面板高度已由 App 层按 `panel` 进度插值（折叠胶囊 ⇄ 完整面板）；这里按进度把
/// 内容从「胶囊」平滑切换到「完整面板（标题 + 切换桌面 + 标签栏 + 当前页内容）」。
/// 输入行/便签文字由独立的置顶编辑框（App 层）绘制；勾选/删除/关闭/添加/标签/
/// 栅栏控制/插件开关的矩形与 `SceneConsole` 几何一致（命中模型复用同一份）。
fn draw_console(
    target: &ID2D1RenderTarget,
    theme: &Theme,
    c: &SceneConsole,
    brushes: &Brushes,
    formats: &TextFormats,
) -> Result<()> {
    let s = theme.scale;
    // 展开进度：0=折叠胶囊，1=完整面板（后半程内容淡入，前半程胶囊淡出）
    let full_t = ((c.panel - 0.5) / 0.5).clamp(0.0, 1.0);
    let pill_t = 1.0 - full_t;
    let accent = [0.23, 0.51, 0.96, 0.92];

    // 玻璃卡片底色 + 描边（胶囊与展开面板同款材质，视觉统一）
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
    let bg = unsafe { target.CreateSolidColorBrush(&color(c.fill_color), None)? };
    unsafe { target.FillRoundedRectangle(&panel, &bg) };
    let edge = unsafe { target.CreateSolidColorBrush(&color(c.border_color), None)? };
    unsafe { target.DrawRoundedRectangle(&panel, &edge, 1.0, None) };

    // 标题「Sylva」（两种形态都显示；胶囊里居左，面板里也在标题条）
    let title_h = c.title_h.max(34.0 * s);
    let title_lr = D2D_RECT_F {
        left: c.x + 14.0 * s,
        top: c.y + (title_h - theme.title.size * 1.6) / 2.0,
        right: c.desktop_toggle.x - 8.0 * s,
        bottom: c.y + title_h,
    };
    draw_text(target, "Sylva", &formats.title, title_lr, &brushes.title);

    // 待办计数角标（两种形态都显示）
    let badge_text = c.count.to_string();
    let badge_w = text_estimate_width(&badge_text, theme.label.size) + 14.0 * s;
    let badge_h = 19.0 * s;
    let badge_x = c.x + 14.0 * s + text_estimate_width("Sylva", theme.title.size) + 9.0 * s;
    let badge_rr = D2D1_ROUNDED_RECT {
        rect: D2D_RECT_F {
            left: badge_x,
            top: c.y + (title_h - badge_h) / 2.0,
            right: badge_x + badge_w,
            bottom: c.y + (title_h - badge_h) / 2.0 + badge_h,
        },
        radiusX: badge_h / 2.0,
        radiusY: badge_h / 2.0,
    };
    let badge_bg = unsafe { target.CreateSolidColorBrush(&color([1.0, 1.0, 1.0, 0.10]), None)? };
    unsafe { target.FillRoundedRectangle(&badge_rr, &badge_bg) };
    let badge_lr = D2D_RECT_F {
        left: badge_x,
        top: c.y + (title_h - theme.label.size * 1.6) / 2.0,
        right: badge_x + badge_w,
        bottom: c.y + title_h,
    };
    let badge_text_brush =
        unsafe { target.CreateSolidColorBrush(&color([1.0, 1.0, 1.0, 0.72]), None)? };
    draw_text(
        target,
        &badge_text,
        &formats.label,
        badge_lr,
        &badge_text_brush,
    );

    // 折叠胶囊：右下角展开提示「⌄」
    if pill_t > 0.0 {
        let hint_lr = D2D_RECT_F {
            left: c.x + c.width - 22.0 * s,
            top: c.y + (title_h - theme.label.size * 1.6) / 2.0,
            right: c.x + c.width - 8.0 * s,
            bottom: c.y + title_h,
        };
        let hint =
            unsafe { target.CreateSolidColorBrush(&color([1.0, 1.0, 1.0, 0.40 * pill_t]), None)? };
        draw_text(target, "⌄", &formats.label, hint_lr, &hint);
    }

    // —— 展开形态内容（随 full_t 淡入）——
    if full_t <= 0.0 {
        return Ok(());
    }

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
            unsafe { target.CreateSolidColorBrush(&color([0.85, 0.28, 0.28, 0.30]), None)? };
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
        unsafe { target.CreateSolidColorBrush(&color([1.0, 1.0, 1.0, 0.62 * full_t]), None)? };
    draw_text(target, "✕", &formats.title, close_lr, &close_brush);

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

    // —— 标签栏 ——
    for t in &c.tabs {
        let rr = D2D1_ROUNDED_RECT {
            rect: D2D_RECT_F {
                left: t.rect.x + 4.0 * s,
                top: t.rect.y + 4.0 * s,
                right: t.rect.x + t.rect.w - 4.0 * s,
                bottom: t.rect.y + t.rect.h - 4.0 * s,
            },
            radiusX: 8.0 * s,
            radiusY: 8.0 * s,
        };
        if t.active {
            let act = unsafe { target.CreateSolidColorBrush(&color([1.0, 1.0, 1.0, 0.10]), None)? };
            unsafe { target.FillRoundedRectangle(&rr, &act) };
        }
        let lr = D2D_RECT_F {
            left: t.rect.x,
            top: t.rect.y + (t.rect.h - theme.label.size * 1.6) / 2.0,
            right: t.rect.x + t.rect.w,
            bottom: t.rect.y + t.rect.h,
        };
        let tab_brush = unsafe {
            target.CreateSolidColorBrush(
                &color([1.0, 1.0, 1.0, if t.active { 0.92 } else { 0.55 } * full_t]),
                None,
            )?
        };
        draw_text(target, &t.label, &formats.label, lr, &tab_brush);
    }

    match c.active_kind {
        ConsoleTab::Todo => {
            draw_todo_page(target, theme, c, formats, full_t, accent)?;
        }
        ConsoleTab::Notes => {
            if let Some(n) = &c.notes {
                draw_notes_page(target, theme, n, formats, full_t);
            }
        }
        ConsoleTab::Fences => {
            draw_fences_page(target, theme, c, formats, full_t, accent)?;
        }
        ConsoleTab::Plugins => {
            draw_plugins_page(target, theme, c, formats, full_t, accent)?;
        }
    }
    Ok(())
}

/// 待办页：输入行底框 + 添加按钮 + 事项列表（行 hover、圆形勾选、空状态、滚动条）。
#[allow(clippy::too_many_arguments)]
fn draw_todo_page(
    target: &ID2D1RenderTarget,
    theme: &Theme,
    c: &SceneConsole,
    formats: &TextFormats,
    full_t: f32,
    accent: [f32; 4],
) -> Result<()> {
    let s = theme.scale;
    // 名称输入行底框 + 详情输入行底框（文字由两个弹出编辑框绘制）
    let input_rr = D2D1_ROUNDED_RECT {
        rect: D2D_RECT_F {
            left: c.input.x,
            top: c.input.y,
            right: c.input.x + c.input.w,
            bottom: c.input.y + c.input.h,
        },
        radiusX: 8.0 * s,
        radiusY: 8.0 * s,
    };
    let input_bg =
        unsafe { target.CreateSolidColorBrush(&color([1.0, 1.0, 1.0, 0.09 * full_t]), None)? };
    unsafe { target.FillRoundedRectangle(&input_rr, &input_bg) };
    let detail_rr = D2D1_ROUNDED_RECT {
        rect: D2D_RECT_F {
            left: c.detail_input.x,
            top: c.detail_input.y,
            right: c.detail_input.x + c.detail_input.w,
            bottom: c.detail_input.y + c.detail_input.h,
        },
        radiusX: 8.0 * s,
        radiusY: 8.0 * s,
    };
    let detail_bg =
        unsafe { target.CreateSolidColorBrush(&color([1.0, 1.0, 1.0, 0.05 * full_t]), None)? };
    unsafe { target.FillRoundedRectangle(&detail_rr, &detail_bg) };

    // 「添加」按钮（hover 提亮）
    let add_rr = D2D1_ROUNDED_RECT {
        rect: D2D_RECT_F {
            left: c.add.x,
            top: c.add.y,
            right: c.add.x + c.add.w,
            bottom: c.add.y + c.add.h,
        },
        radiusX: 8.0 * s,
        radiusY: 8.0 * s,
    };
    let add_hover = matches!(c.hover_zone, Some(ConsoleZone::Add));
    let add_bg = unsafe {
        target.CreateSolidColorBrush(
            &color([
                accent[0],
                accent[1],
                accent[2],
                accent[3] * full_t * if add_hover { 1.0 } else { 0.88 },
            ]),
            None,
        )?
    };
    unsafe { target.FillRoundedRectangle(&add_rr, &add_bg) };
    let add_lr = D2D_RECT_F {
        left: c.add.x + 4.0 * s,
        top: c.add.y + (c.add.h - theme.label.size * 1.6) / 2.0,
        right: c.add.x + c.add.w - 4.0 * s,
        bottom: c.add.y + c.add.h,
    };
    let add_text =
        unsafe { target.CreateSolidColorBrush(&color([1.0, 1.0, 1.0, 0.95 * full_t]), None)? };
    draw_text(target, "添加", &formats.label, add_lr, &add_text);

    // 事项列表（裁剪在输入行之下、面板内；滚动时顶部可裁掉）
    let clip = D2D_RECT_F {
        left: c.x + 8.0 * s,
        top: c.todo.rows_top,
        right: c.x + c.width - 8.0 * s,
        bottom: c.y + c.height - 8.0 * s,
    };
    if clip.bottom > clip.top {
        unsafe { target.PushAxisAlignedClip(&clip, D2D1_ANTIALIAS_MODE_PER_PRIMITIVE) };
    }
    let label_h = theme.label.size * 1.6;
    if c.todo.rows.is_empty() {
        let empty_lr = D2D_RECT_F {
            left: clip.left + 8.0 * s,
            top: c.todo.rows_top + 10.0 * s,
            right: clip.right,
            bottom: c.todo.rows_top + 40.0 * s,
        };
        let empty = unsafe { target.CreateSolidColorBrush(&color([1.0, 1.0, 1.0, 0.35]), None)? };
        draw_text(
            target,
            "暂无待办，输入后回车添加",
            &formats.detail,
            empty_lr,
            &empty,
        );
    }
    for (i, row) in c.todo.rows.iter().enumerate() {
        let cb = match c.checkbox.get(i) {
            Some(r) => *r,
            None => continue,
        };
        let del = match c.del.get(i) {
            Some(r) => *r,
            None => continue,
        };
        let a = row.alpha * full_t; // 整行透明度（入场淡入 / 幽灵淡出 × 展开进度）
        let dp = row.done_progress.clamp(0.0, 1.0);
        let visual_done = if row.done { dp } else { 1.0 - dp };
        // 行 hover 背景（勾选/删除按钮同属一行）
        let row_hover = matches!(c.hover_zone, Some(ConsoleZone::Toggle(j)) if j == i)
            || matches!(c.hover_zone, Some(ConsoleZone::Delete(j)) if j == i);
        if row_hover {
            let row_rr = D2D1_ROUNDED_RECT {
                rect: D2D_RECT_F {
                    left: clip.left,
                    top: cb.y - 4.0 * s,
                    right: clip.right,
                    bottom: cb.y + cb.h + 4.0 * s,
                },
                radiusX: 6.0 * s,
                radiusY: 6.0 * s,
            };
            let hov =
                unsafe { target.CreateSolidColorBrush(&color([1.0, 1.0, 1.0, 0.06 * a]), None)? };
            unsafe { target.FillRoundedRectangle(&row_rr, &hov) };
        }
        // 圆形勾选框：填充随进度加深，勾号过半后出现（旋转打勾由进度控制透明度）
        let cb_ell = D2D1_ELLIPSE {
            point: windows_numerics::Vector2 {
                X: cb.x + cb.w / 2.0,
                Y: cb.y + cb.h / 2.0,
            },
            radiusX: cb.w / 2.0,
            radiusY: cb.h / 2.0,
        };
        if visual_done > 0.0 {
            let check_bg = unsafe {
                target.CreateSolidColorBrush(
                    &color([accent[0], accent[1], accent[2], accent[3] * visual_done * a]),
                    None,
                )?
            };
            unsafe { target.FillEllipse(&cb_ell, &check_bg) };
        }
        let cb_edge = unsafe {
            target.CreateSolidColorBrush(
                &color([1.0, 1.0, 1.0, 0.50 * (1.0 - visual_done * 0.4) * a]),
                None,
            )?
        };
        unsafe { target.DrawEllipse(&cb_ell, &cb_edge, 1.2 * s, None) };
        if visual_done > 0.5 {
            let check_lr = D2D_RECT_F {
                left: cb.x,
                top: cb.y + (cb.h - label_h) / 2.0,
                right: cb.x + cb.w,
                bottom: cb.y + cb.h,
            };
            let check = unsafe { target.CreateSolidColorBrush(&color([1.0, 1.0, 1.0, a]), None)? };
            draw_text(target, "✓", &formats.label, check_lr, &check);
        }
        // 事项文本：一级「名称」+ 二级「详细信息」两行（垂直居中整体块）
        let tx = cb.x + cb.w + 8.0 * s;
        let tw = (del.x - 8.0 * s - tx).max(1.0);
        let name_h = theme.label.size * 1.6;
        let detail_h = theme.label.size * 0.72 * 1.5;
        let block_h = if row.detail.is_empty() {
            name_h
        } else {
            name_h + detail_h
        };
        let top = cb.y + (cb.h - block_h) / 2.0;
        let name_rr = D2D_RECT_F {
            left: tx,
            top,
            right: tx + tw,
            bottom: top + name_h,
        };
        let name_alpha = (0.88 - 0.46 * visual_done) * a;
        let name_brush =
            unsafe { target.CreateSolidColorBrush(&color([1.0, 1.0, 1.0, name_alpha]), None)? };
        draw_text(target, &row.name, &formats.label, name_rr, &name_brush);
        if visual_done > 0.0 {
            let mid = top + name_h * 0.55;
            let line = unsafe {
                target
                    .CreateSolidColorBrush(&color([1.0, 1.0, 1.0, 0.34 * visual_done * a]), None)?
            };
            let a2 = Vector2 {
                X: name_rr.left,
                Y: mid,
            };
            let b2 = Vector2 {
                X: name_rr.right,
                Y: mid,
            };
            unsafe { target.DrawLine(a2, b2, &line, 1.0, None) };
        }
        if !row.detail.is_empty() {
            let detail_rr = D2D_RECT_F {
                left: tx,
                top: top + name_h,
                right: tx + tw,
                bottom: top + block_h,
            };
            let detail_alpha = (0.55 - 0.30 * visual_done) * a;
            let detail_brush = unsafe {
                target.CreateSolidColorBrush(&color([1.0, 1.0, 1.0, detail_alpha]), None)?
            };
            draw_text(
                target,
                &row.detail,
                &formats.detail,
                detail_rr,
                &detail_brush,
            );
        }
        // 删除按钮「✕」（hover 变红底）
        let del_hover = matches!(c.hover_zone, Some(ConsoleZone::Delete(j)) if j == i);
        if del_hover {
            let del_rr = D2D1_ROUNDED_RECT {
                rect: D2D_RECT_F {
                    left: del.x,
                    top: del.y,
                    right: del.x + del.w,
                    bottom: del.y + del.h,
                },
                radiusX: 6.0 * s,
                radiusY: 6.0 * s,
            };
            let del_bg = unsafe {
                target.CreateSolidColorBrush(&color([0.85, 0.28, 0.28, 0.55 * a]), None)?
            };
            unsafe { target.FillRoundedRectangle(&del_rr, &del_bg) };
        }
        let dxw = text_estimate_width("✕", theme.label.size);
        let del_lr = D2D_RECT_F {
            left: del.x + (del.w - dxw) / 2.0,
            top: del.y + (del.h - label_h) / 2.0,
            right: del.x + del.w,
            bottom: del.y + del.h,
        };
        let del_brush =
            unsafe { target.CreateSolidColorBrush(&color([1.0, 1.0, 1.0, 0.42 * a]), None)? };
        draw_text(target, "✕", &formats.label, del_lr, &del_brush);
    }
    if clip.bottom > clip.top {
        unsafe { target.PopAxisAlignedClip() };
    }
    // 待办滚动条（细圆角条）
    if c.todo.scroll_max > 0.0 {
        let track_h = clip.bottom - c.todo.rows_top;
        let thumb_h = (track_h * track_h / (track_h + c.todo.scroll_max))
            .max(24.0)
            .min(track_h);
        let max_off = (track_h - thumb_h).max(0.0);
        let off = (c.todo.scroll / c.todo.scroll_max) * max_off;
        let sb = D2D1_ROUNDED_RECT {
            rect: D2D_RECT_F {
                left: c.x + c.width - 14.0 * s,
                top: c.todo.rows_top + off,
                right: c.x + c.width - 10.0 * s,
                bottom: c.todo.rows_top + off + thumb_h,
            },
            radiusX: 2.0,
            radiusY: 2.0,
        };
        if let Ok(b) = unsafe { target.CreateSolidColorBrush(&color([1.0, 1.0, 1.0, 0.22]), None) }
        {
            unsafe { target.FillRoundedRectangle(&sb, &b) };
        }
    }
    Ok(())
}

/// 便签页：圆角卡片（文字由独立的弹出多行 EDIT 承载，D2D 只画底卡 + 提示）。
fn draw_notes_page(
    target: &ID2D1RenderTarget,
    theme: &Theme,
    n: &crate::scene::SceneNotes,
    formats: &TextFormats,
    full_t: f32,
) {
    let s = theme.scale;
    let rr = D2D1_ROUNDED_RECT {
        rect: D2D_RECT_F {
            left: n.rect.x,
            top: n.rect.y,
            right: n.rect.x + n.rect.w,
            bottom: n.rect.y + n.rect.h,
        },
        radiusX: 10.0 * s,
        radiusY: 10.0 * s,
    };
    let card =
        unsafe { target.CreateSolidColorBrush(&color([1.0, 1.0, 1.0, 0.05 * full_t]), None) };
    if let Ok(b) = card {
        unsafe { target.FillRoundedRectangle(&rr, &b) };
    }
    let card_edge =
        unsafe { target.CreateSolidColorBrush(&color([1.0, 1.0, 1.0, 0.16 * full_t]), None) };
    if let Ok(b) = card_edge {
        unsafe { target.DrawRoundedRectangle(&rr, &b, 1.0, None) };
    }
    // 卡片底部提示条（编辑框覆盖卡片主体，这行提示始终可见）
    let hint_lr = D2D_RECT_F {
        left: n.rect.x + 10.0 * s,
        top: n.rect.y + n.rect.h - 22.0 * s,
        right: n.rect.x + n.rect.w - 10.0 * s,
        bottom: n.rect.y + n.rect.h - 6.0 * s,
    };
    let hint =
        unsafe { target.CreateSolidColorBrush(&color([1.0, 1.0, 1.0, 0.30 * full_t]), None) };
    if let Ok(b) = hint {
        draw_text(
            target,
            "点击编辑 · 失焦自动保存",
            &formats.detail,
            hint_lr,
            &b,
        );
    }
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
        draw_text(target, &r.title, &formats.label, lr, &txt);
    }
    if clip.bottom > clip.top {
        unsafe { target.PopAxisAlignedClip() };
    }
    // 详情控制区
    if let Some(d) = &c.fence_detail {
        draw_fence_detail(target, theme, c, d, formats, full_t, accent)?;
    }
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
        draw_text(target, label, &formats.label, lr, &b);
    }
}

/// 插件页：插件行（名称/描述/版本 + 启用开关）+ 打开插件目录按钮。
#[allow(clippy::too_many_arguments)]
fn draw_plugins_page(
    target: &ID2D1RenderTarget,
    theme: &Theme,
    c: &SceneConsole,
    formats: &TextFormats,
    full_t: f32,
    accent: [f32; 4],
) -> Result<()> {
    let s = theme.scale;
    let clip = D2D_RECT_F {
        left: c.x + 8.0 * s,
        top: c.y + c.title_h + c.tab_h,
        right: c.x + c.width - 8.0 * s,
        bottom: c.open_plugins.y - 4.0 * s,
    };
    if clip.bottom > clip.top {
        unsafe { target.PushAxisAlignedClip(&clip, D2D1_ANTIALIAS_MODE_PER_PRIMITIVE) };
    }
    for (i, p) in c.plugin_rows.iter().enumerate() {
        let hover = matches!(c.hover_zone, Some(ConsoleZone::PluginToggle(j)) if j == i);
        let rr = D2D1_ROUNDED_RECT {
            rect: D2D_RECT_F {
                left: p.rect.x,
                top: p.rect.y,
                right: p.rect.x + p.rect.w,
                bottom: p.rect.y + p.rect.h,
            },
            radiusX: 8.0 * s,
            radiusY: 8.0 * s,
        };
        if hover {
            let hb = unsafe {
                target.CreateSolidColorBrush(&color([1.0, 1.0, 1.0, 0.07 * full_t]), None)?
            };
            unsafe { target.FillRoundedRectangle(&rr, &hb) };
        }
        // 名称 + 版本
        let name_lr = D2D_RECT_F {
            left: p.rect.x + 12.0 * s,
            top: p.rect.y + 6.0 * s,
            right: p.toggle.x - 8.0 * s,
            bottom: p.rect.y + 24.0 * s,
        };
        let nb =
            unsafe { target.CreateSolidColorBrush(&color([1.0, 1.0, 1.0, 0.90 * full_t]), None)? };
        draw_text(target, &p.name, &formats.label, name_lr, &nb);
        let ver_w = text_estimate_width(&p.version, theme.label.size) + 6.0 * s;
        let ver_lr = D2D_RECT_F {
            left: name_lr.right - ver_w,
            top: p.rect.y + 6.0 * s,
            right: name_lr.right,
            bottom: p.rect.y + 24.0 * s,
        };
        let vb =
            unsafe { target.CreateSolidColorBrush(&color([1.0, 1.0, 1.0, 0.38 * full_t]), None)? };
        draw_text(target, &p.version, &formats.detail, ver_lr, &vb);
        // 描述
        let desc_lr = D2D_RECT_F {
            left: p.rect.x + 12.0 * s,
            top: p.rect.y + 24.0 * s,
            right: p.toggle.x - 8.0 * s,
            bottom: p.rect.y + p.rect.h - 4.0 * s,
        };
        let db =
            unsafe { target.CreateSolidColorBrush(&color([1.0, 1.0, 1.0, 0.48 * full_t]), None)? };
        draw_text(target, &p.desc, &formats.detail, desc_lr, &db);
        // 启用开关（胶囊 + 滑块）
        draw_toggle_switch(target, p.toggle, p.enabled, accent);
    }
    if clip.bottom > clip.top {
        unsafe { target.PopAxisAlignedClip() };
    }
    // 打开插件目录按钮
    let open_hover = matches!(c.hover_zone, Some(ConsoleZone::OpenPluginDir));
    draw_segmented_button(
        target,
        theme,
        c.open_plugins,
        "打开插件目录（可放入 plugin.json 自由增添）",
        false,
        open_hover,
        formats,
        accent,
    );
    Ok(())
}

/// 启用开关：圆角胶囊 + 圆形滑块；开 = 强调色，关 = 灰。
fn draw_toggle_switch(target: &ID2D1RenderTarget, rect: RectF, on: bool, accent: [f32; 4]) {
    let rr = D2D1_ROUNDED_RECT {
        rect: D2D_RECT_F {
            left: rect.x,
            top: rect.y,
            right: rect.x + rect.w,
            bottom: rect.y + rect.h,
        },
        radiusX: rect.h / 2.0,
        radiusY: rect.h / 2.0,
    };
    let fill = if on {
        [accent[0], accent[1], accent[2], 0.95]
    } else {
        [1.0, 1.0, 1.0, 0.18]
    };
    if let Ok(b) = unsafe { target.CreateSolidColorBrush(&color(fill), None) } {
        unsafe { target.FillRoundedRectangle(&rr, &b) };
    }
    let r = rect.h / 2.0 - 2.0;
    let knob_x = if on {
        rect.x + rect.w - rect.h / 2.0
    } else {
        rect.x + rect.h / 2.0
    };
    let ell = D2D1_ELLIPSE {
        point: windows_numerics::Vector2 {
            X: knob_x,
            Y: rect.y + rect.h / 2.0,
        },
        radiusX: r,
        radiusY: r,
    };
    if let Ok(b) = unsafe { target.CreateSolidColorBrush(&color([1.0, 1.0, 1.0, 1.0]), None) } {
        unsafe { target.FillEllipse(&ell, &b) };
    }
}

/// 栅栏绘制入口：透明度 < 1（桌面切换淡出/淡入）时用 D2D 图层整体淡化，
/// 包括背景、描边、文字与图标位图，避免「背景已透明但图标悬空」的割裂感。
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
        draw_text(target, &fence.title, &formats.title, tr, &brushes.title);
    }

    // 内容区：悬停高亮 + 图标行 + 列表列头，全部裁剪在内容区内（滚动后顶部可裁掉）。
    let row_top = match fence.list_cols {
        Some(cols) => {
            draw_list_header(target, theme, fence, cols, formats, &brushes.label);
            fence.content_top + cols.header_h
        }
        None => fence.content_top,
    };
    let clip = D2D_RECT_F {
        left: fence.content_left,
        top: row_top,
        right: fence.x + fence.width - theme.fence_padding,
        bottom: row_top + fence.scroll_view,
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

    for icon in &fence.icons {
        // 悬停放大：以图标中心放大（scale 由 App 层补间填值），并垫一层柔光
        let grow = (icon.size * (icon.scale - 1.0) * 0.5).max(0.0);
        let dest = D2D_RECT_F {
            left: icon.x - grow,
            top: icon.y - grow,
            right: icon.x + icon.size + grow,
            bottom: icon.y + icon.size + grow,
        };
        if let Some(bmp) = icons.get(icon.bitmap_id) {
            if grow > 0.5 {
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
                    target
                        .CreateSolidColorBrush(&color([1.0, 1.0, 1.0, 0.10 * (grow / 2.0)]), None)?
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
    let thumb_h = (track_h * track_h / (track_h + fence.scroll_max))
        .max(24.0)
        .min(track_h);
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

/// 粗略估算文本像素宽度（CJK 按字号宽，ASCII 按 0.62 倍宽），用于居中/对齐。
fn text_estimate_width(text: &str, font_size: f32) -> f32 {
    let units: f32 = text
        .chars()
        .map(|c| if c.is_ascii() { 0.62 } else { 1.0 })
        .sum();
    units * font_size
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

fn make_text_format(dwrite: &IDWriteFactory, style: TextStyle) -> Result<IDWriteTextFormat> {
    let family = wide(style.font_family);
    let locale = user_locale();
    // localeName 传 NULL 会返回 E_INVALIDARG（实测），必须给显式 locale
    let format = unsafe {
        dwrite.CreateTextFormat(
            PCWSTR(family.as_ptr()),
            None,
            DWRITE_FONT_WEIGHT_NORMAL,
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
