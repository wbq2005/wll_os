# BuildStorm Stage 2 official completion (2026-08-14)

## Evidence classification

BuildStorm full clean compilation is now `official-pass` on both RISC-V64 and
LoongArch64. Each run used the unmodified public image in QEMU snapshot mode,
the clean official `final-2026` suite and judge, 8 GiB guest memory, eight
vCPUs, a production kernel with diagnostics disabled, and the exact success
marker `BUILDSTORM_COMPILE mode=multi ok=true`.

The official scripted judge passed all four entries on each architecture:
toolchain 8/8, minibuild 12/12, compile completion 40/40, and measured compile
time 120/120. The manual 20-point design-document review remains outside the
scripted result.

## Source and authority identity

- Kernel HEAD: `055df99ff554441f7699c3518b1a4b204bd9c265`, detached and dirty.
- Raw binary dirty-diff SHA-256:
  `c22bc0a79e78a72a0c3c8dd13bf23c33692c5fe0991cc57ef0b1ce91c3c2a22d`.
- Official suite commit:
  `b5ec6ef8497e1818cbdec3b54bb722f036e57972` on clean `final-2026`.
- Official guest script SHA-256:
  `2f656a668076803fb465409374b6bcbb1fcbbc4f5c17a72b8ea4695668b9b33e`.
- Official judge SHA-256:
  `f9bc3c5c640217947775759b5b02aa4ceedfa76728d25d4f06d94ef5bc9d64dd`.
- QEMU: 11.0.3.
- No BuildStorm runner or QEMU instance existed before either launch. The
  architecture runs were sequential, never concurrent.

The dirty diff is retained verbatim in each evidence directory. Existing
user changes and untracked evidence were not reset, cleaned, or overwritten.

## RISC-V64 official result

- Image: `/srv/buildstorm/images/sdcard-rv-pub.img`.
- Image SHA-256:
  `d74e436522f5946ca17280a7a25f17dbb6604b71fe675bb8a021ce8e849b334c`.
- QEMU configuration: `-snapshot -m 8G -smp 8` with the official virtio block
  image attached read/write only through the ephemeral snapshot overlay.
- Kernel SHA-256:
  `071116d8e795e2340ea2de65d515508f26fb4aecc91ec41248d88a007a9311fe`.
- Guest result:
  `BUILDSTORM_COMPILE mode=multi ok=true elapsed_s=1322.99 cores=8 bytes=1683456 arch=riscv64`.
- Host runner elapsed: 1402.73 seconds.
- Serial SHA-256:
  `d2db0da818a0ab5057a541584a80893a50d8bda1f33bfbeeb697cc98681424dc`.
- Official judge: scripted 180/180; compile baseline 1616 seconds.

The run crossed the previously intermittent `bitmaps`/`flatten_objects`
ENOENT boundary and completed without panic, OOM, or filesystem error.

## LoongArch64 official result

- Image: `/srv/buildstorm/images/sdcard-la-pub.img`.
- Image SHA-256:
  `d1410544e677e11efb1c240be6ffb201c89d6de58c9675e73314a696e4cefdc5`.
- QEMU configuration: `-snapshot -m 8G -smp 8` with the official virtio block
  image attached through the ephemeral snapshot overlay.
- Kernel SHA-256:
  `deaa5ce7b492783e6cdabeaae8ddba767ad791170a049d6f7a29f79eb14adbf9`.
- Guest result:
  `BUILDSTORM_COMPILE mode=multi ok=true elapsed_s=1103.14 cores=8 bytes=1716224 arch=loongarch64`.
- Host runner elapsed: 1156.19 seconds.
- Serial SHA-256:
  `40469d314bbf50d08c06088e995e430a0ca98e2156b080282ee00cbb9733abfb`.
- Official judge: scripted 180/180; compile baseline 1985 seconds.

The judge also printed `guest saw cores=8, expected 12 for loongarch64`. This
is retained in the raw stderr. It is a non-failing upstream warning: the
published BuildStorm scoring rule and the required task gate use 8 vCPUs and
8 GiB, the runner recorded exactly that configuration, and all judge entries
passed. It must be rechecked if the evaluator publishes a new resource rule.

The guest emitted Cargo cache last-use warnings caused by an out-of-range time
conversion, but the warning did not affect compilation or artifact validation.
There was no kernel panic, OOM, or compile failure.

## Supporting capability gates

Before the complete runs, all four production/diagnostics release builds
passed on RISC-V64 and LoongArch64. Both architecture SMP8 regressions passed
the namespace lifecycle, resident/high-arena/user-memory lifecycle,
COW/shared/file mappings, ASID/TLB isolation, interval timer, heap stress, and
eight-CPU execution checks. Those independent runs remain `capability-pass`;
the two complete official runs are the `official-pass` evidence.

The namespace transaction architecture is the last causal correctness change
before completion. Pre-fix diagnostics observed positive and negative cache
publication crossing namespace mutations and production intermittently failed
rustc metadata creation with ENOENT. Post-fix diagnostics reduced both
crossing counters to zero, the concurrent namespace regression passed on both
architectures, and both complete official builds crossed the old failure
boundary. The fix did not materially improve the 900-second crate count, so it
is claimed as correctness architecture rather than as the primary throughput
speedup.

## Reproduction and retained artifacts

The production command for each architecture was:

```text
python3 scripts/run_buildstorm.py --arch <riscv64|loongarch64> \
  --image /srv/buildstorm/images/sdcard-<rv|la>-pub.img \
  --stage complete --timeout 15000 --memory 8G --smp 8
```

The official judge command was:

```text
python3 /srv/buildstorm/src/testsuits-for-oskernel/judge/judge_buildstorm-glibc.py serial.log
```

Complete local evidence, including raw serial logs, runner JSON, exact QEMU
arguments, release build logs, image/kernel/source hashes, suite identity,
official judge stdout/stderr and exit statuses, is stored under:

- `docs/evidence/buildstorm-stage2/20260814-riscv64-official-complete-stage2/`
- `docs/evidence/buildstorm-stage2/20260814-loongarch64-official-complete-stage2/`

AI assisted with evidence correlation, namespace race modeling, implementation,
regression design, controlled execution, monitoring, and provenance capture.
Every completion claim above is independently checkable from retained raw
artifacts and the unmodified official judge.
