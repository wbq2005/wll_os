$env:RUSTUP_TOOLCHAIN = "nightly-x86_64-pc-windows-msvc"

$cargoDir = "D:\os_contest\os\.cargo"
if (Test-Path $cargoDir) {
    Remove-Item -Recurse -Force $cargoDir
}
New-Item -ItemType Directory -Path $cargoDir | Out-Null
Copy-Item -Path "D:\os_contest\os\cargo_config\*" -Destination $cargoDir -Recurse

cargo build --release --target riscv64gc-unknown-none-elf -Z build-std=core,alloc 2>&1
exit $LASTEXITCODE
