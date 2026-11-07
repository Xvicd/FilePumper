# Upload release assets built locally in dist/ (e.g. when CI is not used).
# Usage: powershell -NoProfile -ExecutionPolicy Bypass -File scripts\release_upload.ps1 [-Tag v0.2.0]
# ASCII only on purpose: PowerShell 5.1 reads a .ps1 without BOM as ANSI.
param(
    [string]$Tag = 'v0.2.0'
)

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
Set-Location (Split-Path $PSScriptRoot -Parent)
$env:GIT_TERMINAL_PROMPT = '0'

$version = $Tag -replace '^v', ''
$repo = 'Xvicd/FilePumper'

$inputText = "protocol=https`nhost=github.com`n`n"
$cred = ($inputText | git credential fill 2>$null) | Out-String
$token = ([regex]::Match($cred, '(?m)^password=(.+)$')).Groups[1].Value.Trim()
if (-not $token) { throw 'Cannot obtain a token from git credential fill' }

$headers = @{
    Authorization = "token $token"
    Accept        = 'application/vnd.github+json'
    'User-Agent'  = 'filepumper'
}

$release = Invoke-RestMethod -Uri "https://api.github.com/repos/$repo/releases/tags/$Tag" -Headers $headers
Write-Output ("release: {0} (draft={1}) | assets now: {2}" -f $release.name, $release.draft, $release.assets.Count)

$wanted = @(
    "dist\FilePumper-$version-windows-x64.zip",
    "dist\FilePumper-$version-linux-x64.tar.gz"
)

foreach ($file in $wanted) {
    $leaf = Split-Path $file -Leaf
    $existing = $release.assets | Where-Object { $_.name -eq $leaf -and $_.state -eq 'uploaded' }
    if ($existing) {
        Write-Output ("already uploaded: {0} ({1} bytes)" -f $existing.name, $existing.size)
        continue
    }
    if (-not (Test-Path -LiteralPath $file)) { Write-Output ("missing locally: {0}" -f $file); continue }

    $name = [uri]::EscapeDataString($leaf)
    $uri = "https://uploads.github.com/repos/$repo/releases/$($release.id)/assets?name=$name"
    $asset = Invoke-RestMethod -Method Post -Uri $uri -Headers $headers -InFile $file -ContentType 'application/octet-stream'
    Write-Output ("uploaded: {0} ({1} bytes)" -f $asset.name, $asset.size)
}

$final = Invoke-RestMethod -Uri "https://api.github.com/repos/$repo/releases/tags/$Tag" -Headers $headers
Write-Output ("total assets: {0}" -f $final.assets.Count)
foreach ($asset in $final.assets) {
    Write-Output ("  {0} - {1:N2} MB - {2}" -f $asset.name, ($asset.size / 1MB), $asset.browser_download_url)
}
