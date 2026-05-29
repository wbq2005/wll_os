@echo off
chcp 65001 >nul
REM 运行 RISC-V 内核的批处理脚本 - 简化输出

set "QEMU=C:\Program Files\qemu\qemu-system-riscv64.exe"
set "KERNEL=kernel-rv"
set "SDCARD=sdcard-rv.img"
set "BIOS=default"

echo Starting QEMU...
echo.

set "QEMU_ARGS=-machine virt -nographic -bios %BIOS% -kernel %KERNEL% -m 128M -smp 1 -serial stdio -no-reboot"

if exist "%SDCARD%" (
    echo Attaching VirtIO block device...
    set "QEMU_ARGS=%QEMU_ARGS% -drive file=%SDCARD%,format=raw,if=none,id=hd0 -device virtio-blk-device,drive=hd0"
) else (
    echo WARNING: %SDCARD% not found. VirtIO ext4 will not be available.
)

%QEMU% %QEMU_ARGS%
