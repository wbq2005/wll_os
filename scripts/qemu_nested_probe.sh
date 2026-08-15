#!/bin/sh
# Independent nested-QEMU startup probe for a derived development image.

set -u

QEMU_ROOT=${QEMU_ROOT:-/opt/qemu-rv}
QEMU_BIN="$QEMU_ROOT/usr/bin/qemu-system-riscv64"
QEMU_LOADER="$QEMU_ROOT/lib/ld-linux-riscv64-lp64d.so.1"
QEMU_LIBS="$QEMU_ROOT/lib/riscv64-linux-gnu:$QEMU_ROOT/usr/lib/riscv64-linux-gnu:$QEMU_ROOT/lib:$QEMU_ROOT/usr/lib"
QEMU_SHARE="$QEMU_ROOT/usr/share/qemu"
QEMU_BIOS="$QEMU_ROOT/usr/lib/riscv64-linux-gnu/opensbi/generic/fw_dynamic.bin"
NESTED_KERNEL=${NESTED_KERNEL:-/opt/qemu-nested-kernel}
RUN_OUT=/tmp/qemu-nested-probe.out

echo "[qemu-probe] begin"
for input in "$QEMU_BIN" "$QEMU_LOADER" "$QEMU_BIOS" "$NESTED_KERNEL"; do
    if [ ! -f "$input" ]; then
        echo "[qemu-probe] missing input: $input"
        exit 2
    fi
done

if ! "$QEMU_LOADER" --library-path "$QEMU_LIBS" "$QEMU_BIN" --version; then
    echo "[qemu-probe] dynamic launch failed"
    exit 3
fi

: >"$RUN_OUT"
"$QEMU_LOADER" --library-path "$QEMU_LIBS" "$QEMU_BIN" \
    -L "$QEMU_SHARE" -machine virt -smp 1 -m 256M -nographic \
    -monitor none -serial stdio -bios "$QEMU_BIOS" -no-reboot \
    -kernel "$NESTED_KERNEL" \
    >"$RUN_OUT" 2>&1 &
qemu_pid=$!

seconds=0
while [ "$seconds" -lt 30 ]; do
    if grep -Fq "[kernel] Hello, OS!" "$RUN_OUT" 2>/dev/null; then
        kill "$qemu_pid" 2>/dev/null || true
        wait "$qemu_pid" 2>/dev/null || true
        echo "[qemu-probe] pass stage=kernel elapsed_s=$seconds"
        exit 0
    fi
    if ! kill -0 "$qemu_pid" 2>/dev/null; then
        break
    fi
    sleep 1
    seconds=$((seconds + 1))
done

kill "$qemu_pid" 2>/dev/null || true
wait "$qemu_pid" 2>/dev/null || true
echo "[qemu-probe] fail stage=kernel elapsed_s=$seconds"
tail -40 "$RUN_OUT" 2>/dev/null || true
exit 1
