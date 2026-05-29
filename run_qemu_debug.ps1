$ErrorActionPreference = "Continue"
$qemu = "C:\Program Files\qemu\qemu-system-riscv64.exe"
$kernel = "D:\os_contest\kernel-rv"

Write-Host "=========================================="
Write-Host "OS Competition Kernel - RISC-V Debug"
Write-Host "=========================================="
Write-Host ""
Write-Host "QEMU: $qemu"
Write-Host "Kernel: $kernel"
Write-Host ""

if (-not (Test-Path $kernel)) {
    Write-Host "ERROR: Kernel not found at $kernel"
    Write-Host "Please build first: make all"
    exit 1
}

# 使用 fw_dynamic 固件以正确传递 DTB
$bios = "C:\Program Files\qemu\share\opensbi-riscv64-generic-fw_dynamic.bin"
if (-not (Test-Path $bios)) {
    Write-Host "WARNING: OpenSBI firmware not found at $bios"
    Write-Host "Falling back to default BIOS"
    $bios = "default"
}

$sdcard = "D:\os_contest\sdcard-rv.img"

Write-Host "BIOS: $bios"
Write-Host "SDCard: $sdcard"
Write-Host ""
Write-Host "Press Ctrl+A then X to exit QEMU"
Write-Host ""

$qemu_args = @(
    "-machine", "virt",
    "-kernel", $kernel,
    "-m", "128M",
    "-nographic",
    "-smp", "1",
    "-bios", $bios,
    "-no-reboot"
)

if (Test-Path $sdcard) {
    Write-Host "Attaching VirtIO block device..."
    $qemu_args += "-drive"
    $qemu_args += "file=$sdcard,format=raw,if=none,id=hd0"
    $qemu_args += "-device"
    $qemu_args += "virtio-blk-device,drive=hd0"
}

& $qemu @qemu_args 2>&1
