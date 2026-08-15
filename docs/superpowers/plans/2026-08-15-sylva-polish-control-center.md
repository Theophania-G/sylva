# Sylva 视觉打磨 + 控制中心 实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 修复待办/重命名输入框与动画短板，给栅栏加中粗圆角描边，并把控制台升级为带标签页的控制中心（栅栏管理、插件、桌面一键切换）。

**Architecture:** 沿用 core/shell/render/app 四层架构。核心改动集中在 `crates/app/src/main.rs`（事件路由、动画状态、模型变更）、`crates/render/src/draw.rs`（控制台/待办/栅栏绘制）、`crates/render/src/scene.rs`（场景结构）、`crates/render/src/overlay.rs`（输入框视觉、命中区域）、`crates/core/src/model.rs`（默认外观、插件与桌面模式持久化）。

**Tech Stack:** Rust + windows-rs 0.62（Win32/D2D/DComp/DWrite/DWM），serde 持久化。

## Global Constraints

- 空闲保持 0% CPU：所有动画走现有 16ms `AnimTick`，无动画时停表。
- 配置向后兼容：新增持久化字段一律 `#[serde(default)]`。
- 任何退出路径都恢复桌面真实图标（`IconGuard` 兜底不变）。
- 中文 IME 输入必须可用：文本输入继续用系统 EDIT 控件承载。
- 所有尺寸按 DIP 定义、× `theme.scale` 变物理像素；颜色直通 alpha。

---

### Task 1: 编辑框视觉重构（重命名框黑边 + 待办输入）

**Files:**
- Modify: `crates/render/src/overlay.rs`（`edit_brush` 颜色、`WM_CTLCOLOREDIT`、输入框圆角/边框辅助）
- Modify: `crates/app/src/main.rs:1149-1268`（重命名编辑框创建：去边框、区域圆角、内边距）
- Modify: `crates/app/src/main.rs:1896-1965`（待办输入框创建：同款视觉）

- [ ] 把 `edit_brush` 颜色改为面板填充色 RGB(16,22,34)，与 D2D 输入行底色一致。
- [ ] 重命名/待办编辑框：保持去除 `WS_EX_CLIENTEDGE`，创建后 `SetWindowRgn(CreateRoundRectRgn)` 做 8px 圆角（Win10 也圆角），Win11 同步设 DWM 圆角并把 `DWMWA_BORDER_COLOR` 设为面板底色（消除系统黑边）。
- [ ] 输入行内边距：编辑框比命中矩形内缩 1.5px，D2D 在周围画聚焦描边；失焦时画极淡描边。
- [ ] 焦点链路：点击输入行时 `SetForegroundWindow` 失败兜底用 `AttachThreadInput` + `BringWindowToTop` + `SetFocus`；编辑框收到 `WM_MOUSEACTIVATE` 时返回 `MA_ACTIVATE`。
- [ ] 构建验证：`cargo build` 通过。

### Task 2: 栅栏中粗圆角矩形描边

**Files:**
- Modify: `crates/core/src/model.rs:149-215`（`FenceAppearance::default`：`border_width = 1.75`）
- Modify: `crates/render/src/theme.rs`（`fence_border` alpha 提到 0.35 左右）
- Modify: `crates/app/src/main.rs`（`layout_fence` 里 `border_width × scale`、`border_color` 使用亮色；确认所有风格都描边）

- [ ] 默认 `border_width` 1.0 → 1.75 DIP；`layout_fence` 输出物理像素宽并始终描边。
- [ ] 描边颜色从 `[1,1,1,0.10]` 提到 `[1,1,1,0.34]`（描边风格时再亮一档）。
- [ ] `cargo test --workspace` 通过（含 `fence_style_fields_are_plumbed` 等既有测试）。

### Task 3: 待办插件重设计

**Files:**
- Modify: `crates/render/src/draw.rs:153-420`（`draw_console` 待办部分）
- Modify: `crates/app/src/main.rs`（输入行占位提示绘制数据；空状态）
- Modify: `crates/render/src/scene.rs`（`SceneConsole`/`SceneTodo` 增加 hover、占位等字段）

- [ ] 行 hover 背景（圆角 6px、白 6%）、勾选框改圆形 + 打勾旋转/缩放动画、删除按钮 hover 变红。
- [ ] 输入行绘制占位文字（“添加待办事项…”“备注（可选）”），聚焦时隐藏。
- [ ] 空状态：无待办时显示“暂无待办，输入后回车添加”。
- [ ] 滚动条细圆角条（复用栅栏滚动条风格）。

### Task 4: 丝滑动效

**Files:**
- Modify: `crates/core/src/animation.rs`（新增 `Ease::BackOut`、`Ease::Spring` 及测试）
- Modify: `crates/app/src/main.rs`（栅栏移动/缩放目标补间、面板展开过冲、hover 缩放状态）
- Modify: `crates/render/src/draw.rs`（图标/按钮 hover 缩放与光晕：多层圆角矩形降 alpha）

- [ ] core 增加 `ease_out_back`（过冲）与阻尼弹簧求值函数 + 单测。
- [ ] `FenceMove`/`FenceResize` 改为记录目标矩形，`AnimTick` 用 0.18s `BackOut` 补间推进，结束后持久化。
- [ ] 面板展开/折叠用 `BackOut(0.14s)`；内容区错峰淡入（stagger 24ms）。
- [ ] 图标/按钮 hover：目标缩放 1.06，光晕多层圆角矩形。
- [ ] 空闲停表逻辑保持不变；`cargo test` 通过。

### Task 5: 控制中心（标签页 + 栅栏管理 + 插件 + 桌面切换）

**Files:**
- Modify: `crates/core/src/model.rs`（`desk.plugins`、`desk.desktop_mode`、插件清单模型；均 `#[serde(default)]`）
- Modify: `crates/render/src/scene.rs`（`SceneConsole` 增加 tabs/fence 管理行/插件行/动作按钮几何）
- Modify: `crates/render/src/overlay.rs`（`ConsoleZone` 新枚举 + 命中）
- Modify: `crates/render/src/draw.rs`（标签页、栅栏管理页、插件页绘制）
- Modify: `crates/app/src/main.rs`（事件路由、桌面切换、插件目录扫描、便签插件）
- Modify: `crates/shell/src/takeover.rs`（暴露可重复 `hide_icons`/`restore_icons` 供切换）

- [ ] 模型：`PluginEntry { id, name, kind: PluginKind(Todo|Notes), enabled }`，`desk.plugins` 默认含待办；`desk.desktop_mode: bool`。
- [ ] 控制台加标签栏：待办事项 / 栅栏管理 / 插件；顶部加“切换桌面”与关闭按钮。
- [ ] 栅栏管理页：每栅栏一行（名称 + 布局切换 + 图标大小 + 风格 + 色调色板），动作复用右键菜单的变更逻辑并持久化。
- [ ] 插件页：内置插件启用/禁用；“打开插件目录”创建并打开 `data/plugins`；扫描 `plugin.json` 清单列出外部配置插件。
- [ ] 桌面切换：`desktop_mode` 切换时调用 `hierarchy.hide_icons()/restore_icons()`，栅栏 0.22s 淡出/淡入（`SceneFence.alpha`），控制台保持可见。
- [ ] 新增内置“便签”插件：多行文本 + 保存。

### Task 6: 验证与收尾

- [ ] `cargo fmt --all`、`cargo clippy --workspace -- -D warnings`、`cargo test --workspace` 全绿。
- [ ] 发布构建 `cargo build --release` 成功。
- [ ] 运行时诊断：启动应用，截图核对输入框无黑边、栅栏描边可见、控制中心可用；修复发现的问题。
- [ ] 更新 README（控制中心、快捷键、插件说明）。
- [ ] 提交最终状态。
