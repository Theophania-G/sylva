//! 文件操作：添加路径、库同步、链接文件夹镜像、库内复制、剪贴板文件读取。

use std::collections::HashSet;

use windows::Win32::Storage::FileSystem::{
    GetFileAttributesW, FILE_ATTRIBUTE_HIDDEN, FILE_ATTRIBUTE_SYSTEM,
};

use crate::*;
pub(crate) fn new_path_for_rename(old_path: &str, new_name: &str) -> Option<PathBuf> {
    let p = Path::new(old_path);
    let parent = p.parent()?;
    let old_file = p.file_name()?.to_string_lossy().into_owned();
    let lower = old_file.to_ascii_lowercase();
    let stripped_ext = ["lnk", "url", "appref-ms"].iter().copied().find(|ext| {
        let dot = format!(".{ext}");
        lower.ends_with(&dot) && old_file.len() > ext.len() + 1
    });
    let final_name = match stripped_ext {
        Some(ext) if !new_name.to_ascii_lowercase().ends_with(&format!(".{ext}")) => {
            format!("{new_name}.{ext}")
        }
        _ => new_name.to_string(),
    };
    if final_name.is_empty() {
        return None;
    }
    Some(parent.join(final_name))
}

/// 把任意文件/文件夹/快捷方式路径加入指定栅栏（拖入 / 粘贴共用）。
///
/// 目标目录 = 栅栏的链接文件夹（`storage_path`，有则用）否则内部库。链接栅栏由此实现
/// 「栅栏 → 文件夹」方向：拖入/粘贴的文件落进文件夹而非内部库。源已在目标目录内
/// （幂等粘贴/跨栅栏移动）直接复用。位图随后在 `handle_event` 末尾随场景上传。
pub(crate) fn add_paths_to_fence(rt: &mut Runtime, fence: usize, paths: &[String]) {
    let dest_dir = rt
        .desk
        .fences
        .get(fence)
        .and_then(|f| f.storage_path.clone())
        .map(PathBuf::from);
    for src in paths {
        let src_path = Path::new(src);
        // 先物理复制进目标目录：栅栏索引的是「目录内副本」，目录/库内删除 → 栅栏项同步删。
        let target: PathBuf = match &dest_dir {
            Some(d) if path_within(d, src_path) => src_path.to_path_buf(),
            Some(d) => match copy_into_dir(src_path, d) {
                Ok(p) => p,
                Err(e) => {
                    tracing::warn!(src, "复制进链接文件夹失败: {e}");
                    continue;
                }
            },
            None if is_inside_library(rt, src_path) => src_path.to_path_buf(),
            None => match copy_into_library(rt, src_path) {
                Ok(p) => p,
                Err(e) => {
                    tracing::warn!(src, "复制进库失败: {e}");
                    continue;
                }
            },
        };
        register_fence_item(rt, fence, &target.to_string_lossy());
    }
}

/// 把一个已落在目标目录内的路径注册为栅栏图标：建 `DesktopItem`、录入元数据、
/// 分配位图槽、追加成员、提取图标（进 `pending_uploads`）。已在本栅栏则幂等跳过。
/// 拖入/粘贴（`add_paths_to_fence`）与链接文件夹镜像（`mirror_linked_fence`）共用。
pub(crate) fn register_fence_item(rt: &mut Runtime, fence: usize, path: &str) -> bool {
    let item = match sylva_shell::items::item_from_path(path) {
        Ok(it) => it,
        Err(e) => {
            tracing::warn!(path, "无法创建图标项: {e}");
            return false;
        }
    };
    let id = item.id.clone();
    // 已在本栅栏则跳过（防止同栅栏内重复）；在其它栅栏/自由区允许再添加——
    // 「复制→粘贴到另一个栅栏」就是让同一项出现在两个栅栏里（跨栅栏粘贴失效根因）。
    // 除同 id 外再按「同路径（小写）」判重：旧版「更改位置」迁移后 id 可能未随路径
    // 重建（陈旧 id），避免文件夹镜像把同一文件重复加进来。
    let already = rt
        .desk
        .fences
        .get(fence)
        .map(|f| {
            f.icon_ids.iter().any(|existing_id| {
                if *existing_id == id {
                    return true;
                }
                rt.desk
                    .icons
                    .get(existing_id)
                    .and_then(|ic| ic.path.clone())
                    .map(|p| p.eq_ignore_ascii_case(path))
                    .unwrap_or(false)
            })
        })
        .unwrap_or(false);
    if already {
        return false;
    }
    // 录入元数据：持久化目标目录内路径（重启恢复图标/打开）；added=true 表示受 Sylva 管理
    let mut ic = Icon::new(id.clone(), item.display_name.clone(), item.kind);
    ic.path = Some(path.to_string());
    ic.added = true;
    sylva_core::details::enrich(&mut ic, path);
    rt.desk.icons.insert(id.clone(), ic);
    let new_bitmap = rt.items.len() as u64;
    rt.items.push(item);
    rt.item_index.insert(id.clone(), rt.items.len() - 1);
    rt.bitmap_ids.insert(id.clone(), new_bitmap);
    if let Some(f) = rt.desk.fences.get_mut(fence) {
        f.icon_ids.push(id.clone());
    }
    // 提取图标（阻塞但命中系统图标缓存，通常很快）；失败不阻断添加
    let idx = rt.item_index[&id];
    match sylva_shell::icons::extract_icon(&rt.items[idx], ICON_EXTRACT_SIZE) {
        Ok(data) => rt.pending_uploads.push((new_bitmap, data)),
        Err(e) => tracing::warn!(path, "图标提取失败: {e}"),
    }
    true
}

/// 更改栅栏的存储位置：将栅栏内所有库内项移动到新目录，更新路径引用。
/// 桌面枚举项（added=false）不受影响（它们的文件由系统管理）。
pub(crate) fn change_fence_storage(rt: &mut Runtime, fence_idx: usize, new_dir: &str) {
    let new_path = std::path::Path::new(new_dir);
    if !new_path.is_dir() {
        tracing::warn!(new_dir, "目标路径不是文件夹");
        return;
    }
    let Some(fence) = rt.desk.fences.get(fence_idx) else {
        return;
    };
    let fence_id = fence.id;
    // 旧链接文件夹：换链接后，旧文件夹里的镜像项不再是本栅栏成员（文件保留在磁盘）
    let old_dir = fence.storage_path.clone();
    // 收集需要移动的库内项（added=true 且路径在旧库内）
    let items_to_move: Vec<String> = fence
        .icon_ids
        .iter()
        .filter(|id| {
            rt.desk
                .icons
                .get(*id)
                .map(|ic| {
                    ic.added
                        && ic
                            .path
                            .as_ref()
                            .map(|p| is_inside_library(rt, std::path::Path::new(p)))
                            .unwrap_or(false)
                })
                .unwrap_or(false)
        })
        .cloned()
        .collect();
    if items_to_move.is_empty() {
        // 没有库内项需要移动，直接更新 storage_path
        if let Some(f) = rt.desk.fences.get_mut(fence_idx) {
            f.storage_path = Some(new_dir.to_string());
        }
        // 换链接：清理旧链接文件夹里的残留镜像项（文件保留在旧文件夹，不删）
        if let Some(old) = &old_dir {
            clear_stale_linked_items(rt, fence_idx, old, new_path);
        }
        // 链接：把文件夹里已有的文件立即镜像进栅栏（此后增删由后台 SyncLibrary 持续同步）
        if reconcile_fences(rt) {
            tracing::info!(fence = fence_id, "链接后新增了文件夹里的栅栏项");
        }
        let _ = rt.store.save(&rt.desk);
        tracing::info!(fence = fence_id, new_dir, "无库内项需移动，仅更新存储路径");
        return;
    }
    // 确保目标目录存在
    let _ = std::fs::create_dir_all(new_path);
    let mut moved = 0usize;
    for id in &items_to_move {
        let old_path_str = match rt.desk.icons.get(id).and_then(|ic| ic.path.clone()) {
            Some(p) => p,
            None => continue,
        };
        let old_path = std::path::Path::new(&old_path_str);
        let file_name = match old_path.file_name().and_then(|n| n.to_str()) {
            Some(n) => n.to_string(),
            None => continue,
        };
        let dest = unique_library_path(new_path, &file_name);
        // 移动文件/文件夹
        let move_ok = if old_path.is_dir() {
            copy_dir_all(old_path, &dest).is_ok()
        } else {
            std::fs::copy(old_path, &dest).is_ok()
        };
        if !move_ok {
            tracing::warn!(old_path_str, "移动文件失败");
            continue;
        }
        // 删除旧文件（移动语义）
        if old_path.is_dir() {
            let _ = std::fs::remove_dir_all(old_path);
        } else {
            let _ = std::fs::remove_file(old_path);
        }
        // 更新 Icon.path
        let new_path_str = dest.to_string_lossy().into_owned();
        if let Some(ic) = rt.desk.icons.get_mut(id) {
            ic.path = Some(new_path_str.clone());
        }
        // 更新 DesktopItem 路径（用于后续打开/提取图标）
        if let Some(&idx) = rt.item_index.get(id) {
            if let Some(item) = rt.items.get_mut(idx) {
                item.path = Some(new_path_str.clone());
            }
        }
        moved += 1;
    }
    // 更新栅栏的存储路径
    if let Some(f) = rt.desk.fences.get_mut(fence_idx) {
        f.storage_path = Some(new_dir.to_string());
    }
    // 换链接：清理旧链接文件夹里的残留镜像项（文件保留在旧文件夹，不删）
    if let Some(old) = &old_dir {
        clear_stale_linked_items(rt, fence_idx, old, new_path);
    }
    // 链接：把文件夹里已有的文件立即镜像进栅栏（此后增删由后台 SyncLibrary 持续同步）
    if reconcile_fences(rt) {
        tracing::info!(fence = fence_id, "链接后新增了文件夹里的栅栏项");
    }
    let _ = rt.store.save(&rt.desk);
    tracing::info!(
        fence = fence_id,
        new_dir,
        moved,
        total = items_to_move.len(),
        "栅栏存储位置已更改"
    );
}

/// 换链接后清理旧链接文件夹里的残留镜像项：`added` 项路径在旧文件夹内、
/// 不在新文件夹内，说明它属于旧链接而非新链接。从栅栏整体摘下（引用删除、
/// 文件保留在旧文件夹磁盘上，用户仍可从资源管理器访问），使栅栏内容与
/// 新链接文件夹保持一致，不再残留旧文件夹的项。
fn clear_stale_linked_items(rt: &mut Runtime, fence_idx: usize, old_dir: &str, new_path: &Path) {
    let stale: Vec<String> = rt
        .desk
        .fences
        .get(fence_idx)
        .map(|f| {
            f.icon_ids
                .iter()
                .filter(|id| {
                    rt.desk
                        .icons
                        .get(*id)
                        .map(|ic| {
                            ic.added
                                && ic
                                    .path
                                    .as_ref()
                                    .map(|p| {
                                        path_within(Path::new(old_dir), Path::new(p))
                                            && !path_within(new_path, Path::new(p))
                                    })
                                    .unwrap_or(false)
                        })
                        .unwrap_or(false)
                })
                .cloned()
                .collect()
        })
        .unwrap_or_default();
    for id in stale {
        tracing::info!(
            fence = fence_idx,
            id,
            "换链接：旧文件夹残留项移出栅栏（文件保留在磁盘）"
        );
        remove_icon_entirely(rt, &id);
    }
}

/// 库同步：检查所有内部库项，`library` 里的文件已被外部删除时，栅栏对应项同步移除。
/// 返回是否有项被移除（调用方可据此决定持久化；重绘由事件尾部统一完成）。
pub(crate) fn reconcile_library(rt: &mut Runtime) -> bool {
    let missing: Vec<String> = rt
        .desk
        .icons
        .iter()
        .filter(|(_, ic)| ic.added)
        .filter(|(_, ic)| {
            ic.path
                .as_ref()
                .map(|p| is_inside_library(rt, Path::new(p)) && !Path::new(p).exists())
                .unwrap_or(false)
        })
        .map(|(id, _)| id.clone())
        .collect();
    let mut changed = false;
    for id in missing {
        changed = true;
        remove_icon_entirely(rt, &id);
    }
    if changed {
        tracing::info!(removed = changed, "库内文件被删，同步移除栅栏项");
    }
    changed
}

/// 双向同步总入口：内部库删除同步（`reconcile_library`）+ 每个链接栅栏的文件夹镜像。
/// 返回是否有任何项被增/删（调用方据此持久化；重绘由事件尾部统一完成）。
pub(crate) fn reconcile_fences(rt: &mut Runtime) -> bool {
    let mut changed = reconcile_library(rt);
    for idx in 0..rt.desk.fences.len() {
        if mirror_linked_fence(rt, idx) {
            changed = true;
        }
    }
    changed
}

/// 镜像一个链接栅栏的存储文件夹（文件夹 → 栅栏方向）：资源管理器里对文件夹的
/// 新增/删除/改名 ≤ 后台 `SyncLibrary` 周期（4s）反映到栅栏。栅栏即文件夹——
/// 目录内容与栅栏成员互为差集：多出的路径注册进栅栏，消失的路径移除对应图标。
///
/// 廉价快路径：枚举目录名集合与栅栏内「路径在该目录下」的图标路径集合做比较，
/// 集合相同立即返回——平时每周期只做一次 `read_dir`，低 CPU；仅差集非空才做
/// 昂贵的 `DesktopItem` 构建/移除。文件夹不可用（被删/断连）时不动（防御）。
fn mirror_linked_fence(rt: &mut Runtime, idx: usize) -> bool {
    let Some(dir) = rt.desk.fences.get(idx).and_then(|f| f.storage_path.clone()) else {
        return false;
    };
    let dir = PathBuf::from(dir);
    if !dir.is_dir() {
        return false;
    }
    // 链接 = 双向：先把栅栏里仍留在内部库的旧项物理迁进链接文件夹（栅栏里的东西 →
    // 文件夹），之后栅栏才真正镜像该文件夹。一次性迁移，完成后路径都在文件夹内，
    // 之后不再触发。文件已不存在的不迁（交给移除分支处理）。
    let mut changed = rehome_linked_library_items(rt, idx, &dir);
    // 栅栏内「路径在该目录下」的图标路径集合（小写，与磁盘大小写无关）
    let existing: HashSet<String> = rt
        .desk
        .fences
        .get(idx)
        .map(|f| {
            f.icon_ids
                .iter()
                .filter_map(|id| rt.desk.icons.get(id).and_then(|ic| ic.path.clone()))
                .filter(|p| path_within(&dir, Path::new(p)))
                .map(|p| p.to_ascii_lowercase())
                .collect()
        })
        .unwrap_or_default();
    // 枚举目录（跳过隐藏/系统文件，与资源管理器默认一致）；读取失败视为空 → 走删除分支
    let entries: Vec<PathBuf> = std::fs::read_dir(&dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| should_mirror(p))
                .collect()
        })
        .unwrap_or_default();
    let entry_set: HashSet<String> = entries
        .iter()
        .map(|p| p.to_string_lossy().to_ascii_lowercase())
        .collect();
    if entry_set == existing {
        return changed; // 目录内容与栅栏一致 → 无事可做（可能刚迁移过库内项）
    }
    // 文件夹 → 栅栏：新出现的文件/子文件夹注册进栅栏（含改名产生的新路径）
    for p in &entries {
        let lower = p.to_string_lossy().to_ascii_lowercase();
        if !existing.contains(&lower) && register_fence_item(rt, idx, &p.to_string_lossy()) {
            changed = true;
        }
    }
    // 栅栏 → 文件夹：路径在该目录下但文件已不存在 → 移除（外部删除/改名）
    let ids: Vec<String> = rt
        .desk
        .fences
        .get(idx)
        .map(|f| f.icon_ids.clone())
        .unwrap_or_default();
    for id in ids {
        let Some(p) = rt.desk.icons.get(&id).and_then(|ic| ic.path.clone()) else {
            continue;
        };
        if !path_within(&dir, Path::new(&p)) {
            continue;
        }
        if !Path::new(&p).exists() {
            remove_icon_entirely(rt, &id);
            changed = true;
        }
    }
    changed
}

/// 链接栅栏的旧库内项迁移：把「路径仍在内部库」的栅栏图标物理移到链接文件夹，
/// 实现「栅栏里的东西 → 文件夹」。一次性——迁移后路径都在文件夹内不再触发。
fn rehome_linked_library_items(rt: &mut Runtime, idx: usize, dir: &Path) -> bool {
    let ids: Vec<String> = rt
        .desk
        .fences
        .get(idx)
        .map(|f| f.icon_ids.clone())
        .unwrap_or_default();
    let mut changed = false;
    for id in ids {
        let in_library = rt
            .desk
            .icons
            .get(&id)
            .and_then(|ic| ic.path.clone())
            .map(|p| is_inside_library(rt, Path::new(&p)))
            .unwrap_or(false);
        if in_library && rehome_item(rt, &id, dir) {
            changed = true;
        }
    }
    changed
}

/// 把单个图标对应的磁盘文件从旧路径移动到 `dest_dir`（同名冲突自动改名），
/// 更新图标/项的路径引用。id（= 小写路径）保持旧值——`register_fence_item` 的
/// 路径级判重兜底陈旧 id，避免镜像重复添加。返回是否真的移动了。
fn rehome_item(rt: &mut Runtime, id: &str, dest_dir: &Path) -> bool {
    let Some(old_path_str) = rt.desk.icons.get(id).and_then(|ic| ic.path.clone()) else {
        return false;
    };
    let old_path = Path::new(&old_path_str);
    if !old_path.exists() {
        return false; // 文件已不存在：交给移除分支
    }
    let Some(file_name) = old_path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    let dest = unique_library_path(dest_dir, file_name);
    let move_ok = if old_path.is_dir() {
        copy_dir_all(old_path, &dest).is_ok()
    } else {
        std::fs::copy(old_path, &dest).is_ok()
    };
    if !move_ok {
        tracing::warn!(old_path_str, "迁移到链接文件夹失败");
        return false;
    }
    if old_path.is_dir() {
        let _ = std::fs::remove_dir_all(old_path);
    } else {
        let _ = std::fs::remove_file(old_path);
    }
    let new_path_str = dest.to_string_lossy().into_owned();
    if let Some(ic) = rt.desk.icons.get_mut(id) {
        ic.path = Some(new_path_str.clone());
    }
    if let Some(&idx) = rt.item_index.get(id) {
        if let Some(item) = rt.items.get_mut(idx) {
            item.path = Some(new_path_str.clone());
        }
    }
    tracing::info!(id, from = %old_path_str, to = %new_path_str, "栅栏库内项已迁移到链接文件夹");
    true
}

/// 该路径是否应在栅栏镜像中显示：跳过隐藏/系统文件（与资源管理器默认一致，
/// 免 `desktop.ini`、缩略图缓存等混入）。读取失败（被占用/已删）视为不显示。
fn should_mirror(path: &Path) -> bool {
    let w = crate::context_menu::wide(&path.to_string_lossy());
    let attr = unsafe { GetFileAttributesW(PCWSTR(w.as_ptr())) };
    if attr == u32::MAX {
        return false;
    }
    attr & (FILE_ATTRIBUTE_HIDDEN.0 | FILE_ATTRIBUTE_SYSTEM.0) == 0
}

/// `path` 是否位于内部库文件夹内（组件级大小写不敏感前缀比较，含边界）。
pub(crate) fn is_inside_library(rt: &Runtime, path: &Path) -> bool {
    let lib: Vec<String> = rt
        .library
        .components()
        .map(|c| c.as_os_str().to_string_lossy().to_lowercase())
        .collect();
    if lib.is_empty() {
        return false;
    }
    let comps: Vec<String> = path
        .components()
        .map(|c| c.as_os_str().to_string_lossy().to_lowercase())
        .collect();
    comps.len() >= lib.len() && lib.iter().zip(comps.iter()).all(|(a, b)| a == b)
}

/// `path` 是否**严格位于** `root` 目录内（组件级大小写不敏感前缀比较）。
/// 组件边界保证 `C:\lib` 不会匹配 `C:\library\f.txt`；路径即根本身不算「在内」。
pub(crate) fn path_within(root: &Path, path: &Path) -> bool {
    let root_c: Vec<String> = root
        .components()
        .map(|c| c.as_os_str().to_string_lossy().to_lowercase())
        .collect();
    if root_c.is_empty() {
        return false;
    }
    let comps: Vec<String> = path
        .components()
        .map(|c| c.as_os_str().to_string_lossy().to_lowercase())
        .collect();
    comps.len() > root_c.len() && root_c.iter().zip(comps.iter()).all(|(a, b)| a == b)
}

/// 路径是否位于某个栅栏的链接存储文件夹内（「移出栅栏 = 删除文件」判定用）。
pub(crate) fn is_linked_path(rt: &Runtime, path: &str) -> bool {
    let p = Path::new(path);
    rt.desk.fences.iter().any(|f| {
        f.storage_path
            .as_ref()
            .map(|d| path_within(Path::new(d), p))
            .unwrap_or(false)
    })
}

/// 是否属于 Sylva 管理区：内部库或任一栅栏的链接存储文件夹。管理区内的文件删除
/// 会真实删到磁盘（删除/移出动作），管理区外（桌面源文件）只动引用不碰文件。
pub(crate) fn is_managed_path(rt: &Runtime, path: &Path) -> bool {
    is_inside_library(rt, path) || is_linked_path(rt, &path.to_string_lossy())
}

/// 删除 Sylva 管理区内的磁盘文件/文件夹（内部库或链接文件夹）；不在管理区则不碰。
/// 供「删除」动作使用：管理区内删文件，桌面源文件保留。
pub(crate) fn delete_managed_file(rt: &Runtime, id: &str) {
    let Some(p) = rt.desk.icons.get(id).and_then(|ic| ic.path.clone()) else {
        return;
    };
    let pp = Path::new(&p);
    if !is_managed_path(rt, pp) || !pp.exists() {
        return;
    }
    if pp.is_dir() {
        let _ = std::fs::remove_dir_all(pp);
    } else {
        let _ = std::fs::remove_file(pp);
    }
}

/// 把文件/文件夹复制进指定目录；返回目录内目标路径。同名自动改名 `name (1).ext`。
pub(crate) fn copy_into_dir(src: &Path, dir: &Path) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let name = src.file_name().and_then(|n| n.to_str()).unwrap_or("item");
    let dest = unique_library_path(dir, name);
    if src.is_dir() {
        copy_dir_all(src, &dest)?;
    } else {
        std::fs::copy(src, &dest)?;
    }
    Ok(dest)
}

/// 把文件/文件夹复制进内部库；返回库内目标路径。
pub(crate) fn copy_into_library(rt: &Runtime, src: &Path) -> std::io::Result<PathBuf> {
    copy_into_dir(src, &rt.library)
}

/// 库内不重名路径：存在则 `name (n).ext` 递增。
pub(crate) fn unique_library_path(lib: &Path, name: &str) -> PathBuf {
    let cand = lib.join(name);
    if !cand.exists() {
        return cand;
    }
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i..]),
        _ => (name, ""),
    };
    for i in 1..1000 {
        let cand = lib.join(format!("{stem} ({i}){ext}"));
        if !cand.exists() {
            return cand;
        }
    }
    cand
}

/// 递归复制目录（不跟符号链接，按常规文件处理）。
pub(crate) fn copy_dir_all(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        let target = dst.join(entry.file_name());
        if ty.is_dir() {
            copy_dir_all(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

/// 把一个图标从 Sylva 中整体移除（仅删引用，不碰磁盘文件）。
/// 用于「拖入/粘贴新增项」的移除——它们不属于真实桌面，移出栅栏即删除。
pub(crate) fn remove_icon_entirely(rt: &mut Runtime, id: &str) {
    rt.desk.icons.remove(id);
    rt.desk.free_icons.retain(|x| x != id);
    for f in &mut rt.desk.fences {
        f.icon_ids.retain(|x| x != id);
    }
    rt.bitmap_ids.remove(id);
    // 从 items 池移除对应 DesktopItem（持有 PIDL，Drop 时释放），并重建下标
    if let Some(i) = rt.items.iter().position(|it| it.id == *id) {
        rt.items.remove(i);
    }
    rt.item_index = rt
        .items
        .iter()
        .enumerate()
        .map(|(i, it)| (it.id.clone(), i))
        .collect();
}

/// 剪贴板格式：CF_HDROP（拖放文件列表）。windows-rs 0.62 将其定义在
/// `Win32_System_Ole` 里；这里用文档稳定值 15，避免引入整个 Ole 功能集。
pub(crate) const CF_HDROP: u32 = 15;

/// 读剪贴板里的文件列表（CF_HDROP）。
pub(crate) fn clipboard_file_paths() -> Vec<String> {
    let mut out = Vec::new();
    unsafe {
        if OpenClipboard(None).is_err() {
            return out;
        }
        if let Ok(handle) = GetClipboardData(CF_HDROP) {
            if !handle.is_invalid() {
                let hdrop = HDROP(handle.0);
                let n = DragQueryFileW(hdrop, u32::MAX, None);
                for i in 0..n {
                    let len = DragQueryFileW(hdrop, i, None);
                    let mut buf = vec![0u16; len as usize + 1];
                    DragQueryFileW(hdrop, i, Some(&mut buf));
                    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
                    out.push(String::from_utf16_lossy(&buf[..end]));
                }
            }
        }
        let _ = CloseClipboard();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_within_component_aware_case_insensitive() {
        assert!(path_within(
            Path::new(r"C:\lib"),
            Path::new(r"C:\lib\a.txt")
        ));
        assert!(path_within(
            Path::new(r"C:\Lib"),
            Path::new(r"c:\lib\sub\b.txt")
        ));
        // 组件边界：C:\lib 不匹配 C:\library\f.txt
        assert!(!path_within(
            Path::new(r"C:\lib"),
            Path::new(r"C:\library\f.txt")
        ));
        // 路径即根本身：不算「在内」（严格在内）
        assert!(!path_within(Path::new(r"C:\lib"), Path::new(r"C:\lib")));
        // 父级不是子的前缀
        assert!(!path_within(Path::new(r"C:\lib\a"), Path::new(r"C:\lib")));
    }

    #[test]
    fn path_within_empty_root_is_never_within() {
        assert!(!path_within(Path::new(""), Path::new(r"C:\x\y.txt")));
    }
}
