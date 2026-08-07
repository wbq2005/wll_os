# BuildStorm rejected and blocked candidate summary

Date: 2026-08-07

## Evidence boundary

The retained production baseline contains range-local `mprotect` processing.
Its comparable feature-off timeline reached the first timed crate at
`113.13476717598678` seconds and the 23rd `Compiling` event at
`123.74908572799177` seconds. Candidates below were tested one at a time on
the unmodified official image and script with `-snapshot -m 8G -smp 8` and
diagnostics disabled unless explicitly described as diagnostic-only.

No run in this table emitted `BUILDSTORM_COMPILE mode=multi ok=true`. A
candidate with less than 5% progress improvement was reverted immediately.

## Production candidates

| Candidate | Comparable result | Decision and evidence |
| --- | --- | --- |
| Anonymous fault window 8 to 16 pages | Same 23 events in 300 seconds; 0% coarse improvement | Reverted. `20260806-riscv64-production-smp8-anonymous-fault-window16-window300/` |
| Local anonymous VMA coalescing before the retained mprotect baseline | Same 23 events; 0% coarse improvement | Reverted. `20260806-riscv64-production-smp8-anonymous-local-coalesce-window300/` |
| Foreground empty-ready idle wait | Same 23 events; 0% | Reverted. `20260806-riscv64-production-smp8-foreground-idle-wait-window300/` |
| Shared pipe OFD nonblocking state | Same 23 events; 0% | Reverted as a BuildStorm performance candidate. The ABI issue remains separate correctness work. `20260806-riscv64-production-smp8-pipe-ofd-nonblock-window300/` |
| Normal boundary in blocked-owner continuation | Same 23 events; 0% | Reverted. `20260806-riscv64-production-smp8-blocked-owner-normal-boundary-window300/` |
| Anonymous local coalescing on the retained mprotect baseline | First timed crate 9.03% slower; 23rd event 7.28% slower | Reverted. `20260807-riscv64-production-smp8-mprotect-anonymous-local-coalesce-window300/` |
| Adjacent resident VMA extension | First timed crate 8.85% slower; 23rd event 8.25% slower | Reverted. `20260807-riscv64-production-smp8-mprotect-anonymous-adjacent-extend-window300/` |
| Left-side zero-move subset | First timed crate 3.00% slower; 23rd event 2.26% slower | Reverted. `20260807-riscv64-production-smp8-mprotect-anonymous-left-zero-move-window300/` |
| `VecDeque<MapArea>` plus local coalescing | First timed crate 9.56% slower; 23rd event 8.10% slower | Reverted. `20260807-riscv64-production-smp8-mprotect-vma-vecdeque-local-coalesce-window300/` |
| Batch global coalescing every 64 anonymous installs | First timed crate 10.27% slower; 23rd event 8.90% slower | Reverted. `20260807-riscv64-production-smp8-anonymous-coalesce-batch64-window300/` |
| Encapsulated bounded vacant VMA slots | First timed crate 10.44% slower; 23rd event 9.22% slower | Reverted. `20260807-riscv64-production-smp8-vma-vacant-slots-window300/` |

The rejected active-root identity, CPU-locality, and lockless-activation
experiments predating this table are documented in sections 20 through 24 of
`docs/buildstorm-kernel-design-optimization.md`. The ambiguous active-root
fast path caused an official-image `SIGILL` and was reverted; the exact-root
identity check remains retained.

## Diagnostic findings that did not authorize production retention

- Cargo observed all eight CPUs and jobserver/pipe traffic made progress.
  Maximum sampled rustc concurrency was three live processes but only one
  runnable process; compiler dependency serialization versus single-thread
  compiler work remains unresolved.
- Wake-to-run delays were small enough that the tested scheduler boundary
  changes produced no progress improvement.
- Anonymous coalescer scan/rebuild consumed 59,834,257 of 62,364,765 measured
  microseconds (95.95%), but removing it inside contiguous-vector variants did
  not improve feature-off production.
- The vacant-slot diagnostic reduced anonymous coalesce time by 94.73% and
  MemorySet lock wait by 14.13%, yet only 32 of 194,622 splits reused a vacant
  slot. Production remained 9--10% slower because middle insertion and
  physical-slot/search costs remained.
- Host evidence showed no swap, no sustained I/O saturation, and all eight
  QEMU TCG threads active. Host environment is not the current blocker.

## Closed and blocked directions

The following directions are closed unless new, independent evidence changes
their mechanism:

1. Increasing the anonymous fault window without changing VMA boundaries.
2. Another local `Vec::remove`/adjacent-extension special case.
3. Another batching, vacancy-density, or tombstone threshold over contiguous
   `Vec<MapArea>`.
4. Another scheduler idle/boundary tweak without evidence of delayed runnable
   work.
5. Treating pipe OFD propagation as the first BuildStorm throughput hotspot.

The remaining representation-level direction is blocked on a full design and
correctness audit: a sparse resident-page representation must preserve frame
ownership, anonymous and file offsets, shared mappings, COW, mprotect/munmap,
fork/clone, writeback, page-table shootdown, and release semantics while
avoiding both middle VMA insertion and whole-vector reconstruction. No such
production candidate is currently retained or authorized.

Detailed raw attribution, hashes, host metrics, and official judge output are
indexed by `20260806-attribution-report.md` and `20260807-global-vma-audit.md`.
