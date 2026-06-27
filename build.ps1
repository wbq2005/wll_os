# wll_OS Windows Build Script
param(
    [ValidateSet("riscv64", "loongarch64", "both")]
    [string]$Arch = "both",

    [switch]$Check,

    [switch]$LibcTest,

    [switch]$Iozone,

    [switch]$Lmbench
)

$ErrorActionPreference = "Stop"
$ProgressPreference = "SilentlyContinue"

$TOOLCHAIN = ""  # toolchain resolved by rust-toolchain.toml in project root
$WORK_DIR = Split-Path -Parent $MyInvocation.MyCommand.Path

function Build-Arch {
    param([string]$TargetArch)

    $TARGET = switch ($TargetArch) {
        "riscv64"     { "riscv64gc-unknown-none-elf" }
        "loongarch64" { "loongarch64-unknown-none" }
    }

    $EXTRA = if ($TargetArch -eq "loongarch64") {
        "--no-default-features --features loongarch"
    } else {
        ""
    }

    $MODE = if ($Check) { "check" } else { "build" }
    Write-Host "[$TargetArch] cargo $MODE -p wll_OS --release --target $TARGET ..." -ForegroundColor Cyan

    Push-Location "$WORK_DIR"
    try {
        # Build using cargo directly (toolchain from rust-toolchain.toml)
        # Note: -p must come AFTER check/build command, -Z must come at the end
        $cargoArgs = @($MODE, "-p", "wll_OS", "--locked", "--offline", "--release", "--target", $TARGET)
        if ($EXTRA -ne "") {
            $cargoArgs += $EXTRA.Split(" ") | Where-Object { $_ -ne "" }
        }
        if ($LibcTest) {
            $cargoArgs += @("--features", "libctest")
        }
        if ($Iozone) {
            $cargoArgs += @("--features", "iozone")
        }
        if ($Lmbench) {
            $cargoArgs += @("--features", "lmbench")
        }
        Write-Host "[$TargetArch] cargo @($cargoArgs)" -ForegroundColor Gray
        & cargo $cargoArgs

        if ($LASTEXITCODE -ne 0) {
            Write-Host "[$TargetArch] FAILED (exit $LASTEXITCODE)" -ForegroundColor Red
            exit 1
        }

        # Copy output binary
        if (-not $Check) {
            $src = "target/$TARGET/release/wll_OS"
            $dst = if ($TargetArch -eq "riscv64") { "$WORK_DIR\kernel-rv" } else { "$WORK_DIR\kernel-la" }
            Copy-Item -Force $src $dst
            $size = (Get-Item $dst).Length
            Write-Host "[$TargetArch] Done: $dst ($('{0:N1}' -f ($size / 1MB)) MB)" -ForegroundColor Green
        } else {
            Write-Host "[$TargetArch] Check passed" -ForegroundColor Green
        }
    } finally {
        Pop-Location
    }
}

Push-Location $WORK_DIR
try {
    if ($Arch -eq "both") {
        Build-Arch "riscv64"
        Build-Arch "loongarch64"
    } else {
        Build-Arch $Arch
    }
} finally {
    Pop-Location
}
