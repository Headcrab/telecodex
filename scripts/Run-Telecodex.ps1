Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$RepoRoot = Split-Path -Parent $PSScriptRoot
$ExePath = Join-Path $RepoRoot "target\release\telecodex.exe"
$EnvPath = Join-Path $RepoRoot ".env"
$ConfigPath = Join-Path $RepoRoot "telecodex.toml"
$TmpPath = Join-Path $RepoRoot "tmp"
$MsvcRoot = Join-Path $RepoRoot ".msvc-kit\VC\Tools\MSVC\14.44.35207"
$SdkRoot = "C:\Program Files (x86)\Windows Kits\10"
$SdkVersion = "10.0.26100.0"

if (-not (Test-Path -LiteralPath $ExePath)) {
    throw "telecodex.exe not found: $ExePath"
}
if (-not (Test-Path -LiteralPath $ConfigPath)) {
    throw "telecodex config not found: $ConfigPath"
}
if (-not (Test-Path -LiteralPath $EnvPath)) {
    throw ".env not found: $EnvPath"
}

New-Item -ItemType Directory -Force -Path $TmpPath | Out-Null

Get-Content -LiteralPath $EnvPath | ForEach-Object {
    if ($_ -match '^\s*$' -or $_ -match '^\s*#') {
        return
    }
    $parts = $_ -split '=', 2
    if ($parts.Count -eq 2) {
        [System.Environment]::SetEnvironmentVariable($parts[0].Trim(), $parts[1].Trim(), "Process")
    }
}

$RustBin = "C:\Users\admin\.rustup\toolchains\1.85.0-x86_64-pc-windows-msvc\bin"
[System.Environment]::SetEnvironmentVariable("VCToolsInstallDir", $MsvcRoot, "Process")
[System.Environment]::SetEnvironmentVariable("VCINSTALLDIR", (Join-Path $RepoRoot ".msvc-kit\VC\") , "Process")
[System.Environment]::SetEnvironmentVariable("WindowsSdkDir", "$SdkRoot\", "Process")
[System.Environment]::SetEnvironmentVariable("WindowsSDKVersion", "$SdkVersion\", "Process")
[System.Environment]::SetEnvironmentVariable("WindowsSdkBinPath", "$SdkRoot\bin\$SdkVersion\", "Process")
[System.Environment]::SetEnvironmentVariable(
    "INCLUDE",
    "$MsvcRoot\include;$SdkRoot\Include\$SdkVersion\ucrt;$SdkRoot\Include\$SdkVersion\shared;$SdkRoot\Include\$SdkVersion\um;$SdkRoot\Include\$SdkVersion\winrt;$SdkRoot\Include\$SdkVersion\cppwinrt",
    "Process"
)
[System.Environment]::SetEnvironmentVariable(
    "LIB",
    "$MsvcRoot\lib\x64;$SdkRoot\Lib\$SdkVersion\ucrt\x64;$SdkRoot\Lib\$SdkVersion\um\x64",
    "Process"
)
$env:PATH = "$RustBin;$MsvcRoot\bin\Hostx64\x64;$SdkRoot\bin\$SdkVersion\x64;$env:PATH"
[System.Environment]::SetEnvironmentVariable("RUSTC", "$RustBin\rustc.exe", "Process")
[System.Environment]::SetEnvironmentVariable("CARGO", "$RustBin\cargo.exe", "Process")
[System.Environment]::SetEnvironmentVariable("RUSTDOC", "$RustBin\rustdoc.exe", "Process")

Set-Location -LiteralPath $RepoRoot
& $ExePath $ConfigPath
