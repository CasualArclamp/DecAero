# made by CasualArclamp
# Builds the Android app (apps\decaero-android) into exe\DecAero-<version>.apk,
# which git ignores:
#
#   powershell -ExecutionPolicy Bypass -File scripts\build-android.ps1
#   powershell -ExecutionPolicy Bypass -File scripts\build-android.ps1 -Emulator
#
# -Emulator adds the x86_64 library, for the emulator on a PC; without it the
# APK holds only arm64, which is every phone that can run it.
#
# Needs, besides Rust: a JDK 17, the Android SDK (platform 35, build-tools,
# NDK 27) and Gradle 8.9, found through JAVA_HOME, ANDROID_HOME and PATH, or
# under C:\Android (jdk, sdk, gradle-8.9); the Rust targets
# (rustup target add aarch64-linux-android x86_64-linux-android); cargo-ndk
# (cargo install cargo-ndk); and CMake with Ninja for libopus.
param([switch]$Emulator)
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

if (-not $env:JAVA_HOME -and (Test-Path 'C:\Android\jdk')) { $env:JAVA_HOME = 'C:\Android\jdk' }
if (-not $env:ANDROID_HOME -and (Test-Path 'C:\Android\sdk')) { $env:ANDROID_HOME = 'C:\Android\sdk' }
if (-not $env:ANDROID_HOME) { throw 'ANDROID_HOME is not set and C:\Android\sdk does not exist' }
if (-not $env:ANDROID_NDK_HOME) {
    $ndk = Get-ChildItem (Join-Path $env:ANDROID_HOME 'ndk') -Directory | Sort-Object Name | Select-Object -Last 1
    if (-not $ndk) { throw 'no NDK under ANDROID_HOME\ndk' }
    $env:ANDROID_NDK_HOME = $ndk.FullName
}

# libopus (call recordings) is built with CMake, and for Android that needs
# Ninja: Visual Studio's generator cannot cross-compile.
foreach ($tool in @(
        @('cmake', @('C:\Program Files\CMake\bin',
                'C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\Common7\IDE\CommonExtensions\Microsoft\CMake\CMake\bin')),
        @('ninja', @('C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\Common7\IDE\CommonExtensions\Microsoft\CMake\Ninja',
                'C:\Program Files\Microsoft Visual Studio\2022\Community\Common7\IDE\CommonExtensions\Microsoft\CMake\Ninja')),
        @('gradle', @('C:\Android\gradle-8.9\bin')))) {
    if (-not (Get-Command $tool[0] -ErrorAction SilentlyContinue)) {
        foreach ($dir in $tool[1]) {
            if (Test-Path $dir) { $env:PATH = "$env:PATH;$dir"; break }
        }
    }
    if (-not (Get-Command $tool[0] -ErrorAction SilentlyContinue)) { throw "$($tool[0]) was not found" }
}
$env:CMAKE_GENERATOR = 'Ninja'

# The Rust library, one per kind of processor, where Gradle picks them up.
$libs = Join-Path $root 'apps\decaero-android\android\app\src\main\jniLibs'
if (Test-Path $libs) { Remove-Item $libs -Recurse -Force }
$targets = @('-t', 'arm64-v8a')
if ($Emulator) { $targets += @('-t', 'x86_64') }
cargo ndk @targets --platform 26 -o $libs build --release -p decaero-android
if ($LASTEXITCODE -ne 0) { throw 'the Rust build failed' }

# The APK around it.
Push-Location (Join-Path $root 'apps\decaero-android\android')
try {
    gradle --no-daemon -q assembleRelease
    if ($LASTEXITCODE -ne 0) { throw 'the Gradle build failed' }
} finally {
    Pop-Location
}

$version = (Select-String -Path (Join-Path $root 'Cargo.toml') -Pattern '^version = "([0-9.]+)"').Matches[0].Groups[1].Value
$built = Join-Path $root 'apps\decaero-android\android\app\build\outputs\apk\release\app-release.apk'
New-Item -ItemType Directory -Force (Join-Path $root 'exe') | Out-Null
$name = if ($Emulator) { "DecAero-$version-with-x86_64.apk" } else { "DecAero-$version.apk" }
$apk = Join-Path $root "exe\$name"
Copy-Item $built $apk -Force
$item = Get-Item $apk
"{0}  {1:N1} MB  {2}" -f $item.FullName, ($item.Length / 1MB), (Get-FileHash $item.FullName -Algorithm SHA256).Hash
