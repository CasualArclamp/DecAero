# made by CasualArclamp
# Builds the portable decaero-gui.exe (static C runtime, nothing to install)
# into exe\, which git ignores:
#
#   powershell -ExecutionPolicy Bypass -File scripts\build-portable.ps1
#
# It uses its own target folder so it does not disturb a build in progress,
# and checks the result the way the release workflow does.
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

# libopus (call recordings) is built with CMake.
if (-not (Get-Command cmake -ErrorAction SilentlyContinue)) {
    foreach ($dir in @(
            'C:\Program Files\CMake\bin',
            'C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\Common7\IDE\CommonExtensions\Microsoft\CMake\CMake\bin')) {
        if (Test-Path (Join-Path $dir 'cmake.exe')) { $env:PATH = "$env:PATH;$dir"; break }
    }
}

$env:RUSTFLAGS = '-C target-feature=+crt-static'
$env:CARGO_TARGET_DIR = Join-Path $root 'target\portable'
cargo build --release --target x86_64-pc-windows-msvc -p decaero-gui
if ($LASTEXITCODE -ne 0) { throw 'build failed' }

$built = Join-Path $env:CARGO_TARGET_DIR 'x86_64-pc-windows-msvc\release\decaero-gui.exe'
# A static build must not ask for the C runtime DLLs.
$text = [System.Text.Encoding]::ASCII.GetString([System.IO.File]::ReadAllBytes($built))
if ($text -match '(?i)vcruntime140|api-ms-win-crt') { throw 'decaero-gui.exe imports a C runtime DLL: not a static build' }

New-Item -ItemType Directory -Force (Join-Path $root 'exe') | Out-Null
Copy-Item $built (Join-Path $root 'exe\decaero-gui.exe') -Force
$item = Get-Item (Join-Path $root 'exe\decaero-gui.exe')
"{0}  {1:N1} MB  {2}" -f $item.FullName, ($item.Length / 1MB), (Get-FileHash $item.FullName -Algorithm SHA256).Hash
