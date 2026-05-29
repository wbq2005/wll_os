@echo off
chcp 65001 >nul
echo ==========================================
echo OS Competition Kernel - RISC-V Test
echo ==========================================
echo.

set "QEMU=C:\Program Files\qemu\qemu-system-riscv64.exe"
set "KERNEL=target\riscv64gc-unknown-none-elf\release\os"
set "BIOS=C:\Program Files\qemu\share\opensbi-riscv64-generic-fw_dynamic.bin"

echo QEMU: %QEMU%
echo Kernel: %KERNEL%
echo.

if not exist "%KERNEL%" (
    echo ERROR: Kernel not found!
    echo Please build first: cargo build --release
    pause
    exit /b 1
)

echo Starting QEMU...
echo Press Ctrl+A then X to exit
echo.

"%QEMU%" ^
    -machine virt ^
    -nographic ^
    -bios "%BIOS%" ^
    -kernel "%KERNEL%" ^
    -m 128M ^
    -smp 1 ^
    -no-reboot ^
    -no-shutdown

echo.
echo QEMU exited.
pause
