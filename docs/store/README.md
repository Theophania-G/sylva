# Sylva 商店上架资料

本目录汇总 Sylva 上架 Microsoft Store 所需的全部产物与清单。

## 产物

| 内容 | 位置 |
| --- | --- |
| MSIX 安装包（未签名，供商店提交） | `dist/Sylva-0.1.3-x64.msix` |
| 图标/磁贴/闪屏素材 | `packaging/msix/assets/` |
| 商店截图（1536×960，仅本地，不入公开仓库） | `docs/store/screenshots/` |
| winget 清单 | `packaging/winget/manifests/` |

> 截图拍自真实桌面，包含个人内容，出于隐私考虑已从公开仓库排除
> （`.gitignore` 忽略 `docs/store/screenshots/`）。上传商店前请先自行
> 检查截图内容，确认没有敏感信息。

## 产品身份（Partner Center 已确认）

| 清单元素 | 值 |
| --- | --- |
| Package/Identity/Name | `Theophania.Sylva` |
| Package/Identity/Publisher | `CN=01501FE3-FC6B-4246-A4F2-33974DB842B5` |
| Package/Properties/PublisherDisplayName | `Theophania` |
| PFN | `Theophania.Sylva_q2tpaj1gh3f94` |
| Store ID / 商店链接 | `9P70TQ6406QQ` / https://apps.microsoft.com/detail/9P70TQ6406QQ |

## 打包命令

```powershell
# 1) 从 assets/sylva.ico 重新生成商店素材（改了图标才需要）
powershell -ExecutionPolicy Bypass -File scripts\make-store-assets.ps1

# 2) 打包 MSIX（默认未签名，商店提交用）
#    注意：-DisplayName 必须用 Partner Center 保留的显示名称（Sylva栅栏），
#    不能用「Sylva」等未保留名，否则上传报"使用了你未保留的显示名称"。
#    每次重新上传需 -Version 递增（相同 Identity+Version 但内容不同会被拒）。
powershell -ExecutionPolicy Bypass -File scripts\package-msix.ps1 `
  -Publisher "CN=01501FE3-FC6B-4246-A4F2-33974DB842B5" `
  -PublisherDisplayName "Theophania" `
  -PackageName "Theophania.Sylva" `
  -DisplayName "Sylva栅栏" `
  -Version "0.1.3" -OutName "Sylva-0.1.3-x64"

# 3) 本地侧载验证（自动装/卸，需要正常用户会话）
powershell -ExecutionPolicy Bypass -File scripts\msix-sideload-test.ps1
```

## 提交 Microsoft Store（需要你自己的账号操作）

注册免费（2025 年 9 月起个人开发者免 19 美元注册费，需证件+自拍验证）：

1. 打开 https://partner.microsoft.com 用个人微软账号注册开发者
2. 完成身份验证后，进入"产品 → 新建产品 → Windows"
3. 保留一个应用名称（例如 Sylva），会得到一个 Publisher 身份
4. **重新打包**，把 Publisher 换成你账号的发布者，PublisherDisplayName 换成你的发布者显示名：
   ```powershell
   powershell -ExecutionPolicy Bypass -File scripts\package-msix.ps1 `
     -Publisher "CN=你的发布者ID" -PublisherDisplayName "你的显示名"
   ```
   `Publisher`（发布者 ID，形如 CN=xxxxxxxx-xxxx-...）和 `Identity Name`
   （包标识名称）在 Partner Center → 产品 → Sylva → 产品标识 里查看
5. 上传 `dist\Sylva-0.1.3-x64.msix`，填写描述、类别，上传截图与图标
   - 上传后如果有“受限功能”区块：勾选 `runFullTrust` 并填理由
     （Win32 桌面应用需完全信任访问桌面壳层），保存提交
6. 提交审核（首次 24–72 小时）

## 提交 winget

```powershell
powershell -ExecutionPolicy Bypass -File scripts\winget-submit.ps1
```

脚本会自动：读取 winget-pkgs 最新分支 → 在 `Theophania-G/winget-pkgs`
fork 上建分支 → 上传清单 → 打开 PR。

当前 PR：https://github.com/microsoft/winget-pkgs/pull/418256

## 商店文案

### 中文

**名称**：Sylva 桌面栅栏整理器

**简短说明**：轻量、美观的 Windows 桌面整理工具，用栅栏把桌面图标整理得井井有条。

**详细说明**：
Sylva 栅栏 是一款轻量、美观的 Windows 桌面整理工具，帮助你把桌面图标按自己的习惯归类，让杂乱无章的桌面变得井井有条。

主要功能：
- 自由创建栅栏：在桌面任意位置创建多个栅栏区域，把常用软件、文件夹和文档分门别类放进不同的栅栏里，一目了然。
- 两种布局：每个栅栏支持网格和列表两种展示方式，图标大小、间距、圆角、背景风格都可以单独调节，满足不同使用习惯。
- 智能防重叠：拖动栅栏时自动磁吸对齐，栅栏之间互不重叠，桌面布局始终保持整齐。
- 控制中心：集中管理所有栅栏——新增、移出、重命名，以及调整布局、背景风格和色调，所有设置一目了然。
- 一键切换桌面：随时在“栅栏整理桌面”和“原始桌面”之间一键切换，互不影响。
- 低占用、高颜值：GPU 加速渲染，系统资源占用极低；支持玻璃、透明、纯色等多种背景风格，视觉简洁美观。

适用场景：适合希望高效整理 Windows 桌面图标、让桌面保持整洁美观的所有用户。安装后可从开始菜单或桌面快捷方式启动，应用常驻系统托盘，随时唤出。

### English

**Name**: Sylva Fences

**Short description**: A lightweight, good-looking Windows desktop organizer that groups desktop icons into fences.

**Long description**:
Sylva Fences is a lightweight, good-looking Windows desktop organizer that helps you group desktop icons the way you like, keeping your desktop tidy and easy to find things.

Key features:
- Create fences anywhere on the desktop and organize apps, folders and documents into separate groups.
- Grid or list layout per fence, with adjustable icon size, spacing, corner radius and background style.
- Fences snap and never overlap while dragging, so your desktop always stays neat.
- A control center manages all fences: add, remove, rename, and adjust layout, background and tint.
- One-click switch between the fenced desktop and the original desktop.
- GPU-accelerated rendering with very low resource usage; glass, outline and filled background styles.

Suitable for anyone who wants a clean, well-organized Windows desktop. Launch from the Start menu or desktop shortcut; the app lives in the system tray for quick access.

## 属性页（Properties）填写

适用于 Partner Center → 产品 → 提交 →「属性」页。

| 字段 | 填写 | 说明 |
| --- | --- | --- |
| 类别和子类别 | **个性化**（Personalization），子类别无 | 桌面整理/外观工具；备选「实用工具 + 工具」，勿选「文件管理器」 |
| 隐私策略 | **否，我的产品不使用任何个人信息** | 清单仅 runFullTrust、无任何网络/PII 采集能力（见 docs/PRIVACY.md），不提供 URL |
| 支持部门联系信息 | 239883253@qq.com | 公开显示 |
| 电话号码 | 19918264924 | 公开显示，介意可留空 |
| 网站 | https://github.com/Theophania-G/sylva | 建议填 |
| 地址行 1/2、邮编、市/县、省、国家/地区 | 留空 | 可选、公开显示，不建议填私人住址 |
| 显示模式（Mixed Reality 边界设置） | 不勾选 | 2D 桌面应用，MR 不适用 |
| 产品声明 | 全部不勾选 | 广播/录制仅限游戏；其余均不适用 |
| 系统要求·触摸屏 / 键盘 | 不勾选 | 非必需 |
| 系统要求·鼠标 | **最低硬件：勾选** | 桌面整理必需 |
| 系统要求·相机/NFC/蓝牙LE/电话/麦克风/Xbox/MR | 全部不勾选 | 未使用 |
| 系统要求·内存 | 最低未指定 / 推荐 1 GB | 占用极低，别设最低限制 |
| 系统要求·DirectX | 未指定 / 未指定 | Win10/11 自带 DX11+ |
| 系统要求·视频内存 | 未指定 | — |
| 系统要求·处理器 | 最低：1 GHz 或更高的兼容处理器 | 标准值 |
| 系统要求·图形 | 最低：兼容 DirectX 11 的显卡 | GPU 加速；无独显时 Composition 走 WARP 软渲染 |

> 产品系统门槛（Windows 10 1809+、x64）由 MSIX 的 MinVersion 决定，不在本页填写。

## Store 一览（listing）填写

中文页与英文页都填。素材：

- 截图（3 张）：`docs/store/screenshots/desktop-fences.png`（主图）、`desktop-console.png`、`desktop-plain.png`
- 9:16 招贴画 720×1080：`docs/store/listing-poster-9x16.png`
- 1:1 酷图 1080×1080：`docs/store/listing-hero-1x1.png`
- 1:1 应用磁贴图标 300/150/71：`docs/store/listing-tile-300.png`、`listing-tile-150.png`、`listing-tile-71.png`

### 中文(中国)

| 字段 | 内容 |
| --- | --- |
| 产品名称 | Sylva栅栏（保留名，必填） |
| 说明 | 见下方「详细说明」 |
| 此版本的新增功能 | 留空（首次提交） |
| 产品功能 | 自由创建多个栅栏区域 / 网格·列表两种布局 / 图标大小·间距·圆角可调 / 玻璃·透明·纯色背景风格 / 拖动自动磁吸、互不重叠 / 控制中心统一管理 / 一键切换整理桌面·原始桌面 / GPU 加速，占用极低 / 常驻托盘，随时唤出 |
| 屏幕截图 | 上传上面 3 张，fences 放第一张 |
| 9:16 招贴画 | listing-poster-9x16.png |
| 1:1 酷图 | listing-hero-1x1.png |
| 短标题 | Sylva |
| 简短描述 | 轻量、美观的 Windows 桌面整理工具，用栅栏把桌面图标整理得井井有条。 |
| 关键字 | 桌面整理 / 桌面栅栏 / 桌面图标 / 图标整理 / 桌面工具 / 效率工具 / 桌面美化 |

**详细说明**：Sylva 栅栏 是一款轻量、美观的 Windows 桌面整理工具，帮助你把桌面图标按自己的习惯归类，让杂乱无章的桌面变得井井有条。

主要功能：
- 自由创建栅栏：在桌面任意位置创建多个栅栏区域，把常用软件、文件夹和文档分门别类放进不同的栅栏里，一目了然。
- 两种布局：每个栅栏支持网格和列表两种展示方式，图标大小、间距、圆角、背景风格都可以单独调节，满足不同使用习惯。
- 智能防重叠：拖动栅栏时自动磁吸对齐，栅栏之间互不重叠，桌面布局始终保持整齐。
- 控制中心：集中管理所有栅栏——新增、移出、重命名，以及调整布局、背景风格和色调，所有设置一目了然。
- 一键切换桌面：随时在“栅栏整理桌面”和“原始桌面”之间一键切换，互不影响。
- 低占用、高颜值：GPU 加速渲染，系统资源占用极低；支持玻璃、透明、纯色等多种背景风格，视觉简洁美观。

适用场景：适合希望高效整理 Windows 桌面图标、让桌面保持整洁美观的所有用户。安装后可从开始菜单或桌面快捷方式启动，应用常驻系统托盘，随时唤出。

### English (US)

| 字段 | 内容 |
| --- | --- |
| Product name | Sylva Fences（需先保留该名称） |
| Short description | A lightweight, good-looking Windows desktop organizer that groups desktop icons into fences. |
| Keywords | desktop organizer / desktop fences / desktop icons / icon organizer / desktop tool / productivity |

**Long description**：Sylva Fences is a lightweight, good-looking Windows desktop organizer that helps you group desktop icons the way you like, keeping your desktop tidy and easy to find things.

Key features:
- Create fences anywhere on the desktop and organize apps, folders and documents into separate groups.
- Grid or list layout per fence, with adjustable icon size, spacing, corner radius and background style.
- Fences snap and never overlap while dragging, so your desktop always stays neat.
- A control center manages all fences: add, remove, rename, and adjust layout, background and tint.
- One-click switch between the fenced desktop and the original desktop.
- GPU-accelerated rendering with very low resource usage; glass, outline and filled background styles.

Suitable for anyone who wants a clean, well-organized Windows desktop. Launch from the Start menu or desktop shortcut; the app lives in the system tray for quick access.
