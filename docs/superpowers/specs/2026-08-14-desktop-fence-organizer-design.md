# 桌面栅栏整理器（Desktop Fence Organizer）设计文档

日期：2026-08-14
状态：已确认

## 1. 背景与目标

做一个 Windows 桌面图标整理栅栏工具，对标 Stardock Fences，但在**功能和使用体验上全面超越**。目标形态为商业级产品。

**核心主张**：Fences 是「给你一堆栅栏壳子」；本产品是「桌面自己学会整理」——开箱即分类、拖一拖就教会它、换图标自动归位，配合亚克力质感与 60fps 丝滑动画，且常驻内存 <30MB。

## 2. 差异化定位（vs Fences）

用户确认的三个差异化方向：
1. **智能自动整理**——规则引擎 + 本地行为学习，比 Fences 的简单静态分类聪明
2. **现代交互质感**——丝滑动画、磁吸对齐、圆角亚克力、全局热键，整体质感碾压 Fences 的老式 Win32 UI
3. **极致轻量性能**——常驻 <30MB、空闲 CPU≈0、动画 60fps、长期运行无内存泄漏

明确不做（v1）：
- 多场景/多配置切换（用户未选）
- 云同步
- 本地 LLM 分类（v1 用规则 + 行为学习替代）
- 在线激活服务端（仅预留接口）
- 跨平台 / 移动端
- MSIX 安装器

## 3. 平台与技术栈

- **平台**：仅 Windows（Win10 / Win11）
- **语言**：Rust
- **关键依赖**：`windows-rs`、Direct2D、DirectWrite、DirectComposition、egui（设置 UI）、crossbeam-channel（事件总线）
- **不引入**：tokio、WebView2/Tauri（常驻桌面悬浮层用 WebView 太重）

## 4. 架构总览

单进程、单实例（mutex 防重复启动）。三个窗口形态：
- **覆盖层窗口**（每显示器一个）：无边框 layered，`SetParent` 到桌面 worker 层（WorkerW），层级在壁纸之上、其他窗口之下，DirectComposition 驱动视觉树
- **设置窗口**（egui）：普通窗口
- **托盘图标**：`Shell_NotifyIcon`

**线程模型**：主线程跑 Win32 消息循环；后台线程处理枚举/文件/网络；层间用 crossbeam-channel 事件总线通信。

### 模块划分（Cargo workspace）

```
crates/
├── core/      纯 Rust 业务内核：模型·布局·磁吸·动画状态机·规则引擎·行为学习·配置
├── shell/     windows-rs 适配层：接管·图标枚举·图标提取·右键菜单·拖拽·Shell事件·多屏
├── render/    Direct2D + DirectWrite + DirectComposition：栅栏绘制·亚克力·图标·文字
├── ui/        egui 设置界面
├── update/    自动更新
├── crash/     崩溃捕获 + 上报
└── app/       组合根 + 事件总线接线 + 生命周期
```

**核心引擎（`core/`）零 Win32 依赖，可单元测试。**

## 5. 数据模型

```rust
struct Desk {
    fences: Vec<Fence>,
    icons: HashMap<ItemId, Icon>,
    settings: AppSettings,
    version: u32,
}

struct Fence {
    id: FenceId,
    title: Option<String>,
    category: CategoryId,        // 栅栏绑定分类，而非固定图标列表
    bounds: Rect,                // 逻辑坐标，每 monitor 独立
    state: FenceState,           // Expanded | Folded
    icon_ids: Vec<ItemId>,       // 有序，即布局顺序
    appearance: FenceAppearance, // 颜色/亚克力/圆角
}

struct Icon {
    shell_ref: ShellItemRef,     // 引用 shell 项（PIDL/路径指纹）
    display_name: String,
    kind: ItemKind,              // App | Folder | Doc | Drive | Link
    category: CategoryId,
    user_pinned: bool,           // 用户是否手动指定过分类
    grid_pos: Vec2,              // 栅栏内网格坐标
}
```

**关键设计**：栅栏绑定一个「分类」而不是绑定固定图标。图标分类由规则/学习决定 → 自动归入对应栅栏；用户手动拖拽 = 对该分类的确认与学习。这是智能整理的地基。

## 6. 壳层接管（最难点 + Wallpaper Engine 反冲突）

### 6.1 启动序列
1. `FindWindow("Progman")` → 运行时探测「当前持有 `SHELLDLL_DefView` 的那个 WorkerW」（不假设经典 Progman→WorkerW→DefView 结构）
2. 若图标视图未启动，发 `WM_SPAWN_WORKERW`（`0x052C`）触发
3. 隐藏真实 `SysListView32`（保留句柄，卸载时 `SW_SHOW` 恢复）
4. overlay 窗口作为**最后一个子窗口**插入持有 DefView 的 WorkerW
5. 通过 `IShellFolder`（桌面文件夹）枚举桌面项（PIDL），`IShellItemImageFactory` 按 DPI 提取多尺寸图标，内存 LRU 缓存
6. **Explorer 重启 / Wallpaper Engine 重启 / 换壁纸 → 层级重建自愈**：监听窗口树变化，自动重新插入 overlay，不白屏

### 6.2 与 Wallpaper Engine 反冲突（硬约束）
Wallpaper Engine 用同一套 WorkerW/Progman 机制，且会重挂 `SHELLDLL_DefView`。本产品采取以下硬约束根除冲突：

1. **运行时探测真实层级**：不写死层级链，动态找当前持有 `SHELLDLL_DefView` 的 WorkerW，它在哪就挂到哪
2. **绝不重挂、绝不销毁别人的窗口**：只做「隐藏 SysListView32 + 插入自己的 overlay 为 sibling」，不新建 WorkerW、不移动 DefView、不销毁任何非己创建的窗口
3. **overlay 透明可穿透**：
   - 无栅栏区域：全透明 + `WM_NCHITTEST` 返回 `HTTRANSPARENT`，WE 动画壁纸完整透出
   - 栅栏区域：仅栅栏像素参与绘制
   - 桌面空白处右键菜单由本产品自行接管实现（DefView 隐藏后无人处理）
4. **WE 进程检测与优雅降级**：检测 `wallpaper32/64.exe`，存在时走共存模式并记录日志；同一套逻辑不特判
5. **层级重建自愈**：WE 换壁纸/Explorer 重启都会重建窗口树，需自动重新插入 overlay
6. **图标枚举不依赖 DefView**：走 `IShellFolder`，WE 开启「隐藏图标」模式时 DefView 不存在也能取到图标

**视觉结果**：WE 壁纸在最底 → 透明栅栏浮在上面 → 图标渲染在本产品层。壁纸动画完整、栅栏正常、无遮挡无闪烁。

## 7. 渲染层

- **DirectComposition**：每个栅栏一个 Visual，折叠/展开动画为 Visual 的 scale/translate——硬件合成 60fps，几乎零 CPU
- **Direct2D**：圆角栅栏背景 + 亚克力效果
- **DirectWrite**：图标标签文字，DPI 感知
- **亚克力兼容**：Win11 用 `DWM_SYSTEMBACKDROP_TYPE`，Win10 降级半透明纯色

## 8. 智能自动整理（本地、隐私干净）

- **规则引擎（v1 主力）**：输入 = 项类型 + 扩展名 + 路径 + 名称 + 父目录
  - `.exe` 位于 Program Files / 用户安装目录 / Start Menu → 按产品名归「工具/娱乐/开发」
  - 文档目录 + 扩展名白名单 → 「文档」
  - 快捷方式 → 解析 target 后再判定（.lnk 指向浏览器 = 上网工具）
  - 目录 → 「项目/文件夹」
  - 优先级有序，先命中先归位
- **行为学习（差异化核心）**：用户手动把图标从 A 栅栏拖到 B → 记录 (项特征 → 分类) 加权，下次同特征新图标自动归 B。数据存 SQLite。提供「撤销上一条学习」「一键重置分类」。
- **新增图标自动归位**：桌面冒出新图标 → 匹配规则 → 自动进对应栅栏 + 短暂高亮动效提示
- **隐私卖点**：全部本地处理，不上传文件名

## 9. 交互 & 动画

- **折叠/展开**：双击栅栏标题 → 折叠成一条标题栏（DComp 高度动画）；双击桌面空白 → 全部折叠/全部展开（可配置）
- **磁吸对齐**：拖动栅栏靠近屏幕边缘/其他栅栏时吸附；栅栏移动时图标整体跟随
- **拖入**：资源管理器/浏览器文件拖进栅栏 → `IDropTarget` 接收 → 自动分类 + 加入
- **图标拖拽**：栅栏内网格重排（实时预览）、拖到别的栅栏、拖到桌面空白 = 归「未分类」
- **边缘自动隐藏（可选，默认关）**：栅栏贴屏幕边缘自动收成细边，鼠标悬停滑出
- **全局热键**：全部折叠 / 全部展开 / 呼出搜索
- **搜索（v1.1）**：呼出即搜全部桌面图标，方向键导航 + Enter 启动

## 10. 商业级配套

- **持久化**：配置 `%APPDATA%`（JSON + 版本迁移），学习数据 SQLite
- **安装器**：NSIS（MSIX 对壳层集成限制太多，弃用）。传统安装 + 可选开机自启
- **授权**：v1 离线签名校验 license key，预留在线激活接口
- **崩溃上报**：panic/异常处理器 → 生 minidump → 用户确认后上传或本地留存
- **自动更新**：启动异步查 manifest → 下载 → 校验签名 → 重启替换
- **日志**：tracing + rolling file，release 默认 info，文件路径默认不上传

## 11. 测试策略

- **核心引擎**：纯 Rust → 单元测试 + property test（布局 / 磁吸 / 规则引擎 / 学习权重）
- **壳层**：集成测试标 `#[ignore]`，CI 的 Windows runner 手动跑（真实桌面环境）
- **WE 共存**：验收清单专列「安装 Wallpaper Engine 后全流程回归」
- **性能基准**：常驻 <30MB、空闲 CPU≈0、动画 60fps、连续跑 7 天无内存增长

## 12. 里程碑（垂直切片路径）

| 阶段 | 内容 | 估时 |
|---|---|---|
| M0 | crate 骨架 + CI + 事件总线 + tracing | 1周 |
| M1 | 接管图标层 + overlay + 枚举图标 + 画出第一个栅栏 + 图标显示 | 2-3周 |
| M2 | 拖拽重排/拖入/右键菜单/折叠动画/磁吸 + WE 共存验证 | 2-3周 |
| M3 | 规则引擎 + 行为学习 + 新增图标归位 | 2周 |
| M4 | 亚克力/动画打磨 + 托盘 + egui 设置界面 | 2周 |
| M5 | 安装器/授权/自动更新/崩溃上报 | 2周 |

## 13. 风险与开放问题

1. **壳层接管是最大风险**：Windows 各版本桌面结构差异、Explorer/Wallpaper Engine 重启重建，自愈逻辑必须稳健
2. **右键菜单转发**（`IContextMenu`）在自绘视图下完整复刻桌面行为，工作量不可低估
3. **行为学习**的权重与隐私边界需要在 v1 划定清楚（默认不上传任何路径）
4. **授权/激活**服务端形态未定，v1 仅做离线校验 + 预留接口
5. **OpenAI 之类云端分类**明确排除，作为隐私卖点
