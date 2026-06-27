# Regression Baseline

Frozen on 2026-06-04 for later basic/busybox regression work.

This file records the current real execution path. It is intentionally a
verification baseline only; it must not be used as permission to hardcode judge
output or testcase success.

## Harness Path

1. Boot calls `task::add_initproc()`.
2. The kernel first tries to load `init` or `/init`.
3. If init is missing or cannot be loaded, `harness::try_start_runtime_test_harness()`
   collects runtime scripts and starts a kernel harness task.
4. Script discovery uses `ext4_vol::ext4_list_all_file_paths()` and keeps paths
   whose basename is `*_testcode.sh`.
5. With the `dev-preload` feature only, an empty ext4 discovery result falls
   back to `fs::list_files()` from MemFS.
6. The default enabled groups are only `basic` and `busybox`.
7. Scripts are sorted by group rank, then by libc root priority:
   `/glibc` before `/musl`, then other paths.
8. Each script is launched as a real user program:
   `/busybox sh <logical_script>`.
9. The harness waits in foreground mode until the launched task tree exits or
   times out.
10. After all enabled scripts finish, the harness leaves foreground mode and
    shuts QEMU down.

## Real Output Contract

- The kernel does not print testcase success markers for basic or busybox.
- User stdout/stderr goes through fd 1/2, then `FileDescriptor::write()`, then
  the line-buffered console path in `fs/fd.rs`.
- Basic markers are expected to come from the test programs/scripts, commonly:
  `========== START <case> ==========` and `========== END <case> ==========`.
- Busybox markers are expected to come from the busybox test script, commonly:
  `testcase busybox <command> success` or `testcase busybox <command> fail`.
- Group markers such as `#### OS COMP TEST GROUP START ... ####` and matching
  `END` markers are script/judge framing output, not kernel-forged pass data.

## Commands

Preferred checks:

```bash
make ARCH=riscv64 check
make ARCH=loongarch64 check
```

Fallback checks when `make` is unavailable:

```bash
cd os
cargo +nightly-2025-01-18 check --locked --offline --release --target riscv64gc-unknown-none-elf
cargo +nightly-2025-01-18 check --locked --offline --release --target loongarch64-unknown-none --no-default-features --features loongarch
```

RISC-V basic/busybox smoke, when Docker/QEMU and `sdcard-rv.img` are available:

```bash
python scripts/run_basic_judge.py --arch riscv64 --timeout 180
```

Equivalent manual smoke command used by the script:

```bash
docker run --rm -v "$PWD:/workspace" -w /workspace zhouzhouyi/os-contest:20260510 bash -lc 'make ARCH=riscv64 build && cp target/riscv64gc-unknown-none-elf/release/wll_OS kernel-rv.test && timeout --foreground 180s qemu-system-riscv64 -machine virt -kernel kernel-rv.test -m 1G -nographic -smp 1 -bios default -drive file=sdcard-rv.img,if=none,format=raw,id=x0 -device virtio-blk-device,drive=x0,bus=virtio-mmio-bus.0 -no-reboot -device virtio-net-device,netdev=net -netdev user,id=net -rtc base=utc'
```

## Failure Localization

- No init and no harness: inspect ext4 mount logs, `ext4_list_all_file_paths()`,
  and whether `*_testcode.sh` exists on the attached disk.
- `[harness] failed to launch script`: inspect `/busybox`, the target script,
  root mapping for `/glibc` or `/musl`, and interpreter availability.
- `[harness] TIMEOUT pid=...`: inspect scheduler/blocking/wait queue behavior
  for the active foreground task tree.
- Missing basic markers: inspect the real test binary output and its syscalls;
  do not add kernel-side marker fabrication.
- Missing busybox `testcase ... success`: inspect busybox shell/app execution,
  filesystem side effects, and fd output.
- Build-only failures: compare with the exact check command above before
  debugging QEMU behavior.
