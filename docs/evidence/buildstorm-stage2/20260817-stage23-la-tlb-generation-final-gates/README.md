# Stage23 final capability gates

Candidate base: `d676f7508bc08fb563ab451f184028158b942ed2`

Production patch-id: `d7d03aeb5c30375b6b6b7b4a295715db67af0a49`

The four release configurations completed successfully:

- RISC-V64 production
- RISC-V64 `buildstorm-diagnostics`
- LoongArch64 production
- LoongArch64 `buildstorm-diagnostics`

The two architecture-specific SMP8 runs completed with `pass cpus=8`. They cover
resident/high-arena/user-memory lifecycle, regular-file lifecycle, address-space
isolation and ASID reuse, TLB shootdown, 160 MiB heap stress, and a 320 MiB large
kernel allocation. These runs are `capability-pass`, not official score evidence.

## Raw log SHA-256

```text
671d4e2aaf17306a2e2d3d27a119cf9928e33e60ee1858493276ad4462696376  la-diagnostics.log
47ecd1d348503a2471581ac166bd92b76d513f6c625448c39ee492d1d2a15d38  la-production.log
83be6dff07dc527e90d91ca3bfde7a0f3d87410327134bc829136780181c482f  rv-diagnostics.log
ccb2a136c0b2878985cab6c6f21fa3299e9b90575b1c3ef7554076cb7b1607b7  rv-production.log
6ca719a82983927d3843170e85c52cbb999c869ce18b554e493fb8848677a08b  smp-loongarch64.log
ff1c57c3adf95d89ac431227d0943e3a5682710721d301dcc3b98e70c50cd8c0  smp-riscv64.log
```

The repository ignores `*.log`; preserve these six files with an explicit forced
add when the Stage23 evidence is committed.
