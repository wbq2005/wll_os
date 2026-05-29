@echo off
chcp 65001 >nul
echo ==========================================
echo OS Competition Kernel - RISC-V Test
echo ==========================================
echo.

set "SDCARD=%cd%\sdcard-rv.img"

echo QEMU: %QEMU%
echo Kernel: %KERNEL%
echo SDCard: %SDCARD%
echo.

if not exist "%KERNEL%" (
    echo ERROR: Kernel not found!
    echo Please build first: cargo build --release
    pause
    exit /b 1
)

set "QEMU_ARGS=-machine virt -nographic -kernel "%KERNEL%" -m 128M -smp 1 -no-reboot -bios default"

if exist "%SDCARD%" (
    echo Attaching VirtIO block device...
    set "QEMU_ARGS=%QEMU_ARGS% -drive file=%SDCARD%,format=raw,if=none,id=hd0 -device virtio-blk-device,drive=hd0"
) else (
    echo WARNING: sdcard-rv.img not found. VirtIO ext4 will not be available.
    echo          Run: make unpack-sdcard
)

echo.
echo Starting QEMU...
echo.

"%QEMU%" %QEMU_ARGS%

echo.
echo QEMU exited.
pause
