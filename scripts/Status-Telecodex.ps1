Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$RepoRoot = Split-Path -Parent $PSScriptRoot
$PidPath = Join-Path $RepoRoot "run\telecodex.pid"
$StdoutPath = Join-Path $RepoRoot "logs\telecodex.stdout.log"
$StderrPath = Join-Path $RepoRoot "logs\telecodex.stderr.log"
$RunnerProcess = @(Get-CimInstance Win32_Process | Where-Object {
    $_.CommandLine -and $_.Name -ieq "powershell.exe" -and $_.CommandLine -like "*$RepoRoot*Run-Telecodex.ps1*"
} | Select-Object -First 1)

if (-not (Test-Path -LiteralPath $PidPath)) {
    if (-not $RunnerProcess) {
        Write-Output "Status: stopped"
        exit 0
    }
}

$PidValue = if (Test-Path -LiteralPath $PidPath) { (Get-Content -LiteralPath $PidPath -Raw).Trim() } else { "" }
$Process = $null
if ($PidValue) {
    $Process = Get-Process -Id ([int]$PidValue) -ErrorAction SilentlyContinue
}

if (-not $Process) {
    if ($RunnerProcess) {
        $Process = Get-Process -Id ([int]$RunnerProcess.ProcessId) -ErrorAction SilentlyContinue
    }
}

if (-not $Process) {
    Write-Output "Status: stale pid file"
    exit 0
}

Write-Output "Status: running"
Write-Output "PID: $($Process.Id)"
Write-Output "Started: $($Process.StartTime.ToString('s'))"
Write-Output "Stdout: $StdoutPath"
Write-Output "Stderr: $StderrPath"
