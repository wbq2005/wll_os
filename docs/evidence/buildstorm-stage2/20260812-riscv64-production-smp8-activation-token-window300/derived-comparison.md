# RISC-V activation-token candidate comparison

This production window tested one hypothesis: avoid taking the shared
`MemorySet` lock merely to read the RISC-V root/ASID identity at user entry.
The candidate used one atomically published SATP token and kept all mapping
edits under the existing `MemorySet` mutex. Diagnostics were disabled.

The comparable baseline is
`20260812-riscv64-production-smp8-nested-cow-remap-window300/`. Object hashes
confirmed that its candidate-related source files exactly matched committed
`b6f518f` before this candidate was applied. Both runs used the same official
image SHA-256, QEMU 11.0.3, `-snapshot -m 8G -smp 8`, and a 300-second window
starting at `BUILDSTORM_BEGIN mode=multi`.

| metric | baseline | activation token | change |
| --- | ---: | ---: | ---: |
| `Compiling` lines | 23 | 23 | 0% |
| last crate | `ax-posix-api` | `ax-posix-api` | unchanged |
| first timed crate | 177.413 s | 193.633 s | 9.14% slower |
| 23rd crate | 187.227 s | 205.649 s | 9.84% slower |
| marker window | 300.183 s | 300.203 s | comparable |

Neither run panicked, OOMed, swapped, or saturated host storage. All eight
TCG threads were active. The candidate therefore failed the 5% retention
floor and was reverted from production source immediately. The independent
four-configuration builds and dual-architecture SMP regressions had passed
before measurement, but correctness coverage does not override the measured
performance rejection.

The audit also found an unresolved pre-existing semantic hazard: a process
created with `CLONE_VM` but without `CLONE_THREAD` shares the same current
`MemorySet` owner, so an exec by one process can replace the address space
observed by the other. The failed candidate did not establish a correct
detachment model for that case. This is source-derived risk, not a logged
BuildStorm failure cause.

Raw authority is `summary.json`, `serial.log`, `launch.json`, and the host
sampler logs in this directory. Candidate kernel SHA-256 is
`f7349c21c1421b89c6cdc3655b9395daa240e9416f159a0dc9f2dd3d9e4dc4ea`;
dirty diff SHA-256 is
`8406cf597b6f8a2a3e1f792615a0c94a72b8dd83b7031c9e647e147bc8d6678d`.
There is no exact `BUILDSTORM_COMPILE mode=multi ok=true` marker, so complete
BuildStorm remains unverified.
