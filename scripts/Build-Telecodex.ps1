Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$RepoRoot = Split-Path -Parent $PSScriptRoot
$MsvcRoot = Join-Path $RepoRoot ".msvc-kit\VC\Tools\MSVC\14.44.35207"
$SdkRoot = "C:\Program Files (x86)\Windows Kits\10"
$SdkVersion = "10.0.26100.0"
$RustBin = "C:\Users\admin\.rustup\toolchains\1.85.0-x86_64-pc-windows-msvc\bin"

if (-not (Test-Path -LiteralPath $MsvcRoot)) {
    throw "MSVC toolchain not found: $MsvcRoot"
}

$env:VCToolsInstallDir = $MsvcRoot
$env:VCINSTALLDIR = Join-Path $RepoRoot ".msvc-kit\VC\"
$env:WindowsSdkDir = "$SdkRoot\"
$env:WindowsSDKVersion = "$SdkVersion\"
$env:WindowsSdkBinPath = "$SdkRoot\bin\$SdkVersion\"
$env:INCLUDE = "$MsvcRoot\include;$SdkRoot\Include\$SdkVersion\ucrt;$SdkRoot\Include\$SdkVersion\shared;$SdkRoot\Include\$SdkVersion\um;$SdkRoot\Include\$SdkVersion\winrt;$SdkRoot\Include\$SdkVersion\cppwinrt"
$env:LIB = "$MsvcRoot\lib\x64;$SdkRoot\Lib\$SdkVersion\ucrt\x64;$SdkRoot\Lib\$SdkVersion\um\x64"
$env:PATH = "$RustBin;$MsvcRoot\bin\Hostx64\x64;$SdkRoot\bin\$SdkVersion\x64;$env:PATH"
$env:RUSTC = "$RustBin\rustc.exe"
$env:CARGO = "$RustBin\cargo.exe"
$env:RUSTDOC = "$RustBin\rustdoc.exe"

Set-Location -LiteralPath $RepoRoot
& "$RustBin\cargo.exe" build --release
