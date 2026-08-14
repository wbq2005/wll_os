# BuildStorm current global hotspot and MM architecture audit

Date: 2026-08-13 (Asia/Shanghai)

Evidence status: `unverified` for complete BuildStorm.  The repository contains
no saved exact successful `BUILDSTORM_COMPILE mode=multi ok=true` result for the
current source.  Static builds and independent memory-lifecycle runs are useful
engineering evidence only; they do not establish a new official score.

This document is an audit and architecture decision.  It adds no production
kernel change and was prepared without starting QEMU.

## 1. Audited source identity

The source reviewed here is the Codex worktree, not the similarly named branch
checkout or the stale remote clone:

| Property | Audited value |
| --- | --- |
| Worktree | `C:\Users\22478\.codex\worktrees\618f\wll_os-master1` |
| Git state | detached HEAD |
| HEAD | `055df99ff554441f7699c3518b1a4b204bd9c265` |
| Branch owner elsewhere | `D:\wll_os-master1`, branch `wbq_final_buildstorm_compile`, same HEAD |
| Tracked changes | 21 paths: 15 modified and 6 deleted |
| Untracked paths | 552 at audit input; 554 after adding this report and the independent re-audit prompt |
| Full tracked binary diff blob | `2300da8f5afb9f539d7b9ea1dfa3ae613a3b7d38` |
| Current 50-file `os/src/**/*.rs` manifest SHA-256 | `d7db7575fd69040221f682358f28f91f4eddef87a07d129b1b9deb19e4b2e7c9` |

The manifest above recursively enumerates all current `os/src` Rust files,
sorts forward-slash relative paths ordinally, writes each record as lowercase
file SHA-256, two spaces, path and LF, then hashes that UTF-8 manifest.  The
earlier draft value `431cfa...` had no recorded reconstruction algorithm and is
superseded.  The previous handoff's 150-file Rust manifest is not the current
`os/src` manifest and must not be reused.  The current grouped dirty patches
are:

| Patch group | Paths | `git diff ... | git hash-object --stdin` |
| --- | --- | --- |
| frame-refcount candidate | `os/src/main.rs`, `os/src/mm/frame_allocator.rs`, `os/src/mm/mod.rs` | `42f81b2ee3ce516b998e882722b75a5b374f06d1` |
| diagnostics/attribution | diagnostics, VFS, heap, MemorySet, mmap and exec paths | `3008faa81d6b661885fa0186fa5084b3d3f73711` |
| harness/runner | three BuildStorm runners plus `os/src/task/harness.rs` | `29c097208b8f9e1fa8c7a7cffe95c4a48e78d897` |
| cumulative tracked docs | the two previously modified cumulative reports | `5b3afa5431c8089e1b149182e4e024d7779e5d90` |

User-deleted plans and the deleted DOCX are unrelated changes and were not
restored.  Untracked candidate copies and evidence directories were not treated
as production source.

The official `final-2026` branch was re-fetched on 2026-08-13 and resolves to
`b5ec6ef8497e1818cbdec3b54bb722f036e57972`.  The new remote
`root@47.110.253.40` had no actual QEMU or BuildStorm runner at the audit check.
`/srv/buildstorm/src/wll_os` is a different dirty source at
`4d6f10506d1bff10bb07e0f569daa86362459566` and is not a runnable copy of this
audit state.

`cargo metadata --no-deps --format-version 1` confirms one workspace package,
`wll_OS`, with separate `riscv`, `loongarch`, `buildstorm-diagnostics`, and
`smp-regression` features.  `rust-analyzer` was unavailable in the pinned local
toolchain, so semantic checks used the current local PolyHAL source,
cfg-specific builds, destructor/trait inspection, and `rg` call-site searches.

## 2. Fable5 result: useful ownership audit, invalid first candidate

The complete attached Fable5 report was read as UTF-8 and checked against this
worktree.  Its first-ranked claim is stale and is rejected:

1. Current anonymous demand fault code calls
   `MapArea::insert_resident_run()` at `memory_set.rs:1050` and
   `map_area_page_window()` at `memory_set.rs:1057`.
2. That path does not call `split_area_at()`, `Vec::insert()`, or
   `coalesce_areas()`.
3. The hardware fault path now uses `buildstorm_memory_set_lock!` at
   `trap/mod.rs:343`; Fable5's claimed direct-lock instrumentation blind spot is
   also stale.
4. The current 1,800-second diagnostic interval records
   `anonymous_coalesce calls=0`.

Consequently the recommended "remove anonymous-fault VMA splitting" patch is
already present in the audited source and cannot explain the current late
boundary.  The older 402-second anonymous coalescing result belongs to an older
source state.  Reimplementing it would be duplicate work and would not test a
new hypothesis.

Fable5 remains useful for its frame/resident ownership inventory and for
identifying the old per-frame `Arc` destruction cost.  It does not establish the
current global hotspot because it did not reconcile its graph with the current
dirty source or the later mmap-failure counters.

## 3. Current raw evidence

The strongest current run is:

`docs/evidence/buildstorm-stage2/20260813-riscv64-diagnostics-smp8-frame-refcount-late-boundary-window1800/`

It used the unchanged RISC-V official image, QEMU 11.0.3,
`-snapshot -m 8G -smp 8`, and the frame-refcount candidate plus feature-gated
diagnostics.  It reached 33 `Compiling` events and stopped at
`rustc-literal-escaper v0.0.7`; it emitted no successful compile marker.  The
following values are counter differences, not final cumulative counters.

### 3.1 Broad marker window: snapshot 6 to 185

Actual interval: 1,791.801 seconds.

| Signal | Delta |
| --- | ---: |
| `mmap` work calls | 2,283,912 |
| initial mmap selection success | 834,695 |
| initial mmap selection `ENOMEM` | 1,449,285 |
| commit reselect success / `ENOMEM` | 67,196 / 108,359 |
| successful anonymous mmap shape | 833,522 |
| `munmap` calls | 702,180 |
| hardware store/load/exec faults | 770,364 / 15,479 / 30,507 |
| user-entry activation lock acquisitions | 4,310,694 |
| user-entry activation wait / hold | 3,057.305 s / 10.069 s |
| global heap acquisitions | 177,090,567 |
| global heap wait / hold | 832.806 s / 1,051.303 s |
| frame-allocator wait / hold | 0.110 s / 14.654 s |
| PTE writes | 1,275,840 |
| local invalidations | 636,242 |
| remote shootdowns / target CPUs | 448,845 / 2,360,894 |
| exec-replace MemorySet hold | 0.278 s |
| anonymous coalesces | 0 |

Every initial selection failure is in the diagnostic bucket above 16 MiB and
at most 256 MiB.  The maximum failed request is 128 MiB.  Rate-limited scans
observed a maximum free gap of 89,518,080 bytes (85.37 MiB) and a maximum of
2,305 VMAs.  The bucket does not prove that every failure was exactly 128 MiB,
but it proves that the failures are large reservations and that sampled
128-MiB requests cannot fit.

Wait and hold totals are summed CPU/task time.  They may exceed wall time and
must not be divided by 1,791.801 seconds and presented as a wall-time
percentage.

### 3.2 Late interval: snapshot 81 to 88

Actual interval: 70.088 seconds.  No exec replacement occurred.

| Signal | Delta |
| --- | ---: |
| `mmap` calls | 88,498 |
| initial selection success / `ENOMEM` | 31,863 / 56,625 |
| successful anonymous mmap shape | 31,864 |
| `munmap` calls | 26,685 |
| store faults | 18,401 |
| MemorySet activation wait / hold | 126.979 s / 0.363 s |
| mmap-select wait / hold | 64.414 s / 0.647 s |
| mmap-commit wait / hold | 19.138 s / 20.082 s |
| munmap wait / hold | 14.618 s / 38.392 s |
| hardware-fault wait / hold | 12.989 s / 5.323 s |
| heap acquisitions / wait / hold | 3,287,413 / 59.439 s / 56.955 s |
| remote shootdowns / target CPUs | 16,364 / 88,990 |

This interval directly rejects exec destruction as the late-boundary owner.
It also rejects anonymous VMA coalescing.  mmap/munmap topology work and the
allocation retries are active while activation and fault callers queue behind
the same `MemorySet` mutex.

## 4. Current causal model and hotspot ranking

The strongest falsifiable model is:

```text
MMAP_BASE=0x2000_0000, USER_STACK_TOP approximately 0x8000_0000
  -> only approximately 1.5 GiB of general mmap VA
  -> large (up to 128 MiB) lazy arena reservations stop fitting
  -> user allocator retries mmap and trims/unmaps arenas
  -> 2.28M mmap + 0.70M munmap calls in 29.9 minutes
  -> VMA scans/vector rebuilds + resident release
  -> 1.28M PTE writes + 0.45M remote TLB commits
  -> MemorySet lock convoy on every shared address space
  -> 4.31M user-entry identity reads take the same mutable lock
  -> global heap traffic is amplified
  -> compile progress reaches but does not cross the observed 33-crate boundary
```

This model is `unverified`, not a proven unique root cause.  It has stronger
upstream evidence than the competing local-lock explanations because the
failure/retry operation count is measured before the downstream lock waits.

The current ranking is:

1. **User virtual-address capacity and placement policy.**  The immediate,
   measured failure is inability to reserve a large contiguous lazy range.
2. **VMA topology mutation under `MemorySet`.**  Lookups use binary search, but
   `find_free_area()` scans forward, insertion shifts a `Vec`, `unmap_range()`
   drains/rebuilds the whole vector, and general coalescing sorts/rebuilds it.
   These costs are multiplied by the retry storm and by up to 2,305 VMAs.
3. **PTE/TLB edit granularity.**  PolyHAL flushes locally inside every
   `map_page()` and `unmap_page()`; upper layers then issue address-space-wide
   remote shootdowns.  New mappings pay unnecessary local invalidation, while
   COW can issue two remote shootdowns for one logical replacement.
4. **Mutable-lock coupling of address-space identity and VMA state.**  The
   scheduler takes the full `MemorySet` mutex to read root/ASID and activate it.
   This is a downstream multiplier, not the first fix.
5. **Global heap serialization.**  177 million acquisitions are real, but the
   current diagnostics do not attribute them by size class and call domain.
   Selecting slab, per-CPU caches, or another allocator now would be guesswork.
6. **Resident/frame destruction.**  The fixed refcount table materially moved
   startup timestamps, but current late exec-drop time is only 0.278 seconds
   and frame-allocator wait is 0.110 seconds.  It is not the present late
   hotspot.
7. **VFS, scheduler and host I/O.**  Existing evidence shows smaller VFS phase
   time, negligible ready-queue wait, active TCG threads, no swap, and no disk
   saturation.  They remain later candidates, not the current upstream cause.

## 5. Four cfg-specific semantic graphs

### 5.1 RISC-V64 production

```text
sys_mmap
  -> find_mmap_area(MMAP_BASE..USER_STACK_TOP)
  -> MemorySet::find_free_area(Vec<MapArea>)
  -> insert_lazy_area_with_backing
  -> coalesce_areas

trap page fault
  -> buildstorm_memory_set_lock!(HardwarePageFault)
  -> MemorySet::handle_page_fault
  -> ResidentSet::insert_run(relative VPN)
  -> PageTableWrapper::map_page -> sfence.vma(vaddr, ASID 0)

munmap/mprotect/fork-COW
  -> PTE edits, per-page local invalidation
  -> platform::tlb_shootdown(root)
  -> SBI remote_sfence_vma for CPUs currently publishing that root

user entry
  -> MemorySet mutex
  -> root comparison fast path
  -> activate only when root changed; user ASID entries may be retained
```

RISC-V page-table creation is the critical layout constraint.
`PageTableWrapper::alloc_new()` calls `PageTable::restore()`.  Current
`USER_ROOT_PTE_END=2`, so root entries 2 through 511 are inherited.  The boot
table creates valid supervisor leaves in all 512 root entries: entries 0..255
are low identity mappings and 256..511 are high-half aliases of the same
physical ranges.  The kernel uses low physical/identity addresses and an 8-GiB
guest occupies multiple entries starting at entry 2.  A generic high-user
mapping cannot overwrite an inherited leaf or allow drop to release it.

The configuration comment is also wrong: positive canonical Sv39 is 256 GiB
(`0x0000_0000_0000..0x0000_003f_ffff_ffff`), not the much larger value declared
as `USER_VADDR_END` in `config/rv.rs`.

### 5.2 RISC-V64 diagnostics

The same production graph executes.  The feature adds aggregate work, lock,
mmap, VMA, PTE and TLB counters and replaces the global heap wrapper with an
instrumented lock.  This materially perturbs allocation and timing, so its
throughput must not be compared directly with production.  It is valid for
within-run counter deltas and causal attribution.

### 5.3 LoongArch64 production

The VMA/resident graph is shared, but the platform edges differ:

```text
PageTable::restore
  -> user-owned root entries 0..255
  -> copy kernel entries 256..511

kernel RAM access
  -> cached DMW1 (0x9000... | PA)
kernel MMIO access
  -> uncached DMW0 (0x8000... | PA)
low PLV0 transition access
  -> DMW2 identity window

user entry
  -> activate PGDL/ASID
  -> conservative local flush_all every activation

remote edit commit
  -> ACTIVE_ADDRESS_SPACE filter
  -> IOCSR IPI + generation acknowledgement under shootdown lock
```

LoongArch root ownership already permits the full positive lower half in the
local PolyHAL model, while privileged direct maps bypass PGDL.  A high user
arena is therefore structurally easier than on RISC-V, but must still prove
PLV3 canonical-address behavior, DMW privilege isolation, high fault/COW/drop,
and the current conservative activation protocol on real cfg builds and SMP
regressions.

### 5.4 LoongArch64 diagnostics

The production semantics remain, with the same feature-gated aggregate
instrumentation.  LoongArch additionally pays full local invalidation on every
user-root activation and synchronous IPI acknowledgement for remote edits.
This makes a later transactional TLB interface more important, but no
LoongArch runtime performance counter in this audit proves it is currently the
same magnitude as RISC-V.

All four cfg-specific release compile gates passed on 2026-08-13 with nightly
2025-01-18.  This is not a runtime gate.  Cargo warned that profile, patch, and
target-rustflags entries under `os/Cargo.toml` are ignored from the workspace
root.  Root patches are effective, but linker/target settings must be made
explicit in the runner or workspace configuration before a paging refactor is
accepted.

## 6. Lifecycle and ownership invariants

The architecture must preserve these boundaries:

### `VmaMap`

- Owns sorted, non-overlapping virtual policy intervals and gap selection.
- Owns mmap/munmap/mprotect/brk topology, backing metadata and merge rules.
- Does not own physical frames or issue TLB invalidations.
- Fixed mappings may replace only explicitly validated user-owned intervals.

### `ResidentSet`

- Owns every resident data-frame reference by VMA-relative VPN.
- Demand fault changes residency without changing VMA policy topology.
- Split/merge translates relative keys exactly once.
- Shared/file-clean/COW/private state controls clone and last-owner behavior.

### `PageTableOps`

- Publishes, replaces, protects and removes leaves.
- Does not free a data frame or decide VMA policy.
- Distinguishes known-absent new mappings from stale-translation-bearing edits.

### `TlbProtocol`

- Commits one logical edit transaction after all PTE writes are visible.
- Targets only CPUs that may run the address space, with architecture-specific
  ASID/root semantics.
- A revoked or replaced mapping's old frame cannot be released until required
  local and remote acknowledgement is complete.

### Process lifecycle

- `CLONE_VM` shares the same `Arc<Mutex<MemorySet>>` and `MmContext`.
- Fork clones resident owners and republishes every parent page newly entering
  COW, even if the VMA already carries a COW policy bit.
- Exec switches to the stable kernel root before replacing and dropping the old
  page-table owner.
- Exit/drop retires the ASID, releases user-owned page-table branches exactly
  once, then drops resident owners.
- Shared file mappings are written back before topology removal; clean cache
  frames retain their cache owner independently of process unmap.

The retained refcount table preserves these boundaries through an exclusive
`into_raw_ppn()` ownership transfer and last-owner `Release` decrement plus
`Acquire` fence.  Its three-file patch must remain isolated during subsequent
experiments so it cannot be confused with the address-space change.

## 7. Target MM architecture

The intended design is broad, but experiments remain narrow and reversible.

### Layer A: architecture-owned `UserVaLayout`

Replace scattered `MMAP_BASE`, `USER_STACK_TOP`, exec reset values and fixed
range checks with one architecture policy describing:

- low ELF/brk range;
- stack reservation and guard;
- one or more mmap arenas;
- forbidden kernel/direct-map holes;
- root entries owned by each process versus shared with the kernel;
- canonical-address and fixed-map validation.

For RISC-V, keep root entries needed by the live low identity map shared and
make a non-contiguous process-owned high arena explicit.  The already audited
shape `0x20_0000_0000..0x40_0000_0000` corresponds to root entries 128..255 and
does not collide with the 8-GiB low identity region.  Root copy and drop must
consume the same ownership policy; a single `USER_ROOT_PTE_END` integer is
insufficient for non-contiguous ownership.

For LoongArch, describe the same logical high arena if PLV3 and canonical
checks pass, while keeping DMW mappings strictly platform-owned.  mmap remains
lazy: reserving 128 MiB must not allocate 128 MiB of frames or page tables.

Selection should prefer the high arena for no-hint anonymous reservations,
while honoring explicit legal hints and wrapping within each arena.  It must
not branch on BuildStorm, crate names, paths, commands, or output.

### Layer B: gap-augmented `VmaMap`

Replace `Vec<MapArea>` only after Layer A is causally validated.  Required
operations are predecessor/containing lookup, first-fit from a cursor, maximum
gap, range split/drain, neighbor merge, and ordered iteration for fork/drop.
A small augmented balanced tree or chunked ordered tree is sufficient; copying
Linux Maple Tree APIs is unnecessary.  The important property is logarithmic
lookup/insertion and gap-aware placement without full-vector rebuilds.

Resident ownership stays inside `MapArea`/`ResidentSet`.  The container change
must not silently change COW, file backing, writeback, or destructor ordering.

### Layer C: `PageTableEdit` / `TlbProtocol` transaction

Introduce an MMU-gather-like edit object:

1. Collect PTE writes and classify them as new map, replacement, permission
   reduction, or unmap.
2. Publish all writes with the architecture-required memory barrier.
3. Perform at most one local/range invalidation and one remote commit per
   address-space transaction.
4. Release retired page-table nodes and data-frame owners only after required
   acknowledgements.

Known-absent demand mappings require no stale-entry invalidation.  COW
replacement should publish the new leaf, commit once, then drop the old owner.
munmap/mprotect should not combine per-page local flushes with another full
remote shootdown for every logical operation.

### Layer D: immutable address-space identity

After PTE transaction generations exist, publish root, ASID and generation in
an identity object readable without the mutable VMA lock.  This can remove
millions of activation lock acquisitions.  Do not revive the rejected cached
root fast path: identity publication, root lifetime, `CLONE_VM`, exec replace,
ASID reuse and edit-generation observation must be proved together.

### Layer E: allocation domains

Only after the mmap retry storm is removed, add feature-gated size-class and
call-domain sampling.  If small fixed-size MM metadata remains dominant, a
slab/per-CPU cache is justified.  If frame last-owner return dominates, add
bulk buddy release or per-CPU page lists.  The current global count alone does
not select either design.

Relevant design references are Linux's Maple Tree and VMA locking
documentation, `mmu_gather`/cache-TLB documentation, and per-CPU page lists:

- <https://docs.kernel.org/mm/process_addrs.html>
- <https://docs.kernel.org/core-api/cachetlb.html>
- <https://docs.kernel.org/mm/arch_pgtable_helpers.html>
- <https://github.com/torvalds/linux/blob/master/include/asm-generic/tlb.h>
- <https://github.com/torvalds/linux/blob/master/mm/page_alloc.c>

They supply invariants and batching ideas, not code to copy into this small
`no_std` kernel.

## 8. Recommended implementation campaign

Use a wide architecture branch with narrow experimental stages.  Never measure
two new kernel hypotheses in the same A/B comparison.

### Stage 0: freeze provenance and direct counters

- Preserve the current three-file refcount patch as its own diff artifact.
- Add only diagnostic aggregates for exact mmap length values or tighter
  buckets, per-process success/failure counts, high/low arena placement, VMA
  maximum gap, PTE edit kinds, and TLB transaction batches.
- Keep all counters behind `buildstorm-diagnostics`, rate-limited and off by
  default.  Do not log per syscall.

### Stage 1: VA capacity hypothesis

Change only `UserVaLayout`, root-entry ownership/copy/drop, and lazy placement.
Keep the current `Vec<MapArea>` and per-PTE flush behavior.  This directly asks:

> Does removing large-reservation `ENOMEM` stop the retry storm and cross the
> late 33-crate boundary?

Expected direct movement: large mmap `ENOMEM` approaches zero, mmap/munmap
calls fall sharply, high-arena reservations succeed, and resident physical
pages do not jump at reservation time.  Falsification: failures disappear but
mmap/munmap call rate and compile progress do not improve, or the allocator
simply moves the same loop into another syscall.

### Stage 2: VMA cost hypothesis

With Stage 1 held constant, replace `Vec<MapArea>` with `VmaMap`.  Expected
movement: mmap-select/commit and munmap hold per operation fall with large VMA
counts.  Falsification: operation counts remain comparable and hold-time
distribution does not improve.

### Stage 3: PTE/TLB batching hypothesis

With layout and VMA behavior constant, introduce transaction commits.  Expected
movement: `local_flush / PTE edit` and `remote_shootdown / topology syscall`
fall by an order consistent with the batch size; COW retains correctness.
Falsification: invalidation counts fall but user progress and MemorySet hold do
not move, or any stale mapping appears.

### Stage 4: identity and allocator follow-up

Only if activation or heap remains the leading residual, test one of immutable
identity reads or an attributed allocation-domain design, not both.

## 9. Verification and rollback matrix

Each stage must pass in this order:

| Gate | Required result |
| --- | --- |
| Diff/compliance review | no workload/path/command/output specialization; diagnostics feature-gated; official artifacts unchanged |
| Four release cfg builds | RV production, RV diagnostics+SMP, LA production, LA diagnostics+SMP |
| Independent VA/lifecycle gate | high and low lazy fault, fixed-map rejection, partial munmap, mprotect, fork/COW, nested COW, `CLONE_VM`, exec/drop, shared/file mapping, ASID reuse |
| Dual-architecture SMP | all CPUs participate; TLB acknowledgement and mapping isolation pass |
| 300-second production A/B | same image, QEMU, SMP, memory, source base and refcount patch; apply the existing >=15%, 5%-15% with direct-hotspot reduction, and <5% rollback rule |
| 300-second diagnostics attribution | separate from production; direct hypothesis counters move as predicted |
| 1,800-second production A/B | must cross the 33-crate boundary and show sustained progress; this is essential because the mmap storm is late |
| Complete official run | single QEMU, `-snapshot -m 8G -smp 8`, unchanged official guest timeout; require exact `BUILDSTORM_COMPILE mode=multi ok=true` |

The old high-window 300-second experiment is not a complete disproof of Stage
1: it predates the current late failure window and moved no crate-count
boundary.  It is also not evidence to retain the change.  A redesigned Stage 1
must satisfy the current retention rule and the late gate.  If a prerequisite
architecture stage is correct but below the performance threshold, keep it only
on the isolated architecture branch until a causally separated combined
candidate passes; do not silently make it the production baseline.

Immediate rollback conditions include any dual-architecture lifecycle failure,
user mapping overlapping a shared root leaf or DMW region, page-table double
free, frame retirement before TLB acknowledgement, eager physical allocation
for a lazy reservation, production diagnostics leakage, or less than 5%
comparable 300-second progress without the allowed direct-hotspot evidence.

## 10. Decision

The directly observed current hotspot is not a single slow lock or destructor.
It is the large-reservation `ENOMEM` retry loop, with `Vec` topology and
per-PTE invalidation amplifying the resulting syscall storm.  The strongest
but still `unverified` upstream explanation is the approximately 1.5-GiB mmap
VA policy; Stage 1 is designed to falsify that causality.  The frame-refcount
table is a startup optimization and should be frozen as an isolated
provisional patch; no further refcount tuning is justified by the late window.

The next production hypothesis is therefore the architecture-owned, lazy high
mmap arena with explicit non-contiguous root-entry ownership.  The complete MM
design includes `VmaMap` and transaction-based PTE/TLB work, but those layers
must be implemented and measured separately before the final combined
candidate.  No QEMU run or production-code implementation was performed in
this audit.
