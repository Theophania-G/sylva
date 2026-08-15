# Sylva — Windows 桌面栅栏整理器

顺手的桌面图标整理工具。Rust 编写（Win10/11）。

- **栅栏**：桌面图标按组收纳，网格 / 列表两种布局，透明玻璃卡片，支持框选多选、拖拽移动 / 缩放、磁吸对齐、就地重命名。
- **控制中心**：关闭后完全隐藏；打开后是单页「栅栏管理」：
  - 每栅栏的布局（网格/列表）、图标大小、背景风格（玻璃/描边/纯色）、色调色板一键调整，实时生效并持久化；
  - 「＋ 添加栅栏」新建空白栅栏；「移出」删除选中栅栏（成员退回桌面）；
  - 标题栏「切换桌面」按钮：一键在栅栏接管与原始桌面之间切换；
  - 入口：`Ctrl+Alt+T`、托盘图标双击、托盘右键「显示 Sylva 控制中心」。
- **D2D 内联文本编辑**：重命名栅栏/图标直接在合成表面内绘制与编辑，。
- **动效**：栅栏拖动 / 缩放丝滑跟随（过冲回弹）、面板开合淡入淡出、图标悬停放大 + 柔光；空闲 0% CPU。
- **壳层接管**：低风险接管桌面 WorkerW / DefView，不碰 Wallpaper Engine；任何退出路径都自动恢复桌面图标。
- **性能**：DirectComposition + Direct2D / DirectWrite 合成，空闲 0% CPU，动画全部走 16ms 定时器逐帧补间。

## 目录结构

```
.
├── Cargo.toml               # workspace 根（依赖与成员声明）
├── assets/                  # 顶层共享资源
│   ├── sylva.ico            #   应用图标（build.rs 嵌入 exe 资源）
│   └── sylva.jpg            #   效果截图
├── docs/design/             # 设计文档
├── scripts/                 # 开发 / 构建脚本
├── crates/
│   ├── core/                # 领域模型与纯逻辑（布局、磁吸、序列化，无 UI 依赖）
│   ├── shell/               # 壳层：桌面接管、COM、图标枚举、库同步
│   ├── render/              # 渲染：overlay 窗口、D2D/DComp 合成、命中模型
│   └── app/                 # 应用组装：主循环、事件路由、主题、资源
└── .github/workflows/       # CI（build + test + clippy + fmt）
```

依赖方向自下而上：`core ← shell ← render ← app`，各层只依赖下层，边界清晰。

## 构建与运行

需要 Windows + Rust 工具链（MSVC）。

```bash
# 开发构建（生成 target\debug\sylva.exe）
cargo build

# 发布构建（生成 target\release\sylva.exe，双击运行无控制台窗口）
cargo build --release

# 运行
target\debug\sylva.exe        # 开发构建
target\release\sylva.exe      # 发布构建

# 测试 / 静态检查
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt --all -- --check
```

也可用脚本：`scripts\build-release.ps1`（发布构建并把 exe 拷到 `dist\`）。

## 快捷键

| 按键 | 作用 |
| --- | --- |
| `Ctrl+Alt+T` | 打开 / 关闭控制中心（关闭后完全隐藏） |
| `Ctrl+Shift+F10` | 全局退出（恢复桌面图标） |
| 控制中心「切换桌面」 | 栅栏 ⇄ 原始桌面一键切换 |
| 托盘图标右键 | 显示 Sylva 控制中心 / 退出 Sylva |
| 托盘图标双击 | 打开 / 关闭控制中心 |
| 双击栅栏内图标 | 打开对应项（多选时全部打开） |
| `Ctrl` + 单击图标 | 不连续多选 |
| 空白处拖拽 | 框选多选 |
| 标题栏拖拽 / 边缘缩放 | 移动 / 调整栅栏、控制台 |

## 许可

MIT。
