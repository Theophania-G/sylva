# Package sylva.exe as MSIX. Unsigned by default (for Microsoft Store).
# Use -Sign for local sideload testing.
# Usage:
#   powershell -ExecutionPolicy Bypass -File scripts\package-msix.ps1
#   powershell -ExecutionPolicy Bypass -File scripts\package-msix.ps1 -Publisher "CN=YourPublisher" -Sign -CertPath cert.pfx -CertPassword xxx
param(
    [string]$Publisher = 'CN=Sylva',
    [string]$PublisherDisplayName = 'Sylva',
    [string]$PackageName = 'Sylva.DesktopFences',
    [string]$DisplayName = 'Sylva',
    [string]$Version = '0.1.0',
    [string]$OutName = 'Sylva-0.1.0-x64',
    [switch]$Sign,
    [string]$CertPath = '',
    [string]$CertPassword = ''
)
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot

# ---- 1) Locate Windows SDK tools ----
$kitBin = Get-ChildItem 'C:\Program Files (x86)\Windows Kits\10\bin' -Recurse -Filter 'makeappx.exe' -ErrorAction SilentlyContinue |
    Where-Object { $_.FullName -match '\\x64\\' } | Sort-Object FullName -Descending | Select-Object -First 1
if (-not $kitBin) { throw 'Windows SDK makeappx.exe not found; install Windows 10/11 SDK.' }
$signtool = Join-Path (Split-Path $kitBin.FullName) 'signtool.exe'
if (-not (Test-Path $signtool)) { throw 'signtool.exe not found' }

# ---- 2) Prepare package directory ----
$build = Join-Path $root 'packaging\msix\build'
$assets = Join-Path $root 'packaging\msix\assets'
if (Test-Path $build) { Remove-Item -LiteralPath $build -Recurse -Force }
New-Item -ItemType Directory -Force -Path (Join-Path $build 'assets') | Out-Null
$exe = Join-Path $root 'dist\Sylva\sylva.exe'
if (-not (Test-Path $exe)) { $exe = Join-Path $root 'dist\sylva.exe' }
if (-not (Test-Path $exe)) { throw 'sylva.exe not found under dist' }
Copy-Item $exe (Join-Path $build 'sylva.exe') -Force
Copy-Item (Join-Path $assets '*') (Join-Path $build 'assets') -Force

# ---- 3) Render manifest (replace placeholders) ----
$bg = (Get-Content (Join-Path $assets 'background.txt') -Raw).Trim()
$manifest = Get-Content (Join-Path $root 'packaging\msix\AppxManifest.xml') -Raw -Encoding UTF8
$manifest = $manifest.Replace('{{PACKAGE_NAME}}', $PackageName).Replace('{{PUBLISHER}}', $Publisher).Replace('{{PUBLISHER_DISPLAY_NAME}}', $PublisherDisplayName).Replace('{{DISPLAY_NAME}}', $DisplayName).Replace('{{VERSION}}', $Version).Replace('{{BG_COLOR}}', $bg)
[System.IO.File]::WriteAllText((Join-Path $build 'AppxManifest.xml'), $manifest, (New-Object System.Text.UTF8Encoding($true)))

# ---- 4) MakeAppx pack ----
$outDir = Join-Path $root 'dist'
New-Item -ItemType Directory -Force -Path $outDir | Out-Null
$msix = Join-Path $outDir "$OutName.msix"
& $kitBin.FullName pack /d $build /p $msix /o
if ($LASTEXITCODE -ne 0) { throw 'MakeAppx pack failed' }

if ($Sign) {
    if (-not $CertPath -or -not (Test-Path $CertPath)) { throw '-Sign requires -CertPath pointing to a .pfx' }
    $signed = Join-Path $outDir "$OutName-signed.msix"
    Copy-Item $msix $signed -Force
    if ($CertPassword) {
        & $signtool sign /fd SHA256 /f $CertPath /p $CertPassword /a /tr http://timestamp.digicert.com /td SHA256 $signed
    } else {
        & $signtool sign /fd SHA256 /f $CertPath /a /tr http://timestamp.digicert.com /td SHA256 $signed
    }
    if ($LASTEXITCODE -ne 0) { throw 'sign failed' }
    Write-Host "==> signed: $signed"
}

Write-Host "==> MSIX: $msix"
