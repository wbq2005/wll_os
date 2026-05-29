$ErrorActionPreference = "Continue"
$qemu = "C:\Program Files\qemu\qemu-system-loongarch64.exe"
$kernel = "D:\os_contest\kernel-la"
$sdcard = "D:\os_contest\sdcard-la.img"

Write-Host "=========================================="
Write-Host "OS Competition Kernel - LoongArch Test"
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

Write-Host "Starting QEMU (will exit after 15 seconds)..."
Write-Host ""

$qemu_args = @(
    "-kernel", $kernel,
    "-m", "128M",
    "-nographic",
    "-smp", "1",
    "-no-reboot",
    "-serial", "mon:stdio"
)

if (Test-Path $sdcard) {
    Write-Host "Attaching VirtIO block device with sdcard image..."
    $qemu_args += "-drive"
    $qemu_args += "file=$sdcard,format=raw,if=none,id=hd0"
    $qemu_args += "-device"
    $qemu_args += "virtio-blk-pci,drive=hd0,bus=pcie.0"
} else {
    Write-Host "WARNING: sdcard-la.img not found. VirtIO ext4 will not be available."
    Write-Host "         Build with sdcard-la.img present to preload test programs."
    Write-Host "         Or run: make unpack-sdcard"
}

& $qemu @qemu_args
