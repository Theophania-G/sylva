# Publish / update Sylva on winget: create a branch in the user's fork of
# microsoft/winget-pkgs via the GitHub API (no full clone needed), write the
# manifest files, and open a PR.
# Usage: powershell -ExecutionPolicy Bypass -File scripts\winget-submit.ps1
param(
    [string]$Version = '0.1.0',
    [string]$Branch = '',
    [string]$Fork = 'Theophania-G/winget-pkgs'
)
$ErrorActionPreference = 'Stop'
if (-not $Branch) { $Branch = "Sylva.Sylva-$Version" }
$root = Split-Path -Parent $PSScriptRoot
$dir = Join-Path $root "packaging\winget\manifests\s\Sylva\Sylva\$Version"
if (-not (Test-Path $dir)) { throw "manifest dir not found: $dir" }

Write-Host "==> upstream main ref"
$base = gh api repos/microsoft/winget-pkgs --jq .default_branch
if ($LASTEXITCODE -ne 0 -or -not $base) { throw 'failed to read upstream default branch' }
Write-Host "base branch: $base"
$up = gh api "repos/microsoft/winget-pkgs/git/ref/heads/$base" --jq .object.sha
if ($LASTEXITCODE -ne 0 -or -not $up) { throw 'failed to read upstream main sha' }
Write-Host "main sha: $up"

Write-Host "==> create branch $Branch on $Fork"
$ref = "refs/heads/$Branch"
gh api "repos/$Fork/git/refs" -f ref=$ref -f sha=$up --jq .ref 2>$null | Out-Null
if ($LASTEXITCODE -ne 0) {
    $existing = gh api "repos/$Fork/git/ref/$ref" --jq .ref 2>$null
    if ($LASTEXITCODE -ne 0 -or -not $existing) { throw 'failed to create or find branch' }
    Write-Host "branch exists, reusing"
}

Write-Host "==> upload manifest files"
$files = Get-ChildItem $dir -Filter '*.yaml' | Sort-Object Name
foreach ($f in $files) {
    $text = [System.IO.File]::ReadAllText($f.FullName, [System.Text.Encoding]::UTF8)
    $content = [Convert]::ToBase64String([System.Text.Encoding]::UTF8.GetBytes($text))
    $path = "manifests/s/Sylva/Sylva/$Version/$($f.Name)"
    $out = gh api "repos/$Fork/contents/$path" -f message="Add Sylva v$Version" -f branch=$Branch -f content=$content -X PUT --jq '.content.name' 2>&1
    if ($LASTEXITCODE -ne 0) { throw "failed to upload $($f.Name): $out" }
    Write-Host "uploaded: $out"
}

Write-Host "==> open PR"
$shaLine = (Get-Content (Join-Path $dir 'Sylva.Sylva.installer.yaml') -Encoding UTF8 | Where-Object { $_ -match '^\s*InstallerSha256:' } | Select-Object -First 1)
$sha = ($shaLine -split ':', 2)[1].Trim()
$body = @"
New version: Sylva.Sylva version $Version

Portable zip (x64) hosted on the official release page:
https://github.com/Theophania-G/sylva/releases/download/v$Version/Sylva-v$Version-win64.zip

SHA-256: $sha
"@
$pr = gh pr create --repo microsoft/winget-pkgs --head "$Fork`:$Branch" --base $base `
    --title "New version: Sylva.Sylva version $Version" --body $body 2>&1
if ($LASTEXITCODE -ne 0) { throw "PR creation failed: $pr" }
Write-Host $pr
