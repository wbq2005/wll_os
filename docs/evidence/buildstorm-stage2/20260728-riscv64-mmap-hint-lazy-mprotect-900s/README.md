# RISC-V64 mmap hint and lazy mprotect diagnostic

Classification: `unverified`.

This diagnostic used the unmodified public RISC-V64 BuildStorm image and the
normal runner arguments `-m 16G -smp 8`, but built the kernel with the
`buildstorm-diagnostics` feature. It is not official score evidence.

The previous short diagnostic failed early in pthread/Rust thread creation with
`failed to spawn thread: Os { code: 11, kind: WouldBlock }` around the first few
pre-build crates. After avoiding eager anonymous-page allocation in `mprotect`
and resetting the process mmap hint to `0x2000_0000`, the 900-second diagnostic
did not hit that thread-spawn failure and progressed past `once_cell` to
`thiserror`.

The run still timed out before the official complete marker, so the RV official
gate remains open.
