@echo off
chcp 65001 >nul
echo ==========================================
echo OS Competition Kernel - RISC-V Test
echo ==========================================
echo.

set "QEMU=C:\Program Files\qemu\qemu-system-riscv64.exe"
set "KERNEL=kernel-rv"
set "SDCARD=sdcard-rv.img"
set "BIOS=default"

echo QEMU: %QEMU%
echo Kernel: %KERNEL%
echo SDCard: %SDCARD%
echo.

if not exist "%KERNEL%" (
    echo ERROR: Kernel not found!
    echo Please build first: make all
    pause
    exit /b 1
)

echo Starting QEMU...
echo.

set "QEMU_ARGS=-machine virt -nographic -bios %BIOS% -kernel "%KERNEL%" -m 128M -smp 1 -no-reboot"

if exist "%SDCARD%" (
    echo Attaching VirtIO block device...
    set "QEMU_ARGS=%QEMU_ARGS% -drive file=%SDCARD%,format=raw,if=none,id=hd0 -device virtio-blk-device,drive=hd0"
) else (
    echo WARNING: %SDCARD% not found. VirtIO ext4 will not be available.
    echo          Run: make unpack-sdcard
)

"%QEMU%" %QEMU_ARGS%

echo.
echo QEMU exited.
pause
