# LoongArch64 public official BuildStorm completion after fchdir

Date: 2026-08-16

## Identity

- Kernel HEAD: `2953af6a725c015f89e64db9eab8f60ee27f6fe1` plus the dirty diff in
  `kernel-source-dirty.diff`.
- Public official suite: `b5ec6ef8497e1818cbdec3b54bb722f036e57972`.
- LoongArch64 image SHA-256:
  `d1410544e677e11efb1c240be6ffb201c89d6de58c9675e73314a696e4cefdc5`.
- Kernel SHA-256:
  `07307571458a5e24e05b01a7dac768507d1d390ae1618a89c128e5113fb54cf5`.
- Serial SHA-256:
  `f10fd92e2a14b2c3926c18ffec55ee53b0dbd6e2598a471d99f8c51ecc91b1cd`.

The source worktree was intentionally dirty. Its complete status and diff are
preserved; unrelated user changes were not cleaned, overwritten, or reverted.

## Public official result

The unmodified public image ran with `-m 8G -smp 8 -snapshot` and production
diagnostics disabled. `serial.log` contains the exact terminal result:

```text
BUILDSTORM_TOOLCHAIN ok
BUILDSTORM_MINIBUILD ok
BUILDSTORM_BEGIN mode=multi
BUILDSTORM_COMPILE mode=multi ok=true elapsed_s=553.62 cores=8 bytes=1716224 arch=loongarch64
```

The official judge parsed 180/180 automated points. The public judge's
LoongArch64 12-core warning is a non-scoring stale sanity check; the 2026
scoring configuration and this run use eight vCPUs.

This public run is `official-pass`. The evaluator-only UEFI boot preparation
stage is absent from the public script, so that hidden stage remains
`unverified` until a new evaluator run. The independent glibc/coreutils
directory ABI test in the sibling capability evidence is `capability-pass`.

## Reproduction

```sh
export PATH=/root/.cargo/bin:$PATH
python3 scripts/run_buildstorm.py \
  --arch loongarch64 \
  --image /srv/buildstorm/images/sdcard-la-pub.img \
  --stage complete --timeout 15000 --memory 8G --smp 8
python3 /srv/buildstorm/src/testsuits-for-oskernel/judge/judge_buildstorm-glibc.py \
  _tmp/buildstorm-loongarch64-complete.log
```

Only one QEMU instance was active during the run.
