//! 场景构建：从领域模型 + 主题排布出渲染场景（栅栏/侧边栏/控制台/命中模型）。

use crate::*;
pub(crate) fn system_dark_mode() -> bool {
    use windows::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD};
    let subkey = wide(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize");
    let value = wide("AppsUseLightTheme");
    let mut data: u32 = 1;
    let mut size = std::mem::size_of::<u32>() as u32;
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            PCWSTR(subkey.as_ptr()),
            PCWSTR(value.as_ptr()),
            RRF_RT_REG_DWORD,
            None,
            Some(&mut data as *mut u32 as *mut core::ffi::c_void),
            Some(&mut size),
        )
    };
    status.is_ok() && data == 0
}

/// 列表布局下建议的栅栏宽度：名称列 + 类型/修改日期/大小三列 + 内边距。
pub(crate) fn list_auto_width(rt: &Runtime, fence: usize) -> f32 {
    let f = &rt.desk.fences[fence];
    let s = rt.theme.scale;
    let max_w: f32 = f
        .icon_ids
        .iter()
        .filter_map(|id| rt.desk.icons.get(id))
        .map(|ic| label_width(&ic.display_name, rt.theme.label.size))
        .fold(0.0, f32::max);
    let fixed = (LIST_TYPE_W + LIST_MOD_W + LIST_SIZE_W + LIST_COL_GAP * 3.0) * s;
    let base =
        f.appearance.padding * s * 2.0 + LIST_ICON_SIZE * s + rt.theme.list_label_gap + max_w;
    (base + fixed).max(420.0 * s)
}

/// 粗略估算标签文本宽度（CJK 按字号宽，ASCII 按 0.62 倍宽）。
pub(crate) fn label_width(text: &str, font_size: f32) -> f32 {
    let units: f32 = text
        .chars()
        .map(|c| if c.is_ascii() { 0.62 } else { 1.0 })
        .sum();
    units * font_size
}

/// 首次运行：把全部图标按稳定顺序平分成两个演示栅栏，并建立图标元数据。
/// 已有布局时不作任何改动（栅栏是用户显式成员列表）。
pub(crate) fn seed_fences(desk: &mut Desk, items: &[DesktopItem], theme: &Theme) {
    if !desk.fences.is_empty() || items.is_empty() {
        return;
    }
    // 元数据落库：图标按稳定 id 持久化，供渲染标签与成员引用
    for it in items {
        let mut ic = Icon::new(it.id.clone(), it.display_name.clone(), it.kind);
        ic.path = it.path.clone();
        if let Some(p) = it.path.as_deref() {
            sylva_core::details::enrich(&mut ic, p);
        }
        desk.icons.entry(it.id.clone()).or_insert(ic);
    }

    let cell = theme.icon_size + theme.icon_gap;
    let w = theme.fence_padding * 2.0 + theme.icon_cols as f32 * cell - theme.icon_gap;
    let empty: HashMap<String, u64> = HashMap::new();
    let (a, b) = items.split_at(items.len().div_ceil(2));

    let f1 = Fence {
        id: desk.next_fence_id(),
        title: Some("常用".into()),
        monitor_id: 0,
        bounds: Rect::new(80.0, 80.0, w, 0.0), // 高度由布局自适应
        state: FenceState::Expanded,
        icon_ids: a.iter().map(|it| it.id.clone()).collect(),
        appearance: FenceAppearance::default(),
        scroll: 0.0,
        storage_path: None,
        sidebar_collapsed: false,
    };
    // 用布局算出的高度定位第二个栅栏（首栅栏暂不在 desk 中，单独算一次几何）
    let sf1 = layout_fence(theme, &f1, desk, &empty, None, &[], None, 0);

    // 第二个栅栏用「透明」风格演示多种背景风格
    let app2 = FenceAppearance {
        bg_style: FenceStyle::Outline,
        ..FenceAppearance::default()
    };
    let f2 = Fence {
        id: desk.next_fence_id(),
        title: Some("其他".into()),
        monitor_id: 0,
        bounds: Rect::new(80.0, 80.0 + sf1.height + 40.0, w, 0.0),
        state: FenceState::Expanded,
        icon_ids: b.iter().map(|it| it.id.clone()).collect(),
        appearance: app2,
        scroll: 0.0,
        storage_path: None,
        sidebar_collapsed: false,
    };

    desk.fences.push(f1);
    desk.fences.push(f2);
    tracing::info!(
        fences = desk.fences.len(),
        icons = desk.icons.len(),
        "首次运行：已创建演示栅栏布局"
    );
}

/// 把整个桌面状态排布成场景（每个栅栏按网格/列表排布；内容超出滚动）。
pub(crate) fn build_scene(rt: &mut Runtime, now: Instant) -> Scene {
    let alpha = fence_alpha(rt, now);
    let mut scene = Scene::new(rt.vw, rt.vh);
    for i in 0..rt.desk.fences.len() {
        // 视觉矩形：拖拽/缩放补间期间用插值（模型已是目标值，场景跟随动画）
        let mut f = rt.desk.fences[i].clone();
        f.bounds = fence_visual_rect(rt, i);
        let mut sf = layout_fence(
            &rt.theme,
            &f,
            &rt.desk,
            &rt.bitmap_ids,
            rt.hover,
            &rt.selected,
            rt.select_band,
            i,
        );
        // 正在重命名栅栏标题/图标时，隐藏原始文字避免与编辑框重叠（编辑框独立绘制）
        if let Some(edit) = &rt.edit {
            match edit.target {
                EditTarget::FenceTitle { fence: ef } if ef == i => sf.title.clear(),
                EditTarget::Item {
                    fence: ef,
                    icon: ei,
                } if ef == i => {
                    if let Some(ic) = sf.icons.get_mut(ei) {
                        ic.label.clear();
                    }
                }
                _ => {}
            }
        }
        // 侧边栏布局：不显示标题（Dock 无标题）；固定为完整 dock，无折叠状态。
        if f.appearance.layout == FenceLayout::Sidebar {
            sf.title.clear();
        }
        // 桌面切换整体淡入淡出
        sf.alpha = alpha;
        // 悬停放大：普通布局（网格/列表）在循环内做；侧边栏 Dock 放大
        // 依赖全部栅栏的真实几何，放到循环结束后统一处理（见 build_dock_magnify）。
        if f.appearance.layout != FenceLayout::Sidebar {
            if let Some((hf, hi)) = rt.hover {
                if hf == i {
                    if let Some(ic) = sf.icons.get_mut(hi) {
                        ic.scale = icon_hover_scale(rt, hf, hi, now);
                    }
                }
            }
        }
        // 回写钳制后的滚动偏移（滚轮事件在 layout 内被限制在 [0, max_scroll]）
        rt.desk.fences[i].scroll = sf.scroll;
        // 模糊风格无需任何截图/CPU 处理：`sf.blur` 已由 layout_fence 置位，
        // 合成器据此建/删该栅栏的 BackdropBrush + GaussianBlurEffect 视觉（GPU 实时）。
        scene.fences.push(sf);
    }
    // 侧边栏 Dock 放大（第二遍：全部栅栏真实几何已就绪，光标若落在其他栅栏内则
    // 不放大，避免影响半径越过网格/列表抢走它们的悬停焦点）。
    build_dock_magnify(rt, &mut scene.fences);
    // 控制中心：关闭后完全不渲染（不留胶囊）；展开动画期间 panel > 0 才画。
    let panel = rt.console_anim.panel;
    if rt.desk.console_open || panel > 0.01 {
        scene.console = Some(build_console(rt, &rt.console_anim));
    }
    // 内联编辑（最后绘制，浮于所有内容之上）
    scene.edit = rt.edit.as_ref().map(|e| SceneEdit {
        rect: e.rect,
        lines: e.lines.clone(),
        line: e.line,
        col: e.col,
        placeholder: e.placeholder.clone(),
        single_line: e.single_line,
        focused: e.focused,
        composing: e.composing,
        comp: e.comp.clone(),
    });
    scene
}

/// 栅栏整体透明度（0=隐藏，1=完全显示）：桌面切换补间推进中取插值，否则按模式取终值。
pub(crate) fn console_full_height(desk: &Desk, s: f32) -> f32 {
    let title_h = CONSOLE_TITLE_H * s;
    let rows = desk.fences.len().min(CONSOLE_FENCE_MAX_ROWS) as f32 * CONSOLE_FENCE_ROW_H * s;
    title_h
        + 8.0 * s
        + rows
        + 8.0 * s
        + CONSOLE_FENCE_DETAIL_H * s
        + 8.0 * s
        // 添加 / 删除栅栏 / 切换桌面 三个等宽按钮 + 两处间隙
        + CONSOLE_ADD_BTN_H * s * 3.0
        + 8.0 * s * 2.0
        + 12.0 * s.clamp(CONSOLE_MIN_H * s, CONSOLE_MAX_H * s)
}

/// 控制中心面板矩形（物理像素，虚拟屏幕坐标）。
///
/// 未拖动过（`console_pos == None`）时默认摆右上角；拖动后记住 `console_pos`
/// 左上角。高度按 `panel` 进度（0..1）在「折叠胶囊」与「完整面板」间插值——
/// 胶囊始终可见（控制中心入口不会再找不到）。
///
/// 尺寸策略：未手动缩放过（`console_size == None`）时宽取 `CONSOLE_W`、高取
/// `console_full_height`（标签页/内容自适应）；用户拖边缘/角缩放后
/// `console_size` 落为具体宽高（钳制在最小尺寸之上），之后高度固定、超出滚动。
pub(crate) fn console_geometry(desk: &Desk, theme: &Theme, vw: f32, _vh: f32, panel: f32) -> RectF {
    let s = theme.scale;
    let auto_full_h = console_full_height(desk, s);
    let (w, full_h) = match desk.console_size {
        Some((w, h)) => (w.max(CONSOLE_MIN_W * s), h.max(CONSOLE_MIN_H * s)),
        None => (CONSOLE_W * s, auto_full_h),
    };
    // 允许小幅过冲（展开回弹），1.12 上限避免面板瞬时过高
    let h = full_h * panel.clamp(0.0, 1.12);
    let (x, y) = match desk.console_pos {
        Some(p) => (p.x, p.y),
        None => (
            (vw - w - CONSOLE_MARGIN * s).max(8.0 * s),
            (CONSOLE_MARGIN * s).max(8.0 * s),
        ),
    };
    RectF { x, y, w, h }
}

/// 构建控制中心面板场景（栅栏管理单页）。关闭后完全隐藏（无胶囊），
/// 高度按 `anim.panel` 从 0 插值到完整面板。
pub(crate) fn build_console(rt: &Runtime, anim: &ConsoleAnim) -> SceneConsole {
    let desk = &rt.desk;
    let theme = &rt.theme;
    let s = theme.scale;
    let panel = console_geometry(desk, theme, rt.vw, rt.vh, anim.panel);
    let title_h = CONSOLE_TITLE_H * s;
    let content_top = panel.y + title_h;

    // —— 标题栏：关闭 + 切换桌面 ——
    let close = RectF {
        x: panel.x + panel.w - CONSOLE_CLOSE_W * s - 8.0 * s,
        y: panel.y + 8.0 * s,
        w: CONSOLE_CLOSE_W * s,
        h: CONSOLE_CLOSE_W * s,
    };
    // —— 栅栏管理页 ——
    let list_top = content_top + 8.0 * s;
    let row_h_f = CONSOLE_FENCE_ROW_H * s;
    let fence_n = desk.fences.len();
    let fence_shown = fence_n.min(CONSOLE_FENCE_MAX_ROWS);
    let fence_scroll_max = if fence_n > fence_shown {
        (fence_n - fence_shown) as f32 * row_h_f
    } else {
        0.0
    };
    let fence_scroll = rt.fence_scroll.clamp(0.0, fence_scroll_max);
    let fence_list_view = RectF {
        x: panel.x + CONSOLE_PAD * s,
        y: list_top,
        w: panel.w - 2.0 * CONSOLE_PAD * s,
        h: fence_shown as f32 * row_h_f,
    };
    let sel = rt.selected_fence.min(fence_n.saturating_sub(1));
    let fence_rows: Vec<SceneFenceRow> = desk
        .fences
        .iter()
        .enumerate()
        .map(|(i, f)| SceneFenceRow {
            rect: RectF {
                x: panel.x + CONSOLE_PAD * s,
                y: list_top + i as f32 * row_h_f - fence_scroll,
                w: panel.w - 2.0 * CONSOLE_PAD * s,
                h: row_h_f,
            },
            title: f.title.clone().unwrap_or_else(|| format!("栅栏 {}", i + 1)),
            selected: i == sel,
        })
        .collect();
    let fence_detail = if fence_n > 0 {
        let d = RectF {
            x: panel.x + CONSOLE_PAD * s,
            y: list_top + fence_shown as f32 * row_h_f + 8.0 * s,
            w: panel.w - 2.0 * CONSOLE_PAD * s,
            h: CONSOLE_FENCE_DETAIL_H * s,
        };
        let app = &desk.fences[sel].appearance;
        let btn_h = 24.0 * s;
        let label_w = 40.0 * s;
        let row_y = |i: usize| d.y + 24.0 * s + i as f32 * 30.0 * s;
        let layout_grid = RectF {
            x: d.x + label_w,
            y: row_y(0),
            w: 52.0 * s,
            h: btn_h,
        };
        let layout_list = RectF {
            x: layout_grid.x + layout_grid.w + 6.0 * s,
            y: row_y(0),
            w: 52.0 * s,
            h: btn_h,
        };
        let layout_sidebar = RectF {
            x: layout_list.x + layout_list.w + 6.0 * s,
            y: row_y(0),
            w: 52.0 * s,
            h: btn_h,
        };
        let size_s = RectF {
            x: d.x + label_w,
            y: row_y(1),
            w: 40.0 * s,
            h: btn_h,
        };
        let size_m = RectF {
            x: size_s.x + size_s.w + 6.0 * s,
            y: row_y(1),
            w: 40.0 * s,
            h: btn_h,
        };
        let size_l = RectF {
            x: size_m.x + size_m.w + 6.0 * s,
            y: row_y(1),
            w: 40.0 * s,
            h: btn_h,
        };
        let style_glass = RectF {
            x: d.x + label_w,
            y: row_y(2),
            w: 48.0 * s,
            h: btn_h,
        };
        let style_outline = RectF {
            x: style_glass.x + style_glass.w + 6.0 * s,
            y: row_y(2),
            w: 48.0 * s,
            h: btn_h,
        };
        let style_filled = RectF {
            x: style_outline.x + style_outline.w + 6.0 * s,
            y: row_y(2),
            w: 48.0 * s,
            h: btn_h,
        };
        let style_blur = RectF {
            x: style_filled.x + style_filled.w + 6.0 * s,
            y: row_y(2),
            w: 48.0 * s,
            h: btn_h,
        };
        let sw = 18.0 * s;
        let gap = 6.0 * s;
        let tint_y = row_y(3) + (btn_h - sw) / 2.0;
        let tint_default = RectF {
            x: d.x + label_w,
            y: tint_y,
            w: sw,
            h: sw,
        };
        let mut tints = Vec::with_capacity(TINT_PRESETS.len());
        let mut x = tint_default.x + sw + gap;
        for _ in TINT_PRESETS {
            tints.push(RectF {
                x,
                y: tint_y,
                w: sw,
                h: sw,
            });
            x += sw + gap;
        }
        // 「更改位置…」按钮（存储位置行，第 5 行）
        let storage_btn = RectF {
            x: d.x + label_w,
            y: row_y(4),
            w: 120.0 * s,
            h: btn_h,
        };
        // 侧边栏位置按钮（第 6 行，仅 Sidebar 布局有效）
        let sidebar_left = RectF {
            x: d.x + label_w,
            y: row_y(5),
            w: 40.0 * s,
            h: btn_h,
        };
        let sidebar_top = RectF {
            x: sidebar_left.x + sidebar_left.w + 6.0 * s,
            y: row_y(5),
            w: 40.0 * s,
            h: btn_h,
        };
        let sidebar_right = RectF {
            x: sidebar_top.x + sidebar_top.w + 6.0 * s,
            y: row_y(5),
            w: 40.0 * s,
            h: btn_h,
        };
        Some(SceneFenceDetail {
            rect: d,
            title: desk.fences[sel]
                .title
                .clone()
                .unwrap_or_else(|| format!("栅栏 {}", sel + 1)),
            layout: app.layout,
            icon_size: app.icon_size,
            style: app.bg_style,
            tint: app.tint,
            layout_grid,
            layout_list,
            layout_sidebar,
            size_s,
            size_m,
            size_l,
            style_glass,
            style_outline,
            style_filled,
            style_blur,
            tint_default,
            tints,
            storage_btn,
            sidebar_pos: app.sidebar_pos,
            sidebar_left,
            sidebar_top,
            sidebar_right,
        })
    } else {
        None
    };
    // —— 底部操作按钮：添加栅栏 / 删除栅栏 / 切换桌面（自下而上，等宽等高） ——
    let btn_w = panel.w - 2.0 * CONSOLE_PAD * s;
    let btn_h = CONSOLE_ADD_BTN_H * s;
    let btn_gap = 8.0 * s;
    let btn_top =
        list_top + fence_shown as f32 * row_h_f + 8.0 * s + CONSOLE_FENCE_DETAIL_H * s + 8.0 * s;
    let add_fence = RectF {
        x: panel.x + CONSOLE_PAD * s,
        y: btn_top,
        w: btn_w,
        h: btn_h,
    };
    // 「删除栅栏」按钮：放在「添加栅栏」正下方，等宽等高。
    let remove_btn = RectF {
        x: add_fence.x,
        y: add_fence.y + btn_h + btn_gap,
        w: btn_w,
        h: btn_h,
    };
    // 「切回桌面 / 切换桌面」按钮：移到删除栅栏下方。
    let desktop_toggle = RectF {
        x: add_fence.x,
        y: remove_btn.y + btn_h + btn_gap,
        w: btn_w,
        h: btn_h,
    };

    SceneConsole {
        x: panel.x,
        y: panel.y,
        width: panel.w,
        height: panel.h,
        title_h,
        close,
        desktop_toggle,
        fence_rows,
        fence_list_view,
        fence_detail,
        add_fence,
        remove_btn,
        fill_color: [0.062, 0.086, 0.133, 0.92],
        border_color: [1.0, 1.0, 1.0, 0.18],
        panel: anim.panel,
        hover_zone: if anim.panel >= 0.5 {
            rt.console_hover
        } else {
            None
        },
        desktop_mode: desk.desktop_mode,
    }
}

/// 构建一个桌面小组件的场景（待办/便签卡片；位置尺寸 = DIP × scale）。
/// 列表布局详情列的固定宽度与间距（物理像素）。
pub(crate) const LIST_TYPE_W: f32 = 90.0;
pub(crate) const LIST_MOD_W: f32 = 140.0;
pub(crate) const LIST_SIZE_W: f32 = 80.0;
pub(crate) const LIST_COL_GAP: f32 = 16.0;
/// 列表行内的小图标尺寸（详情列表风格，与「图标大小」无关）。
pub(crate) const LIST_ICON_SIZE: f32 = 20.0;
/// 列表未手动缩放时的最大可见行数（超出滚动）。
pub(crate) const LIST_AUTO_ROWS: usize = 8;

/// 单个栅栏的排布（网格 / 列表）。
///
/// - 宽度：用户控制的 `fence.bounds.w`（列表切过去时按最长标签自动收窄）；
/// - 高度：未手动缩放过（`bounds.h <= 0`）时按内容自适应——网格长满、列表最多
///   8 行（超出滚动）；手动缩放过则固定，内容超出后用滚轮滚动（右缘有指示条）；
/// - 网格：自左向右、自上而下排布，标签在图标下方；
/// - 列表：单列纵向，名称/类型/修改日期/大小四列 + 固定列头，滚轮滚动。
/// - 滚动：所有图标都在场景里（位置按 `scroll` 平移），绘制时用内容区裁剪，
///   命中模型跳过滚出可视区的项。返回的 `SceneFence.scroll` 已被钳制，
///   `build_scene` 据此回写 `desk.fences[i].scroll`。
#[allow(clippy::too_many_arguments)]
pub(crate) fn layout_fence(
    theme: &Theme,
    fence: &Fence,
    desk: &Desk,
    bitmap_ids: &HashMap<String, u64>,
    hover: Option<(usize, usize)>,
    selected: &[(usize, usize)],
    select_band: Option<(usize, RectF)>,
    fence_idx: usize,
) -> SceneFence {
    let app = &fence.appearance;
    // 模型里的外观是 DIP 逻辑值（持久化，跨 DPI 稳定），布局时 × scale 变物理像素。
    let scale = theme.scale;
    let pad = app.padding * scale;
    let n = fence.icon_ids.len();
    let title_block_h = theme.title.size * 1.6 + theme.title_padding_bottom;
    // 侧边栏无标题栏：内容顶边 = 栅栏顶部 + 内边距。否则标题块高度会把顶部
    // 图标整体裁掉（内容裁剪区从 content_top 起算，而侧边栏图标从 y+pad 起排）。
    let content_top = if app.layout == FenceLayout::Sidebar {
        fence.bounds.y + pad
    } else {
        fence.bounds.y + pad + title_block_h + pad
    };
    let content_left = fence.bounds.x + pad;
    let inner_w = (fence.bounds.w - 2.0 * pad).max(1.0);

    // 预取名称 + 位图 + 详情列文本（布局与绘制共用）。
    let rows: Vec<(String, u64, String, String, String)> = fence
        .icon_ids
        .iter()
        .map(|id| {
            let ic = desk.icons.get(id);
            let label = ic.map(|i| i.display_name.clone()).unwrap_or_default();
            let bitmap = bitmap_ids.get(id).copied().unwrap_or(u64::MAX);
            match ic {
                Some(i) => (
                    label,
                    bitmap,
                    i.type_label.clone(),
                    sylva_shell::time::format_modified(i.modified_secs),
                    sylva_core::details::format_size(i.size_bytes),
                ),
                None => (label, bitmap, String::new(), String::new(), String::new()),
            }
        })
        .collect();

    let hover_icon = hover.filter(|&(fi, _)| fi == fence_idx).map(|(_, ii)| ii);
    let selected: Vec<usize> = selected
        .iter()
        .filter(|&&(fi, _)| fi == fence_idx)
        .map(|&(_, ii)| ii)
        .collect();
    let select_band = select_band
        .filter(|&(fi, _)| fi == fence_idx)
        .map(|(_, r)| r);

    let (icons, scroll, scroll_max, scroll_view, list_cols, height) = match app.layout {
        FenceLayout::Grid => {
            let icon_size = app.icon_size * scale;
            let gap = app.gap * scale;
            // 横向步距：图标 + 图标间距；纵向步距：图标 + 标签间距 + 标签高。
            // 二者必须分开——共用一个 cell 会让标签叠进下一行图标（行列重叠根因）。
            let cell_w = icon_size + gap;
            let row_h = icon_size + theme.icon_caption_gap + theme.label.size * 1.6;
            let cols = ((inner_w / cell_w).floor() as usize).max(1);
            let rows_n = n.div_ceil(cols).max(1);
            // 内容总高 = 每行（图标 + 标签）累加，末行标签也有高度
            let content_full = rows_n as f32 * row_h;
            let auto_h = pad + title_block_h + pad + content_full + pad;
            let h = if fence.bounds.h > 0.0 {
                fence.bounds.h.max(MIN_FENCE_H)
            } else {
                auto_h
            };
            let view = (h - pad - title_block_h - pad - pad).max(0.0);
            let scroll_max = (content_full - view).max(0.0);
            let scroll = fence.scroll.clamp(0.0, scroll_max);
            let icons = grid_icons(
                theme,
                fence,
                &rows,
                cols,
                cell_w,
                row_h,
                content_top,
                content_left,
                scroll,
                icon_size,
            );
            (icons, scroll, scroll_max, view, None, h)
        }
        FenceLayout::List => {
            let label_h = theme.label.size * 1.6;
            let list_icon = LIST_ICON_SIZE * scale;
            // 行高至少容纳标签高（或图标高），再加行距；否则 24px 文字叠进下一行
            let row_h = list_icon.max(label_h) + theme.list_row_gap;
            let header_h = label_h + 8.0 * scale;
            // 内容总高（所有行）
            let content_full = if n > 0 {
                n as f32 * row_h - theme.list_row_gap
            } else {
                0.0
            };
            // 未手动缩放：最多显示 LIST_AUTO_ROWS 行，超出滚动
            let auto_rows = n.clamp(1, LIST_AUTO_ROWS);
            let auto_rows_h = (auto_rows as f32 * row_h - theme.list_row_gap).max(0.0);
            let auto_h = pad + title_block_h + pad + header_h + auto_rows_h + pad;
            let h = if fence.bounds.h > 0.0 {
                fence.bounds.h.max(MIN_FENCE_H)
            } else {
                auto_h
            };
            let view = (h - pad - title_block_h - pad - header_h - pad).max(0.0);
            let scroll_max = (content_full - view).max(0.0);
            let scroll = fence.scroll.clamp(0.0, scroll_max);
            // 四列：名称列吃剩余宽度，其余固定（列宽同样 × scale）
            let type_w = LIST_TYPE_W * scale;
            let mod_w = LIST_MOD_W * scale;
            let size_w = LIST_SIZE_W * scale;
            let col_gap = LIST_COL_GAP * scale;
            let name_w = (inner_w - col_gap * 3.0 - type_w - mod_w - size_w).max(60.0 * scale);
            let type_x = content_left + name_w + col_gap;
            let modified_x = type_x + type_w + col_gap;
            let size_x = modified_x + mod_w + col_gap;
            let cols = ListColumns {
                type_x,
                modified_x,
                size_x,
                header_h,
            };
            let icons = list_icons(
                theme,
                fence,
                &rows,
                content_top,
                content_left,
                header_h,
                row_h,
                scroll,
                list_icon,
            );
            (icons, scroll, scroll_max, view, Some(cols), h)
        }
        FenceLayout::Sidebar => {
            let icon_size = app.icon_size * scale;
            let gap = app.gap * scale;
            let is_vert = app.sidebar_pos != SidebarPosition::Top;
            // 侧边栏：无标题栏、无标签，仅图标排列。间距用有效间距（放大不重叠）。
            let eff_gap = sidebar_eff_gap(icon_size, gap);
            let content_full = if n > 0 {
                n as f32 * (icon_size + eff_gap) - eff_gap
            } else {
                0.0
            };
            if is_vert {
                // 纵向（左/右）：宽 = 紧贴放大图标的停靠宽（图标水平居中），
                // 高 = 内容自适应或用户缩放。
                let auto_h = pad + content_full + pad;
                let h = if fence.bounds.h > 0.0 {
                    fence.bounds.h.max(MIN_FENCE_H)
                } else {
                    auto_h
                };
                let view = (h - pad - pad).max(0.0);
                let scroll_max = (content_full - view).max(0.0);
                let scroll = fence.scroll.clamp(0.0, scroll_max);
                // 图标在 dock 内水平居中：放大围绕中心展开，1.5× 时两侧对称留白。
                let dock_w = fence.bounds.w.max(icon_size);
                let start_x = fence.bounds.x + (dock_w - icon_size) / 2.0;
                let icons = sidebar_icons(
                    fence,
                    &rows,
                    icon_size,
                    eff_gap,
                    start_x,
                    fence.bounds.y + pad,
                    scroll,
                    true,
                );
                (icons, scroll, scroll_max, view, None, h)
            } else {
                // 横向（上侧）：厚度 = 紧贴放大图标的停靠高（图标垂直居中），
                // 宽 = 内容自适应或用户缩放。
                let auto_w = pad + content_full + pad;
                let w = if fence.bounds.w > 0.0 {
                    fence.bounds.w.max(MIN_FENCE_W)
                } else {
                    auto_w
                };
                let view = (w - pad - pad).max(0.0);
                let scroll_max = (content_full - view).max(0.0);
                let scroll = fence.scroll.clamp(0.0, scroll_max);
                let h = sidebar_dock_thickness(icon_size, scale);
                let start_y = fence.bounds.y + (h - icon_size) / 2.0;
                let icons = sidebar_icons(
                    fence,
                    &rows,
                    icon_size,
                    eff_gap,
                    fence.bounds.x + pad,
                    start_y,
                    scroll,
                    false,
                );
                (icons, scroll, scroll_max, view, None, h)
            }
        }
    };

    // 背景填充按「背景风格」决定（玻璃 / 透明 / 颜色），与旧的透明度滑块无关：
    // - 颜色（Filled）：不透明纯色，颜色 = 背景色调（未选时用默认背景色）；
    // - 玻璃（Glass）：半透明玻璃底，默认底色并向「背景色调」靠拢 45%（保留暗底质感）；
    // - 透明（Outline）：完全透明，只留圆角描边（fill_color = None，边框照常绘制）。
    let bg = app.bg_color;
    let tint_rgb = match app.tint {
        Some(t) => [t[0], t[1], t[2]],
        None => [bg[0], bg[1], bg[2]],
    };
    let glass_rgb = match app.tint {
        Some(t) => [
            bg[0] + (t[0] - bg[0]) * 0.45,
            bg[1] + (t[1] - bg[1]) * 0.45,
            bg[2] + (t[2] - bg[2]) * 0.45,
        ],
        None => [bg[0], bg[1], bg[2]],
    };
    let fill_color = match app.bg_style {
        FenceStyle::Filled => Some([tint_rgb[0], tint_rgb[1], tint_rgb[2], 1.0]),
        FenceStyle::Glass => Some([glass_rgb[0], glass_rgb[1], glass_rgb[2], 0.55]),
        FenceStyle::Outline => None,
        // 模糊：无实心填充，背景由合成器里独立的 GaussianBlurEffect 视觉提供
        // （GPU 实时，BackdropBrush 采样窗口背后桌面），内容区透明透出。
        FenceStyle::Blur => None,
    };
    // 圆角矩形边框跟随 Windows 主题（深色=白、浅色=黑），中粗固定宽度；
    // 透明度保持清晰可见（暗色 42% / 浅色 45%）。
    let border_color = if system_dark_mode() {
        [1.0, 1.0, 1.0, 0.42]
    } else {
        [0.0, 0.0, 0.0, 0.45]
    };
    let border_width = MEDIUM_BORDER_WIDTH * scale;

    SceneFence {
        x: fence.bounds.x,
        y: fence.bounds.y,
        width: fence.bounds.w,
        height,
        title: fence.title.clone().unwrap_or_default(),
        icons,
        layout: app.layout,
        list_cols,
        scroll,
        scroll_max,
        scroll_view,
        content_top,
        content_left,
        hover_icon,
        selected,
        select_band,
        border_width,
        border_color,
        fill_color,
        // 模糊风格：合成器据此为该栅栏建 BackdropBrush + 高斯模糊视觉（GPU 实时）
        blur: app.bg_style == FenceStyle::Blur,
        alpha: 1.0,
        // 侧边栏工具提示矩形由 build_dock_magnify 在第二遍计算后填入
        tooltip_rect: None,
    }
}

/// 网格排布全部图标位置（不裁剪；滚动用 `scroll` 平移，绘制时裁剪）。
#[allow(clippy::too_many_arguments)]
pub(crate) fn grid_icons(
    theme: &Theme,
    _fence: &Fence,
    rows: &[(String, u64, String, String, String)],
    cols: usize,
    cell_w: f32,
    row_h: f32,
    content_top: f32,
    content_left: f32,
    scroll: f32,
    icon_size: f32,
) -> Vec<SceneIcon> {
    let _ = theme;
    rows.iter()
        .enumerate()
        .map(|(i, (label, bitmap, ct, cm, cs))| SceneIcon {
            label: label.clone(),
            bitmap_id: *bitmap,
            x: content_left + (i % cols) as f32 * cell_w,
            y: content_top + (i / cols) as f32 * row_h - scroll,
            size: icon_size,
            col_type: ct.clone(),
            col_modified: cm.clone(),
            col_size: cs.clone(),
            scale: 1.0,
        })
        .collect()
}

/// 列表排布全部图标位置（单列纵向；滚动用 `scroll` 平移，绘制时裁剪）。
#[allow(clippy::too_many_arguments)]
pub(crate) fn list_icons(
    theme: &Theme,
    fence: &Fence,
    rows: &[(String, u64, String, String, String)],
    content_top: f32,
    content_left: f32,
    header_h: f32,
    row_h: f32,
    scroll: f32,
    size: f32,
) -> Vec<SceneIcon> {
    let _ = (theme, fence);
    rows.iter()
        .enumerate()
        .map(|(i, (label, bitmap, ct, cm, cs))| SceneIcon {
            label: label.clone(),
            bitmap_id: *bitmap,
            x: content_left,
            y: content_top + header_h + i as f32 * row_h - scroll,
            size,
            col_type: ct.clone(),
            col_modified: cm.clone(),
            col_size: cs.clone(),
            scale: 1.0,
        })
        .collect()
}

/// 计算侧边栏的推荐停靠矩形（位置切换 / 首次切换 / 启动归一化共用）。
///
/// 侧边栏 dock 厚度（纵向 dock 的宽 / 横向 dock 的高）：放大后最大图标宽（1.5×图标）
/// 加两侧呼吸边（每侧 6 逻辑 px）。紧贴放大图标——放大到 1.5× 的图标恰在 dock 内居中，
/// 两侧各留少量边距，不被 dock 边缘裁掉。
fn sidebar_dock_thickness(icon_size: f32, scale: f32) -> f32 {
    let margin = 6.0 * scale;
    icon_size * 1.5 + margin * 2.0
}

/// 侧边栏有效图标间距：把间距调大到「相邻两图标都放大到 1.5× 时恰好不相叠」。
/// `step = icon_size + eff_gap = 1.5×icon_size`（默认 icon 32：间距 10 → 26 逻辑）。
/// 布局/放大/dock 尺寸三处统一用它，口径一致。
fn sidebar_eff_gap(icon_size: f32, gap: f32) -> f32 {
    gap + icon_size * 0.5
}

/// 把侧边栏 dock 夹在工作区内（任务栏扣除后，`wa` 由 `SPI_GETWORKAREA` 取得）：
/// 纵向（左/右）dock 的下沿不越过任务栏——限 `h` 到工作区高、`y` 夹到
/// `[wa.y, wa.bottom-h]`；横向（上）dock 右沿同理限 `w`/`x`，`y` 也夹进工作区。
/// 拖动、缩放高度、切换停靠边与启动 re-anchor 统一用它，dock 永远进不了任务栏以下。
pub(crate) fn clamp_sidebar_work_rect(r: Rect, wa: Rect, pos: SidebarPosition) -> Rect {
    let mut out = r;
    match pos {
        SidebarPosition::Left | SidebarPosition::Right => {
            if out.h > wa.h {
                out.h = wa.h;
            }
            out.y = out.y.clamp(wa.y, wa.bottom() - out.h);
        }
        SidebarPosition::Top => {
            if out.w > wa.w {
                out.w = wa.w;
            }
            out.x = out.x.clamp(wa.x, wa.right() - out.w);
            if out.h > wa.h {
                out.h = wa.h;
            }
            out.y = out.y.clamp(wa.y, wa.bottom() - out.h);
        }
    }
    out
}

/// 夹到屏幕内：纵向（左/右）宽 = 紧贴放大图标（1.5×图标 + 两侧 6 逻辑 px 呼吸边），
/// 高随图标数自适应但不超过屏幕高（超出则滚动）；横向（上）厚度对称于纵向、
/// 宽自适应但不超过屏幕宽。`fence.appearance.sidebar_pos` 决定停靠在哪一侧。
pub(crate) fn sidebar_dock_rect(vw: f32, vh: f32, scale: f32, fence: &Fence) -> Rect {
    let icon_size = fence.appearance.icon_size * scale;
    let gap = fence.appearance.gap * scale;
    let pad = 12.0 * scale;
    let n = fence.icon_ids.len();
    let eff_gap = sidebar_eff_gap(icon_size, gap);
    let content = if n > 0 {
        n as f32 * (icon_size + eff_gap) - eff_gap
    } else {
        0.0
    };
    match fence.appearance.sidebar_pos {
        SidebarPosition::Left | SidebarPosition::Right => {
            // 紧贴放大图标：宽 = 1.5×图标 + 两侧呼吸边，图标在 dock 内居中（放大围绕
            // 中心展开），1.5× 时恰被完整容纳。高 = 内容 + 上下内边距，夹到屏幕内。
            let w = sidebar_dock_thickness(icon_size, scale);
            let h = (content + pad * 2.0).clamp(100.0 * scale, vh);
            let x = if fence.appearance.sidebar_pos == SidebarPosition::Left {
                0.0
            } else {
                (vw - w).max(0.0)
            };
            Rect::new(x, ((vh - h) / 2.0).max(0.0), w, h)
        }
        SidebarPosition::Top => {
            // 横向 dock：厚度对称于纵向（1.5×图标 + 两侧呼吸边），宽随内容自适应。
            let h = sidebar_dock_thickness(icon_size, scale);
            let w = (content + pad * 2.0).clamp(100.0 * scale, vw);
            Rect::new(((vw - w) / 2.0).max(0.0), 0.0, w, h)
        }
    }
}

/// 侧边栏排布全部图标位置（单行/单列，无标签；滚动用 `scroll` 平移）。
/// `vertical` = true 时纵向排列（左/右停靠），false 时横向排列（上侧停靠）。
#[allow(clippy::too_many_arguments)]
pub(crate) fn sidebar_icons(
    _fence: &Fence,
    rows: &[(String, u64, String, String, String)],
    icon_size: f32,
    gap: f32,
    start_x: f32,
    start_y: f32,
    scroll: f32,
    vertical: bool,
) -> Vec<SceneIcon> {
    rows.iter()
        .enumerate()
        .map(|(i, (label, bitmap, ct, cm, cs))| {
            let (x, y) = if vertical {
                (start_x, start_y + i as f32 * (icon_size + gap) - scroll)
            } else {
                (start_x + i as f32 * (icon_size + gap) - scroll, start_y)
            };
            SceneIcon {
                label: label.clone(),
                bitmap_id: *bitmap,
                x,
                y,
                size: icon_size,
                col_type: ct.clone(),
                col_modified: cm.clone(),
                col_size: cs.clone(),
                scale: 1.0,
            }
        })
        .collect()
}

/// 侧边栏 Dock 放大效果：根据鼠标位置计算每个图标的缩放。
/// 返回 (hovered_index, scales)，scales[i] 是第 i 个图标的缩放因子。
/// 鼠标未悬停在任何图标上时返回 (None, vec![1.0; n])。
/// 侧边栏 Dock 放大：按光标到每个图标中心的**距离**连续缩放，跨图标平滑过渡。
///
/// 影响半径 = 4 个图标步距（icon_size + gap）；中心 1.5x、每远一步递减，
/// 与旧的分级曲线 1.5/1.3/1.15/1.05 一致，但每个像素都是平滑的：
/// 光标在两个图标之间移动时，两个图标各自按距离连续变化，不再跳档。
/// 返回 `(最近图标, 各图标缩放)`；光标离开影响半径后返回 `(None, 全 1.0)`。
pub(crate) fn sidebar_magnify(
    icons: &[SceneIcon],
    cursor: Option<(f32, f32)>,
    icon_size: f32,
    gap: f32,
) -> (Option<usize>, Vec<f32>) {
    let n = icons.len();
    if n == 0 {
        return (None, Vec::new());
    }
    let Some((mx, my)) = cursor else {
        return (None, vec![1.0; n]);
    };
    let step = (icon_size + gap).max(1.0);
    let radius = 4.0 * step;
    let max_scale = 1.5;
    let mut scales = vec![1.0f32; n];
    let mut nearest: Option<usize> = None;
    let mut nearest_d = f32::INFINITY;
    for (i, ic) in icons.iter().enumerate() {
        let cx = ic.x + ic.size / 2.0;
        let cy = ic.y + ic.size / 2.0;
        let dx = mx - cx;
        let dy = my - cy;
        let dist = (dx * dx + dy * dy).sqrt();
        if dist < nearest_d {
            nearest_d = dist;
            nearest = Some(i);
        }
        if dist >= radius {
            continue;
        }
        // 二次衰减：t=1 在圆心（1.5x），t=0 在半径边缘（1.0x）
        let t = (1.0 - dist / radius).max(0.0);
        scales[i] = 1.0 + (max_scale - 1.0) * t * t;
    }
    // 光标离所有图标都超出影响半径 → 视为未悬停（全部复位，避免 Dock 悬空）
    if nearest_d > radius {
        nearest = None;
    }
    (nearest, scales)
}

/// 侧边栏 Dock 放大（第二遍，全部栅栏真实几何就绪后调用）。
///
/// 只有光标**不在任何非侧边栏栅栏内**时才放大：否则 Dock 的影响半径会越过
/// 网格/列表并抢走它们的悬停焦点（Dock 放大与工具提示跟随最近图标）。
pub(crate) fn build_dock_magnify(rt: &mut Runtime, scene_fences: &mut [SceneFence]) {
    let Some((mx, my)) = rt.cursor else {
        return;
    };
    // 光标落在某个非侧边栏栅栏（且未淡出）内 → 本帧所有 Dock 都不放大
    let in_other = scene_fences.iter().enumerate().any(|(j, of)| {
        rt.desk.fences[j].appearance.layout != FenceLayout::Sidebar
            && of.alpha > 0.01
            && mx >= of.x
            && mx <= of.x + of.width
            && my >= of.y
            && my <= of.y + of.height
    });
    let cursor = if in_other { None } else { Some((mx, my)) };
    for (i, sf) in scene_fences.iter_mut().enumerate() {
        let layout = rt.desk.fences[i].appearance.layout;
        if layout != FenceLayout::Sidebar {
            continue;
        }
        let icon_size = rt.desk.fences[i].appearance.icon_size * rt.theme.scale;
        let gap = rt.desk.fences[i].appearance.gap * rt.theme.scale;
        let (hovered, scales) = sidebar_magnify(
            &sf.icons,
            cursor,
            icon_size,
            sidebar_eff_gap(icon_size, gap),
        );
        for (ic, &sc) in sf.icons.iter_mut().zip(scales.iter()) {
            ic.scale = sc;
        }
        // 悬停目标跟随光标最近的图标（高亮/工具提示与放大一致）。
        // 仅当光标进入影响半径（hovered 为 Some）才覆盖 rt.hover，否则会把
        // 其他栅栏上的悬停一并清掉。
        sf.hover_icon = hovered;
        if let Some(hi) = hovered {
            rt.hover = Some((i, hi));
            // 工具提示：完整名称放在图标旁侧，可延伸到 dock 之外（绘制层不受
            // 栅栏裁剪限制），位置由这里按屏幕边界计算好。
            if let Some(icon) = sf.icons.get(hi) {
                let geom = SidebarGeom {
                    vw: rt.vw,
                    vh: rt.vh,
                    font_size: rt.theme.label.size,
                    scale: rt.theme.scale,
                    pos: rt.desk.fences[i].appearance.sidebar_pos,
                };
                sf.tooltip_rect = Some(sidebar_tooltip_rect(&geom, icon, &icon.label));
            }
        } else {
            sf.tooltip_rect = None;
        }
    }
}

/// 估算文本物理宽度（委托 core 统一口径：CJK 按字号宽，ASCII 按 0.62 倍宽）。
pub(crate) fn estimate_text_width(text: &str, font_size: f32) -> f32 {
    sylva_core::text::estimate_width(text, font_size)
}

/// 侧边栏工具提示所需的屏幕/主题几何（虚拟屏尺寸、字号、缩放、停靠边），
/// 供 `sidebar_tooltip_rect` 归组传参，避免长参数列表。
#[derive(Debug, Clone, Copy)]
pub(crate) struct SidebarGeom {
    pub vw: f32,
    pub vh: f32,
    pub font_size: f32,
    pub scale: f32,
    pub pos: SidebarPosition,
}

/// 侧边栏悬停工具提示矩形：完整名称放在图标旁侧，钳制到虚拟屏幕内。
///
/// 纵向 dock（左/右）放图标右侧/左侧、垂直居中对齐；横向 dock（上）放图标下方、
/// 水平居中对齐。宽度按文本估算 + 20% 安全余量，保证绘制层按同一口径判断时
/// 不会截断文字。
pub(crate) fn sidebar_tooltip_rect(geom: &SidebarGeom, icon: &SceneIcon, label: &str) -> RectF {
    let pad = 7.0 * geom.scale;
    let w = (estimate_text_width(label, geom.font_size) * 1.2 + pad * 2.0).max(geom.font_size);
    let h = geom.font_size * 1.6 + pad * 2.0;
    let gap = 10.0 * geom.scale;
    let (mut x, mut y) = match geom.pos {
        SidebarPosition::Left => {
            let y = icon.y + icon.size / 2.0 - h / 2.0;
            (icon.x + icon.size + gap, y)
        }
        SidebarPosition::Right => {
            let y = icon.y + icon.size / 2.0 - h / 2.0;
            (icon.x - gap - w, y)
        }
        SidebarPosition::Top => {
            let x = icon.x + icon.size / 2.0 - w / 2.0;
            (x, icon.y + icon.size + gap)
        }
    };
    // 钳制到虚拟屏幕内（宽/高超出屏幕时贴边即可，不再外溢）
    x = x.max(4.0).min((geom.vw - w).max(4.0));
    y = y.max(4.0).min((geom.vh - h).max(4.0));
    RectF { x, y, w, h }
}

/// 由场景几何生成命中模型：栅栏（标题移动把手 + 右下角缩放把手 + 整体区域）与
/// 图标（双击打开）。`fence`/`icon` 下标与 `desk.fences` / `icon_ids` 对应。
pub(crate) fn hit_model_from(theme: &Theme, scene: &Scene, _desk: &Desk) -> HitModel {
    let mut fences = Vec::with_capacity(scene.fences.len());
    let mut icons = Vec::new();
    for (fi, f) in scene.fences.iter().enumerate() {
        // 桌面切换淡出中的栅栏不参与命中（区域随之收缩，点击穿透到桌面）
        if f.alpha <= 0.01 {
            continue;
        }
        let body = RectF {
            x: f.x,
            y: f.y,
            w: f.width,
            h: f.height,
        };
        let title_h =
            (theme.title.size * 1.6 + theme.title_padding_bottom + 2.0 * theme.fence_padding)
                .min(f.height);
        fences.push(FenceHit {
            body,
            title: RectF {
                x: f.x,
                y: f.y,
                w: f.width,
                h: title_h,
            },
            grip: RectF {
                x: f.x + f.width - GRIP_SIZE,
                y: f.y + f.height - GRIP_SIZE,
                w: GRIP_SIZE,
                h: GRIP_SIZE,
            },
            id: fi,
            tooltip: f.tooltip_rect,
        });
        // 可视区（列头之下）：滚出可视区的图标不参与命中，避免误点。
        let view_top = f.content_top + f.list_cols.map(|c| c.header_h).unwrap_or(0.0);
        let view_bottom = view_top + f.scroll_view;
        for (ii, icon) in f.icons.iter().enumerate() {
            if icon.y + icon.size < view_top || icon.y > view_bottom {
                continue;
            }
            // 列表布局整行可点（图标 + 名称 + 详情列），双击/右键/悬停都对整行生效，
            // 不再只有那一小块图标有反应（列表栅栏「点了没反应」根因）。
            let rect = if f.layout == FenceLayout::List {
                let pad = f.content_left - f.x;
                let row_extent = icon.size.max(theme.label.size * 1.6) + theme.list_row_gap;
                RectF {
                    x: f.content_left,
                    y: icon.y,
                    w: (f.width - 2.0 * pad).max(1.0),
                    h: row_extent,
                }
            } else {
                RectF {
                    x: icon.x,
                    y: icon.y,
                    w: icon.size,
                    h: icon.size,
                }
            };
            icons.push(IconHit {
                rect,
                fence: fi,
                icon: ii,
            });
        }
    }
    // 控制中心命中（与绘制几何同源）。关闭/淡出中（panel < 0.5）不接收点击；
    // 展开面板：关闭 / 切换桌面 / 栅栏列表 / 详情控制 / 添加 / 移出。
    let mut console = None;
    if let Some(c) = &scene.console {
        let mut zones = Vec::new();
        let body = RectF {
            x: c.x,
            y: c.y,
            w: c.width,
            h: c.height,
        };
        if c.panel >= 0.5 {
            zones.push((ConsoleZone::Close, c.close));
            zones.push((ConsoleZone::AddFence, c.add_fence));
            zones.push((ConsoleZone::RemoveFence, c.remove_btn));
            zones.push((ConsoleZone::DesktopToggle, c.desktop_toggle));
            for (i, r) in c.fence_rows.iter().enumerate() {
                // 滚出可视区的行不参与命中（避免点到详情区时误中隐藏行）
                if r.rect.y + r.rect.h < c.fence_list_view.y
                    || r.rect.y > c.fence_list_view.y + c.fence_list_view.h
                {
                    continue;
                }
                zones.push((ConsoleZone::FenceSelect(i), r.rect));
            }
            if let Some(d) = &c.fence_detail {
                zones.push((ConsoleZone::ChangeStoragePath, d.storage_btn));
                zones.push((ConsoleZone::FenceLayout(FenceLayout::Grid), d.layout_grid));
                zones.push((ConsoleZone::FenceLayout(FenceLayout::List), d.layout_list));
                zones.push((
                    ConsoleZone::FenceLayout(FenceLayout::Sidebar),
                    d.layout_sidebar,
                ));
                zones.push((
                    ConsoleZone::FenceSidebarPos(SidebarPosition::Left),
                    d.sidebar_left,
                ));
                zones.push((
                    ConsoleZone::FenceSidebarPos(SidebarPosition::Top),
                    d.sidebar_top,
                ));
                zones.push((
                    ConsoleZone::FenceSidebarPos(SidebarPosition::Right),
                    d.sidebar_right,
                ));
                zones.push((ConsoleZone::FenceIconSize(32.0), d.size_s));
                zones.push((ConsoleZone::FenceIconSize(48.0), d.size_m));
                zones.push((ConsoleZone::FenceIconSize(64.0), d.size_l));
                zones.push((ConsoleZone::FenceStyle(FenceStyle::Glass), d.style_glass));
                zones.push((
                    ConsoleZone::FenceStyle(FenceStyle::Outline),
                    d.style_outline,
                ));
                zones.push((ConsoleZone::FenceStyle(FenceStyle::Filled), d.style_filled));
                zones.push((ConsoleZone::FenceStyle(FenceStyle::Blur), d.style_blur));
                zones.push((ConsoleZone::FenceTint(None), d.tint_default));
                for (i, r) in d.tints.iter().enumerate() {
                    if let Some((_, c)) = TINT_PRESETS.get(i) {
                        zones.push((ConsoleZone::FenceTint(Some(*c)), *r));
                    }
                }
            }
        }
        console = Some(ConsoleHit {
            rect: body,
            title: RectF {
                x: c.x,
                y: c.y,
                w: c.width,
                h: c.title_h,
            },
            zones,
        });
    }
    HitModel {
        fences,
        icons,
        console,
        // 内联编辑框浮于栅栏之上：overlay 据此把框内点击路由到 EditCaret（定位光标）
        edit_rect: scene.edit.as_ref().map(|e| e.rect),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_fence(layout: FenceLayout) -> Fence {
        Fence {
            id: 1,
            title: Some("测试".into()),
            monitor_id: 0,
            bounds: Rect::default(),
            state: FenceState::Expanded,
            icon_ids: vec!["a".into(), "b".into(), "c".into()],
            appearance: FenceAppearance {
                layout,
                icon_size: 48.0,
                gap: 10.0,
                ..FenceAppearance::default()
            },
            scroll: 0.0,
            storage_path: None,
            sidebar_collapsed: false,
        }
    }

    /// 纵向 dock 被拖到任务栏以下 → 夹回工作区：下沿恰好贴住任务栏上沿。
    #[test]
    fn clamp_sidebar_work_rect_keeps_dock_above_taskbar() {
        // 工作区 = 全屏扣掉底部 120px 任务栏
        let wa = Rect::new(0.0, 0.0, 1920.0, 1560.0);
        let dock = Rect::new(0.0, 1580.0, 168.0, 472.0); // 底 2052 > wa.bottom
        let out = clamp_sidebar_work_rect(dock, wa, SidebarPosition::Left);
        assert_eq!(out.bottom(), wa.bottom());
        assert!(out.y >= wa.y);
    }

    /// 纵向 dock 高度超出工作区 → 收缩到工作区高并贴顶，不溢出。
    #[test]
    fn clamp_sidebar_work_rect_shrinks_oversized_dock() {
        let wa = Rect::new(0.0, 0.0, 1920.0, 1560.0);
        let dock = Rect::new(0.0, 0.0, 168.0, 3000.0);
        let out = clamp_sidebar_work_rect(dock, wa, SidebarPosition::Right);
        assert_eq!(out.h, wa.h);
        assert_eq!(out.y, wa.y);
        assert!(out.bottom() <= wa.bottom());
    }

    /// 横向 dock 右沿越出工作区 → 夹回工作区右缘，左边不越界。
    #[test]
    fn clamp_sidebar_work_rect_top_dock_horizontal() {
        let wa = Rect::new(0.0, 0.0, 1920.0, 1560.0);
        let dock = Rect::new(1800.0, 0.0, 400.0, 168.0); // 右 2200 > wa.right
        let out = clamp_sidebar_work_rect(dock, wa, SidebarPosition::Top);
        assert_eq!(out.right(), wa.right());
        assert!(out.x >= wa.x);
    }

    /// 已在工作区内的 dock 原样保留（不动）。
    #[test]
    fn clamp_sidebar_work_rect_untouched_when_inside() {
        let wa = Rect::new(0.0, 0.0, 1920.0, 1560.0);
        let dock = Rect::new(0.0, 400.0, 168.0, 472.0);
        assert_eq!(
            clamp_sidebar_work_rect(dock, wa, SidebarPosition::Left),
            dock
        );
    }

    /// 纵向 dock：宽 = 紧贴放大图标（1.5×图标 + 两侧 6 逻辑 px 呼吸边），
    /// 高随图标数自适应但不超过屏幕。
    #[test]
    fn sidebar_dock_rect_vertical_fits_within_screen() {
        // scale=2：icon=96, gap=20, eff_gap=68, pad=24；3 图标内容 = 3*96+2*68 = 424
        let f = test_fence(FenceLayout::Sidebar);
        let r = sidebar_dock_rect(3072.0, 1920.0, 2.0, &f);
        assert_eq!(r.x, 0.0);
        assert_eq!(r.w, 168.0); // 96*1.5 + 6*2*2
        assert_eq!(r.h, 472.0); // 424 + 48
        assert_eq!(r.y, (1920.0 - 472.0) / 2.0);
        assert!(r.y >= 0.0 && r.y + r.h <= 1920.0);
    }

    /// 图标多到放不下时，高度被夹到屏幕高、顶部贴屏幕边缘（不会跑到屏幕外）。
    #[test]
    fn sidebar_dock_rect_clamps_full_height_for_many_icons() {
        let mut f = test_fence(FenceLayout::Sidebar);
        f.icon_ids = (0..29).map(|i| format!("icon{i}")).collect();
        let r = sidebar_dock_rect(1920.0, 1920.0, 2.0, &f);
        assert_eq!(r.h, 1920.0);
        assert_eq!(r.y, 0.0);
        assert!(r.x + r.w <= 1920.0);
    }

    /// 右侧停靠：dock 贴屏幕右缘。
    #[test]
    fn sidebar_dock_rect_right_anchors_to_right_edge() {
        let mut f = test_fence(FenceLayout::Sidebar);
        f.appearance.sidebar_pos = SidebarPosition::Right;
        let r = sidebar_dock_rect(3072.0, 1920.0, 2.0, &f);
        assert_eq!(r.x, 3072.0 - r.w);
        assert!(r.x >= 0.0);
    }

    /// 上侧停靠：横向 dock（厚度 = 紧贴放大图标、宽自适应），贴屏幕顶缘。
    #[test]
    fn sidebar_dock_rect_top_makes_horizontal_dock() {
        let mut f = test_fence(FenceLayout::Sidebar);
        f.appearance.sidebar_pos = SidebarPosition::Top;
        let r = sidebar_dock_rect(3072.0, 1920.0, 2.0, &f);
        assert_eq!(r.h, 168.0); // 96*1.5 + 6*2*2
        assert_eq!(r.y, 0.0);
        assert_eq!(r.w, 472.0); // 424 + 48
        assert_eq!(r.x, (3072.0 - 472.0) / 2.0);
        assert!(r.x >= 0.0 && r.x + r.w <= 3072.0);
    }

    /// 空栅栏：至少留出一个可点击的最小 dock 尺寸。
    #[test]
    fn sidebar_dock_rect_empty_fence_keeps_min_size() {
        let mut f = test_fence(FenceLayout::Sidebar);
        f.icon_ids.clear();
        let r = sidebar_dock_rect(1920.0, 1920.0, 2.0, &f);
        assert_eq!(r.h, 200.0); // clamp 下限 100*scale
        assert_eq!(r.w, 168.0); // 96*1.5 + 6*2*2
    }

    /// 有效间距：step = icon + eff_gap ≥ 1.5×icon，相邻两图标都放大到 1.5× 时不相叠。
    #[test]
    fn sidebar_eff_gap_prevents_magnified_overlap() {
        let icon = 48.0;
        let gap = 10.0;
        let eff = sidebar_eff_gap(icon, gap);
        assert_eq!(eff, gap + icon * 0.5);
        let step = icon + eff;
        assert!(step >= icon * 1.5, "step {step} 应 ≥ 1.5×图标");
        // 两图标都放大到 1.5× 时，相邻边缘应不相交（≥ 恰好贴合）
        assert!(step - icon * 1.5 >= -0.001);
    }

    fn test_icon(x: f32, y: f32, size: f32) -> SceneIcon {
        SceneIcon {
            label: String::new(),
            bitmap_id: 0,
            x,
            y,
            size,
            col_type: String::new(),
            col_modified: String::new(),
            col_size: String::new(),
            scale: 1.0,
        }
    }

    /// 光标不在任何图标附近：全部复位 1.0，无悬停。
    #[test]
    fn magnify_resets_when_cursor_far() {
        let icons = vec![test_icon(0.0, 0.0, 48.0), test_icon(0.0, 116.0, 48.0)];
        // 影响半径 = 4*(48+10)=232，中心约在 (24, 24)/(24, 140)
        let (hovered, scales) = sidebar_magnify(&icons, Some((24.0, 1000.0)), 48.0, 10.0);
        assert!(hovered.is_none());
        assert!(scales.iter().all(|&s| s == 1.0));
    }

    /// 光标在图标正中心：该图标 1.5x，最近 = 该图标。
    #[test]
    fn magnify_peak_at_icon_center() {
        let icons = vec![test_icon(0.0, 0.0, 48.0), test_icon(0.0, 116.0, 48.0)];
        let (hovered, scales) = sidebar_magnify(&icons, Some((24.0, 24.0)), 48.0, 10.0);
        assert_eq!(hovered, Some(0));
        assert!((scales[0] - 1.5).abs() < 0.001);
        assert!(scales[1] > 1.0 && scales[1] < 1.5);
    }

    /// 在两个图标中点：两个图标缩放相等（连续过渡不跳档），最近 = 先遇到的。
    #[test]
    fn magnify_is_continuous_between_icons() {
        let icons = vec![test_icon(0.0, 0.0, 48.0), test_icon(0.0, 116.0, 48.0)];
        // 中点 y=58（图标中心 24 与 140 的中点）
        let (hovered, scales) = sidebar_magnify(&icons, Some((24.0, 82.0)), 48.0, 10.0);
        assert_eq!(hovered, Some(0)); // 距离相等时保持先遇到的
        assert!((scales[0] - scales[1]).abs() < 0.001);
        assert!(scales[0] > 1.0);
    }

    /// 越靠近图标中心缩放越大（单调）；靠近中心一侧的图标更大。
    #[test]
    fn magnify_monotonic_toward_center() {
        let icons = vec![test_icon(0.0, 0.0, 48.0), test_icon(0.0, 116.0, 48.0)];
        let center = (24.0, 24.0);
        let (_, s_at_center) = sidebar_magnify(&icons, Some(center), 48.0, 10.0);
        let (_, s_mid) = sidebar_magnify(&icons, Some((24.0, 60.0)), 48.0, 10.0);
        // 中心图标在自身中心处比靠近中点时更大
        assert!(s_at_center[0] > s_mid[0]);
        // 靠近中点时第二个图标开始被带起来
        assert!(s_mid[1] > 1.0);
    }
}
