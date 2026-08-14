# Stage 2 raw-frame ownership audit

Date: 2026-08-14
Source: `055df99ff554441f7699c3518b1a4b204bd9c265`, detached and dirty
Evidence level: diagnostic candidate only; no BuildStorm completion claim

## Scope

This audit isolates the residual delayed-QMP failure:

```text
malloc_consolidate(): unaligned fastbin chunk detected
BUILDSTORM_COMPILE mode=multi ok=false ... SIGABRT
```

The ext4 directory-lock defect is retained as a separate fixed capability. No
scheduler, VFS, MM topology, TLB, image, judge, marker, or guest-time behavior
is changed by this experiment.

## Ownership model checked

`FrameTracker` owns resident user frames and uses the per-frame `AtomicUsize`
table. `FrameTracker::clone`, COW/fork, page-fault installation, resident-set
split/merge, and `MemorySet` destruction must preserve the tracked owner count.

Raw single-page ownership is distinct: `polyhal::PageAlloc::alloc` is used for
new page-table roots and intermediate page tables. It transfers a sole
`FrameTracker` through `into_raw_ppn`; page-table release returns it through
`PageAlloc::dealloc`.

Raw contiguous ownership is used by the kernel heap extension, VirtIO DMA, and
kernel stacks. These ranges are not resident `FrameTracker`s and must never
overlap tracked or page-table ownership.

## Diagnostic invariant experiment

Only `buildstorm-diagnostics` adds a byte owner shadow per managed page:

```text
FREE -> TRACKED -> FREE
FREE -> PAGE_TABLE -> FREE
FREE -> CONTIGUOUS -> FREE
```

Transitions use acquire/release CAS and panic on an unexpected source state.
The existing tracked refcount still asserts underflow, overflow, and raw
conversion of a shared frame. A periodic fixed-size line reports active owner
totals and transition count:

```text
BUILDSTORM_DIAG frame_ownership tracked=... page_table=... contiguous=... transitions=...
```

Production builds contain no shadow, counters, or diagnostic branch.

## Causal model and falsification

If the glibc corruption is caused by raw frame lifetime/allocator overlap, a
diagnostic run should show an ownership violation before the SIGABRT, or a
nonzero active owner total after the corresponding raw release. A long run with
no violation and the same SIGABRT falsifies this hypothesis and moves the audit
to heap metadata writes or another subsystem. A run with no SIGABRT but a
violation is also a failed correctness result and cannot be used as a
performance result.

## Build evidence

- RISC-V64 production release check: passed.
- RISC-V64 `buildstorm-diagnostics` release check: passed.
- LoongArch64 production release check: passed on the established remote
  cross-build host.
- LoongArch64 `buildstorm-diagnostics` release check: passed on the same host.
- RISC-V64 and LoongArch64 SMP8 resident-memory, high-arena and user-memory
  lifecycle regressions passed, including 64 isolation iterations, ASID/root
  checks and 160-MiB heap stress. These independent regressions are
  `capability-pass`, not BuildStorm completion evidence.
- `git diff --check`: passed.

## QEMU evidence

All runs used the unmodified RISC-V64 image at
`/srv/buildstorm/images/sdcard-rv-pub.img`, SHA-256
`d74e436522f5946ca17280a7a25f17dbb6604b71fe675bb8a021ce8e849b334c`,
with QEMU 11.0.3, `-snapshot -m 8G -smp 8`. The runner checked for an existing
QEMU/runner before launch, and no QEMU instances overlapped.

| Run | Window | Compile events | Last crate | Ownership result | Other result |
| --- | ---: | ---: | --- | --- | --- |
| diagnostics ownership | 700 s | 8 | `num-traits` | 1,623,609 legal transitions; no violation | cargo-only userspace stall after about 430 s; no panic/OOM/result marker |
| diagnostics delayed QMP | 600 s | 54 | `ax-memory-addr` | 2,887,086 legal transitions; no violation | changing user PCs and active rustc threads; no panic/OOM/result marker |
| production control | 700 s | 112 | `uguid` | diagnostic shadow absent by cfg | no panic/OOM/SIGABRT; no result marker |

The first diagnostic run's cargo-only stall did not reproduce in the second
run and is not a stable kernel hotspot. Delayed QMP samples in the second run
showed changing user PCs and later eight or more runnable rustc threads rather
than a fixed cargo PC. Conversely, the earlier fastbin SIGABRT did not
reproduce in either diagnostic window or the production control. The raw-frame
overlap hypothesis is therefore weakened, but not formally falsified because
the exact SIGABRT and a clean ownership trace have not co-occurred.

The production control was still making progress at the boundary: its final
crate appeared at about 699 seconds. It is `unverified`, not a successful
BuildStorm run, because no retained log contains
`BUILDSTORM_COMPILE mode=multi ok=true`. The historical SIGABRT is now treated
as a non-stable anomaly unless a comparable run reproduces it. The next
attribution step is a longer production window with delayed, low-frequency QMP
sampling; no new production optimization is justified by the ownership data
alone.

## Residual hotspot selected for the next production candidate

The 1,800-second production window did not reproduce the fastbin abort and
reached 131 compile events, ending at `flatten_objects`. Delayed QMP samples
showed the recurring PC `0x80283a50` in `task::manager::live_tasks()`. Return
addresses identified its callers as
`syscall::other::next_interval_timer_deadline_us` and
`syscall::other::wake_expired_interval_timers`. Host `pidstat` showed all
eight TCG CPU threads around 81--93% while these samples appeared, so this is
not a single blocked rustc thread.

The production candidate is an active-owner index for interval timers. The
index stores only weak TCB references; `TaskInner.interval_timers` remains the
source of truth. `setitimer` registers or unregisters after releasing the TCB
inner lock, exit unregisters after clearing timer state, and timer scans hold
the index lock before an inner lock. This removes the timer tick's allocation
and full historical-task scan when no interval timer is armed. The candidate
does not alter scheduler, VMA, resident, page-table, TLB, VFS or block-cache
ownership.

Falsification conditions are: any dual-architecture release or lifecycle
regression failure attributable to the change; changed setitimer one-shot or
periodic semantics; a 300-second production result below 5% progress gain or
with a new panic/OOM; or QMP still dominated by `live_tasks()` after the
candidate. The initial gates are four release builds (`capability-pass`) and
RISC-V64 SMP8 lifecycle (`capability-pass`); LoongArch64 lifecycle remains
`unverified` because the independent gate timed out after its high-arena phase.

The RISC-V64 candidate window reached 23 compile events in 300 seconds, the
same early boundary as the control, so measured compile-progress gain is 0%.
This fails the performance threshold for claiming an optimization. It does,
however, falsify the narrower hotspot prediction: QMP no longer sampled
`live_tasks()`; dominant PCs moved into the scheduler's `wfi` and idle-mask
publication path. The active-owner index is retained as a semantics-preserving
structural cleanup, while scheduler idle/wakeup accounting becomes the next
separate candidate. No BuildStorm completion claim is made.

## Evidence paths and hashes

- `20260814-stage2-raw-frame-ownership-gates/`
- `20260814-riscv64-diagnostics-stage2-raw-frame-ownership-window700/`
  (`serial.log` SHA-256 `16b9b31c1b721621f1734b3ee7ecc15ce8af12037bdf31fc36f6354f476081e3`)
- `20260814-riscv64-diagnostics-stage2-cargo-user-spin-qmp-window600/`
  (`serial.log` SHA-256 `2d81ee64d415fdb75a055e4f9dcb82c040459a3651d4ea66c878ace7ed75b984`)
- `20260814-riscv64-production-stage2-ext4-refcount-control-window700/`
  (`serial.log` SHA-256 `d566da8688d591bb060bc18336588623e43aeeadd41616de5a559065e455c6ea`)
- `20260814-riscv64-production-stage2-refcount-late-qmp-window1800/`
  (`serial.log` SHA-256 `bf515e0dfe7e4a7774afc2210ca4e89448c1af71c0b3578af0907045da5b06ce`)
- `20260814-riscv64-production-stage2-interval-timer-index-window300/`
  (`serial.log` SHA-256 `552d239a907a61b7a8f3f439eaafc2b9df4139149d5f92a3a53fbf9a3ce118cd`)
- `20260814-stage2-interval-timer-index-gates/`

Complete BuildStorm remains **`unverified / not completed`**.
