# 发布构建：编译 sylva.exe（无控制台窗口）并把成品拷到 dist\。
# 用法：powershell -ExecutionPolicy Bypass -File scripts\build-release.ps1
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

Write-Host '==> cargo build --release'
cargo build --release
if ($LASTEXITCODE -ne 0) { throw '构建失败' }

$exe = Join-Path $root 'target\release\sylva.exe'
if (-not (Test-Path $exe)) { throw "未找到 $exe" }

$dist = Join-Path $root 'dist'
New-Item -ItemType Directory -Force -Path $dist | Out-Null
Copy-Item $exe (Join-Path $dist 'sylva.exe') -Force
Write-Host "==> 完成：dist\sylva.exe"
