# Wait for the newest run of a workflow and print its status.
# Usage: powershell -NoProfile -ExecutionPolicy Bypass -File scripts\wait_release.ps1 [-Workflow Release] [-MaxSeconds 240]
# ASCII only on purpose: PowerShell 5.1 reads a .ps1 without BOM as ANSI.
param(
    [string]$Workflow = 'Release',
    [int]$MaxSeconds = 240
)

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
Set-Location (Split-Path $PSScriptRoot -Parent)
$env:GIT_TERMINAL_PROMPT = '0'

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

function Get-LatestRun {
    $runs = Invoke-RestMethod -Uri "https://api.github.com/repos/$repo/actions/runs?per_page=20" -Headers $headers
    $runs.workflow_runs | Where-Object { $_.name -eq $Workflow } | Select-Object -First 1
}

$deadline = (Get-Date).AddSeconds($MaxSeconds)
$run = $null
do {
    $run = Get-LatestRun
    if (-not $run) { Write-Output "no run of '$Workflow' yet"; break }
    Write-Output ("{0:HH:mm:ss} run {1} ({2}) status={3} conclusion={4}" -f (Get-Date), $run.id, $run.head_branch, $run.status, $run.conclusion)
    if ($run.status -eq 'completed') { break }
    Start-Sleep -Seconds 25
} while ((Get-Date) -lt $deadline)

if ($run) { Write-Output ("html_url: {0}" -f $run.html_url) }
