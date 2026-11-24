$ErrorActionPreference = "Stop"

$repoRoot = Resolve-Path (Join-Path $PSScriptRoot "..")
Push-Location $repoRoot
try {
    if (-not $env:ANDROID_NDK_HOME) {
        $sdkNdkDir = Join-Path $env:LOCALAPPDATA "Android\Sdk\ndk"
        if (Test-Path $sdkNdkDir) {
            $latestNdk = Get-ChildItem $sdkNdkDir -Directory | Sort-Object Name -Descending | Select-Object -First 1
            if ($latestNdk) {
                $env:ANDROID_NDK_HOME = $latestNdk.FullName
            }
        }
    }
    if (-not $env:ANDROID_NDK_HOME -or -not (Test-Path $env:ANDROID_NDK_HOME)) {
        throw "ANDROID_NDK_HOME is not set and no NDK was found under $env:LOCALAPPDATA\Android\Sdk\ndk"
    }
    Write-Host "Using ANDROID_NDK_HOME=$($env:ANDROID_NDK_HOME)"

    $jniLibsDir = Join-Path $repoRoot "android\app\src\main\jniLibs"
    $kotlinOutDir = Join-Path $repoRoot "android\app\src\main\java"
    New-Item -ItemType Directory -Force $jniLibsDir | Out-Null
    New-Item -ItemType Directory -Force $kotlinOutDir | Out-Null

    $prevRustflags = $env:RUSTFLAGS
    $env:RUSTFLAGS = "-C link-arg=-Wl,-z,max-page-size=16384"
    try {
        cargo ndk -t arm64-v8a -t x86_64 -o $jniLibsDir build -p luckymatch-ffi --release
        if ($LASTEXITCODE -ne 0) {
            throw "cargo ndk build failed with exit code $LASTEXITCODE"
        }
    }
    finally {
        $env:RUSTFLAGS = $prevRustflags
    }

    $arm64So = Join-Path $jniLibsDir "arm64-v8a\libluckymatch_ffi.so"
    if (-not (Test-Path $arm64So)) {
        throw "Expected $arm64So after cargo ndk build"
    }

    cargo run -p luckymatch-ffi --features bindgen-cli --bin uniffi-bindgen -- generate --library $arm64So --language kotlin --out-dir $kotlinOutDir
    if ($LASTEXITCODE -ne 0) {
        throw "uniffi-bindgen failed with exit code $LASTEXITCODE"
    }

    Write-Host "Android Rust .so libraries and UniFFI Kotlin bindings built successfully."
}
finally {
    Pop-Location
}
