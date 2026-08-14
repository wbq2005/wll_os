# BuildStorm PTE/TLB transaction factorial conclusion

Date: 2026-08-13

BuildStorm status: `unverified / not completed`. No run emitted an exact
`BUILDSTORM_COMPILE mode=multi ok=true` result.

## Worktree identity and isolation

- Local worktree: `C:\Users\22478\.codex\worktrees\618f\wll_os-master1`
- Detached HEAD: `055df99ff554441f7699c3518b1a4b204bd9c265`
- The pre-existing three-file frame-refcount candidate remained identical
  throughout this experiment. Its diff hash was
  `42f81b2ee3ce516b998e882722b75a5b374f06d1` before and after the run.
- The failed high-address and `VmaMap` candidates remained rolled back.
- The production A/B arms differed only in `os/src/mm/memory_set.rs` and
  `patch/polyhal/src/pagetable/mod.rs` after excluding build and evidence
  directories.

Remote isolated trees:

- `/srv/buildstorm/worktrees/stage3-pte-tlb-control-20260813`
- `/srv/buildstorm/worktrees/stage3-pte-tlb-candidate-20260813`

Remote evidence:

- `/srv/buildstorm/evidence/20260813-stage3-pte-tlb-factorial/control-window300`
- `/srv/buildstorm/evidence/20260813-stage3-pte-tlb-factorial/candidate-window300`

## Audited hypothesis

The candidate combined the retained per-frame `AtomicUsize` reference table
with a `PageTableEdit` transaction. Known-absent leaves were published without
an immediate invalidation. Replacement, permission reduction, and unmap
published their leaf changes, executed one local invalidation and one
root-scoped remote shootdown, and retired old COW/unmapped frame owners only
after the synchronous shootdown returned.

The intended direct effect was to reduce the saved long-window counts of about
1.276 million PTE writes and 448,845 remote shootdowns. The falsifiable
production claim was that this reduction would improve 300-second crate
progress, not merely shift startup timestamps.

## Correctness gates

Before performance testing, the candidate passed:

- `git diff --check`;
- RISC-V64 production release build;
- RISC-V64 diagnostics plus SMP release build;
- LoongArch64 production release build;
- LoongArch64 diagnostics plus SMP release build;
- RISC-V64 resident-memory lifecycle, independent user-memory lifecycle, and
  8-CPU TLB/ASID isolation regression;
- LoongArch64 resident-memory lifecycle, independent user-memory lifecycle,
  and 8-CPU TLB/root-CSR isolation regression.

These are `capability-pass`, not an official BuildStorm pass.

## Production factorial result

Both runs used QEMU 11.0.3, the unchanged image
`/srv/buildstorm/images/sdcard-rv-pub.img`, `-snapshot -m 8G -smp 8`, production
diagnostics disabled, and a 300-second window after
`BUILDSTORM_BEGIN mode=multi`.

| Metric | Control: refcount only | Candidate: refcount + PTE transaction | Change |
| --- | ---: | ---: | ---: |
| Real `Compiling` lines | 23 | 23 | 0% |
| First `Compiling` | 48.458 s | 44.453 s | 8.26% earlier |
| 23rd `Compiling` | 57.070 s | 47.058 s | 17.54% earlier |
| Last crate | `ax-posix-api v0.5.29` | `ax-posix-api v0.5.29` | unchanged |
| Marker window | 300.182 s | 300.200 s | comparable |
| Panic / OOM | none | none | no regression observed |

The candidate reproduced the same pattern as the frame-refcount change: a
large startup timestamp shift without any 300-second crate-count improvement.
It therefore failed the predeclared `<5%` production retention gate. No
1,800-second or complete run was started. The two-file production candidate
was removed locally; the isolated remote tree and raw evidence were retained.

## Architectural conclusion

The frame-refcount and PTE/TLB changes can be composed correctly, and the
combined candidate improves early kernel work. They are not sufficient to
remove the post-`ax-posix-api` steady-state boundary. This falsifies a model in
which per-frame ownership allocation plus immediate PTE invalidation is the
complete BuildStorm bottleneck.

The next audit must focus on the residual late-phase compound path, not another
container-only change:

1. Attribute the 177 million diagnostic heap-lock acquisitions by allocation
   size and kernel domain with feature-gated aggregate counters.
2. Separate time executing with the `MemorySet` lock from time waiting to
   activate the same address space, including the direct hardware-fault path.
3. Correlate those deltas with the exact rustc processes and runnable/blocked
   state after the 23-crate dependency boundary.
4. Only then choose between a per-CPU/slab allocation domain, bulk frame
   retirement, or an immutable address-space activation identity. These may
   form one final architecture, but each production experiment must retain an
   independently falsifiable boundary.
