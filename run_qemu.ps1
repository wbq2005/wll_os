param(
    [string]$TimeoutSeconds = 60
)

$ErrorActionPreference = "Stop"

$qemu = "C:\Program Files\qemu\qemu-system-riscv64.exe"
$kernel = "D:\wll_os-master1\kernel-rv"
$sdcard = "D:\wll_os-master1\sdcard-rv.img"
$log = "D:\wll_os-master1\testdata\console_log"

Write-Host "Starting QEMU with ${TimeoutSeconds}s timeout..."

$proc = Start-Process -FilePath $qemu -ArgumentList @(
    "-machine", "virt",
    "-kernel", $kernel,
    "-m", "1G",
    "-nographic",
    "-smp", "1",
    "-bios", "default",
    "-drive", "file=$sdcard,if=none,format=raw,id=x0",
    "-device", "virtio-blk-device,drive=x0,bus=virtio-mmio-bus.0",
    "-no-reboot",
    "-device", "virtio-net-device,netdev=net",
    "-netdev", "user,id=net",
    "-rtc", "base=utc"
) -NoNewWindow -PassThru -RedirectStandardOutput $log -RedirectStandardError "D:\wll_os-master1\testdata\console_err.log"

$waited = 0
$interval = 1000

while (-not $proc.HasExited -and $waited -lt ($TimeoutSeconds * 1000)) {
    Start-Sleep -Milliseconds $interval
    $waited += $interval
    if ($waited % 10000 -eq 0) {
        Write-Host "Still running after $waited ms..."
    }
}

if (-not $proc.HasExited) {
    Write-Host "Timeout reached, killing QEMU..."
    Stop-Process -Id $proc.Id -Force
} else {
    Write-Host "QEMU exited with code: $($proc.ExitCode)"
}

Write-Host "Done. Log written to: $log"
