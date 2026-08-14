# BuildStorm residual compound-hotspot audit

Date: 2026-08-13 (Asia/Shanghai)

Decision status: **audit complete; implementation not started**.

BuildStorm status remains `unverified / not completed`.  No evidence reviewed
here contains the exact successful marker
`BUILDSTORM_COMPILE mode=multi ok=true`.  This report does not claim a score or
a complete compile.  No production source was changed and no QEMU instance was
started while producing it.

## 1. Executive decision

The current residual hotspot is best modeled as a **compound memory-management
feedback loop**, not as one slow function:

```text
glibc/LLVM worker asks for a large lazy anonymous reservation (up to 128 MiB)
  -> the approximately 1.5-GiB low mmap arena has no contiguous hole that large
  -> mmap returns ENOMEM and the allocator retries; successful arenas are trimmed
  -> mmap select/commit + munmap topology work repeats millions of times
  -> Vec<MapArea>, ResidentSet release, PTE edits and remote TLB commits amplify it
  -> all threads in one rustc address space serialize on one MemorySet mutex
  -> activation/fault callers wait or spin behind topology work
  -> diagnostic heap traffic is amplified around the same path
  -> useful LLVM progress continues, but dependency completion is extremely slow
```

The measured operation counts make the large-reservation retry loop the
strongest upstream cause.  The exact one-to-one allocator protocol and the
amount of guest CPU lost to `MemorySet` spinning are still `unverified`.
Therefore the next change is a **small diagnostics-only attribution patch**,
not another production constant change.  If that patch confirms the sequence,
the first production architecture stage is `UserVaLayout` plus explicit root
ownership and lazy multi-arena placement, immediately followed by splitting
mutable topology from activation identity.  `VmaMap` and transactional PTE/TLB
work belong to the same target design, but each production A/B must falsify one
layer at a time.

This is the answer to the earlier "could both be needed?" question: **yes**.
Address-space capacity is the upstream trigger; lock, VMA, PTE/TLB and allocator
costs are downstream multipliers.  The final architecture is combined, while
the validation remains staged.

## 2. Source, worktree and evidence identity

### 2.1 Audited worktree

| Property | Audited value |
| --- | --- |
| Worktree | `C:\Users\22478\.codex\worktrees\618f\wll_os-master1` |
| Git state | detached HEAD |
| HEAD | `055df99ff554441f7699c3518b1a4b204bd9c265` |
| Original checkout | `D:\wll_os-master1`, branch `wbq_final_buildstorm_compile` |
| Status entries | 587: 22 tracked modified, 6 tracked deleted, 559 untracked |
| Full tracked binary diff blob | `2d0c3b0d48951f6a3feace9804011d590aa1bec9` |
| `os` + `patch/polyhal` diff blob | `ab143027adb8ec250b8334d25b3d8e79649fe23d` |
| Three-file refcount diff blob | `42f81b2ee3ce516b998e882722b75a5b374f06d1` |
| `os/src` Rust files | 51 |
| `os/src` manifest SHA-256 | `4fd87436f7459c9ca03ac59f38bcc9a14b38592a00d10c64cfb28e0b00891197` |
| `memory_set.rs` SHA-256 | `262ca680e16e3679533a3316a95e9e938afc13ab6a49b0139d22579fa5fb8495` |
| PolyHAL pagetable mod SHA-256 | `765a59bcc14fdedac92203434c57f93484ebe1b02b8b51ec5a3dac651031dc8f` |

The manifest sorts forward-slash paths ordinally, records lowercase SHA-256,
two spaces, relative path and LF, then hashes the UTF-8 records.  These values
supersede the older identity table in
`20260813-current-global-hotspot-architecture-audit.md`, which was captured
before additional user changes.  Deleted plans/DOCX files, candidate copies,
patch staging directories and evidence directories are user state and were not
restored, cleaned or folded into a production candidate.

The dirty worktree is not one patch.  At minimum it contains:

1. the isolated per-frame `AtomicUsize` candidate in `main.rs`,
   `frame_allocator.rs` and `mm/mod.rs`;
2. sparse `ResidentSet`, lifecycle and diagnostics changes;
3. PolyHAL root/ASID/TLB work;
4. runner and evidence changes;
5. an untracked `os/src/config/user_va.rs` plus tracked callers.

The last group is important.  It defines a high arena and non-contiguous root
ownership concepts, but currently sets `MMAP_ARENAS` to the low arena only,
`HAS_HIGH_MMAP_ARENA=false`, and RISC-V ownership to root entries 0..1.  It is
**compileable scaffolding, not an enabled high-arena candidate**.  Describing
the current tree as either "high arena active" or "high arena fully reverted"
would both be wrong.

### 2.2 Tool and authority ledger

- The official `final-2026` branch was rechecked at
  `b5ec6ef8497e1818cbdec3b54bb722f036e57972`.
- `cargo metadata --no-deps --format-version 1` finds one workspace package,
  `wll_OS`, with `riscv`, `loongarch`, `buildstorm-diagnostics` and
  `smp-regression` features.
- Four current-tree release cfg builds passed locally with pinned
  `nightly-2025-01-18`: RISC-V production, RISC-V diagnostics+SMP, LoongArch
  production, and LoongArch diagnostics+SMP.  These are `capability-pass` build
  evidence, not runtime or official completion evidence.
- `rust-analyzer` was installed into that pinned toolchain with user approval.
  `analysis-stats` loaded the project database, then the analyzer itself
  panicked in `CompressedFileTextQuery(FileId(277))`.  This was not a kernel
  compiler error and wrote no worktree file.  No semantic-analysis pass is
  claimed; call sites and cfg edges were cross-checked with current source,
  Cargo builds and `rg`.
- Cargo warns that profile, patch and target-rustflags tables in
  `os/Cargo.toml` are ignored at workspace scope.  Root patches and `build.rs`
  currently supply the effective dependencies/linker script.  This warning is
  an acceptance risk for any page-table refactor.
- Read-only remote inspection of `root@47.110.253.40` found no real QEMU or
  BuildStorm runner.  The query shell was the only process-name match.  The
  remote source was not used as the audited identity.

### 2.3 Strongest raw run

The primary evidence is
`20260813-riscv64-diagnostics-smp8-frame-refcount-late-boundary-window1800/`:

- official RISC-V image SHA-256
  `d74e436522f5946ca17280a7a25f17dbb6604b71fe675bb8a021ce8e849b334c`;
- QEMU 11.0.3, `-snapshot -m 8G -smp 8`;
- diagnostics and SMP features enabled;
- marker window 1,800.626 seconds;
- 33 real `Compiling` events, last `rustc-literal-escaper v0.0.7`;
- no compile-success marker, panic or OOM.

The run predates the current `UserVaLayout` scaffolding and has its own source
diff SHA-256 in `launch.json`; it is evidence about the refcount+sparse+
diagnostics state, not proof about every later dirty file.

## 3. Corrections to Fable5 and earlier conclusions

Fable5's ownership inventory was useful, but its first-ranked production
candidate is stale:

- anonymous demand faults already insert sparse resident runs;
- they map only the new page window;
- they do not split/coalesce the VMA;
- hardware faults use `buildstorm_memory_set_lock!` in the current tree;
- the long run records `anonymous_coalesce calls=0`.

Reimplementing "remove anonymous-fault VMA splitting" would repeat completed
work and cannot explain the residual boundary.

The following standalone experiments are also closed as complete explanations:

| Experiment | Result and interpretation |
| --- | --- |
| frame ref table | first/23rd startup moved strongly, but both arms were 23 crates at 300 s; retain isolated as startup-only provisional patch |
| high mmap window / low-high VmaMap combinations | 23/23 at 300 s; old tests did not cross the late ENOMEM interval and do not validate retention |
| refcount + PTE/TLB transaction | 23/23; first/23rd earlier, so batching is useful architecture but not the complete boundary |
| activation token | 23/23 and about 9% slower; rejected cached-root style must not be revived |
| ready-queue linear dedup | 23/23; reverted |
| munmap range drain | 23/23; insufficient alone |

The correct conclusion is not that each mechanism is worthless.  It is that
no one of them, tested alone at the coarse 300-second crate boundary, removed
the compound late loop.

## 4. Phase-aligned progress and counter recomputation

`Compiling` marks process/dependency dispatch, not crate completion.  The long
run shows stair-step progress:

| Event | Seconds after marker |
| --- | ---: |
| first two crates | 64.085 |
| 23rd, `ax-posix-api` | 73.098 |
| 24th | 434.959 |
| next wave | 437.163, 550.323, 551.925, 554.128 |
| later wave | 575.359, 591.781, 617.619, 620.824 |
| 33rd | 881.595 |

Thus 23/23 at 300 seconds did not mean zero progress.  It meant the initially
dispatched rustc/LLVM jobs had not completed enough dependencies to unlock the
next wave.

Recomputation from `guest-aggregates.json`, not transcription from an older
report:

| Counter | snapshot 6 -> 185, 1,791.801 s | snapshot 81 -> 88, 70.088 s | snapshot 94 -> 185, 911.020 s |
| --- | ---: | ---: | ---: |
| mmap work calls | 2,283,912 | 88,498 | 1,331,766 |
| initial select success / ENOMEM | 834,695 / 1,449,285 | 31,863 / 56,625 | 483,384 / 848,359 |
| anonymous lazy success | 833,522 / 834,110 lazy total | 31,864 | 483,384 |
| munmap calls | 702,180 | 26,685 | 414,083 |
| store faults | 770,364 | 18,401 | 261,146 |
| PTE writes | 1,275,840 | 42,880 | 728,039 |
| remote shootdowns / targets | 448,845 / 2,360,894 | 16,364 / 88,990 | 258,166 / 1,375,405 |
| anonymous coalesces | 0 | 0 | 0 |

Every failed initial selection is in the diagnostic bucket above 16 MiB and at
most 256 MiB.  Maximum failed length is 128 MiB; maximum sampled free gap is
85.37 MiB; maximum observed VMA count is 2,305.  The bucket cannot prove every
failed request was exactly 128 MiB.

In snapshot 94 -> 185, the large-request bucket grows by 1,065,394.  Subtracting
848,359 failed initial selections gives an approximate 217,035 initial large
successes.  `414,083 / 217,035 = 1.908` munmaps per successful large request.
That is strong evidence for the common glibc large-alignment protocol (reserve,
then trim up to two sides), but it is not yet a per-call proof.

The same late interval records:

| `MemorySet` site | Wait | Hold |
| --- | ---: | ---: |
| activation | 1,828.805 s | 5.753 s |
| mmap select | 908.971 s | 12.489 s |
| mmap commit | 280.594 s | 265.762 s |
| munmap | 214.516 s | 503.366 s |
| hardware fault | 182.911 s | 67.066 s |

Wait/hold are summed task/CPU time and may exceed wall time.  They are not wall
percentages.  No exec/fork/openat/statx/readlink work was added in this late
interval.  Frame-allocator wait was only 4.83 ms.  VFS, virtio and block-cache
wait did not increase; task-manager wait rose only 2.036 ms.

Futex blocks rose by 1,748 with 3,040.378 seconds of summed blocked time, but
snapshot 185 still has eight rustc tasks runnable.  TGID 227 has six
Running/Ready heavy workers plus another newly running worker, and TGID 503 has
one running worker.  Saved host evidence showed QEMU near 797% CPU with eight
TCG threads near 78.5%-99.1%, no swap, no host iowait and no storage saturation.
Futex coordination is present, but it cannot explain away concurrent runnable
work or the measured MM retry counts.  Busy TCG threads also do not prove useful
guest progress; guest spinlock contention consumes TCG CPU.

## 5. Four cfg-specific semantic graphs

### 5.1 RISC-V64 production

```text
sys_mmap
  -> UserVaLayout low arena (currently 0x2000_0000..USER_STACK_TOP)
  -> find_free_area over Vec<MapArea>
  -> lazy MapArea insertion / later munmap drain

page fault
  -> MemorySet mutex (HardwarePageFault)
  -> ResidentSet relative-VPN run insertion
  -> PageTableWrapper::map_page -> local sfence.vma

fork/COW, mprotect, munmap
  -> publish/revoke leaves, often per-page local invalidation
  -> platform::tlb_shootdown(root)
  -> synchronous SBI remote_sfence_vma for CPUs publishing that root

user entry
  -> same MemorySet mutex to read root/ASID
  -> root/ASID activation or retained translation
```

The boot root contains valid 1-GiB low identity leaves in entries 0..255 and
global high aliases in 256..511.  With an 8-GiB guest, live low physical
identity mappings occupy at least entries 2..9; the exact available RAM/DTB
layout must be captured per run.  Current process roots own only entries 0..1
and inherit 2..511.  A high arena at `0x20_0000_0000..0x40_0000_0000`
corresponds to root entries 128..255, so enabling it requires a non-contiguous
ownership predicate used identically by `alloc_new/restore`, mapping
validation, release/drop and fixed-map checks.  Merely adding the range to
`MMAP_ARENAS` would overwrite inherited supervisor leaves and is forbidden.

### 5.2 RISC-V64 diagnostics

The production graph is unchanged semantically.  Aggregate work, mmap, VMA,
PTE/TLB and lock accounting is feature gated.  `DiagnosticLockedHeap` is a
material perturbation: every allocation/free performs `try_lock`, possibly
`lock`, reads timers, updates shared atomic counters, and updates hold counters
before releasing the heap mutex.  Therefore 177,090,567 acquisitions are a
valid observed event count, but 832.806 seconds wait and 1,051.303 seconds hold
cannot be projected to production timing.

### 5.3 LoongArch64 production

```text
shared mmap/fault/VMA/ResidentSet graph
  -> user root entries 0..255; inherit kernel entries 256..511

kernel RAM/MMIO/low boot access
  -> PLV0-only DMW1 cached / DMW0 uncached / DMW2 low identity

user entry
  -> PGDL + ASID publication and current conservative activation invalidation

remote edit commit
  -> active-root filter
  -> IOCSR IPI generation/ack under shootdown serialization
```

The entry assembly programs DMW0/1/2 with PLV0 enabled, not PLV3, so these
windows must not expose a user arena.  Current lower-half root ownership can
represent the proposed positive high arena, but static constants are not a
proof of real PLV3 canonical behavior.  A high user fault, COW, partial unmap,
exec/drop and SMP TLB regression on LoongArch are mandatory before acceptance.

### 5.4 LoongArch64 diagnostics

The same feature-gated instrumentation runs.  The architecture additionally
uses conservative local invalidation around activation and synchronous remote
IPI acknowledgement.  This motivates a transaction layer, but no current
LoongArch runtime run proves the same cost distribution as RISC-V; transferring
RISC-V percentages would be invalid.

## 6. Lifecycle, ownership and lock audit

### 6.1 Required ownership boundaries

- `VmaMap` owns sorted, non-overlapping policy intervals, gap lookup,
  split/merge, mmap/munmap/mprotect/brk policy and backing metadata.  It does
  not own frames or TLB state.
- `ResidentSet` is the sole owner of resident frame references keyed by
  VMA-relative VPN.  Demand residency must not mutate policy topology.  Split
  and merge translate keys exactly once.
- `PageTableOps` publishes, protects, replaces and removes mappings.  It does
  not decide VMA placement and does not free data owners.
- `TlbProtocol` orders PTE visibility, local/remote invalidation and
  acknowledgement.  A replaced/revoked frame or page-table node remains owned
  until the required acknowledgement completes.

### 6.2 Critical lifecycles

| Path | Required order and current risk |
| --- | --- |
| anonymous fault | VMA lookup -> frame allocation -> resident insertion -> known-absent PTE publication; current sparse ownership is correct, but work is under the global address-space mutex |
| file fault/shared mapping | backing read/cache owner -> ResidentSet state -> PTE; shared dirty data must be collected/written back before topology removal |
| COW store | allocate/copy -> replace resident owner -> publish replacement -> invalidate/ack -> drop old owner; current path can issue two shootdowns |
| fork | parent policy/resident traversal -> mark every newly shared private leaf COW -> republish parent read-only -> clone child owners -> publish child; `CLONE_VM` instead shares the same `Arc<Mutex<MemorySet>>` |
| exec | switch to stable kernel root -> replace address-space owner -> retire ASID -> release user page-table branches once -> drop resident owners |
| exit | last `Arc` owner triggers `MemorySet` destruction; threads sharing `CLONE_VM` defer destruction until the final owner |
| munmap/mprotect | split policy boundaries -> collect writeback -> revoke/protect leaves -> commit TLB -> release residents; current vector drain and implicit drops happen under broad locking |

The current fixed frame-reference table uses relaxed increments, release
decrements and an acquire fence on the last owner.  `into_raw_ppn()` requires a
unique owner.  For 8 GiB / 4 KiB pages, a single 8-byte counter per managed
page is at most 16 MiB before excluding kernel/reserved gaps; actual footprint
is the sum of managed regions plus vector metadata.  Region lookup is linear in
the small region count; counters sharing a cacheline can still contend.  The
saved late interval shows frame allocator wait is not the present bottleneck,
so further tuning is unjustified now.

### 6.3 Locking and destruction architecture

Current ownership is effectively:

```text
task state (must not nest over MemorySet)
  -> MemorySet mutex
       -> VMA Vec / ResidentSet mutations
       -> page-table edits / synchronous TLB commit
       -> FRAME_ALLOCATOR and heap allocations/frees
       -> implicit resident/container destruction
```

The target writer order is:

```text
VMA topology writer
  -> resident-region lock
  -> page-table edit lock
  -> TLB publish/commit/ack
  -> bounded retirement queue
```

Activation reads an immutable `AddressSpaceIdentity` and does not take the VMA
writer lock.  No large implicit frame/heap destructor may run while holding the
topology writer.  Deferred retirement must be bounded with backpressure; it
cannot turn exec latency into unbounded memory retention.

## 7. Facts, inference, alternatives and unknowns

| Class | Conclusion |
| --- | --- |
| verified | current fault path is sparse and has zero anonymous coalesces |
| verified | late mmap/ENOMEM/munmap/PTE/TLB counts and MemorySet waits are active while exec/VFS/I/O deltas are absent or negligible |
| verified | rustc makes CPU progress and later crates unlock; 23/33 are stair-step signals, not a deadlock |
| verified | high arena is currently disabled despite present scaffolding |
| strong inference | large allocator arenas repeatedly fail for VA contiguity, and successful aligned reservations commonly cause two trims |
| strong inference | shared MemorySet locking converts topology churn into an SMP convoy and TCG-visible guest spinning |
| alternative | user allocator has a leak/bug independent of VA capacity; falsified if per-TGID successful high reservations eliminate retries and trims without changing allocator binary |
| alternative | LLVM is intrinsically slow under TCG; still a baseline cost, but it does not explain millions of avoidable ENOMEM/topology operations |
| alternative | futex/jobserver dependency waits dominate; possible for some workers, but eight runnable rustc tasks and active MM churn rule it out as the sole cause |
| unknown | exact per-TGID mapping of attempt -> success/ENOMEM -> trim1/trim2 |
| unknown | production heap size-class/domain distribution and the amount of diagnostics-induced contention |
| unknown | LoongArch runtime magnitude and real PLV3 high-arena behavior |
| unknown | end-to-end gain after removing the retry loop; only production A/B can establish it |

## 8. Ranked residual hotspots

1. **Large anonymous reservation failure/retry.** Highest upstream operation
   count and clearest falsification; likely trigger of the feedback loop.
2. **Broad `MemorySet` ownership and lock convoy.** Directly measured wait;
   activation identity, mmap topology and fault residency are wrongly coupled.
3. **`Vec<MapArea>` topology under retry load.** Binary lookup is acceptable,
   but first-fit scans, insert shifts, range drain/rebuild and coalesce are
   O(V) or O(V log V) at a measured V up to 2,305.
4. **PTE/TLB edit granularity and retirement.** 1.276M writes and 0.449M remote
   commits are substantial; standalone batching improved startup but not the
   300-second boundary.
5. **Global heap/metadata allocation domains.** The acquisition count is huge,
   but size and caller attribution are missing and timing is perturbed.
6. **Futex/TCG interaction.** Real secondary effect and useful progress-cost
   indicator, not a sufficiently specific kernel production candidate.
7. **VFS/scheduler/block I/O/frame destruction.** Existing late deltas argue
   strongly against them as the current first cause.

## 9. Target architecture

### Layer A: `UserVaLayout` and root ownership

One architecture-neutral policy describes ELF/brk, stack+guard, ordered mmap
arenas, forbidden ranges, canonical checks and fixed-map validation.  Platform
code supplies process-owned root-entry predicates and DMW/direct-map holes.

RISC-V `restore`, release and fixed mapping must all consume the same
non-contiguous ownership predicate.  LoongArch must retain PLV0-only DMW
ownership.  Large anonymous reservations remain lazy: a 128-MiB reservation
must not allocate 128 MiB of frames/page tables.

### Layer B: gap-augmented `VmaMap`

Use a small no-std augmented balanced tree (or a B-tree whose nodes carry
subtree maximum-gap metadata), not a verbatim Linux Maple Tree port.  Required
operations are containing/predecessor lookup, first-fit from cursor, maximum
gap, split/drain, neighbor merge and ordered fork/drop iteration, normally
O(log V + K).  A chunked vector has lower memory overhead but O(chunks) gap
selection and awkward worst-case mutation at 2,305 fragmented VMAs; it is a
fallback only if measurements show very low mutation after Layer A.

Migration first wraps the existing `Vec` behind `VmaMap`, adds invariant tests,
then replaces storage without changing `MapArea` or `ResidentSet` relative
keys.  Container migration must not change COW/file/writeback/destructor rules.

### Layer C: `PageTableEdit` / `TlbProtocol`

An edit transaction classifies:

- known-absent new map: publish with architecture barrier; no stale leaf to
  invalidate;
- permission reduction: publish, invalidate affected address/range, wait for
  remote acknowledgement before returning to user;
- replacement/COW: retain old frame, publish new leaf, commit once, then drop
  old owner;
- unmap: retain data and empty page-table nodes, revoke leaves, commit/ack, then
  retire both.

RISC-V uses `sfence.vma` plus synchronous SBI remote maintenance for CPUs that
may hold the root/ASID.  LoongArch uses `dbar`, `invtlb` and IPI generation/ack.
ASID reuse requires a completed all-CPU invalidation before publication to a
new root.

### Layer D: `AddressSpaceIdentity`

Publish immutable `{root, asid, generation, lifetime}` behind an `Arc` or
equivalent stable owner.  Task switch loads it without the mutable topology
lock.  PTE commit advances generation only after publication ordering; exec
atomically swaps identities while the old owner remains alive; `CLONE_VM`
shares one identity; retired ASIDs become reusable only after all CPUs have
acknowledged.  This differs from the rejected cached-root token because root
lifetime and edit generation are part of the protocol, not an unchecked
cached scalar.

### Layer E: bounded retirement and allocation domains

Retirement batches owners only after TLB acknowledgement and caps queued pages
per address space/CPU; overflow applies synchronous backpressure.  Only after
low-overhead attribution should allocation specialize by size class/domain.
Measure at least VMA node, ResidentSet/BTree node, page-table page, task/FD,
small Vec backing, frame last-owner and cross-CPU alloc/free.  Those results
select slab/per-CPU cache, bulk buddy return or a different resident layout.

## 10. Diagnostics design: a kernel progress model, not a UI bar

The requested Linux-style "progress bar" should be understood as `/proc`, perf
and tracepoint style observability, not visible official output.  Production
and official markers stay unchanged.  Under `buildstorm-diagnostics` only:

1. assign a per-TGID monotonically increasing generic mmap sequence ID;
2. count large attempt, select success, select ENOMEM, commit success/failure,
   trim1 and trim2 within bounded time/order buckets;
3. sample `MemorySet` owner site and waiter site per CPU, including spin loops;
4. record user/kernel progress, process exit and parent/dependency wake;
5. sample allocator domain, log2 size, alloc CPU and free CPU at 1/1024 or
   1/4096, into fixed per-CPU arrays;
6. aggregate periodically from a reporter; no event-path allocation, serial
   print, shared atomic RMW hot line, crate/path/command/output matching.

This directly separates three models:

- VA-capacity cause: the same TGID shows large ENOMEM loops; enabling a legal
  high arena removes failures, retries and paired trims;
- allocator leak/bug: mappings succeed in ample VA but live reservations and
  retry rate continue growing without paired unmaps;
- lock-only cause: operation counts remain stable while sampled owner hold/spin
  dominates and a topology/identity split improves progress.

## 11. Staged implementation and falsification

| Stage | Single production hypothesis | Direct expected metric | Falsification / rollback |
| --- | --- | --- | --- |
| 0 diagnostics | retry protocol and owner convoy are attributable per TGID | attempt->ENOMEM/success->trim sequences; owner/wait samples; low overhead | no correlation, or diagnostics materially alter crate/timing distribution; revise counters, no production keep |
| 1 VA layout | contiguous VA shortage triggers allocator retry | large ENOMEM approaches zero; mmap/munmap rate collapses; lazy frame usage unchanged | failures disappear but retry/trim and 1,800-s progress do not improve; any root/DMW overlap rolls back immediately |
| 2 topology ownership | Vec/topology serialization is the next residual | select/commit/munmap hold per op and spin samples fall | counts comparable but hold/progress unchanged; any VMA/resident invariant failure rolls back |
| 3 PTE/TLB transaction | fine-grained invalidation/retirement remains material | invalidations per edit and remote commits per syscall fall; no stale mapping | counters fall without progress, or any COW/shared/ASID isolation failure |
| 4 immutable identity | activation waits are a downstream residual | activation takes no topology mutex; wait/spin collapses | stale root/generation, unsafe exec/CLONE_VM lifetime, or no progress gain |
| 5 allocation domains | attributed metadata/frame traffic is final residual | selected size/domain misses and heap acquisitions fall | unmeasured class moves elsewhere, cross-CPU lifetime breaks, or <5% progress |

Every stage runs in this order: diff/compliance check; four release cfg builds;
dual-architecture independent lazy/fixed/partial-unmap/mprotect/fork/COW/nested
COW/`CLONE_VM`/exec/drop/shared/file/ASID lifecycle; SMP TLB isolation; same-host
production 300-second A/B; separate diagnostics A/B; comparable 1,800-second
production A/B; only then one complete official run.

Retention remains: at least 15% same-configuration 300-second progress, or
5%-15% with the first hotspot clearly reduced and no regression; below 5%
rolls back from production.  Since the root loop is late, acceptance also
requires crossing the 33-crate boundary in the 1,800-second production arm.
A prerequisite refactor that misses performance gates stays only in an
isolated architecture branch until a causally separated combined candidate
passes.  QEMU is single-instance after checking runners/processes.

## 12. Answers to the mandatory technical questions

1. **Why did refcount move startup but not 300-second count?** It removed
   per-frame ownership heap allocation/destruction on early exec/fault paths,
   but the next dependency boundary is governed by long-running rustc/LLVM and
   the later mmap retry loop.  A competing explanation is crate-count
   quantization: useful work improved without completing the 24th dependency.
   Per-process CPU/progress counters plus 1,800-second production A/B separate
   these.
2. **Is the 33-crate boundary the same exec destructor?** No direct evidence
   supports that.  Snapshot 94->185 has zero exec/drop events while the retry
   loop persists.  The minimal proof was per-phase exec/drop counters; it
   refuted synchronous exec destruction as the late owner.
3. **Ref-table footprint and cost?** At most about 16 MiB for all 8-GiB/4-KiB
   pages with 8-byte counters, lower after reserved gaps; initialization is
   linear in managed frames and region lookup linear in few memory regions.
   Adjacent counters can share cachelines.  Exact remote DTB region footprint
   should be emitted once at boot, diagnostics only.
4. **What remains in resident drop after Arc removal?** BTreeMap nodes and run
   vectors free through the heap; last frame owners enter the buddy lock.
   Late evidence shows only 4.83 ms frame-lock wait but perturbed heap traffic;
   size/domain sampling is required before choosing which dominates.
5. **Ranking of destruction candidates?** Bounded bulk resident retirement and
   bulk buddy return are plausible after attribution; per-CPU frame queues need
   remote-free/cache bounds; slab ownership needs a dominant size class;
   deferred destruction needs TLB ack and backpressure; a different resident
   layout needs measured BTree/run overhead.  None is the next candidate.
6. **Can destruction be deferred safely?** Yes only after revocation and all
   required TLB acks, with an owned retirement record, bounded queue and
   synchronous backpressure.  It must preserve `CLONE_VM` last-owner semantics
   and never execute implicit drops in an unknown lock context.
7. **How do COW/shared file owners affect locality?** Fork clones counters for
   shared/COW pages; COW replacement creates one last-owner candidate after
   TLB commit; file-clean pages retain cache ownership; shared dirty mappings
   require writeback.  Per-address-space peak resident/drop sampling is still
   missing.
8. **Are VFS/scheduler separate late bottlenecks?** They can become residuals,
   but the audited late interval adds zero VFS/virtio work, negligible ready
   queue wait and active runnable rustc tasks.  The remaining gap is a
   production post-MM profile, not evidence to optimize them now.
9. **Which minimal counters distinguish phases?** Per-TGID mmap sequence/trim,
   per-CPU sampled MemorySet owner/wait, generic process CPU/progress/exit,
   dependency wake, and sampled allocator domain/size.  Compare 0-300,
   300-1,800 and the 33-crate interval separately.
10. **Is a broad refactor justified?** Yes as the target design because
    capacity, topology, identity, PTE/TLB and retirement responsibilities are
    currently coupled.  It is not justification for one unmeasurable mega
    patch: each layer has an explicit causal metric and rollback gate.

## 13. Literature and transfer limits

- Linux `Documentation/mm/process_addrs.rst` and Maple Tree: topology locking,
  gap search and VMA iteration invariants.  Applicable concepts: separate VMA
  policy from page residency and augment gaps.  Not transferable: Linux RCU,
  Maple allocation machinery and mature lock hierarchy cannot be copied into
  this small `no_std` kernel unchanged.
- Linux cache/TLB and `asm-generic/tlb.h` (`mmu_gather`): batch page-table edits
  and delay frees until invalidation.  Applicable invariant: publish, flush,
  acknowledge, retire.  Not transferable: Linux arch hooks and scheduler/RCU
  guarantees; RISC-V SBI and LoongArch IOCSR transports remain platform code.
- Linux `mm/page_alloc.c` per-CPU page lists: reduce global buddy traffic.
  Applicable only if attributed last-owner frees dominate.  Linux NUMA,
  watermarks, reclaim and memory hotplug assumptions do not exist here.
- glibc malloc arena/heap implementation (`malloc/arena.c`): aligned large
  reservations can be trimmed with leading/trailing `munmap`.  The 1.908 ratio
  motivates sequence counters; it does not prove the exact glibc branch from
  aggregate buckets alone.
- RISC-V privileged specification (`satp`, `SFENCE.VMA`) and LoongArch
  architecture manuals (`PGDL`, ASID, DMW, `invtlb`): define canonical ranges,
  privilege and invalidation ordering.  QEMU behavior alone cannot substitute
  for a later board gate.

## 14. Final recommendation and disproof criteria

Implement Stage 0 diagnostics next.  If it proves the allocator sequence, the
first production implementation is the already-scaffolded `UserVaLayout`
completed with lazy high/low arenas and explicit per-architecture root
ownership.  Do not treat a constants-only high window as complete: its safety
depends on PolyHAL restore/release/fixed-map ownership, and its performance
depends on the subsequent topology/identity split.

The recommendation is overturned if any of the following occurs:

- per-TGID data shows large ENOMEM is not produced by active rustc allocator
  address spaces or is not followed by retries/trims;
- a safe high arena removes ENOMEM but mmap/munmap rate and 1,800-second
  production progress remain unchanged;
- owner sampling shows another lock/work domain dominates before mmap topology;
- LoongArch PLV3 or RISC-V live root ownership cannot represent the arena
  without shadowing platform mappings;
- the combined architecture cannot preserve resident/COW/file/TLB retirement
  invariants under dual-architecture lifecycle and SMP tests.

Until a comparable production run emits the exact successful compile marker,
the only defensible status is `unverified / not completed`.
