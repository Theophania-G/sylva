# Local MSIX sideload test: sign with a throwaway dev cert, install,
# launch, then remove package and cert.
# Usage: powershell -ExecutionPolicy Bypass -File scripts\msix-sideload-test.ps1
param(
    [string]$Publisher = 'CN=01501FE3-FC6B-4246-A4F2-33974DB842B5'
)
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot

$msix = Join-Path $root 'dist\Sylva-0.1.0-x64.msix'
$signed = Join-Path $root 'dist\Sylva-0.1.0-x64-signed.msix'
if (-not (Test-Path $msix)) { throw 'msix not found; run scripts\package-msix.ps1 first' }
$asciiTmp = Join-Path $env:TEMP 'sylva-msix-test'
New-Item -ItemType Directory -Force -Path $asciiTmp | Out-Null
$signed = Join-Path $asciiTmp 'Sylva-signed.msix'
Copy-Item $msix $signed -Force

$kitBin = Get-ChildItem 'C:\Program Files (x86)\Windows Kits\10\bin' -Recurse -Filter 'signtool.exe' -ErrorAction SilentlyContinue |
    Where-Object { $_.FullName -match '\\x64\\' } | Sort-Object FullName -Descending | Select-Object -First 1
if (-not $kitBin) { throw 'signtool not found' }

# 1) throwaway dev cert
$cert = New-SelfSignedCertificate -Type CodeSigningCert -Subject $Publisher `
    -CertStoreLocation Cert:\CurrentUser\My -KeyExportPolicy Exportable -KeySpec Signature `
    -NotAfter (Get-Date).AddYears(3)

$installed = $false
try {
    # 2) sign using the store certificate by thumbprint
    $signOut = & $kitBin.FullName sign /debug /fd SHA256 /sha1 $cert.Thumbprint $signed 2>&1 | Out-String
    Write-Host $signOut
    if ($LASTEXITCODE -ne 0) { throw 'sign failed' }

    # 3) trust the cert for current user (Root + TrustedPeople)
    $store = New-Object System.Security.Cryptography.X509Certificates.X509Store('Root', 'CurrentUser')
    $store.Open('ReadWrite')
    $store.Add($cert)
    $store.Close()
    $tp = New-Object System.Security.Cryptography.X509Certificates.X509Store('TrustedPeople', 'CurrentUser')
    $tp.Open('ReadWrite')
    $tp.Add($cert)
    $tp.Close()

    # 4) install
    Add-AppxPackage -Path $signed
    $installed = $true
    Copy-Item $signed (Join-Path $root 'dist\Sylva-0.1.0-x64-signed.msix') -Force
    $app = Get-AppxPackage -Name Sylva.DesktopFences
    if (-not $app) { throw 'package not installed' }
    Write-Host "installed: $($app.PackageFullName)"

    # 5) launch
    $exe = Join-Path $app.InstallLocation 'sylva.exe'
    $p = Start-Process $exe -PassThru
    Start-Sleep -Seconds 5
    if ($p.HasExited) { throw 'app exited early during launch test' }
    Write-Host "launch ok, pid=$($p.Id)"
    Stop-Process -Id $p.Id -Force -ErrorAction SilentlyContinue
} finally {
    # 6) cleanup: remove package and cert
    if ($installed) {
        $app = Get-AppxPackage -Name Sylva.DesktopFences -ErrorAction SilentlyContinue
        if ($app) { Remove-AppxPackage -Package $app.PackageFullName }
    }
    $rootStore = New-Object System.Security.Cryptography.X509Certificates.X509Store('Root', 'CurrentUser')
    $rootStore.Open('ReadWrite')
    $stale = $rootStore.Certificates | Where-Object { $_.Thumbprint -eq $cert.Thumbprint }
    foreach ($c in $stale) { $rootStore.Remove($c) }
    $rootStore.Close()
    $tp = New-Object System.Security.Cryptography.X509Certificates.X509Store('TrustedPeople', 'CurrentUser')
    $tp.Open('ReadWrite')
    $tpStale = $tp.Certificates | Where-Object { $_.Thumbprint -eq $cert.Thumbprint }
    foreach ($c in $tpStale) { $tp.Remove($c) }
    $tp.Close()
    Remove-Item (Join-Path Cert:\CurrentUser\My ($cert.Thumbprint)) -Force -ErrorAction SilentlyContinue
}
Write-Host 'sideload test passed'
