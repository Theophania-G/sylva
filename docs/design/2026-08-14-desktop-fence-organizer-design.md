# 桌面栅栏整理器（Desktop Fence Organizer）设计文档

日期：2026-08-14
状态：已确认（2026-08-29 按实现现状修订：egui → D2D 内联编辑、UI 收敛为 overlay+控制中心、安装器/MSIX 已落地、模糊走 WinRT Composition）

## 1. 背景与目标

做一个 Windows 桌面图标整理栅栏工具，对标 Stardock Fences，但在**功能和使用体验上全面超越**。目标形态为商业级产品。

**核心主张**：Fences 是「给你一堆栅栏壳子」；本产品是「把栅栏本身做到极致」——完全自由的拖放组织、所见即所得，配合亚克力质感与 60fps 丝滑动画，常驻内存 <30MB、后台零常驻计算，且与 Wallpaper Engine 无缝共存。

## 2. 差异化定位（vs Fences）

用户确认的两个差异化方向：
1. **现代交互质感**——丝滑动画、磁吸对齐、圆角亚克力、全局热键，整体质感碾压 Fences 的老式 Win32 UI
2. **极致轻量性能**——常驻 <30MB、空闲 CPU≈0、动画 60fps、长期运行无内存泄漏、后台零常驻分类/AI 任务

明确不做（v1）：
- **自动分类 / 行为学习 / 本地 AI**（用户决定取消，换取最高自由度与最轻后台）
- 多场景/多配置切换
- 云同步
- 文件夹绑定栅栏（folder-port，可能作为 v1.1 增强）
- 在线激活服务端（仅预留接口）
- 跨平台 / 移动端
- 自动更新 / 崩溃上报（v1.1 起再评估）

## 3. 平台与技术栈

- **平台**：仅 Windows（Win10 / Win11）
- **语言**：Rust
- **关键依赖**：`windows-rs`、Direct2D、DirectWrite、DirectComposition、Windows.UI.Composition（实时模糊）、crossbeam-channel（事件总线）
- **不引入**：tokio、WebView2/Tauri（常驻桌面悬浮层用 WebView 太重）

## 4. 架构总览

单进程、单实例（mutex 防重复启动）。三个窗口形态：
- **覆盖层窗口**（每显示器一个）：无边框 layered，`SetParent` 到桌面 worker 层（WorkerW），层级在壁纸之上、其他窗口之下，DirectComposition 驱动视觉树
- **控制中心**：单页「栅栏管理」面板，直接绘制在覆盖层合成表面内（无独立设置窗口）
- **托盘图标**：`Shell_NotifyIcon`

**线程模型**：主线程跑 Win32 消息循环；后台线程处理枚举/文件/网络；层间用 crossbeam-channel 事件总线通信。

### 模块划分（Cargo workspace）

```
crates/
├── core/      纯 Rust 业务内核：模型·布局·磁吸·动画状态机·配置
├── shell/     windows-rs 适配层：接管·图标枚举·图标提取·右键菜单·拖拽·Shell事件·多屏
├── render/    Direct2D + DirectWrite + DirectComposition：栅栏绘制·亚克力·图标·文字
└── app/       组合根 + 主循环 + 事件总线接线 + 生命周期

另含独立工作区 `scripts/installer/`（Rust 图形化安装器，内嵌主程序）。
```

**核心引擎（`core/`）零 Win32 依赖，可单元测试。**

## 5. 数据模型

```rust
struct Desk {
    fences: Vec<Fence>,
    free_icons: Vec<ItemId>,     // 未分组图标区（不属于任何栅栏）
    icons: HashMap<ItemId, Icon>,
    settings: AppSettings,
    version: u32,
}

struct Fence {
    id: FenceId,
    title: Option<String>,       // 用户可编辑，可留空，无分类语义
    bounds: Rect,                // 逻辑坐标，每 monitor 独立
    state: FenceState,           // Expanded | Folded
    icon_ids: Vec<ItemId>,       // 显式成员列表（有序），拖拽即增删
    appearance: FenceAppearance, // 颜色/亚克力/圆角
}

struct Icon {
    shell_ref: ShellItemRef,     // 引用 shell 项（PIDL/路径指纹）
    display_name: String,
    kind: ItemKind,              // App | Folder | Doc | Drive | Link
    grid_pos: Vec2,              // 栅栏内网格坐标
}
```

**关键设计**：栅栏绑定**显式图标成员列表**，没有分类、没有规则、没有学习。图标归哪个栅栏完全由用户拖拽决定——最高自由度、后台零常驻计算。不属于任何栅栏的图标显示在「未分组图标区」（桌面顶部，默认存在）。

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
- **实时模糊（已落地）**：Win11 用 Windows.UI.Composition `BackdropBrush` 真·实时模糊（无截图、无 CPU 高斯），Win10 降级半透明纯色

## 8. 栅栏与图标归属模型

- **显式成员归属**：图标是否属于某个栅栏 = 是否在它的 `icon_ids` 里。拖进/拖出即增删成员，所见即所得，无任何自动归位
- **未分组图标区**：不属于任何栅栏的图标显示在桌面顶部的「未分组图标区」（默认存在、可折叠），行为与普通栅栏一致；用户可随时把图标拖进任意栅栏
- **标题自定**：栅栏标题用户自拟、可留空，不承担任何分类语义
- **新图标去向**：桌面新增图标一律进入未分组图标区，由用户决定归属
- **明确不做**：不做任何自动分类、行为学习、规则引擎、本地 AI。后台无任何常驻分类任务，不写 SQLite 学习库，不上传任何数据

## 9. 交互 & 动画

- **折叠/展开**：双击栅栏标题 → 折叠成一条标题栏（DComp 高度动画）；双击桌面空白 → 全部折叠/全部展开（可配置）
- **磁吸对齐**：拖动栅栏靠近屏幕边缘/其他栅栏时吸附；栅栏移动时图标整体跟随
- **拖入**：资源管理器/浏览器文件拖进栅栏 → `IDropTarget` 接收 → 自动分类 + 加入
- **图标拖拽**：栅栏内网格重排（实时预览）、拖到别的栅栏、拖到未分组图标区 = 解除归属
- **边缘自动隐藏（可选，默认关）**：栅栏贴屏幕边缘自动收成细边，鼠标悬停滑出
- **全局热键**：全部折叠 / 全部展开 / 呼出搜索
- **搜索（v1.1）**：呼出即搜全部桌面图标，方向键导航 + Enter 启动

## 10. 商业级配套

- **持久化**：配置 `%APPDATA%`（JSON + 版本迁移）。无学习数据、不需要 SQLite，天然更轻
- **安装器（已落地）**：Rust 图形化单文件安装器（`scripts/installer/`），传统安装 + 可选开机自启；另有 MSIX 打包工具链用于 Microsoft Store 上架
- **授权**：v1 离线签名校验 license key，预留在线激活接口
- **崩溃上报**：panic/异常处理器 → 生 minidump → 用户确认后上传或本地留存
- **自动更新**：启动异步查 manifest → 下载 → 校验签名 → 重启替换
- **日志**：tracing + rolling file，release 默认 info，文件路径默认不上传

## 11. 测试策略

- **核心引擎**：纯 Rust → 单元测试 + property test（布局 / 磁吸 / 网格重排）
- **壳层**：集成测试标 `#[ignore]`，CI 的 Windows runner 手动跑（真实桌面环境）
- **WE 共存**：验收清单专列「安装 Wallpaper Engine 后全流程回归」
- **性能基准**：常驻 <30MB、空闲 CPU≈0、动画 60fps、连续跑 7 天无内存增长

## 12. 里程碑（垂直切片路径）

| 阶段 | 内容 | 估时 |
|---|---|---|
| M0 | crate 骨架 + CI + 事件总线 + tracing | 1周 |
| M1 | 接管图标层 + overlay + 枚举图标 + 画出第一个栅栏 + 图标显示 | 2-3周 |
| M2 | 拖拽重排/拖入/右键菜单/折叠动画/磁吸 + WE 共存验证 | 2-3周 |
| M3 | 未分组图标区 + 栅栏标题编辑 + 跨栅栏/到未分组区拖拽完善 | 1-2周 |
| M4 | 亚克力/动画打磨 + 托盘 + 控制中心（D2D 内联编辑替代 egui） | 2周 |
| M5 | 安装器/授权/自动更新/崩溃上报（安装器已落地；授权/更新/上报推迟） | 2周 |

## 13. 风险与开放问题

1. **壳层接管是最大风险**：Windows 各版本桌面结构差异、Explorer/Wallpaper Engine 重启重建，自愈逻辑必须稳健
2. **右键菜单转发**（`IContextMenu`）在自绘视图下完整复刻桌面行为，工作量不可低估
3. **自由模型的可用性风险**：取消自动分类后，「图标放哪」全靠手动——未分组图标区 + 拖拽体验必须足够顺，否则用户会觉得桌面更乱。验证点：M3 后自测高频拖拽场景
4. **授权/激活**服务端形态未定，v1 仅做离线校验 + 预留接口
5. **WE 共存**与 Explorer 重建自愈需持续回归（含换壁纸、重启 WE、重启资源管理器）
