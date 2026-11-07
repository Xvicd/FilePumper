# One-off: after CI re-uploaded the assets, refresh the SHA-256 lines in the release body.
# ASCII only: PowerShell 5.1 reads a .ps1 without BOM as ANSI.
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
Set-Location (Split-Path $PSScriptRoot -Parent)
$env:GIT_TERMINAL_PROMPT = '0'

$repo = 'Xvicd/FilePumper'
$tag = 'v0.2.0'
$inputText = "protocol=https`nhost=github.com`n`n"
$cred = ($inputText | git credential fill 2>$null) | Out-String
$token = ([regex]::Match($cred, '(?m)^password=(.+)$')).Groups[1].Value.Trim()
if (-not $token) { throw 'Cannot obtain a token from git credential fill' }

$headers = @{
    Authorization = "token $token"
    Accept        = 'application/vnd.github+json'
    'User-Agent'  = 'filepumper'
}

$release = Invoke-RestMethod -Uri "https://api.github.com/repos/$repo/releases/tags/$tag" -Headers $headers
$body = $release.body

foreach ($asset in $release.assets) {
    $tmp = Join-Path $env:TEMP $asset.name
    $dlHeaders = @{ Authorization = "token $token"; Accept = 'application/octet-stream'; 'User-Agent' = 'filepumper' }
    Invoke-WebRequest -Uri $asset.url -Headers $dlHeaders -OutFile $tmp
    $sha = (Get-FileHash $tmp -Algorithm SHA256).Hash.ToLower()
    Remove-Item -LiteralPath $tmp -Force
    Write-Output ("{0} | {1} bytes | {2}" -f $asset.name, $asset.size, $sha)

    $escaped = [regex]::Escape($asset.name)
    $pattern = "`[0-9a-f`]{64}(`s+$escaped)"
    if ($body -match $pattern) {
        $body = [regex]::Replace($body, $pattern, ($sha + '$1'))
    } else {
        Write-Output ("  hash line for {0} not found in the body" -f $asset.name)
    }
}

$payload = @{ body = $body } | ConvertTo-Json -Depth 3
$bytes = [System.Text.Encoding]::UTF8.GetBytes($payload)
$patched = Invoke-RestMethod -Method Patch -Uri "https://api.github.com/repos/$repo/releases/$($release.id)" -Headers $headers -Body $bytes -ContentType 'application/json; charset=utf-8'
Write-Output '--- body after patch ---'
Write-Output $patched.body
