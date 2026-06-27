$ErrorActionPreference = "Continue"
$qemu = "C:\Program Files\qemu\qemu-system-riscv64.exe"
$kernel = "D:\os_contest\kernel-rv"
$sdcard = "D:\os_contest\sdcard-rv.img"

Write-Host "=========================================="
Write-Host "OS Competition Kernel - RISC-V Test"
Write-Host "=========================================="
Write-Host ""
Write-Host "QEMU: $qemu"
Write-Host "Kernel: $kernel"
Write-Host "SDCard: $sdcard"
Write-Host ""

if (-not (Test-Path $kernel)) {
    Write-Host "ERROR: Kernel not found at $kernel"
    Write-Host "Please build first: make all"
    exit 1
}

$bios = "C:\Program Files\qemu\share\opensbi-riscv64-generic-fw_dynamic.bin"
if (-not (Test-Path $bios)) {
    $bios = "default"
}

Write-Host "BIOS: $bios"
Write-Host ""
Write-Host "Starting QEMU (will exit after 15 seconds)..."
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
    Write-Host "Attaching VirtIO block device with sdcard image..."
    $qemu_args += "-drive"
    $qemu_args += "file=$sdcard,format=raw,if=none,id=hd0"
    $qemu_args += "-device"
    $qemu_args += "virtio-blk-device,drive=hd0"
} else {
    Write-Host "WARNING: sdcard-rv.img not found. VirtIO ext4 will not be available."
    Write-Host "         Build with sdcard-rv.img present to preload test programs."
    Write-Host "         Or run: make unpack-sdcard"
}

& $qemu @qemu_args
