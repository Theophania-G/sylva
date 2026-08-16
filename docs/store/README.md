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

**简短说明**：桌面栅栏整理器，在桌面上自由创建栅栏，把图标整理得井井有条。

**详细说明**：
Sylva 是一款低占用、高审美的 Windows 桌面栅栏整理器：
- 在桌面任意位置自由创建栅栏，网格 / 列表两种布局
- 栅栏自动磁吸、互不重叠，一键切换回原始桌面
- 控制中心统一管理栅栏布局、背景风格与色调
- 托盘常驻，双击快速开关控制中心

### English

**Name**: Sylva

**Short description**: A lightweight desktop fence organizer for Windows.

**Long description**:
Sylva is a low-footprint desktop fence organizer for Windows:
- Create fences anywhere on the desktop, in grid or list layout
- Fences snap and never overlap; one-click switch back to the original desktop
- A control center manages fence layout, background style and tint
- Lives in the tray; double-click to toggle the control center
