# BuildStorm VMA architecture audit and candidate selection

Date: 2026-08-07

Status: audit complete; both the selected bounded-batching candidate and the
later encapsulated bounded-vacancy candidate failed their first comparable
production windows and were reverted.  No official compile success marker
exists.

## Scope and evidence boundary

This audit follows the completed official 15,000-second RISC-V64 run and the
anonymous-coalescer diagnostic decomposition in
`20260806-attribution-report.md`.  The retained production baseline is the
remote `memory_set.rs` with SHA-256
`622bbbac2a1a7165609d140ee09fb0a0a0da81e51fe5d5b364d753ecad1df59b`.
The remote host had no QEMU process during this audit.  No image, official
script, judge, marker, guest time, scheduler, VFS, TLB, or block-I/O code was
changed for the selected candidate.

The public official `final-2026` branch was refreshed read-only during this
audit and resolved to commit
`b5ec6ef8497e1818cbdec3b54bb722f036e57972`.

## Existing representation invariants

`MemorySet.areas` is a contiguous `Vec<MapArea>`.  Correctness of
`area_index_containing`, `first_area_ending_after`, `range_overlaps`,
`range_covered`, and `find_free_area` requires all of the following:

1. Every area is non-empty (`start_va < end_va`) and page aligned.
2. Areas are ordered by nondecreasing start address.
3. End addresses are also nondecreasing, which follows from sorted,
   non-overlapping, non-empty areas.
4. Areas never overlap.  Adjacent compatible areas may remain temporarily
   separate without changing lookup correctness.
5. A framed area's frame vector is either empty or describes the area from its
   start address in page order.  `MapArea::split_at` and `merge_with` preserve
   that relationship.

The existing callers do not require the vector to be maximally coalesced.
They require only the five invariants above.  `mmap`, `munmap`, `mprotect`,
`msync`, shared-memory attach/detach, user-copy helpers, page-fault handling,
COW resolution, `fork_cow`, `Clone`, and the SMP regression remain correct
with adjacent compatible VMAs left separate for a bounded interval.

## Tombstone prototype audit

The local prototype represented a removed VMA as a zero-length anonymous
`MapArea` stored inside the public `areas` vector.  Its local merge operation
could preserve monotonic starts and ends, and the binary searches can be made
to step over a zero-length boundary.  That is not sufficient for this kernel:

- `areas` is public and is directly iterated by shared-file writeback,
  System V shared-memory accounting/detach, clone logging, fork/clone, and the
  SMP regression.  None of those consumers has a slot/tombstone abstraction.
- `fork_cow` treats an anonymous tombstone as a private COW candidate and
  mutates its flags before the child later compacts it.  This is harmless in
  the narrow prototype but proves that invalid slots leak into semantic code.
- The anonymous adjacency diagnostics use immediate `area_index - 1` and
  `area_index + 1`; tombstones make those measurements false even though the
  production mapping is unchanged.
- `areas.len()` ceases to mean VMA count, contaminating process diagnostics
  and suffix-length measurements.
- There is no intrinsic tombstone-growth bound between unrelated global
  compactions.  Search remains logarithmic only while the monotonic boundary
  encoding is preserved, while local predecessor/successor scans grow with
  tombstone density.
- Public helpers that locate an area by exact start address would become
  ambiguous if a tombstone shares the next live area's start.  They are not
  currently called outside `MemorySet`, but the representation makes a future
  correctness regression easy and silent.

The prototype is therefore rejected before remote testing.  It is not a
production candidate and was removed locally.

## MM/TLB and SMP audit

The candidate changes only when VMA metadata is canonicalized after an
anonymous demand fault.  Page allocation, page-table writes, local map-page
invalidation, and the address-space root are unchanged.  `MemorySet` mutation
continues under the shared `MemorySet` mutex.  `mprotect` still batches its
remote shootdown after PTE permission changes; anonymous demand faults only
install previously absent local mappings and do not add a new remote
shootdown requirement.  RISC-V active-root publication/deferred generation
and LoongArch synchronous IPI handling are outside the candidate diff.

Forking while a batch is pending is safe: every fragment has complete backing,
flags, and frame ownership, the child iterates all fragments, and the child's
existing final global coalesce restores a canonical vector.  The parent may
remain fragmented until its next batch/global coalesce, without overlapping
or losing mappings.

## Audited single candidate: bounded anonymous coalesce batching

Measured evidence shows 129,001 anonymous coalescer calls visiting
122,317,107 VMAs.  Scan/rebuild consumed 59,834,257 of 62,364,765 microseconds
(95.95%).  The selected candidate keeps `Vec<MapArea>` and all lookup and
mapping semantics, but performs a global anonymous coalesce once per 64
anonymous installations rather than after every installation.

Each installation performs at most two splits, so at most 63 deferred events
can exist and the additional vector length before forced canonicalization is
bounded by 126 entries.  Any existing global coalesce caused by mmap, munmap,
file-fault handling, COW resolution, or another global operation resets the
batch earlier.  This trades a small, bounded amount of temporary fragmentation
for approximately 64 times fewer complete VMA scan/rebuild passes under a
pure anonymous-fault stream.

The threshold is architecture-neutral and workload-name independent.  It is
not based on a crate, command, output, score, or official marker.  Diagnostics
remain controlled by `buildstorm-diagnostics`; production has no diagnostic
output.

## Gates completed before the performance window

The following release builds completed successfully:

- RISC-V64 production: `--no-default-features --features riscv`
- LoongArch64 production: `--no-default-features --features loongarch`
- RISC-V64 diagnostics: `--features riscv,buildstorm-diagnostics`
- LoongArch64 diagnostics: `--features loongarch,buildstorm-diagnostics`

These are build gates only, not capability-pass or official-pass evidence.
The candidate `memory_set.rs` SHA-256 deployed to the remote host was
`68d80b2829632bcd97f7f5a37412cad3cd6c3797a24fa61299c86962fe3ac7a4`.
The local build-only artifact SHA-256 values before deployment were:

- RISC-V64: `9b29d80decdc9b459c5ad40d8fab722d163d210e4e875312c2f7d8e9b6ecfa59`
- LoongArch64: `0ce03863fd49abea99e7b890de191ada5b37897f4ab9254700373751515cba7a`

The candidate also passed the existing eight-CPU SMP/TLB/ASID regression on
both architectures before the performance window:

- RISC-V64: CPUs `0xff`, dispatch `0xff`, TLB targets `0xfe`, ASID translation
  isolation and reuse passed.
- LoongArch64: CPUs `0xff`, dispatch `0xff`, TLB targets `0x7f`, root-CSR
  isolation and ASID reuse passed.

## Production 300-second result: rejected

Evidence is stored in
`20260807-riscv64-production-smp8-anonymous-coalesce-batch64-window300/window-run1/`.
The feature-off runner used the unmodified official image SHA-256
`d74e436522f5946ca17280a7a25f17dbb6604b71fe675bb8a021ce8e849b334c`,
QEMU 11.0.3, `-snapshot -m 8G -smp 8`, and a 300.19659674599825-second
window after `BUILDSTORM_BEGIN mode=multi`.

It reached 23 `Compiling` lines and two `Finished` lines.  The last crate was
`ax-posix-api v0.5.29`.  There was no panic, OOM, or exact successful compile
marker.  The serial SHA-256 is
`76d3915c52d162b9f6abe3191694235291e21a94616387a02137b1387e622b14`.

The retained baseline reached the first timed crate at
`113.13476717598678` seconds and the 23rd crate at
`123.74908572799177` seconds.  Batching reached the same boundaries at
`124.7491725430009` and `134.7625188959937` seconds, respectively.  It was
therefore 10.2660% slower at the first boundary and 8.8998% slower at the
23rd boundary.  This is below the 5% retention floor and in the wrong
direction, so the candidate was immediately reverted without further
performance experiments.

The remote and local `memory_set.rs` were restored to SHA-256
`622bbbac2a1a7165609d140ee09fb0a0a0da81e51fe5d5b364d753ecad1df59b`.
No QEMU process remained after restoration.  A fresh feature-off remote
artifact was rebuilt with the official runner toolchain command and had
SHA-256 `177fe076660b410557d0409fdd673eae870c914643c920f6179132072c9d8e1c`.

The slowdown shows that reducing full scans alone does not compensate for the
extra live VMA fragments and repeated middle insertions/search pressure.  The
next candidate must avoid both full-vector rebuilds and growth of the active
VMA sequence; another batching threshold is not justified.

## Remaining work

No candidate is currently retained or authorized for another remote window.
Before selecting a new candidate, obtain evidence for a representation that
keeps the active VMA count bounded without `Vec::remove`, full reconstruction,
or deferred-fragment growth.  Do not vary the batching threshold: that would
repeat a rejected hypothesis rather than test a new root cause.

There is still no `BUILDSTORM_COMPILE mode=multi ok=true`; compilation success
is not claimed.

## Encapsulated bounded vacant-slot follow-up: rejected

The batching result showed that active VMA fragmentation was itself costly,
so the next representation candidate kept active VMAs canonical while turning
locally merged-away entries into private zero-length vacant slots.  The
`MemorySet.areas` field became private; filtered accessors were used by shared
file writeback, System V shared-memory accounting/detach, clone diagnostics,
and the SMP regression.  Fork and clone skipped vacant slots.  Local merge
preserved nondecreasing starts and ends, and compaction ran once there were at
least 64 vacant slots and they occupied at least one quarter of physical
slots.  The change was architecture-neutral and contained no workload-name,
command, output, marker, or score special case.

Before deployment, all `MemorySet` area consumers and every mutation path were
re-audited: lookup, overlap and coverage queries, free-area search, mmap,
munmap, mprotect, msync, anonymous and file faults, COW, fork/clone, exec
replacement, shared memory, release/drop, page-table mapping and TLB
shootdown.  A model test then executed 2,000 independent cases of 500 random
anonymous-fault and mprotect transitions each, checking slot counts, ordering,
non-overlap, canonical active mappings, and 128 address queries after every
transition.  All one million transitions passed.  This is static/model
evidence, not an official pass.

The exact candidate hashes were:

- `memory_set.rs`: `b474beab96ac72241654eea456bfa5c044fbcdd617f822d2c3cb8d5c947c1c8a`
- diagnostics: `b0f97fcd0eb1b1f9f2ea709442a0c5d4172e26e481bf74ee2a54683d0163300b`
- syscall MM: `c0ee63b5097ec52f71d0d9037ef6d5f923046f1eb809687261e5f8eb97cd7d21`
- syscall process: `0e801e2745ecb922d0babc69ab3f1de4140786e8737430fd44668f4c43021573`
- SMP regression: `b689dd2696d6dbe00ad33be0e6a0e4b6441801c017896954553ef68e4af730c3`

RISC-V64 and LoongArch64 production, diagnostics, and SMP-regression release
builds passed locally.  Sequential remote eight-CPU regressions then passed on
both architectures.  RISC-V64 reported CPU and dispatch masks `0xff`, TLB
targets `0x7f`, ASID translation isolation/reuse, and the VMA invariant test.
LoongArch64 reported the same CPU, dispatch, and TLB masks with root-CSR
isolation/reuse and the same VMA invariant test.  These are capability passes,
not official BuildStorm results.

The feature-gated diagnostic evidence is in
`20260807-riscv64-diagnostics-smp8-vma-vacant-slots-window300/window-run1/`.
It used the unmodified official image, QEMU 11.0.3, `-snapshot -m 8G -smp 8`,
and a 300.2011267810012-second post-marker window.  Relative to the comparable
anonymous-coalescer diagnostic, the first timed crate moved from
`129.95505538799625` to `124.14993321799557` seconds (4.47% earlier), and the
23rd crate moved from `141.37062898499426` to `134.76425769399793` seconds
(4.67% earlier).  Anonymous coalesce time fell from 62,560,949 to 3,296,169
microseconds, a 94.73% reduction, and MemorySet lock wait fell from 3,999,737
to 3,434,667 microseconds, a 14.13% reduction.

The final raw slot aggregate was 194,622 splits, only 32 split reuses, 136,207
local merges, 5,442 compactions, 1,955 maximum active VMAs, 2,103 maximum
physical slots, and 526 maximum vacant slots.  Thus the density bound held,
but the proposed adjacent split reuse was effectively inactive.  The design
still paid almost every middle insertion and obtained its benefit primarily
by avoiding immediate `Vec::remove` and amortizing global reconstruction.

The required feature-off evidence is in
`20260807-riscv64-production-smp8-vma-vacant-slots-window300/window-run1/`.
It ran 300.1950164370064 seconds after the official begin marker, reached 23
`Compiling` lines and two `Finished` lines, and ended at `ax-posix-api`.  It
emitted no panic, OOM, successful compile marker, or other compile result.
The first timed crate arrived at `124.94568506799988` seconds versus the
retained baseline's `113.13476717598678`, 10.44% slower.  The 23rd crate
arrived at `135.15931047900813` versus `123.74908572799177`, 9.22% slower.
It therefore failed the 5% retention floor in the wrong direction and was
immediately reverted without a second production run.

Both local and remote source were restored to the exact pre-candidate hashes,
including `memory_set.rs`
`622bbbac2a1a7165609d140ee09fb0a0a0da81e51fe5d5b364d753ecad1df59b`.
The rebuilt remote feature-off RISC-V64 kernel SHA-256 is
`5681d1c0d0be6f977c5a982ca466dda017a81dc9e6941b760c8e5e3f0ebe2ad6`.
No candidate is retained.  This result rejects tombstone/vacant-slot variants
that retain a contiguous `Vec<MapArea>` and still perform almost every middle
split insertion; changing the vacancy threshold or compaction ratio would
repeat the same failed mechanism rather than test a new root cause.
