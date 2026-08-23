# Sylva 商店上架资料

本目录汇总 Sylva 上架 Microsoft Store 所需的全部产物与清单。

## 产物

| 内容 | 位置 |
| --- | --- |
| MSIX 安装包（未签名，供商店提交） | `dist/Sylva-0.1.0-x64.msix` |
| 图标/磁贴/闪屏素材 | `packaging/msix/assets/` |
| 商店截图（1536×960，仅本地，不入公开仓库） | `docs/store/screenshots/` |
| winget 清单 | `packaging/winget/manifests/` |

> 截图拍自真实桌面，包含个人内容，出于隐私考虑已从公开仓库排除
> （`.gitignore` 忽略 `docs/store/screenshots/`）。上传商店前请先自行
> 检查截图内容，确认没有敏感信息。

## 打包命令

```powershell
# 1) 从 assets/sylva.ico 重新生成商店素材（改了图标才需要）
powershell -ExecutionPolicy Bypass -File scripts\make-store-assets.ps1

# 2) 打包 MSIX（默认未签名，商店提交用）
powershell -ExecutionPolicy Bypass -File scripts\package-msix.ps1

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
5. 上传 `dist\Sylva-0.1.0-x64.msix`，填写描述、类别，上传截图与图标
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
