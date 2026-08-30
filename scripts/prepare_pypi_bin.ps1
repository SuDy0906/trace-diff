# Copy the release Rust binary into the Python package before `maturin build`.
$ErrorActionPreference = "Stop"
$Root = Split-Path -Parent $PSScriptRoot
$Dest = Join-Path $Root "python\trace_diff\_bin"
$Target = $env:CARGO_BUILD_TARGET
$Base = Join-Path $Root "target"
if ($Target) {
    $Base = Join-Path $Base $Target
}
$Exe = Join-Path $Base "release\trace-diff.exe"
if (-not (Test-Path $Exe)) {
    $Exe = Join-Path $Base "release\trace-diff"
}
if (-not (Test-Path $Exe)) {
    Write-Error "Release binary not found under $Base\release"
}
New-Item -ItemType Directory -Force -Path $Dest | Out-Null
Copy-Item -Force $Exe (Join-Path $Dest (Split-Path -Leaf $Exe))
Write-Host "Bundled $(Split-Path -Leaf $Exe) -> $Dest"
