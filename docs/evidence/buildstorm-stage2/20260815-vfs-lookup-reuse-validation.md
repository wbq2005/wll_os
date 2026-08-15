# BuildStorm VFS lookup reuse validation

Date: 2026-08-15

## Classification and identity

- Kernel base: `c80c638594216c6e3dda109d85bb492c0c7b8195`.
- Candidate scope: only `os/src/fs/vfs.rs`.
- Candidate diff SHA-256: `94feb228a41f07ee09ac10e4688343333a5056f7113bd998be8a5648a2cc819d`.
- Official suite: `testsuits-for-oskernel/final-2026` at
  `b5ec6ef8497e1818cbdec3b54bb722f036e57972`.
- RISC-V64 and LoongArch64 complete production runs: `official-pass`.
- Release and SMP lifecycle regressions: `capability-pass`.
- RISC-V64 musl complete run: `capability-pass`.
- LoongArch64 musl: `unverified`; the official LoongArch image does not contain
  `/musl/buildstorm_testcode.sh`, so there is no workload to execute.
- The public judge reports 180/180 scriptable points for both runs. The separate
  20-point design review is not claimed by these automated results.

## Causal model

`open_path()` used separate ext4 directory, regular-file, and final inode-kind
queries for the same normalized path. Compiler workloads repeatedly traversed
the same namespace through those queries. In addition, the parent-symlink and
readlink caches discarded their complete working sets when the old 16K limits
were reached.

The candidate performs one generation-validated `lookup_kind()` per ext4 path,
reuses its copyable `(inode, kind)` result throughout `open_path()`, raises the
two VFS cache bounds to 64K, and replaces whole-cache clearing with bounded
single-entry eviction. It does not change tmpfs routing, final-symlink policy,
rename/unlink invalidation, ext4 inode ownership, or diagnostics gating.

## Comparable results

| Configuration | Control | Candidate | Improvement |
| --- | ---: | ---: | ---: |
| RISC-V64 diagnostics, same host | 867.01 s | 793.68 s | 8.46% |
| RISC-V64 production | 860.15-867.16 s | 774.62 s | 9.94-10.67% |
| LoongArch64 production | 674.49-687.23 s | 620.70 s | 7.97-9.68% |

Every complete candidate run contains the exact marker
`BUILDSTORM_COMPILE mode=multi ok=true`, used the unmodified official image in
snapshot mode, and used `-m 8G -smp 8`. The LoongArch public judge still emits
a stale internal warning that it expected 12 cores; the finals BuildStorm
scoring configuration and the recorded QEMU command both use 8 vCPUs.

The finals performance rows above are glibc runs. `mode=multi` means the
multi-core build mode; it does not mean that glibc and musl ran together.
RISC-V64 was also rebuilt with `WLL_HARNESS_LIBC=musl` and completed the image's
musl script with
`BUILDSTORM_RESULT mode=multi status=OK rc=0 elapsed_s=783.15`, the group-end
marker, `ALL TESTS DONE`, QEMU exit 0, and no panic/OOM. This is recorded as a
compatibility `capability-pass`, not as finals score evidence. A read-only
`debugfs` check showed that `sdcard-la-pub.img` has the glibc BuildStorm script
but no musl BuildStorm script, so LoongArch musl remains `unverified` rather
than being reported as a kernel failure or a pass.

The diagnostics hotspot counters moved as predicted:

- `vfs_open_parent_symlink`: 49,802,733 us to 2,637,242 us.
- `vfs_open_final_symlink`: 6,714,761 us to 3,809,435 us.
- `vfs_open_ext4_regular_lookup`: 313,455 us to 7,945 us.

## Validation gates

- `git diff --check`: pass.
- RISC-V64 production release: pass.
- RISC-V64 diagnostics release: pass.
- LoongArch64 production release: pass.
- LoongArch64 diagnostics release: pass.
- RISC-V64 SMP8 lifecycle: pass.
- LoongArch64 SMP8 lifecycle: pass.
- Both SMP runs passed namespace, regular-file, resident/high-arena,
  large-kernel-allocation, TLB/ASID, and independent user-memory lifecycle
  phases.

## Evidence

- `20260815-riscv64-vfs-lookup-reuse-diagnostics-complete/`
- `20260815-riscv64-vfs-lookup-reuse-production-complete/`
- `20260815-loongarch64-vfs-lookup-reuse-production-complete/`
- `20260815-vfs-lookup-reuse-release-gates-retry1/`
- `20260815-vfs-lookup-reuse-smp-gates/`
- `20260815-riscv64-vfs-lookup-reuse-musl-capability-complete/`
- `20260815-loongarch64-vfs-lookup-reuse-musl-capability-complete/` (missing
  image workload evidence; `unverified`)

The initial release-gate wrapper attempt is preserved in
`20260815-vfs-lookup-reuse-release-gates/`. It failed before compilation with
exit 127 because a non-login SSH shell lacked `/root/.cargo/bin` in `PATH`;
`retry1` used the absolute Cargo path. Two later wrappers returned nonzero only
after successful tests because a PowerShell CRLF reached their final shell
line. The raw runner JSON, marker, judge output, and per-test exit files are the
authoritative results.

## Residual risk

The eviction policy is bounded and deterministic but not LRU. It removes the
lexicographically first key at capacity. This avoids catastrophic full-cache
cliffs and is sufficient for the measured workload, but a future general VFS
cache redesign should use an ownership-aware bounded replacement policy rather
than adding more independent global maps.
