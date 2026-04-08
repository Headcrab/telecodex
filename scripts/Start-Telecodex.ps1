Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$RepoRoot = Split-Path -Parent $PSScriptRoot
$RunDir = Join-Path $RepoRoot "run"
$LogDir = Join-Path $RepoRoot "logs"
$PidPath = Join-Path $RunDir "telecodex.pid"
$StdoutPath = Join-Path $LogDir "telecodex.stdout.log"
$StderrPath = Join-Path $LogDir "telecodex.stderr.log"
$Runner = Join-Path $PSScriptRoot "Run-Telecodex.ps1"

New-Item -ItemType Directory -Force -Path $RunDir | Out-Null
New-Item -ItemType Directory -Force -Path $LogDir | Out-Null

if (-not (Test-Path -LiteralPath (Join-Path $RepoRoot "target\release\telecodex.exe"))) {
    & (Join-Path $PSScriptRoot "Build-Telecodex.ps1")
}

if (Test-Path -LiteralPath $PidPath) {
    $ExistingPid = (Get-Content -LiteralPath $PidPath -Raw).Trim()
    if ($ExistingPid) {
        $Existing = Get-Process -Id ([int]$ExistingPid) -ErrorAction SilentlyContinue
        if ($Existing) {
            Write-Output "Telecodex already running with PID $ExistingPid"
            exit 0
        }
    }
    Remove-Item -LiteralPath $PidPath -Force
}

$StaleTargets = @(Get-CimInstance Win32_Process | Where-Object {
    $_.CommandLine -and (
        ($_.Name -ieq "telecodex.exe" -and $_.CommandLine -like "*$RepoRoot*target\\release\\telecodex.exe*") -or
        ($_.Name -ieq "powershell.exe" -and $_.CommandLine -like "*$RepoRoot*Run-Telecodex.ps1*")
    )
})

foreach ($Target in $StaleTargets) {
    Stop-Process -Id $Target.ProcessId -Force -ErrorAction SilentlyContinue
}

if ($StaleTargets.Count -gt 0) {
    Start-Sleep -Seconds 1
}

$Process = Start-Process -FilePath "powershell.exe" `
    -ArgumentList @("-NoProfile", "-ExecutionPolicy", "Bypass", "-File", $Runner) `
    -WorkingDirectory $RepoRoot `
    -RedirectStandardOutput $StdoutPath `
    -RedirectStandardError $StderrPath `
    -PassThru

Set-Content -LiteralPath $PidPath -Value $Process.Id -NoNewline
Write-Output "Telecodex started with PID $($Process.Id)"
