#!/bin/bash
# Test script for running OS in QEMU

SCRIPT_DIR="$(dirname "$0")"
cd "$SCRIPT_DIR"

QEMU="/c/Program Files/qemu/qemu-system-riscv64.exe"
KERNEL="target/riscv64gc-unknown-none-elf/release/os"
SDCARD="sdcard-rv.img"
BIOS="/c/Program Files/qemu/share/opensbi-riscv64-generic-fw_dynamic.bin"

echo "=========================================="
echo "OS Competition Kernel - RISC-V Test"
echo "=========================================="
echo ""
echo "QEMU: $QEMU"
echo "Kernel: $KERNEL"
echo "SDCard: $SDCARD"
echo ""

if [ ! -f "$KERNEL" ]; then
    echo "ERROR: Kernel not found!"
    echo "Please build first: cargo build --release"
    exit 1
fi

if [ ! -f "$BIOS" ]; then
    BIOS="default"
fi

QEMU_ARGS=(
    -machine virt
    -kernel "$KERNEL"
    -m 128M
    -smp 1
    -bios "$BIOS"
    -nographic
    -no-reboot
)

if [ -f "$SDCARD" ]; then
    echo "Attaching VirtIO block device..."
    QEMU_ARGS+=(-drive "file=$SDCARD,format=raw,if=none,id=hd0")
    QEMU_ARGS+=(-device virtio-blk-device,drive=hd0)
else
    echo "WARNING: $SDCARD not found. VirtIO ext4 will not be available."
    echo "         Run: make unpack-sdcard"
fi

echo ""
echo "Starting QEMU..."
echo ""

"$QEMU" "${QEMU_ARGS[@]}"
