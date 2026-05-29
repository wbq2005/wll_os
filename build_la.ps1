$ErrorActionPreference = "Stop"
$env:RUSTUP_TOOLCHAIN = "nightly-x86_64-pc-windows-msvc"

Write-Host "=========================================="
Write-Host "Building LoongArch64 Kernel"
Write-Host "=========================================="

Write-Host "Setting up cargo config..."
$cargoDir = "D:\os_contest\os\.cargo"
if (Test-Path $cargoDir) {
    Remove-Item -Recurse -Force $cargoDir
}
New-Item -ItemType Directory -Path $cargoDir | Out-Null
Copy-Item -Path "D:\os_contest\os\cargo_config\*" -Destination $cargoDir -Recurse

Write-Host "Adding rust-src component..."
& rustup component add rust-src --toolchain $env:RUSTUP_TOOLCHAIN 2>&1 | Out-Null

Write-Host "Building LoongArch64 kernel..."
$buildOutput = & cargo build --release --target loongarch64-unknown-none --no-default-features --features loongarch -Z build-std=core,alloc 2>&1
$buildOutput | Out-Host

if ($LASTEXITCODE -ne 0) {
    Write-Host "[BUILD FAILED] Exit code: $LASTEXITCODE"
    exit 1
}

Write-Host "Copying kernel to D:\os_contest\kernel-la..."
$srcKernel = "D:\os_contest\target\loongarch64-unknown-none\release\wll_OS"
if (Test-Path $srcKernel) {
    $srcInfo = [System.IO.FileInfo]::new($srcKernel)
    Write-Host "Kernel size: $($srcInfo.Length) bytes"
    Copy-Item -Path $srcKernel -Destination "D:\os_contest\kernel-la" -Force
    Write-Host "[BUILD SUCCEEDED] kernel-la updated"
} else {
    Write-Host "[ERROR] Kernel not found at $srcKernel"
    exit 1
}

exit 0
