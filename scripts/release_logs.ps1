# Inspect a workflow run: failing jobs/steps plus the tail of the logs.
# ASCII only: PowerShell 5.1 reads a .ps1 without BOM as ANSI.
param(
    [string]$RunId = '42406304819'
)

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
Set-Location (Split-Path $PSScriptRoot -Parent)
$env:GIT_TERMINAL_PROMPT = '0'

$inputText = "protocol=https`nhost=github.com`n`n"
$cred = ($inputText | git credential fill 2>$null) | Out-String
$token = ([regex]::Match($cred, '(?m)^password=(.+)$')).Groups[1].Value.Trim()
if (-not $token) { throw 'Cannot obtain a token from git credential fill' }

$headers = @{
    Authorization = "token $token"
    Accept        = 'application/vnd.github+json'
    'User-Agent'  = 'filepumper'
}

$jobs = Invoke-RestMethod -Uri "https://api.github.com/repos/Xvicd/FilePumper/actions/runs/$RunId/jobs" -Headers $headers
$failedJobs = @()
foreach ($job in $jobs.jobs) {
    Write-Output ("JOB: {0} -> {1}" -f $job.name, $job.conclusion)
    foreach ($step in $job.steps) {
        if ($step.conclusion -and $step.conclusion -ne 'success' -and $step.conclusion -ne 'skipped') {
            Write-Output ("     step {0} {1} [{2}]" -f $step.number, $step.name, $step.conclusion)
            $failedJobs += [pscustomobject]@{ Job = $job.name; Step = $step.number; StepName = $step.name }
        }
    }
}

$zip = Join-Path $env:TEMP ("run-" + $RunId + ".zip")
$dir = Join-Path $env:TEMP ("run-" + $RunId)
if (Test-Path -LiteralPath $dir) { Remove-Item -LiteralPath $dir -Recurse -Force }
Invoke-WebRequest -Uri "https://api.github.com/repos/Xvicd/FilePumper/actions/runs/$RunId/logs" -Headers $headers -OutFile $zip
Expand-Archive -LiteralPath $zip -DestinationPath $dir -Force

foreach ($failed in $failedJobs) {
    $pattern = "*" + $failed.Step + "_" + $failed.StepName + ".txt"
    $files = Get-ChildItem -LiteralPath $dir -Recurse -File -Filter $pattern
    foreach ($file in $files) {
        Write-Output ("--- {0} ---" -f $file.FullName.Substring($dir.Length + 1))
        Get-Content -LiteralPath $file.FullName -Tail 45 | ForEach-Object { Write-Output ("    " + ($_ -replace '^\d{4}-\d\d-\d\dT[\d:.]+Z ', '')) }
    }
}

