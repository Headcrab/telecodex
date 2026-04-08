Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$ScriptRoot = Split-Path -Parent $MyInvocation.MyCommand.Path

& (Join-Path $ScriptRoot "Stop-Telecodex.ps1")
Start-Sleep -Seconds 1
& (Join-Path $ScriptRoot "Start-Telecodex.ps1")
Start-Sleep -Seconds 2
& (Join-Path $ScriptRoot "Status-Telecodex.ps1")
