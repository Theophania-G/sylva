//! 就地文本编辑：D2D 内联编辑、光标/IME、图标与栅栏重命名。

use crate::*;

/// 内联编辑目标：栅栏内图标重命名 / 栅栏标题重命名。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EditTarget {
    Item { fence: usize, icon: usize },
    FenceTitle { fence: usize },
}

/// D2D 内联文本编辑：文本、光标与 IME 合成状态全在 App 层，绘制与输入同表面，
/// 彻底摆脱 HWND 弹出框的层级/焦点/对齐问题。支持单行（待办/重命名）与多行（便签）。
#[derive(Debug)]
pub(crate) struct InlineEdit {
    pub(crate) target: EditTarget,
    /// 编辑区矩形（物理像素）。
    pub(crate) rect: RectF,
    /// 文本行（单行编辑时恒为 1 行）。
    pub(crate) lines: Vec<String>,
    /// 光标位置（行、列）。
    pub(crate) line: usize,
    pub(crate) col: usize,
    /// 空文本时的占位提示。
    pub(crate) placeholder: String,
    pub(crate) single_line: bool,
    /// 是否聚焦（绘制光标/聚焦描边）。
    pub(crate) focused: bool,
    /// IME 合成状态。
    pub(crate) composing: bool,
    pub(crate) comp: String,
    /// IME 结果上屏中（避免合成结果触发递归提交）。
    pub(crate) committing: bool,
}

impl InlineEdit {
    fn current_line(&self) -> &str {
        self.lines.get(self.line).map(|s| s.as_str()).unwrap_or("")
    }

    /// 在光标处插入一个字符。
    fn insert_char(&mut self, ch: char) {
        let col = self.col.min(self.current_line().chars().count());
        let b = self.byte_col();
        self.lines[self.line].insert(b, ch);
        self.col = col + 1;
    }

    /// 光标前的字节下标（`col` 是字符数，需换算为字节）。
    fn byte_col(&self) -> usize {
        self.current_line()
            .chars()
            .take(self.col)
            .map(char::len_utf8)
            .sum()
    }

    fn backspace(&mut self) {
        if self.col > 0 {
            let b = self.byte_col();
            let prev = self.current_line()[..b]
                .chars()
                .next_back()
                .map(char::len_utf8)
                .unwrap_or(0);
            self.lines[self.line].replace_range(b - prev..b, "");
            self.col -= 1;
        } else if !self.single_line && self.line > 0 {
            let prev_len = self.lines[self.line - 1].chars().count();
            let cur = self.lines.remove(self.line);
            self.lines[self.line - 1].push_str(&cur);
            self.line -= 1;
            self.col = prev_len;
        }
    }

    fn delete_at(&mut self) {
        let b = self.byte_col();
        let line = self.current_line().to_string();
        if b < line.len() {
            let ch_len = line[b..].chars().next().map(char::len_utf8).unwrap_or(0);
            self.lines[self.line].replace_range(b..b + ch_len, "");
        } else if !self.single_line && self.line + 1 < self.lines.len() {
            let next = self.lines.remove(self.line + 1);
            self.lines[self.line].push_str(&next);
        }
    }

    fn cursor_left(&mut self) {
        if self.col > 0 {
            self.col -= 1;
        } else if !self.single_line && self.line > 0 {
            self.line -= 1;
            self.col = self.current_line().chars().count();
        }
    }

    fn cursor_right(&mut self) {
        let len = self.current_line().chars().count();
        if self.col < len {
            self.col += 1;
        } else if !self.single_line && self.line + 1 < self.lines.len() {
            self.line += 1;
            self.col = 0;
        }
    }

    fn cursor_up_down(&mut self, down: bool) {
        if self.single_line {
            return;
        }
        let target = if down {
            if self.line + 1 < self.lines.len() {
                self.line + 1
            } else {
                return;
            }
        } else if self.line > 0 {
            self.line - 1
        } else {
            return;
        };
        let len = self.lines[target].chars().count();
        self.line = target;
        self.col = self.col.min(len);
    }

    fn home_end(&mut self, end: bool) {
        self.col = if end {
            self.current_line().chars().count()
        } else {
            0
        };
    }

    /// Enter：单行 = 提交；多行 = 换行。
    fn enter(&mut self) -> bool {
        if self.single_line {
            true
        } else {
            let b = self.byte_col();
            let rest = self.lines[self.line][b..].to_string();
            self.lines[self.line].truncate(b);
            self.line += 1;
            self.lines.insert(self.line, rest);
            self.col = 0;
            false
        }
    }

    /// IME 合成结束：把结果串插入光标处。
    pub(crate) fn commit_ime(&mut self, text: &str) {
        let col = self.col.min(self.current_line().chars().count());
        let b = self.byte_col();
        self.lines[self.line].insert_str(b, text);
        self.col = col + text.chars().count();
        self.composing = false;
        self.comp.clear();
    }
}
pub(crate) fn dismiss_edit(rt: &mut Runtime) {
    let Some(edit) = rt.edit.take() else {
        return;
    };
    match edit.target {
        EditTarget::FenceTitle { fence } => {
            let text = edit.lines.join("").trim().to_string();
            if apply_rename(rt, EditTarget::FenceTitle { fence }, &text) {
                inject_rebuild(rt);
            }
        }
        EditTarget::Item { fence, icon } => {
            let text = edit.lines.join("").trim().to_string();
            if apply_rename(rt, EditTarget::Item { fence, icon }, &text) {
                inject_rebuild(rt);
            }
        }
    }
}

/// 提交当前编辑（单行 Enter）：重命名提交并关闭。
pub(crate) fn commit_edit(rt: &mut Runtime) {
    let Some((target, text)) = rt.edit.as_ref().map(|e| (e.target, e.lines.join(""))) else {
        return;
    };
    match target {
        target @ (EditTarget::FenceTitle { .. } | EditTarget::Item { .. }) => {
            // 重命名：Enter = 提交并关闭
            let text = text.trim().to_string();
            rt.edit = None;
            match target {
                EditTarget::FenceTitle { fence } => {
                    if apply_rename(rt, EditTarget::FenceTitle { fence }, &text) {
                        inject_rebuild(rt);
                    }
                }
                EditTarget::Item { fence, icon } => {
                    apply_rename(rt, EditTarget::Item { fence, icon }, &text)
                        .then(|| inject_rebuild(rt));
                }
            }
        }
    }
}

/// 键盘事件 → 内联编辑（光标/退格/删除/方向/Home/End/回车/Esc/Ctrl+V 粘贴）。
pub(crate) fn edit_key(rt: &mut Runtime, vk: u32, ctrl: bool) {
    if rt.edit.is_none() {
        return;
    }
    let mut caret_moved = false;
    match vk {
        v if v == VK_RETURN.0 as u32 => {
            let single = rt.edit.as_ref().map(|e| e.single_line).unwrap_or(true);
            if single {
                commit_edit(rt);
            } else if let Some(e) = rt.edit.as_mut() {
                e.enter();
                caret_moved = true;
            }
        }
        v if v == VK_ESCAPE.0 as u32 => {
            if rt.edit.is_some() {
                rt.edit = None;
            }
        }
        v if v == VK_BACK.0 as u32 => {
            if let Some(e) = rt.edit.as_mut() {
                e.backspace();
            }
            caret_moved = true;
        }
        v if v == VK_DELETE.0 as u32 => {
            if let Some(e) = rt.edit.as_mut() {
                e.delete_at();
            }
            caret_moved = true;
        }
        v if v == VK_LEFT.0 as u32 => {
            if let Some(e) = rt.edit.as_mut() {
                if ctrl {
                    e.home_end(false);
                } else {
                    e.cursor_left();
                }
            }
            caret_moved = true;
        }
        v if v == VK_RIGHT.0 as u32 => {
            if let Some(e) = rt.edit.as_mut() {
                if ctrl {
                    e.home_end(true);
                } else {
                    e.cursor_right();
                }
            }
            caret_moved = true;
        }
        v if v == VK_UP.0 as u32 => {
            if let Some(e) = rt.edit.as_mut() {
                e.cursor_up_down(false);
            }
            caret_moved = true;
        }
        v if v == VK_DOWN.0 as u32 => {
            if let Some(e) = rt.edit.as_mut() {
                e.cursor_up_down(true);
            }
            caret_moved = true;
        }
        v if v == VK_HOME.0 as u32 => {
            if let Some(e) = rt.edit.as_mut() {
                e.home_end(false);
            }
            caret_moved = true;
        }
        v if v == VK_END.0 as u32 => {
            if let Some(e) = rt.edit.as_mut() {
                e.home_end(true);
            }
            caret_moved = true;
        }
        v if v == 0x56 && ctrl => {
            // Ctrl+V 粘贴
            edit_paste(rt);
            caret_moved = true;
        }
        _ => {}
    }
    if caret_moved {
        position_ime_window(rt);
    }
}

/// 普通字符（WM_CHAR，非 IME 路径）插入。
pub(crate) fn edit_char(rt: &mut Runtime, ch: u16) {
    let Some(edit) = rt.edit.as_mut() else {
        return;
    };
    if edit.committing {
        return;
    }
    // 代理对缓冲（emoji 由两个 WM_CHAR 到达）
    match ch {
        0xD800..=0xDBFF => {
            rt.edit_high = Some(ch);
            return;
        }
        0xDC00..=0xDFFF => {
            if let Some(hi) = rt.edit_high.take() {
                let c =
                    char::from_u32(0x10000 + ((hi as u32 - 0xD800) << 10) + (ch as u32 - 0xDC00))
                        .unwrap_or('\u{FFFD}');
                edit.insert_char(c);
            }
            position_ime_window(rt);
            return;
        }
        _ => {}
    }
    rt.edit_high = None;
    if ch < 32 {
        return; // 控制字符忽略（回车/退格等已由 KeyDown 处理）
    }
    if let Some(c) = char::from_u32(ch as u32) {
        edit.insert_char(c);
    }
    position_ime_window(rt);
}

/// 从剪贴板读 Unicode 文本。
pub(crate) fn clipboard_text() -> Option<String> {
    unsafe {
        OpenClipboard(None).ok()?;
        // CF_UNICODETEXT = 13（标准剪贴板格式，避免引入 Ole feature）
        let h = GetClipboardData(13).ok()?;
        if h.is_invalid() {
            let _ = CloseClipboard();
            return None;
        }
        let hg = HGLOBAL(h.0);
        let p = GlobalLock(hg);
        let size = GlobalSize(hg);
        let out = if !p.is_null() && size > 0 {
            let n = (size / 2) as usize;
            let slice = std::slice::from_raw_parts(p as *const u16, n);
            let end = slice.iter().position(|&c| c == 0).unwrap_or(n);
            Some(String::from_utf16_lossy(&slice[..end]))
        } else {
            None
        };
        let _ = GlobalUnlock(hg);
        let _ = CloseClipboard();
        out
    }
}

/// 把剪贴板文本插入内联编辑（单行换行转空格，多行按行插入）。
pub(crate) fn edit_paste(rt: &mut Runtime) {
    let Some(text) = clipboard_text() else {
        return;
    };
    let Some(edit) = rt.edit.as_mut() else {
        return;
    };
    if edit.single_line {
        let text = text.replace(['\r', '\n'], " ");
        let col = edit.col.min(edit.current_line().chars().count());
        let b = edit.byte_col();
        edit.lines[0].insert_str(b, &text);
        edit.col = col + text.chars().count();
    } else {
        let normalized = text.replace("\r\n", "\n");
        let parts: Vec<&str> = normalized.split('\n').collect();
        let b = edit.byte_col();
        let line = edit.line;
        let tail = edit.lines[line][b..].to_string();
        edit.lines[line].truncate(b);
        edit.lines[line].push_str(parts[0]);
        let mut rest: Vec<String> = parts[1..].iter().map(|s| s.to_string()).collect();
        if let Some(last) = rest.last_mut() {
            last.push_str(&tail);
        }
        let insert_at = line + 1;
        edit.lines.splice(insert_at..insert_at, rest);
        edit.line = line + parts.len() - 1;
        edit.col = edit.current_line().chars().count();
    }
}

/// 把 IME 合成窗口定位到光标（候选列表跟随光标）。
pub(crate) fn position_ime_window(rt: &Runtime) {
    let (x, y) = edit_caret_point(rt);
    unsafe {
        let ctx = ImmGetContext(rt.hwnd);
        if !ctx.0.is_null() {
            let form = COMPOSITIONFORM {
                dwStyle: CFS_POINT,
                ptCurrentPos: POINT { x, y },
                rcArea: RECT::default(),
            };
            let _ = ImmSetCompositionWindow(ctx, &form);
            let _ = ImmReleaseContext(rt.hwnd, ctx);
        }
    }
}

/// 内联编辑光标屏幕坐标（IME 窗口定位用；物理像素）。
pub(crate) fn edit_caret_point(rt: &Runtime) -> (i32, i32) {
    let Some(edit) = &rt.edit else {
        return (0, 0);
    };
    let s = rt.theme.scale;
    let font = rt.theme.label.size;
    let before: String = edit.current_line().chars().take(edit.col).collect();
    let w = label_width(&before, font) + label_width(&edit.comp, font);
    let x = edit.rect.x + 10.0 * s + w;
    // 文本布局顶 = 剪裁上内缩（4·scale）：光标/IME 候选窗口随文字定位，与绘制一致。
    let y = edit.rect.y + 4.0 * s + edit.line as f32 * (font * 1.5) + font * 0.8;
    (x as i32, y as i32)
}

/// 鼠标点在编辑框内（x 为虚拟屏幕物理坐标）：把光标定位到对应字符。
/// 与 `edit_caret_point` 同口径（文本左缘 = rect.x + 10*s，逐字累计宽度），
/// 半字宽以上的点击进下一格，与常见编辑器行为一致。
pub(crate) fn edit_click(rt: &mut Runtime, x: f32) {
    let s = rt.theme.scale;
    let font = rt.theme.label.size;
    let Some(edit) = rt.edit.as_mut() else {
        return;
    };
    if !edit.single_line {
        return; // 多行定位需按 y 判行，重命名不用；待办编辑暂保持键盘定位
    }
    let text_left = edit.rect.x + 10.0 * s;
    let rel = (x - text_left).max(0.0);
    let line = edit.current_line().to_string();
    let mut col = 0;
    let mut acc = 0.0;
    for (idx, c) in line.char_indices() {
        let w = label_width(&line[..idx + c.len_utf8()], font) - label_width(&line[..idx], font);
        if acc + w / 2.0 >= rel {
            break;
        }
        acc += w;
        col += 1;
    }
    edit.col = col;
    position_ime_window(rt);
}

/// 双击/右键打开：按显式成员列表反查并启动。
pub(crate) fn start_inplace_rename(rt: &mut Runtime, target: EditTarget) {
    // 初始文本 + 定位矩形（物理像素）
    let (current, rect) = match target {
        EditTarget::Item { fence, icon } => {
            let Some(r) = item_label_rect(rt, fence, icon) else {
                tracing::warn!(fence, icon, "无法定位图标标签，跳过就地改名");
                return;
            };
            let Some(name) = item_name(rt, fence, icon) else {
                return;
            };
            (name, r)
        }
        EditTarget::FenceTitle { fence } => {
            let Some(name) = rt.desk.fences.get(fence).map(|f| {
                f.title
                    .clone()
                    .unwrap_or_else(|| format!("栅栏 {}", fence + 1))
            }) else {
                return;
            };
            (name, fence_title_rect(rt, fence))
        }
    };
    rt.edit = Some(InlineEdit {
        target,
        rect,
        lines: vec![current],
        line: 0,
        col: 0,
        placeholder: String::new(),
        single_line: true,
        focused: true,
        composing: false,
        comp: String::new(),
        committing: false,
    });
    focus_overlay(rt);
    position_ime_window(rt);
    tracing::info!(target = ?target, "开始就地重命名（D2D 内联）");
}

/// 应用重命名结果：返回内容是否真的变化（变化才触发重绘）。
pub(crate) fn apply_rename(rt: &mut Runtime, target: EditTarget, new_name: &str) -> bool {
    let new_name = new_name.trim();
    if new_name.is_empty() {
        return false;
    }
    match target {
        EditTarget::FenceTitle { fence } => {
            let Some(f) = rt.desk.fences.get_mut(fence) else {
                return false;
            };
            let old = f
                .title
                .clone()
                .unwrap_or_else(|| format!("栅栏 {}", fence + 1));
            if old == new_name {
                return false;
            }
            f.title = Some(new_name.to_string());
            let _ = rt.store.save(&rt.desk);
            tracing::info!(fence, name = new_name, "栅栏改名");
            true
        }
        EditTarget::Item { fence, icon } => commit_icon_rename(rt, fence, icon, new_name),
    }
}

/// 重命名栅栏内图标（编辑程序名字）：改的是磁盘上的真实文件名（快捷方式保持
/// `.lnk`/`.url`/`.appref-ms` 扩展名），改名后重建元数据、位图与引用。
/// 返回内容是否真的变化。
pub(crate) fn commit_icon_rename(
    rt: &mut Runtime,
    fence: usize,
    icon: usize,
    new_name: &str,
) -> bool {
    let (id, path, current) = match rt.desk.fences.get(fence).and_then(|f| f.icon_ids.get(icon)) {
        Some(id) => match rt.desk.icons.get(id) {
            Some(ic) => (id.clone(), ic.path.clone(), ic.display_name.clone()),
            None => return false,
        },
        None => return false,
    };
    if new_name == current {
        return false;
    }
    let Some(old_path) = path else {
        tracing::warn!(id = %id, "虚拟项（无路径）无法改名");
        return false;
    };
    let Some(new_path) = new_path_for_rename(&old_path, new_name) else {
        tracing::warn!(old_path, "无法计算新路径");
        return false;
    };
    let new_path_str = new_path.to_string_lossy().into_owned();
    if new_path_str == old_path {
        return false;
    }
    // 磁盘改名：与原文件夹名一致——Windows 资源管理器也是这样直接改文件
    if let Err(e) = std::fs::rename(&old_path, &new_path) {
        tracing::warn!(old_path, new_path = %new_path_str, "文件改名失败: {e}");
        return false;
    }
    // 重建 DesktopItem（新路径 → 新 id/显示名/类别），替换 items 池中的项
    let new_item = match sylva_shell::items::item_from_path(&new_path_str) {
        Ok(it) => it,
        Err(e) => {
            tracing::warn!(new_path = %new_path_str, "重建图标项失败（文件已改名，重启后重新识别）: {e}");
            return false;
        }
    };
    let new_display = new_item.display_name.clone();
    let new_id = new_item.id.clone();
    if let Some(idx) = rt.item_index.remove(&id) {
        rt.items[idx] = new_item;
        rt.item_index.insert(new_id.clone(), idx);
    }
    // 更新元数据：旧 id 换新 id，path/显示名/详情按新路径补齐
    if let Some(mut ic) = rt.desk.icons.remove(&id) {
        ic.id = new_id.clone();
        ic.display_name = new_display;
        ic.path = Some(new_path_str.clone());
        sylva_core::details::enrich(&mut ic, &new_path_str);
        rt.desk.icons.insert(new_id.clone(), ic);
    }
    // 替换栅栏成员与自由区引用
    for f in &mut rt.desk.fences {
        if let Some(pos) = f.icon_ids.iter().position(|x| x == &id) {
            f.icon_ids[pos] = new_id.clone();
        }
    }
    if let Some(pos) = rt.desk.free_icons.iter().position(|x| x == &id) {
        rt.desk.free_icons[pos] = new_id.clone();
    }
    // 新 id → 新位图槽（不复用旧槽，避免与既有槽冲突），重新提取图标
    rt.bitmap_ids.remove(&id);
    let slot = rt.bitmap_ids.values().copied().max().unwrap_or(0) + 1;
    if let Some(idx) = rt.item_index.get(&new_id).copied() {
        match sylva_shell::icons::extract_icon(&rt.items[idx], ICON_EXTRACT_SIZE) {
            Ok(data) => {
                rt.bitmap_ids.insert(new_id.clone(), slot);
                rt.pending_uploads.push((slot, data));
            }
            Err(e) => tracing::warn!(new_path = %new_path_str, "改名后图标提取失败: {e}"),
        }
    }
    let _ = rt.store.save(&rt.desk);
    tracing::info!(old_path, new_path = %new_path_str, "图标改名");
    true
}

/// 注入一次「仅重绘」事件：让 `handle_event` 尾部重建场景与命中模型。
pub(crate) fn inject_rebuild(rt: &mut Runtime) {
    let ev = Box::new(OverlayEvent::EditCommitted);
    unsafe {
        let _ = PostMessageW(
            Some(rt.hwnd),
            WM_SYLVA_INJECT,
            WPARAM(0),
            LPARAM(Box::into_raw(ev) as isize),
        );
    }
}

/// 图标标签文本（当前显示名）。
pub(crate) fn item_name(rt: &Runtime, fence: usize, icon: usize) -> Option<String> {
    rt.desk
        .fences
        .get(fence)
        .and_then(|f| f.icon_ids.get(icon))
        .and_then(|id| rt.desk.icons.get(id))
        .map(|ic| ic.display_name.clone())
}

/// 图标标签的绘制矩形（物理像素，虚拟屏幕坐标）：就地编辑框的定位基准。
/// 与 `layout_fence` / `grid_icons` / `list_icons` 的几何保持一致。
pub(crate) fn item_label_rect(rt: &Runtime, fence: usize, icon: usize) -> Option<RectF> {
    let f = rt.desk.fences.get(fence)?;
    f.icon_ids.get(icon)?;
    let s = rt.theme.scale;
    let pad = f.appearance.padding * s;
    let title_block_h = rt.theme.title.size * 1.6 + rt.theme.title_padding_bottom;
    let content_top = f.bounds.y + pad + title_block_h + pad;
    let content_left = f.bounds.x + pad;
    let inner_w = (f.bounds.w - 2.0 * pad).max(1.0);
    match f.appearance.layout {
        FenceLayout::Grid => {
            let icon_size = f.appearance.icon_size * s;
            // 与 scene.rs 同口径：格宽保底 + 两行标签行高（否则编辑框/后续行错位）。
            let cell_w = grid_cell_w(icon_size, f.appearance.gap * s);
            let row_h = grid_row_h(&rt.theme, icon_size);
            let cols = ((inner_w / cell_w).floor() as usize).max(1);
            let ix = content_left + (icon % cols) as f32 * cell_w;
            let iy = content_top + (icon / cols) as f32 * row_h - f.scroll;
            // 编辑框覆盖两行标签区（含上下剪裁余量 8·scale，见 draw_inline_edit 的 4·scale 内缩）。
            let edit_h = rt.theme.label.size * GRID_CAPTION_H_MULT + 8.0 * s;
            Some(RectF {
                x: ix - 2.0,
                y: iy + icon_size + rt.theme.icon_caption_gap,
                w: cell_w - 2.0,
                h: edit_h,
            })
        }
        FenceLayout::List => {
            let label_h = rt.theme.label.size * 1.6;
            let list_icon = LIST_ICON_SIZE * s;
            let row_h = list_icon.max(label_h) + rt.theme.list_row_gap;
            let header_h = label_h + 8.0 * s;
            let type_w = LIST_TYPE_W * s;
            let mod_w = LIST_MOD_W * s;
            let size_w = LIST_SIZE_W * s;
            let col_gap = LIST_COL_GAP * s;
            let name_w = (inner_w - col_gap * 3.0 - type_w - mod_w - size_w).max(60.0 * s);
            let iy = content_top + header_h + icon as f32 * row_h - f.scroll;
            // 编辑框高度 = 文本行高 + 上下剪裁余量（同 Grid：见 Grid 分支注释）。
            let edit_h = label_h + 8.0 * s;
            Some(RectF {
                x: content_left + list_icon + rt.theme.list_label_gap,
                y: iy + (list_icon - label_h) / 2.0,
                w: name_w,
                h: edit_h,
            })
        }
        FenceLayout::Sidebar => {
            // 侧边栏无内联标签：编辑框放在图标旁侧，复用工具提示的定位口径
            // （纵向 dock 放图标右侧/左侧、垂直居中；横向 dock 放图标下方、水平居中）。
            let pos = f.appearance.sidebar_pos;
            let icon_size = f.appearance.icon_size * s;
            let eff_gap = f.appearance.gap * s + icon_size * 0.5;
            let (icon_x, icon_y) = if pos == SidebarPosition::Top {
                // 横向 dock：厚度 = 紧贴放大图标，图标垂直居中，排布自左往右
                let dock_h = icon_size * 1.5 + 6.0 * s * 2.0;
                let start_x = f.bounds.x + pad;
                let start_y = f.bounds.y + (dock_h - icon_size) / 2.0;
                (
                    start_x + icon as f32 * (icon_size + eff_gap) - f.scroll,
                    start_y,
                )
            } else {
                // 纵向 dock：图标在 dock 内水平居中，排布自上往下
                let dock_w = f.bounds.w.max(icon_size);
                let start_x = f.bounds.x + (dock_w - icon_size) / 2.0;
                let start_y = f.bounds.y + pad;
                (
                    start_x,
                    start_y + icon as f32 * (icon_size + eff_gap) - f.scroll,
                )
            };
            let label_h = rt.theme.label.size * 1.6;
            // 编辑框高度 = 文本行高 + 上下剪裁余量（同 Grid：见 Grid 分支注释）。
            let edit_h = label_h + 8.0 * s;
            let text = item_name(rt, fence, icon).unwrap_or_default();
            let w = (crate::scene::estimate_text_width(&text, rt.theme.label.size) * 1.2
                + 12.0 * s)
                .max(label_h);
            let gap_to_icon = 10.0 * s;
            let (mut bx, mut by) = match pos {
                SidebarPosition::Left => (
                    icon_x + icon_size + gap_to_icon,
                    icon_y + (icon_size - label_h) / 2.0,
                ),
                SidebarPosition::Right => (
                    icon_x - gap_to_icon - w,
                    icon_y + (icon_size - label_h) / 2.0,
                ),
                SidebarPosition::Top => (
                    icon_x + icon_size / 2.0 - w / 2.0,
                    icon_y + icon_size + gap_to_icon,
                ),
            };
            // 钳制到虚拟屏幕内：贴边停靠时编辑框不外溢到屏幕外（宽度超出贴边即可）
            bx = bx.max(4.0).min((rt.vw - w - 4.0).max(4.0));
            by = by.max(4.0).min((rt.vh - edit_h - 4.0).max(4.0));
            Some(RectF {
                x: bx,
                y: by,
                w,
                h: edit_h,
            })
        }
    }
}

/// 栅栏标题文本矩形（就地编辑框的定位基准）。
pub(crate) fn fence_title_rect(rt: &Runtime, fence: usize) -> RectF {
    let f = &rt.desk.fences[fence];
    let pad = rt.theme.fence_padding;
    // 编辑框按实际绘制字号（`draw_inline_edit` 恒用 label 格式）定高：
    // 文本行高 + 上下剪裁余量（绘制时内缩 4·scale），字形完整显示且垂直居中。
    let h = rt.theme.label.size * 1.6 + 8.0 * rt.theme.scale;
    RectF {
        x: f.bounds.x + pad,
        y: f.bounds.y + pad,
        w: (f.bounds.w - 2.0 * pad).max(1.0),
        h,
    }
}
