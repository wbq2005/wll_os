# Stage 2 post-hashbrown namespace audit (2026-08-14)

## Evidence classification

- Complete BuildStorm remains **`unverified / not completed`**. No retained
  serial log contains `BUILDSTORM_COMPILE mode=multi ok=true`.
- The RISC-V64 release/diagnostics builds and independent SMP lifecycle runs
  retained from the preceding candidate are `capability-pass` only.
- The 900-second production run in
  `20260814-riscv64-production-stage2-post-hashbrown-qmp-window900/` is
  attribution evidence and therefore `unverified`.

## Source and run identity

- Local and remote source HEAD:
  `055df99ff554441f7699c3518b1a4b204bd9c265` (detached, dirty).
- Remote worktree:
  `/srv/buildstorm/worktrees/stage0-attribution-20260813`.
- The local and remote SHA-256 values matched for `task/mod.rs`,
  `task/manager.rs`, `platform.rs`, `syscall/other.rs`,
  `buildstorm_diagnostics.rs`, and `run_buildstorm_window.py` before launch.
- Official image:
  `/srv/buildstorm/images/sdcard-rv-pub.img`, SHA-256
  `d74e436522f5946ca17280a7a25f17dbb6604b71fe675bb8a021ce8e849b334c`.
- QEMU 11.0.3, `-snapshot -m 8G -smp 8`, production diagnostics disabled.
  The runner verified that no QEMU or BuildStorm runner existed before launch.

## Observed boundary

The run reached 129 `Compiling` events and crossed the old `hashbrown`
boundary. At about 788 seconds rustc failed with:

```text
error: failed to create file encoder: No such file or directory (os error 2)
error: could not compile `bitmaps` (lib) due to 1 previous error
```

There was no kernel panic or OOM and no successful BuildStorm result marker.
The serial SHA-256 is
`01ac1a61ff8b2038124b7c71a4e14e2ffb90be8e1f32923d4b30a03a8c9b2d8f`.
An earlier 1,800-second production control with related source reached
`flatten_objects` without this error. The failure is therefore intermittent,
not an invariant crate boundary.

Twenty-four delayed QMP samples covered 192 vCPU register snapshots. Once
late parallel work became runnable, most PCs were in userspace. Kernel samples
were distributed across idle `wfi`, heap allocation/deallocation, frame
allocation, page-table mapping, blocked-owner scheduling, and timer work.
There was no single stable allocator, MM, or scheduler PC that justifies a
production performance candidate from this run alone.

## Namespace concurrency defect model

The production failure takes priority over further performance tuning. The
current ext4 namespace implementation has two independent stale-resolution
windows:

1. `unlink_non_dir()` resolves the parent and child and constructs inode
   references before acquiring `EXT4_MUTATION_LOCK`. A concurrent rename,
   unlink, or create can invalidate either result before the directory entry is
   removed.
2. `rename_ext4()` resolves both parents, the source, and the destination and
   may remove the destination before acquiring `EXT4_MUTATION_LOCK`. The later
   transaction therefore acts on names and inode identities that were not
   validated under the write lock.

The caches add a publication race even when writers serialize with each other.
`resolve_existing()` can load an old `DIR_CACHE` Arc while a writer is active.
The writer then mutates the directory and invalidates `DIR_CACHE` and
`PATH_CACHE`; afterward the reader can publish its stale result into
`PATH_CACHE`. Because cache entries carry no namespace generation, subsequent
lookups accept the stale entry indefinitely. The symmetric stale-negative
case is also possible.

This model predicts intermittent `ENOENT`, `EEXIST`, or wrong-inode behavior
under Cargo's concurrent create/rename/unlink workload. It does not predict a
fixed kernel PC and is consistent with the observed run-to-run variability.

## Required architecture

The namespace must expose one coherent transaction boundary instead of a
writer-only lock plus unversioned caches:

- A namespace mutation owns the write transaction from the first parent/name
  resolution through directory-entry/inode updates and cache invalidation.
- A lookup either observes the complete state before a mutation or the
  complete state after it. It must not publish a cache result across a
  namespace generation change.
- Cached positive paths, negative paths, and directory snapshots are valid only
  for the namespace generation in which they were produced.
- Rename across parents updates both parents, the child, `..` for directories,
  link counts, and all cache invalidations as one transaction.
- Unlink preserves open-file inode lifetime while making the name disappear
  atomically.
- No namespace lock may be held during unrelated regular-file data writeback.

A Linux-style dentry sequence counter or a namespace read/write lock can
enforce the lookup side. The lowest-risk first implementation is a dedicated
namespace `RwLock`: lookups take read ownership, namespace mutations take write
ownership from initial resolution through invalidation, and internal
already-write-locked helpers prevent recursive locking. A later generation-
tagged cache can reduce read-lock hold time without changing these semantics.

## Falsifiable diagnostic gate

Before a production refactor, add fixed-size diagnostics-only aggregates for:

- O_CREAT parent missing before backend selection;
- ext4 create returning `ENOENT` after the parent probe succeeded;
- rename source/old-parent/new-parent missing;
- unlink parent/target missing;
- positive and negative cache observations invalidated by a namespace
  generation change.

Do not log every syscall or path and do not change the lookup result. A
diagnostic run supports the model if the rustc failure coincides with one of
these namespace categories or a generation-crossing observation. It weakens
the model if the error reproduces with none of them and the failing syscall is
shown to be outside open/create/rename/unlink.

## Validation matrix for a production fix

1. `git diff --check` and `cargo metadata --no-deps --format-version 1`.
2. RISC-V64 and LoongArch64 production and `buildstorm-diagnostics` release
   builds.
3. Independent dual-architecture SMP lifecycle, including resident memory,
   high arena, COW/shared/file mappings, ASID/TLB isolation, and heap stress.
4. A new concurrent namespace lifecycle regression covering create/unlink,
   same-parent and cross-parent rename, negative-to-positive lookup, open-unlink
   lifetime, and cache invalidation.
5. One RISC-V64 diagnostics attribution window, followed by a production
   900-second comparison on the unchanged image and runner.
6. A single full official window only after the short run has no namespace
   failure or regression. Success still requires the exact
   `BUILDSTORM_COMPILE mode=multi ok=true` marker.

## Roll-forward and stop conditions

The user-directed policy retains correct, generally applicable improvements;
it does not permit hiding failures. Stop and redesign if the namespace lock
introduces a lock-order cycle, if open-unlink semantics regress, if either
architecture lifecycle fails, or if the official workload reports a new
filesystem error. Performance is assessed only after correctness. No
scheduler, MM, PTE/TLB, allocator, or guest-time change belongs in this
namespace stage.

## Diagnostic result

The pre-fix RISC-V64 diagnostics run retained under
`20260814-riscv64-diagnostics-stage2-namespace-generation-window900/`
produced 94 complete snapshots. Its final snapshot recorded 146 positive and
170 negative generation crossings. At the late boundary rustc reported
`failed to create encoded metadata ... ENOENT`, then both `bitmaps` and
`flatten_objects` failed. The serial SHA-256 is
`a6e9fcb6eab9c5fa282abf2dfc5a3aea8c326c255b7280609b49920673ad719d`.
This confirms that readers crossed active namespace mutations, while the
temporal association alone does not prove that every rustc ENOENT had that
cause.

The post-fix 300-second diagnostics falsification under
`20260814-riscv64-diagnostics-stage2-namespace-transaction-window300/`
produced 34 complete snapshots and the same 23-event early boundary. Positive
and negative generation crossings were both zero, and no filesystem error,
panic, or OOM appeared. Its serial SHA-256 is
`9f6f927c5117468733675977ac853163288594a3a5caa5134be5f2cbc1ef0fcb`.

## Implemented namespace architecture

`os/src/fs/ext4_vol.rs` now uses a dedicated namespace `RwLock`. Ordinary
lookups hold shared ownership across cache acceptance, ext4 directory
traversal, and positive or negative cache publication. Namespace writers hold
exclusive ownership from their first parent/name resolution through ext4
mutation, cache invalidation, and the release increment of
`NAMESPACE_GENERATION`.

The `spin` 0.9 ordinary write path does not advertise a waiting writer. Writer
entry therefore takes the upgradeable slot and upgrades it: the `UPGRADED` bit
blocks new readers while existing readers drain, so sustained Cargo lookups
cannot starve create, unlink, or rename. Internal `*_locked` helpers make lock
ownership explicit and avoid recursion. Destination replacement and the
three-step exchange operation remain within one outer namespace write
transaction, so readers cannot observe the temporary name. Regular file data
and writeback remain outside the namespace lock. Open-unlink still removes the
name immediately while retaining the inode until the final open reference is
closed.

The generation check remains as a diagnostic and defensive cache-publication
invariant. Under a correctly held namespace transaction it cannot change;
the post-fix 0/0 result tests that all exercised lookup paths obey the new
ownership boundary.

## Independent regression and cfg gates

The `smp-regression` feature now includes an architecture-neutral namespace
lifecycle probe. Seven CPU-pinned readers continuously look up one path while
the coordinator performs 64 create/unlink state transitions. An epoch/ack
protocol verifies positive and negative visibility after every transition and
exposes a stale reader publication from the previous epoch. The probe then
checks same-parent rename, cross-parent rename, open-unlink data lifetime, and
final directory-cache invalidation. It uses a private path only in the
feature-local regression and changes no production path selection.

Both RISC-V64 and LoongArch64 passed the namespace probe together with the
existing interval-timer, resident/high-arena/user-memory, COW/shared/file,
ASID/TLB, heap-stress, and eight-CPU gates. These are `capability-pass`, not
official score evidence. All four release builds also passed:

- RISC-V64 production: `071116d8e795e2340ea2de65d515508f26fb4aecc91ec41248d88a007a9311fe`
- RISC-V64 diagnostics: `4f0c8d9ec0b607bd90f0e1a6749625cffd84e7f6ea60f6e7ab54bd886495bdc5`
- LoongArch64 production: `deaa5ce7b492783e6cdabeaae8ddba767ad791170a049d6f7a29f79eb14adbf9`
- LoongArch64 diagnostics: `cf830f22ebe3e2c71c1ea25101a45dd7f1f22f8fd9c22f0786008f5c2edb9c95`

Logs, JSON provenance, and kernel hashes are under
`20260814-stage2-namespace-transaction-gates/`.

## Production comparison

The same-image RISC-V64 production comparison is retained under
`20260814-riscv64-production-stage2-namespace-transaction-window900/`.
It used QEMU 11.0.3, `-snapshot -m 8G -smp 8`, diagnostics disabled, and the
unchanged image SHA-256
`d74e436522f5946ca17280a7a25f17dbb6604b71fe675bb8a021ce8e849b334c`.
The official suite `final-2026` ref was rechecked as
`b5ec6ef8497e1818cbdec3b54bb722f036e57972`.

The comparable old/new compile-event counts were 23/23 at 300 seconds, 90/91
at 600 seconds, and 129/131 at 900 seconds. The old run failed `bitmaps` with
ENOENT; the new run crossed `bitmaps`, reached `flatten_objects`, and had no
filesystem error, panic, or OOM. The 900-second event-count change is only
about 1.6%, below the performance threshold and not claimed as a speedup.
Twenty-four delayed QMP samples contained no stable namespace-lock PC cluster.
The production serial SHA-256 is
`1256c1ad4911f5e8d9fbf1e6e8b2ffaaa65a618977bf2900a4dac1a409a1d83d`.

The runner-recorded dirty-diff SHA-256 is
`16bc5ab597c67bddbcbe3c19ee371c64ac8538382f0054ac97024d6cab6e365a`.
The evidence directory preserves both the runner-normalized diff and the raw
`git diff --binary` bytes; their hashes differ because the runner decodes
invalid source bytes with UTF-8 replacement before hashing. The exact source
file hashes are `fe270697...534bb2` for `ext4_vol.rs` and
`3915899c...3fd96d` for `smp_regression.rs`.

## Stage conclusion

The namespace race is supported by pre-fix generation crossings, eliminated
by the post-fix 0/0 falsification, covered by dual-architecture concurrent
lifecycle tests, and absent at the prior production failure boundary. The
fix is retained as a correctness architecture improvement. It is not the next
performance candidate: early progress is unchanged and the 900-second gain is
below 5%. Complete BuildStorm remains `unverified / not completed` because no
retained serial contains exact `BUILDSTORM_COMPILE mode=multi ok=true`.
