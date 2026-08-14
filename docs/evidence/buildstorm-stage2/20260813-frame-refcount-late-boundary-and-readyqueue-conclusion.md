# BuildStorm frame-refcount late-boundary conclusion

Date: 2026-08-13

Evidence level: `unverified` for BuildStorm completion; `capability-pass` for
the dual-architecture build and memory-lifecycle regressions described below.

## Fable5 cross-check

Fable5's first recommendation, removing anonymous demand-fault VMA splitting
and global coalescing, does not apply to the current source. The production
fault path already inserts sparse resident runs with `insert_resident_run()`
and publishes only the new PTE window with `map_area_page_window()`. The new
1,800-second marker-window evidence records `anonymous_coalesce calls=0`.

The retained fixed per-frame `AtomicUsize` reference table remains supported
as a startup optimization. Same-host production evidence moved the first real
`Compiling` event from 171.010 seconds in the isolated control to 49.259
seconds, while both control and candidate still reached 23 crates in 300
seconds. It is not evidence of full-build completion.

## 1,800-second diagnostic window

Remote evidence:

`/srv/buildstorm/worktrees/wll_os-exec-drop-diag-20260812/docs/evidence/buildstorm-stage2/20260813-riscv64-diagnostics-smp8-frame-refcount-late-boundary-window1800`

The runner used RISC-V64, 8 vCPUs, 8 GiB, the unmodified official image, and
`buildstorm-diagnostics,smp-regression`. `BUILDSTORM_BEGIN mode=multi` was
followed for 1,800.626 seconds. There were 33 real `Compiling` lines; the last
was `rustc-literal-escaper v0.0.7` at 881.595 seconds after the marker. No
`BUILDSTORM_COMPILE` result, panic, OOM, heap allocation error, or page-fault
failure occurred.

Marker snapshot 6 to final snapshot 185 deltas:

| Metric | Delta |
| --- | ---: |
| anonymous coalesce calls/time | 0 / 0 us |
| exec old resident drop | 86 calls / 257,542 us |
| frame allocator wait | 109,501 us |
| heap lock acquire/hold/wait | 177,090,567 / 1,051,303,359 us / 832,806,111 us |
| MemorySet activation wait/hold | 3,057,305,318 us / 10,068,794 us |
| hardware page-fault hold | 150,657,071 us |
| anonymous demand faults/pages | 751,165 / 965,471 |
| remote shootdowns/targets | 448,845 / 2,360,894 |

The late interval from snapshots 81 to 88 contained no fork, exec, or resident
drop. Eight rustc workers continued running while heap hold time increased by
56.96 seconds and MemorySet activation wait increased by 126.98 seconds. This
falsifies synchronous exec/resident destruction as the direct late-boundary
trigger after the reference-table change. It does not prove that heap or
activation locking is the unique production bottleneck because diagnostics
instrumentation materially perturbs those paths.

## Rejected ready-queue candidate

A single production hypothesis was tested: replace the ready queue's mirrored
`BTreeSet<usize>` membership set with lock-protected linear deduplication over
the `VecDeque`. The maximum observed diagnostic runqueue length was 11, so the
candidate preserved PID uniqueness, FIFO/front order, priority selection,
affinity, and task-state cleanup while removing per-enqueue tree-node
allocation.

Before performance testing, the candidate passed all four release builds:
RISC-V64 production, RISC-V64 diagnostics+SMP, LoongArch64 production, and
LoongArch64 diagnostics+SMP. Both architectures then passed resident-memory
lifecycle, user-memory lifecycle, and the final 8-CPU SMP regression. These
are `capability-pass`, not official BuildStorm score evidence.

Same-host production A/B result:

| Metric | Refcount baseline | Ready-queue candidate | Change |
| --- | ---: | ---: | ---: |
| real crates at 300 s | 23 | 23 | 0% |
| first crate | 49.259 s | 49.460 s | -0.41% |
| 23rd crate | 57.671 s | 57.471 s | +0.35% |

The result was below the predeclared 5% retention threshold. The
`os/src/task/manager.rs` change was therefore fully reverted locally and on the
remote; both copies match the original blob
`17d910ab0590b143b06a15495d85da407f47487b`.

## Current decision

Keep the per-frame reference table as a measured startup optimization. Do not
reimplement Fable5's already-present sparse fault path, do not retain the
ready-queue experiment, and do not claim BuildStorm completion. The next
production candidate remains unselected: the residual heap/MM/TLB costs need a
smaller diagnostics-only attribution experiment that separates real allocator
traffic from instrumentation overhead without changing multiple subsystems.
