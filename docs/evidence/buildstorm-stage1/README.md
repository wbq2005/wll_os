# BuildStorm stage 1 evidence

This directory contains the small, reviewable evidence set for the 2026 BuildStorm stage-1 environment result.

- `baseline-riscv64-no-script-echo.log`: earliest one-HART diagnostic boundary; not scoring evidence.
- `riscv64-minibuild-serial.log`, `loongarch64-minibuild-serial.log`: final raw serial logs from unmodified official images.
- `*-minibuild-run.json`: exact QEMU arguments and host elapsed time produced by the runner.
- `*-judge.txt`: output from the official `judge_buildstorm-glibc.py`.
- `compiler-capability-riscv64.*`: independent cargo create/build/run capability evidence; not an official score.
- `cagent-regression-*.log`: 10/10 CAgent case regression logs. The END marker is absent in both.
- `basic-smoke-*.log`: preliminary-round smoke logs. The LoongArch image lacks `run-all.sh`.
- `provenance.json`: source, image, script, kernel, host and QEMU identities.

Official disk images and kernel binaries are intentionally not committed.
