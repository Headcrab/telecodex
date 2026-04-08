Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$RepoRoot = Split-Path -Parent $PSScriptRoot
$PidPath = Join-Path $RepoRoot "run\telecodex.pid"
$CurrentPid = $PID
$TrackedProcess = $null

$Targets = @(Get-CimInstance Win32_Process | Where-Object {
    $_.CommandLine -and $_.ProcessId -ne $CurrentPid -and (
        ($_.Name -ieq "telecodex.exe" -and $_.CommandLine -like "*$RepoRoot*target\\release\\telecodex.exe*") -or
        ($_.Name -ieq "powershell.exe" -and $_.CommandLine -like "*$RepoRoot*Run-Telecodex.ps1*")
    )
})

if (Test-Path -LiteralPath $PidPath) {
    $PidValue = (Get-Content -LiteralPath $PidPath -Raw).Trim()
    if ($PidValue) {
        $TrackedProcess = Get-Process -Id ([int]$PidValue) -ErrorAction SilentlyContinue
        if ($TrackedProcess) {
            Stop-Process -Id $TrackedProcess.Id -Force -ErrorAction SilentlyContinue
        }
    }
}

foreach ($Target in $Targets) {
    Stop-Process -Id $Target.ProcessId -Force -ErrorAction SilentlyContinue
}

Remove-Item -LiteralPath $PidPath -Force -ErrorAction SilentlyContinue

if ($Targets.Count -eq 0 -and -not $TrackedProcess) {
    Write-Output "Telecodex is not running"
    exit 0
}

Write-Output "Telecodex stopped"
