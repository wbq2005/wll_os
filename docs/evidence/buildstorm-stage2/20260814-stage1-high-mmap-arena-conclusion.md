# Stage 1 conclusion: legal high mmap arena

Date: 2026-08-14
Source HEAD: `055df99ff554441f7699c3518b1a4b204bd9c265` (detached, dirty worktree)
Evidence level: production measurements are `unverified`; independent lifecycle
and SMP regressions are `capability-pass`.

## Decision

Retain the architecture-neutral low/high mmap layout and the matching
architecture-specific root ownership rules.  This stage removed the measured
late virtual-address exhaustion mechanism, crossed the previous 33-crate
production boundary, and introduced no observed correctness regression.  It
does not prove or claim a complete BuildStorm build.

## Causal model tested

The historical general mmap range was approximately 1.5 GiB:
`0x2000_0000..USER_STACK_TOP`.  Late rustc/allocator workers requested lazy
anonymous reservations as large as 128 MiB after that range had fragmented.
The largest sampled free gap was only 85.37 MiB.  Repeated selection failures
then amplified mmap selection, munmap topology, PTE/TLB and shared `MemorySet`
locking work.

The isolated production hypothesis was that a second legal, process-owned
arena would preserve lazy reservation semantics while eliminating the upstream
contiguous-VA failure:

```text
low arena:  0x0000_0000_2000_0000 .. USER_STACK_TOP
high arena: 0x0000_0020_0000_0000 .. 0x0000_0040_0000_0000
```

No guest allocator, official image, suite, command, path, marker or clock was
changed.  Reservations remain lazy; the new range does not eagerly allocate
data frames or page-table leaves.

## Ownership and invariants

- `UserVaLayout` owns legal low/high user ranges and mmap cursor policy.
- `MmContext` keeps independent low and high cursors so a failed large
  reservation cannot displace normal low mappings.
- RISC-V root entries `0..1` and `128..255` are process-owned; the kernel's low
  identity entry and other kernel/global roots remain shared.
- LoongArch process roots cover the legal PLV3 range while PLV0 DMW mappings
  remain platform-owned.
- Page-table restore and release consume the same root-ownership predicate.
- Fixed mappings, shared-memory attachment, shared-file writeback, fork/COW,
  partial unmap, mprotect, exec/drop and ASID/TLB isolation cover both arenas.

## Direct evidence

Before this stage, the late diagnostic interval from snapshot 81 to 88 lasted
70.088 seconds and recorded 88,498 mmap calls: 31,863 selections succeeded and
56,625 returned `ENOMEM`.  The maximum sampled free gap was 89,518,080 bytes
while the maximum failed reservation was 128 MiB.

With the high arena enabled, the RISC-V diagnostics run recorded at snapshot
67:

- 5,542 successful initial selections;
- zero selection `ENOMEM` results;
- zero commit-reselection failures;
- zero failed-length, failed-gap or failed-VMA maxima;
- successful large-reservation protocols through the late parallel rustc
  phase, with no panic or OOM marker.

This falsifies the prior low-arena-capacity mechanism for the retained layout.
It does not show that every later mmap/munmap is avoidable: successful allocator
arenas are still legitimately trimmed and released.

## Comparable progress

| Run | Window | Compiling lines | Last crate | First crate | Last event | Result |
| --- | ---: | ---: | --- | ---: | ---: | --- |
| RISC-V production Stage 1 | 300 s | 23 | `ax-posix-api` | 48.659 s | 56.870 s | no panic/OOM; incomplete |
| RISC-V production Stage 1 | 1,800 s | 34 | `hashbrown` | 49.059 s | 430.325 s | crossed old 33-crate boundary; incomplete |
| RISC-V diagnostics Stage 1 | 1,800 s | 65 | `rdif-reset` | 51.264 s | 628.210 s | attribution only; incomplete |

The prior production long-run platform stopped after 33 compile events at
`rustc-literal-escaper`.  Stage 1 reached the next crate, `hashbrown`, so it
made positive late-boundary progress.  The diagnostic count is not comparable
production throughput and is reported only as evidence that useful compilation
and MM/process activity continued well past the old boundary.

## Validation matrix

The retained tree passed four independent release cfg builds: RISC-V and
LoongArch production, plus both architectures with diagnostics/SMP enabled.
Both eight-CPU architecture regressions passed resident lifecycle, high-arena
lifecycle, user-memory lifecycle, fork/COW, partial unmap, mprotect, shared/file
mapping, ASID/TLB isolation and the 160-MiB heap stress gate.  These runs are
`capability-pass`, not official score evidence.

The unchanged official RISC-V image hash recorded by the production runs is
`d74e436522f5946ca17280a7a25f17dbb6604b71fe675bb8a021ce8e849b334c`.
The Stage 1 production kernel hash is
`56870ac6589aaa62a6e5ce8cbbd4071aee5faa44c7af61d9615da742256d4fb0`.
QEMU was `11.0.3`, with `-snapshot -m 8G -smp 8` and one QEMU instance at a
time.

## Evidence paths

- `20260813-current-global-hotspot-architecture-audit.md`
- `20260813-residual-compound-hotspot-audit.md`
- `20260814-riscv64-production-stage1-high-arena-window300/`
- `20260814-riscv64-production-stage1-high-arena-window1800/`
- `20260814-riscv64-diagnostics-stage1-high-arena-window300/`
- `20260814-riscv64-diagnostics-stage1-high-arena-window1800/`
- `20260814-loongarch64-diagnostics-stage1-high-arena-window300/`

AI assisted with evidence reconciliation, cfg-specific ownership auditing,
candidate design, regression planning, measurement comparison and this
documentation.  Developer-verifiable artifacts are the dirty source diff,
raw serial logs, launch metadata, kernel/image hashes and host samplers.

Complete BuildStorm remains **`unverified / not completed`**.  No retained raw
serial log contains the exact successful marker
`BUILDSTORM_COMPILE mode=multi ok=true`.
