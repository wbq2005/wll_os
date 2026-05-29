#!/bin/bash
# 运行 RISC-V 内核的脚本

SCRIPT_DIR="$(dirname "$0")"
cd "$SCRIPT_DIR"

SDCARD="sdcard-rv.img"
BIOS="/c/Program Files/qemu/share/opensbi-riscv64-generic-fw_dynamic.bin"

if [ ! -f "$BIOS" ]; then
    BIOS="default"
fi

QEMU_ARGS=(
    -machine virt
    -kernel target/riscv64gc-unknown-none-elf/release/os
    -m 128M
    -smp 1
    -bios "$BIOS"
    -nographic
    -serial stdio
    -no-reboot
)

if [ -f "$SDCARD" ]; then
    echo "Attaching VirtIO block device..."
    QEMU_ARGS+=(-drive "file=$SDCARD,format=raw,if=none,id=hd0")
    QEMU_ARGS+=(-device virtio-blk-device,drive=hd0)
else
    echo "WARNING: $SDCARD not found. VirtIO ext4 will not be available."
fi

# 使用 timeout 运行 QEMU，5秒后自动退出
timeout 5 "/c/Program Files/qemu/qemu-system-riscv64.exe" "${QEMU_ARGS[@]}" 2>&1 || echo "QEMU exited or timed out"
