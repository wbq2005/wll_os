# ResidentSet API lifecycle gate

Date: 2026-08-07
Branch: `wbq_final_buildstorm_compile`

## Scope

This change is the compatibility/API stage of the VMA resident-state split. It
does not change the dense resident representation and does not claim a
BuildStorm performance improvement. `MapArea.frames` is replaced by the sole
owner `MapArea.resident: ResidentSet`; all former production callers use the
`ResidentSet` API.

No global resident owner table was introduced. The anonymous page-fault path
still follows the legacy VMA split/coalesce algorithm, so it is not a retained
performance candidate.

## Lifecycle coverage

The `smp-regression` feature adds two independent checks:

- `resident-set-api`: ownership transfer through insert, run insert, lookup,
  range extraction, split, merge and drain;
- `resident-memory-lifecycle`: lazy anonymous fault, fork/COW isolation,
  cross-range mprotect, partial munmap, release, shared-frame identity and
  `CLONE_VM` `Arc` identity.

It also launches a small dynamic user process independently of any contest
script. This covers ELF/interpreter loading, initial stack setup, user entry
and normal teardown, but does not by itself prove every userspace mmap or
clone ABI path.

## Verified evidence

- `git diff --check`: pass.
- RISC-V64 release and `smp-regression` release builds: pass.
- LoongArch64 release and `smp-regression` release builds: pass.
- RISC-V64 8-vCPU regression kernel SHA-256:
  `b3a3d97c94b9887f1be86801dd4f046bfb3ece37957966b90c0f40f07796d9b7`.
- LoongArch64 8-vCPU regression kernel SHA-256:
  `020d14b47caf06d06c2eb28b79df4aa7cda7eea5a373bab9c0172d4b6b34528c`.
- Both architectures emitted `pass phase=resident-memory-lifecycle`,
  `pass phase=user-memory-lifecycle`, and the eight-CPU SMP/ASID marker.
- The LoongArch raw serial log is retained on the evidence host at
  `/srv/buildstorm/evidence/20260807-loongarch64-resident-memory-lifecycle-smp/serial.log`.
  The runner's 45-second bound ended QEMU after the pass markers; this is a
  capability regression, not an official BuildStorm result.

## Remaining gates

The sparse resident-run implementation has not started. It must preserve the
single-owner, COW, shared/file, rollback and TLB-release invariants before a
feature-off RISC-V64 300-second production comparison. Full BuildStorm remains
`unverified / not completed`: no log contains
`BUILDSTORM_COMPILE mode=multi ok=true` for this change.
