# RISC-V64 post mmap fallback diagnostic

Classification: `unverified`.

This diagnostic used the unmodified official RISC-V64 BuildStorm image with
`-m 16G -smp 8` and the `buildstorm-diagnostics` kernel feature. It is a
performance/root-cause run, not official score evidence.

At the 480-second guest snapshot, the largest measured syscall costs were
`readlinkat` 123.7s, `openat` 105.5s, `write` 70.0s, `statx` 62.3s,
`getdents64` 57.2s, and `mprotect` 45.6s. This identified VFS readlink/path
metadata as the next optimization target after fixing mmap ENOMEM/thread-spawn
failure.
