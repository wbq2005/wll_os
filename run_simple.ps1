$qemu = "C:\Program Files\qemu\qemu-system-riscv64.exe"
$kernel = "D:\wll_os-master1\kernel-rv"
$sdcard = "D:\wll_os-master1\sdcard-rv.img"
$log = "D:\wll_os-master1\testdata\console_log"
$err = "D:\wll_os-master1\testdata\console_err.log"
$proc = Start-Process $qemu -ArgumentList @("-machine","virt","-kernel",$kernel,"-m","1G","-nographic","-smp","1","-bios","default","-drive","file=$sdcard,if=none,format=raw,id=x0","-device","virtio-blk-device,drive=x0,bus=virtio-mmio-bus.0","-no-reboot","-device","virtio-net-device,netdev=net","-netdev","user,id=net","-rtc","base=utc") -NoNewWindow -PassThru -RedirectStandardOutput $log -RedirectStandardError $err
Start-Sleep -Seconds 45
if (-not $proc.HasExited) { Stop-Process -Id $proc.Id -Force; Write-Host "Timeout" }
else { Write-Host "Exit:" $proc.ExitCode }
