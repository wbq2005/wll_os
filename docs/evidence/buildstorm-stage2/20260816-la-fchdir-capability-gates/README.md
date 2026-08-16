# LoongArch64 directory-fd ABI capability gates

Date: 2026-08-16

## Scope

This gate validates the generic directory-fd and metadata ABI change used by
GNU coreutils during directory creation. The production change is limited to
`fchdir(50)` and `fchmodat2(452)` dispatch/implementation. The independent
`smp-regression` probe runs `mkdir -p -m`, `chmod`, `chown`, `stat`, `ln -s`,
and `readlink` through the image's glibc/coreutils userland.

The evaluator log that motivated this gate is stored in the sibling official
evidence directory as `evaluator-loongarch-output-25.txt` (SHA-256
`b3dda47be5bbd070a128a23503c5b790e4ab99db79d7d43b8850773475b3133b`).
It completed the LoongArch64 compile in 2439.63 seconds, then failed before the
success marker while creating `/work/buildstorm.esp` with `ENOSYS`.

## Results

- RISC-V64 production release check: pass (`rc=0`).
- RISC-V64 diagnostics release check: pass (`rc=0`).
- LoongArch64 production release check: pass (`rc=0`).
- LoongArch64 diagnostics release check: pass (`rc=0`).
- RISC-V64 SMP8 regression: `capability-pass`, including
  `pass phase=directory-metadata-abi` and the final `pass cpus=8` marker.
- LoongArch64 SMP8 regression: `capability-pass`, including
  `pass phase=directory-metadata-abi` and the final `pass cpus=8` marker.

`results.txt` records an initial invalid invocation (`cargo` was absent from a
non-login SSH PATH). Only `results-rerun.txt` is the valid four-cfg result.

## Reproduction

```sh
export PATH=/root/.cargo/bin:$PATH
cargo check -p wll_OS --release --target riscv64gc-unknown-none-elf \
  --no-default-features --features riscv
cargo check -p wll_OS --release --target riscv64gc-unknown-none-elf \
  --no-default-features --features riscv,buildstorm-diagnostics
cargo check -p wll_OS --release --target loongarch64-unknown-none \
  --no-default-features --features loongarch
cargo check -p wll_OS --release --target loongarch64-unknown-none \
  --no-default-features --features loongarch,buildstorm-diagnostics
python3 scripts/run_smp_regression.py --arch riscv64 \
  --image /srv/buildstorm/images/sdcard-rv-pub.img --timeout 90
python3 scripts/run_smp_regression.py --arch loongarch64 \
  --image /srv/buildstorm/images/sdcard-la-pub.img --timeout 90
```

The two QEMU regressions must be run serially.
