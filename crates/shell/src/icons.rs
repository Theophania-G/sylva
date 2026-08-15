//! 图标提取与缓存。
//!
//! 通过 `IShellItemImageFactory` 从 PIDL 提取 32bpp 图标位图，解码为
//! 顶层 BGRA（premultiplied）像素数据，供渲染层直接 `CopyFromMemory` 上传
//! 到 D2D 位图。`IconCache` 提供按 `ItemId` 的 LRU 缓存。
//!
//! 注意：`GetImage` 返回的位图是 premultiplied alpha，渲染层需用
//! `DXGI_FORMAT_B8G8R8A8_UNORM_PREMULTIPLIED` 对应模式上传，避免半透明
//! 图标出现黑边（若发现暗边则改为直通 alpha + 显式预乘）。

use std::collections::{HashMap, VecDeque};

use windows::Win32::Foundation::SIZE;
use windows::Win32::Graphics::Gdi::{DeleteObject, GetObjectW, DIBSECTION, HBITMAP};
use windows::Win32::UI::Shell::{
    IShellItemImageFactory, SHCreateItemFromIDList, SIIGBF, SIIGBF_ICONONLY,
};

use sylva_core::model::ItemId;

use crate::items::DesktopItem;

/// 一张图标的解码结果：顶层 BGRA（premultiplied），可直接上传 D2D 位图。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IconData {
    pub width: u32,
    pub height: u32,
    /// width*height*4 字节，B8G8R8A8（premultiplied），top-down。
    pub pixels: Vec<u8>,
}

impl IconData {
    /// 每行字节数（不带任何对齐，行与行紧挨）。
    pub fn stride(&self) -> usize {
        self.width as usize * 4
    }
}

// 本地错误码（windows-rs 0.62 未导出这些 HRESULT 常量）。
/// E_POINTER：位图没有可读取的内存位。
const E_NO_BITS: i32 = 0x8000_4003u32 as i32;
/// E_INVALIDARG：位图格式不是 32bpp DIB section。
const E_BAD_FORMAT: i32 = 0x8007_0057u32 as i32;
/// E_PENDING：shell 图标缓存尚未就绪，需要稍后重试。
const E_PENDING: i32 = 0x8000_000Au32 as i32;
/// E_FAIL：shell 首次提取失败的通用码，重试通常可命中。
const E_FAIL: i32 = 0x8000_4005u32 as i32;
/// 提取一个图标的最大尝试次数。冷缓存时 shell 需要数百毫秒到 1 秒完成
/// 后台提取，递增间隔重试覆盖该窗口；仅对真正失败的项付出等待。
const MAX_ICON_ATTEMPTS: u32 = 6;

/// 从图标项提取指定边长的图标像素。
///
/// shell 的系统图标缓存是跨进程共享、惰性填充的：新进程首次请求某图标时
/// `GetImage` 常返回 `E_PENDING` / `E_FAIL`，随后（缓存被填充后）再请求即成功。
/// 因此在有限次数内短重试。该函数设计在后台图标加载线程调用，阻塞可接受。
pub fn extract_icon(item: &DesktopItem, size: u32) -> windows::core::Result<IconData> {
    // 只用 SIIGBF_ICONONLY：带 BIGGERSIZEOK 的首次调用会走异步缩略图路径并
    // 返回 E_PENDING（同一 factory 后续调用才稳定）。纯 ICONONLY 走同步图标路径。
    let flags = SIIGBF(SIIGBF_ICONONLY.0);
    let requested = SIZE {
        cx: size as i32,
        cy: size as i32,
    };

    for attempt in 0..MAX_ICON_ATTEMPTS {
        // 每次重建 factory：shell 在首次 GetImage 时填充系统级图标缓存，
        // 缓存就绪前同一 factory 会反复返回 E_PENDING/E_FAIL，新 factory 才可命中。
        let factory: IShellItemImageFactory = unsafe { SHCreateItemFromIDList(item.pidl)? };
        match unsafe { factory.GetImage(requested, flags) } {
            Ok(b) => {
                let result = hbitmap_to_icon_data(b);
                // 无论成功与否都释放 GDI 对象
                let _del = unsafe { DeleteObject(b.into()) };
                return result;
            }
            Err(e)
                if e.code() == windows::core::HRESULT(E_PENDING)
                    || e.code() == windows::core::HRESULT(E_FAIL) =>
            {
                if attempt + 1 == MAX_ICON_ATTEMPTS {
                    return Err(e);
                }
                // 让出时间给 shell 的异步图标提取；间隔指数递增
                std::thread::sleep(std::time::Duration::from_millis(50 * (1u64 << attempt)));
            }
            Err(e) => return Err(e),
        }
    }
    unreachable!("重试循环必然返回")
}

/// 把 `IShellItemImageFactory` 返回的 HBITMAP 解码为像素数据。
///
/// 要求是 32bpp DIB section（`GetImage` 的标准产物）；非 DIB 或无位指针时报错。
fn hbitmap_to_icon_data(hbm: HBITMAP) -> windows::core::Result<IconData> {
    let mut dib = DIBSECTION::default();
    let n = unsafe {
        GetObjectW(
            hbm.into(),
            std::mem::size_of::<DIBSECTION>() as i32,
            Some(&mut dib as *mut DIBSECTION as *mut _),
        )
    };
    if n as usize != std::mem::size_of::<DIBSECTION>() {
        return Err(windows::core::Error::from_hresult(windows::core::HRESULT(
            E_BAD_FORMAT,
        )));
    }

    let bm = dib.dsBm;
    if bm.bmBits.is_null() {
        return Err(windows::core::Error::from_hresult(windows::core::HRESULT(
            E_NO_BITS,
        )));
    }
    if bm.bmBitsPixel != 32 {
        return Err(windows::core::Error::from_hresult(windows::core::HRESULT(
            E_BAD_FORMAT,
        )));
    }

    let width = bm.bmWidth.max(0) as u32;
    let height = bm.bmHeight.unsigned_abs();
    let mut pixels = vec![0u8; (width * height * 4) as usize];
    // bmHeight > 0 → bottom-up；< 0 → top-down
    copy_rows(
        width,
        height,
        bm.bmWidthBytes.max(0) as usize,
        bm.bmBits as *const u8,
        bm.bmHeight > 0,
        &mut pixels,
    );
    Ok(IconData {
        width,
        height,
        pixels,
    })
}

/// 从 DIB 位复制行像素，必要时垂直翻转（纯函数，可单测）。
///
/// `src_stride` 是源行距（含 DWORD 对齐）；输出行与行紧挨（stride = width*4）。
fn copy_rows(
    width: u32,
    height: u32,
    src_stride: usize,
    src: *const u8,
    bottom_up: bool,
    out: &mut [u8],
) {
    let row_bytes = width as usize * 4;
    debug_assert!(out.len() as u32 >= width * height * 4);
    for dst_row in 0..height {
        // bottom-up 时源的第 0 行是最底行，输出第 0 行对应源最后一行
        let src_row = if bottom_up {
            height - 1 - dst_row
        } else {
            dst_row
        };
        let dst_off = dst_row as usize * row_bytes;
        let src_off = src_row as usize * src_stride;
        unsafe {
            std::ptr::copy_nonoverlapping(
                src.add(src_off),
                out.as_mut_ptr().add(dst_off),
                row_bytes,
            );
        }
    }
}

/// 按 `ItemId` 的图标 LRU 缓存（`get` 刷新新鲜度，满时淘汰最久未用）。
pub struct IconCache {
    capacity: usize,
    map: HashMap<ItemId, IconData>,
    /// 从最久（队首）到最近（队尾）的键队列。
    recency: VecDeque<ItemId>,
}

impl IconCache {
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            map: HashMap::new(),
            recency: VecDeque::new(),
        }
    }

    /// 命中时刷新键的新鲜度并返回数据。
    pub fn get(&mut self, id: &ItemId) -> Option<&IconData> {
        let hit = self.map.get(id)?;
        if let Some(pos) = self.recency.iter().position(|k| k == id) {
            let key = self.recency.remove(pos).unwrap();
            self.recency.push_back(key);
        }
        Some(hit)
    }

    /// 插入（覆盖已存在的键），超出容量时淘汰最久未用的条目。
    ///
    /// 不用 Entry API：维护 recency 队列需在插入前后操作其他字段，
    /// Entry 可变借用会阻止对 self 的其他访问。
    #[allow(clippy::map_entry)]
    pub fn insert(&mut self, id: ItemId, data: IconData) {
        if self.map.contains_key(&id) {
            self.map.insert(id, data);
            return;
        }
        if self.map.len() >= self.capacity {
            self.evict();
        }
        self.recency.push_back(id.clone());
        self.map.insert(id, data);
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    /// 逐出最久未用的条目（队列中已失效的键一并跳过）。
    fn evict(&mut self) {
        while let Some(key) = self.recency.pop_front() {
            if self.map.remove(&key).is_some() {
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 构造一个 width*height*4 的合成 BGRA 缓冲，第 r 行第 0 字节记为 r。
    fn make_bgra_rows(rows: &[u8]) -> Vec<u8> {
        let mut v = Vec::new();
        for &tag in rows {
            v.extend_from_slice(&[tag, 0, 0, 255]);
        }
        v
    }

    #[test]
    fn bottom_up_rows_are_flipped() {
        // 3 行、行距 4（刚好无对齐）：源 bottom-up，输出应是顺序的 0,1,2
        let src = make_bgra_rows(&[2, 1, 0]);
        let mut out = vec![0u8; 12];
        copy_rows(1, 3, 4, src.as_ptr(), true, &mut out);
        assert_eq!(make_bgra_rows(&[0, 1, 2]), out);
    }

    #[test]
    fn top_down_rows_keep_order() {
        let src = make_bgra_rows(&[0, 1, 2]);
        let mut out = vec![0u8; 12];
        copy_rows(1, 3, 4, src.as_ptr(), false, &mut out);
        assert_eq!(src, out);
    }

    #[test]
    fn padded_stride_is_respected() {
        // 行距 8、有效 4 字节：每行 tag 后跟 4 字节垃圾；只拷贝有效区
        let src = make_bgra_rows(&[2, 1, 0]);
        let mut padded = Vec::new();
        for chunk in src.chunks(4) {
            padded.extend_from_slice(chunk);
            padded.extend_from_slice(&[99, 99, 99, 99]);
        }
        let mut out = vec![0u8; 12];
        copy_rows(1, 3, 8, padded.as_ptr(), true, &mut out);
        assert_eq!(make_bgra_rows(&[0, 1, 2]), out);
    }

    #[test]
    fn cache_evicts_least_recently_used() {
        let mut cache = IconCache::new(2);
        cache.insert("a".into(), icon_dummy(1));
        cache.insert("b".into(), icon_dummy(2));
        // 访问 a → 新鲜度顺序变为 [b, a]
        assert!(cache.get(&"a".into()).is_some());
        // 插入 c → 逐出 b
        cache.insert("c".into(), icon_dummy(3));
        assert!(cache.get(&"b".into()).is_none());
        assert!(cache.get(&"a".into()).is_some());
        assert!(cache.get(&"c".into()).is_some());
    }

    #[test]
    fn cache_get_refreshes_recency() {
        let mut cache = IconCache::new(2);
        cache.insert("a".into(), icon_dummy(1));
        cache.insert("b".into(), icon_dummy(2));
        cache.get(&"b".into());
        cache.insert("c".into(), icon_dummy(3));
        // b 最近被访问 → 留下；a 被逐出
        assert!(cache.get(&"a".into()).is_none());
        assert!(cache.get(&"b".into()).is_some());
        assert_eq!(cache.len(), 2);
    }

    fn icon_dummy(tag: u8) -> IconData {
        IconData {
            width: 1,
            height: 1,
            pixels: vec![tag, 0, 0, 255],
        }
    }

    #[test]
    fn live_extract_icons_from_real_desktop() {
        // 真实桌面冒烟：对前 5 个图标提取 32px 图标，验证尺寸与像素有效。
        if crate::com::init().is_err() {
            return;
        }
        let items = crate::items::enumerate_desktop_items().expect("枚举桌面图标不应失败");
        let mut checked = 0;
        for item in items.iter().take(5) {
            match extract_icon(item, 32) {
                Ok(icon) => {
                    eprintln!(
                        "  icon {:?}: {}x{}",
                        item.display_name, icon.width, icon.height
                    );
                    assert_eq!(icon.width, 32);
                    assert_eq!(icon.height, 32);
                    assert_eq!(icon.pixels.len(), 32 * 32 * 4);
                    checked += 1;
                }
                Err(e) => eprintln!("  FAIL {:?}: {e:?}", item.display_name),
            }
        }
        // 桌面上通常有可取图标的项
        assert!(checked >= 1, "真实桌面应至少成功提取 1 个图标");
    }
}
