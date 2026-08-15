# Sylva 安装脚本（由安装器解压后调用，无需管理员权限，按当前用户安装）
$ErrorActionPreference = 'Stop'
$src = Split-Path -Parent $MyInvocation.MyCommand.Path
$dest = Join-Path $env:LOCALAPPDATA 'Programs\Sylva'
New-Item -ItemType Directory -Force -Path $dest | Out-Null
Copy-Item -LiteralPath (Join-Path $src 'sylva.exe') -Destination (Join-Path $dest 'sylva.exe') -Force

$ws = New-Object -ComObject WScript.Shell
# 桌面快捷方式
$desktop = [Environment]::GetFolderPath('Desktop')
$lnk = $ws.CreateShortcut((Join-Path $desktop 'Sylva.lnk'))
$lnk.TargetPath = Join-Path $dest 'sylva.exe'
$lnk.WorkingDirectory = $dest
$lnk.Description = 'Sylva 桌面栅栏整理器'
$lnk.Save()
# 开始菜单快捷方式
$menu = [Environment]::GetFolderPath('Programs')
New-Item -ItemType Directory -Force -Path (Join-Path $menu 'Sylva') | Out-Null
$lnk2 = $ws.CreateShortcut((Join-Path (Join-Path $menu 'Sylva') 'Sylva.lnk'))
$lnk2.TargetPath = Join-Path $dest 'sylva.exe'
$lnk2.WorkingDirectory = $dest
$lnk2.Description = 'Sylva 桌面栅栏整理器'
$lnk2.Save()

# 开机自启动（HKCU，当前用户）
$run = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run'
New-Item -Path $run -Force | Out-Null
Set-ItemProperty -Path $run -Name 'Sylva' -Value ('"' + (Join-Path $dest 'sylva.exe') + '"')

# 卸载信息（控制面板可卸载）
$unreg = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\Sylva'
New-Item -Path $unreg -Force | Out-Null
Set-ItemProperty -Path $unreg -Name 'DisplayName' -Value 'Sylva 桌面栅栏整理器'
Set-ItemProperty -Path $unreg -Name 'DisplayVersion' -Value '0.1.0'
Set-ItemProperty -Path $unreg -Name 'Publisher' -Value 'Sylva'
Set-ItemProperty -Path $unreg -Name 'InstallLocation' -Value $dest
Set-ItemProperty -Path $unreg -Name 'DisplayIcon' -Value (Join-Path $dest 'sylva.exe')
Set-ItemProperty -Path $unreg -Name 'UninstallString' -Value ('"' + (Join-Path $dest 'uninstall.cmd') + '"')
Set-ItemProperty -Path $unreg -Name 'NoModify' -Value 1
Set-ItemProperty -Path $unreg -Name 'NoRepair' -Value 1

# 卸载脚本
$un = "@echo off`r`n" +
"setlocal`r`n" +
"set DEST=%LOCALAPPDATA%\Programs\Sylva`r`n" +
"taskkill /IM sylva.exe /F >nul 2>&1`r`n" +
"reg delete `"HKCU\Software\Microsoft\Windows\CurrentVersion\Run`" /v Sylva /f >nul 2>&1`r`n" +
"reg delete `"HKCU\Software\Microsoft\Windows\CurrentVersion\Uninstall\Sylva`" /f >nul 2>&1`r`n" +
"del `"%USERPROFILE%\Desktop\Sylva.lnk`" >nul 2>&1`r`n" +
"rd /s /q `"%APPDATA%\Microsoft\Windows\Start Menu\Programs\Sylva`" >nul 2>&1`r`n" +
"rd /s /q `"%DEST%`" >nul 2>&1`r`n" +
"echo Sylva 已卸载。`r`n" +
"pause`r`n"
Set-Content -Path (Join-Path $dest 'uninstall.cmd') -Value $un -Encoding Default

# 启动
Start-Process -FilePath (Join-Path $dest 'sylva.exe')
Write-Host 'Sylva 安装完成，已启动。'
Start-Sleep -Seconds 1
