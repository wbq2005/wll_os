# Stage 2 conclusion: single-idle-CPU runnable publication

Date: 2026-08-14
Source HEAD: `055df99ff554441f7699c3518b1a4b204bd9c265` (detached, dirty worktree)
Evidence level: production windows are `unverified`; independent build and SMP
regressions are `capability-pass`.

## Decision

Retain the scheduler wakeup change under the positive-optimization rule. It
removed a measured QEMU TCG idle-vCPU wakeup herd and reduced host resource
waste by an order of magnitude without losing useful SMP execution or failing
the dual-architecture correctness gates. It did not increase BuildStorm crate
progress at either 300 or 1,800 seconds, so it is not the current compile
throughput root cause and does not justify a completion claim.

## Causal model

The shared ready queue previously called `notify_runnable()` after every
publication. That function sent a reschedule IPI to every CPU whose idle bit
was set. Normal time-slice, syscall and kernel-task requeues therefore woke all
idle vCPUs even though the current CPU could immediately continue scheduling
the same global queue.

Under QEMU TCG, one useful vCPU remained near 100% while the seven idle vCPUs
each consumed about 88% host CPU and produced roughly 52,000-70,000 voluntary
context switches per second. This was host work caused by the guest scheduler,
not useful rustc parallelism.

## Design and invariants

- External ready-queue publication inserts and deduplicates under the queue
  lock, then calls `notify_runnable_for(task.affinity_mask())` only for a new
  entry.
- `notify_runnable_for` intersects affinity, online and idle masks, excludes the
  current CPU, atomically claims one idle bit with CAS, and sends one IPI.
- Local time-slice, syscall and kernel-task requeues call `add_task_local` or
  `add_task_front_local`; they send no remote IPI when the current CPU is
  eligible.
- If affinity excludes the current CPU, a local enqueue automatically falls
  back to the external single-CPU notification.
- Blocked-task completion, fork/vfork, new-task publication and continue paths
  remain externally notifying. Thread-group and affinity control events retain
  broadcast notification where several tasks may need reevaluation.

The lost-wakeup proof is unchanged in shape. An idle CPU publishes its idle bit
before rechecking the affinity-filtered ready queue. The producer publishes the
queue entry before claiming that bit. Therefore either the recheck observes
the task, or the producer claims the bit and sends the IPI. Clearing the bit
before the IPI prevents another producer from repeatedly targeting the same
idle CPU.

This stage changes no MM/VMA/resident ownership, PTE/TLB transaction, VFS,
frame-reference, official image, guest script, marker or guest clock behavior.

## Direct host result

At the end of the Stage 1 production 1,800-second run, QEMU used about
718-720% host CPU. One TCG thread was useful at 100%; the other seven were near
88-90% and each reported about 52,000-56,000 voluntary context switches per
second.

At the end of the Stage 2 production 300-second run, QEMU used about 105-108%.
The useful thread remained at 100%; the other vCPUs were 0-2% and normally
reported about 95-131 voluntary context switches per second. In the late
1,800-second phase, two useful TCG threads ran near 100% while the other six
were approximately 1-4%. The aggregate was about 215-217%, showing that useful
parallelism was retained while idle-vCPU host consumption stayed suppressed.

The diagnostics window also showed two useful vCPUs at 100% in its late phase
and nonzero user-run history on all eight guest CPUs. Host swap stayed zero and
storage was not saturated.

## Progress result

| Run | Window | Compile events | Last crate | First event | Event 23 | Last event |
| --- | ---: | ---: | --- | ---: | ---: | ---: |
| Stage 1 production | 300 s | 23 | `ax-posix-api` | 48.659 s | 56.870 s | 56.870 s |
| Stage 2 production | 300 s | 23 | `ax-posix-api` | 48.059 s | 56.871 s | 56.871 s |
| Stage 1 production | 1,800 s | 34 | `hashbrown` | 49.059 s | 57.671 s | 430.325 s |
| Stage 2 production | 1,800 s | 34 | `hashbrown` | 47.658 s | 56.069 s | 430.123 s |

The 1,800-second last-event movement was only 0.202 seconds, about 0.05%, and
the compile-event boundary was identical. No panic or OOM marker occurred.
Both runs contained only toolchain, minibuild and `BUILDSTORM_BEGIN` markers,
not a compile-success marker. Stage 2 therefore falsifies the idea that idle
wakeup waste is the current `hashbrown` dependency-boundary cause.

## Validation and provenance

Four independent release cfg builds passed before QEMU measurement: RISC-V
production and diagnostics, plus LoongArch production and diagnostics. The
corresponding kernel SHA-256 values were:

- RISC-V production:
  `404040dbaae5b0e2f22c72a3eaa7e110cfe45cd57089b55a7d6bf5562a7823ad`;
- RISC-V diagnostics:
  `438f23a83eb99338b7c4bcaeed45a2f9eecd97559167376e5721e517495d20b0`;
- LoongArch production:
  `3228003211a6f06f4eef9a5ce796bf176a5070ac62e4bf18718656487e29030b`;
- LoongArch diagnostics:
  `69b3cdab51c687c5e8c5711abc2efb1086d7649215ebbf3e51fdaa3d265b5235`.

Both eight-CPU regressions passed resident, high-arena and user-memory
lifecycle phases, ASID/TLB isolation and the 160-MiB heap-stress final gate.
These are `capability-pass` results. The production measurement kernel was
rebuilt by the window runner as
`e9a47aa87e67027f8e5628d5c8000d8e045c2570b2d9d153007c4c96598d4ce7`.
The unchanged official RISC-V image hash was
`d74e436522f5946ca17280a7a25f17dbb6604b71fe675bb8a021ce8e849b334c`.
QEMU was `11.0.3` with `-snapshot -m 8G -smp 8` and no concurrent QEMU.

All 24 files in the three Stage 2 evidence directories were copied to the local
worktree and matched the remote SHA-256 values individually.

## Evidence paths

- `20260814-riscv64-production-stage2-single-idle-wakeup-window300/`
- `20260814-riscv64-diagnostics-stage2-single-idle-wakeup-window300/`
- `20260814-riscv64-production-stage2-single-idle-wakeup-window1800/`
- Stage 1 controls under the matching `stage1-high-arena` directories

AI assisted with host/guest timeline correlation, scheduler protocol auditing,
implementation, regression planning, production comparison and documentation.
Developer-verifiable artifacts are the isolated three-file diff, build/SMP
logs, raw serial, launch metadata, kernel/image hashes and host samplers.

Complete BuildStorm remains **`unverified / not completed`**. No retained raw
serial log contains `BUILDSTORM_COMPILE mode=multi ok=true`.
