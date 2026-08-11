# BuildStorm Attribution Report: RISC-V64 Production Baseline and Diagnostic Window

## Evidence status

The official production run is **not an official BuildStorm compile pass**. It
used the unmodified glibc image and official script, reached
`BUILDSTORM_BEGIN mode=multi`, and did not emit
`BUILDSTORM_COMPILE mode=multi ok=true` before the 15,000-second outer
deadline. There was no panic or OOM; the run continued making slow serial
progress. The official judge output is 20.0 / 180.0 scripted points (toolchain
8, minibuild 12, compile 0, compile-time 0).

The first delivery is therefore an attribution record and raw-log index. No
production scheduler, VFS, MM, or block-cache optimization is selected or
claimed from this evidence.

## Evidence paths

* Production baseline (remote and copied locally):
  `docs/evidence/buildstorm-stage2/20260806-riscv64-production-15000-official/`
* Production 1-vCPU/8-vCPU windows:
  `docs/evidence/buildstorm-stage2/20260806-riscv64-production-15000-official/queued-windows/`
* Feature-gated diagnostic 8-vCPU window (remote):
  `/srv/buildstorm/src/wll_os/docs/evidence/buildstorm-stage2/20260806-riscv64-diagnostics-smp8-locks-window300/window-run2/`
* Feature-gated diagnostic 8-vCPU window (local copy):
  `docs/evidence/buildstorm-stage2/20260806-riscv64-diagnostics-smp8-locks-window300-remote/window-run2/`

Every serial log is retained. The `-snapshot` target filesystem is ephemeral,
so target-file counts and bytes are deliberately reported as unavailable.

## Provenance

| Item | Production baseline | Diagnostic window |
| --- | --- | --- |
| Architecture / memory / vCPUs | riscv64 / 8G / 8 | riscv64 / 8G / 8 |
| Kernel source commit | `4d6f10506d1bff10bb07e0f569daa86362459566` | same |
| Dirty-diff SHA-256 | `eca5b4b17480a7f910c203a762775a2dc1138b3316dce5329457d8d8a0392e98` | `30220e0c8b8de02e1e06bf3d06265a767a19ce522dee4f2dc6bda8c898bdbe60` |
| Kernel SHA-256 | `6b3696d2b6437ff1a342c65ee5f0ee6fa535f92db64cc5730e3daaaf3f40ee84` | `e005e79ad0c25c219c77d5ab2bdc5f1b810e7e4eba4a37d4a81030642eea8691` |
| Image SHA-256 | `d74e436522f5946ca17280a7a25f17dbb6604b71fe675bb8a021ce8e849b334c` | same |
| Official suite commit | `b5ec6ef8497e1818cbdec3b54bb722f036e57972` | same suite |
| QEMU | `QEMU emulator version 11.0.3` | same |
| QEMU arguments | `-snapshot -kernel .../release/wll_OS -m 8G -smp 8 -display none -monitor none -serial stdio -drive file=.../sdcard-rv-pub.img,if=none,format=raw,id=x0 -no-reboot -machine virt -bios default -device virtio-blk-device,drive=x0,bus=virtio-mmio-bus.0` | same except `-smp 8`; diagnostics feature enabled |
| Production diagnostics | disabled | enabled only by diagnostic feature |

The authoritative full arguments and hashes are in each run's `runner.json`
or `launch.json`; host samples are `host-pidstat.log`, `host-iostat.log`, and
`host-vmstat.log`.

## Long baseline boundary

* Host elapsed: `15000.182130264 s` (GNU time: `4:10:09`).
* Result lines: `BUILDSTORM_TOOLCHAIN ok`, `BUILDSTORM_MINIBUILD ok`,
  `BUILDSTORM_BEGIN mode=multi`; no successful compile marker.
* No panic/OOM marker. The last serial output remained in the standard-library
  compilation batch, so this is slow forward progress, not a proven deadlock.
* Judge: `official-judge.stdout` records 20.0 / 180.0.

## Comparable production windows

| vCPUs | Marker-window elapsed | `Compiling` lines | Last crate | `Finished` lines | Panic |
| ---: | ---: | ---: | --- | ---: | --- |
| 1 | 300.142606220 s | 2 | `core v0.0.0` | 2 | no |
| 8 | 300.179756154 s | 23 | `ax-posix-api v0.5.29` | 2 | no |

The requested short-window event-count ratio is `23 / 2 = 11.5x`. This is a
crate-event count, not normalized compiler work and not a claim of 11.5x
throughput. `target` file counts/bytes are unavailable because `-snapshot`
does not expose the guest overlay to the host.

## Diagnostic observations (8 vCPUs, 300 s, 37 ten-second snapshots)

The diagnostic run saw the marker at `BUILDSTORM_BEGIN mode=multi`, measured
`300.185014666 s` from that marker, and had `377.683024852 s` host elapsed.
It emitted no `BUILDSTORM_COMPILE ... ok=true` marker and no panic.

### Required classification answers

| Question | Verified result and limit |
| --- | --- |
| Maximum concurrent rustc | **4 observed live**, **1 observed runnable** (maximum over all 37 snapshots; sampling cannot prove an unsampled instantaneous peak). |
| User time on all 8 vCPUs | **Yes in the diagnostic counters**: final user ticks were nonzero on CPUs 0-7 (`6163, 3714, 1994, 734, 529, 557, 613, 562`). The distribution is strongly imbalanced; CPU 3 also accumulated `45183` idle ticks. |
| 8-core / 1-core progress | `11.5x` by `Compiling` event count only. |
| Highest blocking category | `block_io`: count `2006`, cumulative `628,979,824 us`; then `wait4/waitid` `476,144,189 us`, signal `153,556,603 us`. These are aggregate blocked-task durations and may overlap across tasks. |
| Highest lock wait | `memory_set`: `7,857` contended acquires and `13,552,898 us` cumulative wait. `block_cache` was next at `252,016 us`; no other instrumented lock was close. |
| Page-table/TLB time | **Not measured**. Counts were user-root activations `36,362`, kernel-root activations `36,032`, page-table-register writes `72,394`, remote shootdowns `6,072` targeting `10,724`, local flush `0`; no duration field exists, so no time percentage is valid. |
| Block-device cause | **Not distinguished by this window**. Virtio completion time was `4,093,631 us` and queue time `24,922 us`; block-I/O blocked time was much larger. This argues against host-device saturation but does not separate guest wait-queue/global-lock delay from repeated metadata work. Block cache counters were hits `1,327,032`, misses `121,898`; inode/metadata counters were not implemented and remain zero/uncovered. |
| Host environment | **No swap or sustained host I/O wait**: vmstat ended with `swpd=0, si=0, so=0, wa=0`; iostat showed no sustained device utilization. QEMU used multiple TCG threads; final pidstat was about `199-209%` CPU and the final thread sample showed CPU 0-7 TCG threads, so this was not a single busy QEMU thread. |

### Additional aggregate counters

The final snapshot also recorded path components `34,794`; openat
`23,200/24,497,213 us`; statx `54,889/22,259,572 us`; readlink
`2,397/201,888 us`; read `24,592/4,949,161 us`; pread `146/21,532 us`;
mmap `2,375/2,994,538 us`; munmap `787/1,215,170 us`; mprotect
`55,722/28,328,780 us`; page faults load/store/exec
`10,386/6,737,425 us`, `135,872/77,523,821 us`, and
`19,024/14,118,433 us`. These durations are work-category totals, not a
complete TLB-time decomposition.

## Interpretation and next gate

The evidence places the first *measured* contention hotspot at `MemorySet`,
while the largest blocked-time category is block I/O. It does **not** yet prove
that either is the sole compile bottleneck: `runqueue_max` remains unpopulated,
inode/metadata cache counters remain uncovered, and heap/fd-table lock classes
are absent. The one-runnable-rustc observation plus nonzero user time on every
vCPU supports investigating wakeup/runqueue behavior and MemorySet contention
as separate hypotheses, but does not authorize a production patch.

The next valid action is to close those diagnostic coverage gaps or run one
single, narrowly scoped hypothesis test. A production patch is admissible only
after a comparable 300-second window demonstrates the stated retention
threshold (at least 15% progress, or 5-15% with a materially lower first-hotspot
wait and no correctness regression). Until then, keep production behavior
unchanged and do not claim BuildStorm compilation success.

## Rejected production hypothesis

One narrowly scoped MM hypothesis was subsequently tested: cache the shared
address-space root so a CPU already on that root can skip taking the
`MemorySet` lock at user entry, with explicit invalidation/publish around the
in-place `execve` replacement.  The valid production 8-vCPU window used a
candidate kernel SHA-256 of `b2cb942d739e3a372e180bf2d2137c3547c21ef58f79cc1cb60b9886cf64165a`
and produced 23 `Compiling` lines in 300.198 seconds, exactly matching the
23-event production 8-vCPU baseline.  It had no panic but no successful
compile marker.  The measured progress change is 0%, below the 5% retention
floor, so the candidate was reverted and no production optimization remains.
Raw candidate evidence is remote at
`/srv/buildstorm/src/wll_os/docs/evidence/buildstorm-stage2/20260806-riscv64-production-smp8-active-root-fastpath-window300-attempt2/`;
the complete decision record is
`20260806-active-root-fastpath-rejected.md`.

Later follow-up diagnostic samples and the separately rejected scheduler
locality candidate are recorded in
`20260806-followup-diagnostic-classification.md`.

## Futex attribution coverage correction (latest diagnostic sample)

`20260806-riscv64-diagnostics-smp8-futex-window300/window-run1/` closes a
diagnostic-only accounting gap: `futex_wait_addr()` now calls
`note_block_start(..., BlockReason::Futex)` immediately before it parks.  The
call is guarded by `buildstorm-diagnostics`; production futex behavior is
unchanged.  Both local RISC-V64 production and diagnostic release builds
passed.  After the window, the remote artifact was rebuilt without that
feature (`0336c5b88cb67e8544a6f1fc72d0369c71cedfa8831d9d22293deb90be9caaf3`).

The unmodified image and glibc script reached the begin marker, then ran for
`300.223821428 s` (host elapsed `371.314812672 s`).  It produced 22
`Compiling` lines, last `ax-posix-api v0.5.29`, no panic, and no compile-success
marker.  This diagnostic progress count is not compared with the production
window.  Its kernel SHA-256 was
`fff7a6f83b0ca60397e7bd27f2ac269515214f795c33c75cc04b159dc45ba1b5`; launch
provenance and raw host samples are retained beside the serial log.

At the final report snapshot, the recorded futex category was 2,347 waits and
220,782,773 microseconds.  It is real wait time, but it is lower than
wait4/waitid (432,942,194 microseconds) and poll (422,942,548 microseconds),
so futex is not the first aggregate blocked-time category.  The worker
snapshots show keyed poll sleeps (604) and zero unkeyed sleeps.  The counters
start at kernel boot rather than at the begin marker and incomplete waits are
cut off when the host terminates QEMU; their values are therefore supporting
classification evidence, not an exact 300-second total.

The maximum sampled `rustc_live=8` and `rustc_runnable=2` values are **TCB
thread counts**, not de-duplicated process counts.  A final comm snapshot had
one rustc thread group (`tgid=214`) with four live threads, one running and
three futex-blocked.  Consequently this sample must not be used to claim the
maximum number of distinct concurrent rustc processes; that diagnostic
coverage remains open.

## Clean `50c9bd3` diagnostics control (2026-08-07)

Raw evidence: `/srv/buildstorm/evidence/20260807-riscv64-50c9-clean-diagnostics-window300/`.
This is an unmodified official RISC-V64 image and guest script, with only the
`buildstorm-diagnostics` feature enabled, `-snapshot -m 8G -smp 8`, and a
300.299254061003-second marker window. It produced toolchain, minibuild and
begin markers, 23 `Compiling` lines and 2 `Finished` lines; it produced no
panic/OOM or compile-success marker. It is diagnostic evidence only.

| Required classification item | Clean-control result |
| --- | --- |
| Maximum rustc concurrency | 37 snapshots: max **2 rustc processes**, **9 rustc TCBs**, **2 runnable rustc TCBs**. At the final snapshot there was one rustc process, with one running TCB and three futex-blocked TCBs. |
| All eight vCPUs execute user work | Yes. CPUs 0--7 had nonzero user ticks: `3493,2176,781,6211,548,580,487,508`; work was markedly imbalanced. |
| 8-core / 1-core progress | Historical matching 1/8-vCPU controls remain 2/23 `Compiling` events: **11.5x event-count ratio**, not normalized compiler throughput. |
| Largest aggregate blocking class | `wait4/waitid`: `455,686,431 us`; then poll `393,864,240 us`, futex `210,050,706 us`, pipe readiness `201,039,221 us`; block I/O only `3,089,659 us`. These sums are task-time and overlap. |
| Largest lock wait | `memory_set_activation`: `12,263,883 us`; next `memory_set`: `3,500,958 us`. |
| MM/TLB direct time | address-space activation `238,914 us` + TLB shootdown `385,773 us` = `624,687 us` (about 0.21% of the 300-second window if treated as representative). Counts: 36,369 user roots, 36,034 kernel roots, 72,403 page-table writes, 6,553 remote shootdowns / 10,895 targets. |
| Block-device classification | Virtio queue/completion were `14,046 us` / `4,289,285 us`; block-cache hits/misses `1,327,208` / `121,952`; inode metadata hits/misses `54,043` / `26,224`. This excludes device wait or block-cache lock contention as the first measured hotspot, but does not prove metadata lookup is irrelevant. |
| Host health | No swap; no sustained host I/O wait or NVMe utilization. All eight TCG threads existed and had CPU time, though the final QEMU process sample was about 299% CPU because the final rustc phase exposed only one running compiler worker. |

The clean control confirms that global scheduler, TLB and block-device changes
must not be selected merely from cumulative wait sums. The first remaining
production direction requires a new, representation-level MM design with a
measured per-VMA resident-run shape/allocation cost, or an independently
measured VFS metadata subphase; no additional production patch is authorized
by this control alone.

The same control also records the current VMA shape: 129,205 anonymous
installations, 122,665,046 VMA entries scanned during 129,207 coalesces, and
58,146,334 microseconds in anonymous coalescing. Neighbor opportunities are
not absent (`left=71,456`, `right=56,164`, `both=6,206`), but the associated
move counters include 18,675,838 left suffix removals and 30,650,449 right
frame moves. Together with the already rejected left/right extension,
vacancy, batching and sparse-`BTreeMap` candidates, this rules out choosing
another local `Vec<MapArea>` special case without new independent evidence.

All eight active vCPUs had nonzero final user ticks, but work was uneven
(CPU 0-7: `3893, 6361, 2136, 796, 417, 526, 560, 578`).  Instrumented
address-space activation plus TLB-shootdown time was `239,207 + 358,602 =
597,809 microseconds`, about 0.20% of a 300-second window if treated as
representative; it is not a full kernel-time decomposition.  The largest lock
wait was `memory_set_activation` at 15,336,482 microseconds (the other
MemorySet uses added 4,264,534 microseconds).  Virtio queue and completion
time were 18,689 and 3,689,067 microseconds respectively; block-I/O sleep was
2,100,243 microseconds.  The host remained free of swap and sustained I/O
wait, while the final sample showed all eight TCG threads active, so the
evidence does not support host I/O saturation or a single-QEMU-thread cause.

## Process-count coverage completion (latest sample)

`20260806-riscv64-diagnostics-smp8-process-count-window300/window-run1/`
adds diagnostic-only counters for thread-group leaders, alongside the existing
TCB counters.  It introduces no production scheduling branch or allocation.
This closes the ambiguity in the preceding sample: the maximum observed
distinct rustc processes was **2 live** and **1 runnable** over 37 snapshots;
the corresponding TCB-thread maxima were 8 live and 2 runnable.  The final
snapshot had one rustc process with four live TCB threads, one runnable.

The window measured `300.199686937 s` after the official begin marker and
reached 23 `Compiling` lines (last `ax-posix-api v0.5.29`), matching the
production SMP8 event count but not constituting a performance comparison
because diagnostics were enabled.  It had no panic and no successful compile
marker.  All eight vCPUs again had nonzero user ticks.  Its raw serial log,
host samples, image/kernel hashes, complete QEMU arguments, dirty-diff hash,
and restored production build record are retained in that directory; the
restored production kernel SHA-256 is
`96fc7cddc0884110622da82b719f612243e7a193f4ddac552fc73a68088f07a3`.

Its final blocked-time ordering is consistent with the futex coverage run:
wait4/waitid `443,583,838 us`, poll `427,867,057 us`, futex `208,763,559 us`,
and real block I/O `2,779,998 us`.  Thus two rustc processes exist but only
one is sampled runnable, while the host remains unsaturated.  The evidence
supports continued diagnosis of cargo/jobserver, keyed pipe/poll, futex, and
child-wait behavior; it does not justify a production optimization yet.

## Superseding diagnostic coverage sample (run4)

The later valid diagnostic sample supersedes the earlier run2 figures wherever
they differ.  It is retained at
`docs/evidence/buildstorm-stage2/20260806-riscv64-diagnostics-smp8-locks-window300-remote/window-run4/`
(remote source evidence:
`/srv/buildstorm/src/wll_os/docs/evidence/buildstorm-stage2/20260806-riscv64-diagnostics-smp8-locks-window300/window-run4/`).
It used the BuildStorm glibc harness build environment with the diagnostic
feature explicitly enabled, ran 300.199 seconds after the begin marker, and
reached 23 `Compiling` lines (last `ax-posix-api v0.5.29`) without panic or
compile-success marker.

The maximum observed sampled concurrency was five live and two runnable
`rustc` processes; final user-task state was 21 live, 1 runnable, 20 blocked.
Every vCPU had nonzero user ticks (`3809, 1901, 786, 6014, 687, 633, 500,
738`), but the distribution was highly uneven.  The largest aggregate blocked
duration was block I/O: 1,983 waits and 689,350,407 microseconds; wait4/waitid
was next at 472,569,169 microseconds.  The largest measured lock wait remained
`memory_set`: 732,625 acquires, 11,333 contended acquires, 17,957,099
microseconds total wait, and 63,869,088 microseconds aggregate hold time.

MM counts were user-root activations 36,620, kernel-root activations 36,307,
page-table writes 72,927, remote shootdowns 5,192 targeting 8,048 CPUs, and no
measured TLB-time fraction.  Block-cache hits/misses were 1,326,011/121,480;
virtio recorded 64,697 requests, 1,902,617,098 bytes, 15,202 microseconds
queue time, and 3,977,134 microseconds completion time.  Host vmstat showed no
swap or I/O wait, iostat showed no sustained saturation, and the final thread
sample contained all eight TCG threads (not one busy QEMU thread).

## Post-report diagnostic coverage attempt (invalid for comparison)

The diagnostic-only MemorySet lock-coverage build compiled locally in both
production and diagnostic configurations, and on the runner. Its first runtime
attempt, `window-run3`, is retained at
`20260806-riscv64-diagnostics-smp8-locks-window300-remote/window-run3/` but is
**not a 300-second sample**: kernel `a211b272...` mounted ext4 and then exited
after `0.405 s` with `No runnable init task` / `init not found in MemFS`.
It emitted no begin marker, diagnostic aggregate, compiler progress, or panic.
The source dirty-diff identity (`02cf9bb...`) differs from the valid run-2
diagnostic kernel, so this result cannot be attributed to the MemorySet
instrumentation and must not update any hotspot conclusion. The runner's known
good diagnostic artifact (`e005e79...`) was restored after preserving the
failed raw log; no QEMU process remained.

## IPC inheritance diagnostic attempt (invalid for comparison)

`20260806-riscv64-diagnostics-smp8-ipc-inheritance-window300/window-run1/`
preserves an exploratory diagnostic-only attempt to count `pipe2`, `fcntl`,
duplication, and close-on-exec lifecycle events. It is **not** a short-window
result: after `914.786982504 s` of host time it had not emitted
`BUILDSTORM_BEGIN mode=multi`, so its marker-window duration is null. It was
then deliberately terminated. It produced 27 pre-marker `Compiling` lines
(last `unwind`), but neither a panic/OOM nor a compile-success marker.

Consequently those 27 lines must not be compared with either valid 300-second
production window and do not change the concurrency or bottleneck
classification. In addition, the actor label for `fcntl` and `dup` was sampled
after acquiring `task.inner`; the non-blocking diagnostic lookup could then
fall back to `other`. The per-actor IPC attribution is therefore unsound and
no cargo/rustc inheritance conclusion is drawn from it. The temporary IPC
lifecycle counters and hooks were reverted; the prior feature-gated futex and
process-count diagnostics remain. Both the default production and
`buildstorm-diagnostics` RISC-V64 release builds pass locally after the
rollback. The pre-existing production artifact must be restored/rebuilt on the
remote runner before any later remote run; no QEMU was started as part of this
local verification.

## Wake-to-run delay measurement (latest diagnostic sample)

`20260806-riscv64-diagnostics-smp8-wake-to-run-window300/window-run1/` is a
valid, diagnostic-only 8-vCPU window that directly measures the interval from
a recorded ready publication to the task's later dispatch/resume. The feature
is explicitly `buildstorm-diagnostics`; it adds fixed-size per-CPU counters,
does not allocate in the event path, and is absent from the restored production
kernel.

The unmodified glibc image and official script emitted `BUILDSTORM_BEGIN
mode=multi`; the runner then measured `300.218740826 s` (host elapsed
`372.109949565 s`). It recorded 23 `Compiling` lines (last
`ax-posix-api v0.5.29`), two `Finished` lines, no panic, and no
`BUILDSTORM_COMPILE mode=multi ok=true` marker. Its diagnostic kernel SHA-256
was `2646b09345ffe10de0228034df1ef5a77614be9ee0de4c579e9cb4133f01b3e7`;
the serial-log SHA-256 is
`4fec598e34e0f322bef564eec6046283237f5f960641bae5f9b19f88d0217cca`.

| Wake category | Count | Aggregate delay | Maximum delay |
| --- | ---: | ---: | ---: |
| block I/O | 25 | 460,232 us | 184,848 us |
| pipe fd ready | 251 | 569,707 us | 84,488 us |
| poll | 1,518 | 2,283,044 us | 183,597 us |
| wait4/waitid | 222 | 3,873,456 us | 413,489 us |
| futex | 2,207 | 499,847 us | 62,275 us |
| signal | 65 | 267,757 us | 64,598 us |

The aggregate wake-to-run delay is about 8.0 seconds across all categories,
far below the overlapping blocked-task totals in the same sample (for example,
wait4/waitid `449,361,998 us` and poll `421,497,149 us`). At the final
snapshot, 21 user tasks were live but only one was runnable; there was one
live rustc process, not runnable, with four rustc TCB threads of which one was
runnable. CPUs 0--7 each had nonzero user ticks, but were strongly imbalanced.
The top lock waits were `memory_set_activation` (`13,831,446 us`),
`memory_set` (`3,957,186 us`), and `frame_allocator` (`1,051,196 us`).

This evidence rejects a generic scheduler wake-delivery production patch as
the first optimization: its measured delay is too small to explain the
aggregate wait. It does not alter the prior conclusion that no production
optimization is admissible until a separate, single-hypothesis candidate
clears the comparable-window retention gate. After this run, with no QEMU or
runner process present, the remote RISC-V64 production artifact was rebuilt
without diagnostics; its SHA-256 is
`4f830974a27dc44ebd4dd81e4f6bc86e9eca7959dacdee3bb72383fb16ef66e6`.

## Blocked-owner continuation measurement (latest diagnostic sample)

`20260806-riscv64-diagnostics-smp8-blocked-owner-chain-window300/window-run2/`
tests one narrower scheduler hypothesis: a blocking user syscall retains an
owner CPU while its synchronous continuation waits, and a wake may notify that
owner rather than publish globally. The feature-gated counters record only
per-CPU aggregate chain depth, owner-loop dispatches, and the two wake
destinations. They do not alter production scheduling, allocate, or emit
per-event output. The runner parser preserves the final aggregate under
`guest_aggregate_final.blocked_owners`; raw serial remains authoritative.

The first launch in this directory (`window-run1`) reached a valid 300-second
BuildStorm window but is **invalid for this hypothesis**: the new source files
were accidentally copied to the remote repository root, so the old diagnostic
kernel produced no `blocked_owner` line. It is retained solely as deployment
audit evidence and is excluded below. The corrected source hashes were checked
after copying to their real paths, then `window-run2` used diagnostic kernel
SHA-256 `43e02ee2958419c7dfc5b8e72bd0257a12cea88a2c1f3793c75c91babf3091f6`.

`window-run2` used the unmodified glibc image and official script, reached
`BUILDSTORM_BEGIN mode=multi`, then ran `300.208788022 s` (host elapsed
`367.096351426 s`). It recorded 23 `Compiling` lines, last
`ax-posix-api v0.5.29`, two `Finished` lines, no panic, and no
`BUILDSTORM_COMPILE mode=multi ok=true`. The serial SHA-256 is
`6a96d2aea445f469a1572235d7b476d9847789e460cae7550481564b3ded7c2a`.
Like all diagnostic windows, its crate-event count is not compared with the
feature-off production baseline.

These counters begin at kernel boot and include the pre-marker setup period;
they are supporting attribution rather than an exact marker-window total.
Across CPUs 0--7, blocked-owner entries/exits were `4,436/4,435`; the one
unmatched entry is the in-progress wait cut off when the host terminated QEMU.
The maximum observed nested owner depth on any CPU was only 2. The owner-loop
executed 5,272 one-boundary task dispatches, spread across CPUs 0--7 rather
than concentrated on one CPU. Of 4,223 classified wakes, 3,070 (72.7%) were
published to the global ready queue and 1,153 (27.3%) targeted an owner CPU.

This rejects the simple claim that retained blocked-owner continuations are
serializing all BuildStorm work onto a single CPU. It neither proves that the
current continuation design is ideal nor licenses a scheduler rewrite: moving
or reworking a blocked syscall continuation would be a larger correctness
change, while the measured owner-chain depth and wake split do not meet the
evidence threshold for it as the first production patch. The remote
RISC-V64 artifact was then rebuilt with diagnostics disabled; its SHA-256 is
`9ac044ddd3772c664c22d82caaaa2d0bad826960e99583999d9dc4fe80e3d32e`.

## Hardware page-fault source attribution (latest diagnostic sample)

`20260806-riscv64-diagnostics-smp8-fault-source-window300/window-run1/` is a
valid diagnostic-only 8-vCPU window that distinguishes hardware user-trap
faults from the same `MemorySet::handle_page_fault()` routine invoked by
kernel user-buffer preparation.  The feature-gated counters use fixed
per-CPU storage, perform no allocation on the event path, and report only in
the existing aggregate diagnostic output; production builds leave them off.

The unmodified glibc image and official script emitted `BUILDSTORM_BEGIN
mode=multi`; the runner then measured `300.208700851 s` (host elapsed
`367.896378734 s`). It recorded 23 `Compiling` lines, last
`ax-posix-api v0.5.29`, two `Finished` lines, no panic, and no
`BUILDSTORM_COMPILE mode=multi ok=true`. The diagnostic kernel SHA-256 was
`39c2e39a281927aecd9b2f6ff958aac0e2e5f5ba04d859ad2e13ebaf1756f386`; the
serial SHA-256 is
`41079b116ff2cb6b4f650f948593efe1a3177e169e930ba2d28598fb36788e10`.

These source counters begin at kernel boot and therefore include the
pre-marker setup period; they are attribution evidence, not exact
marker-window accounting. During this run the remote runner's parser had not
yet been updated for the new lines, but the raw serial log was preserved and
the updated local parser replayed it successfully.

| Fault source | Count | Aggregate handling time |
| --- | ---: | ---: |
| Hardware load fault | 10,356 | 6,912,806 us |
| Hardware store fault | 130,813 | 73,869,039 us |
| Hardware instruction fault | 18,994 | 14,147,930 us |
| `prepare_read` | 6 | 9,607 us |
| `prepare_write` | 3,221 | 1,653,905 us |

The prior aggregate store-fault measurement is therefore genuinely dominated
by hardware trap-driven store faults, not by `prepare_write` copies in kernel
syscall paths. This is the strongest remaining measured MM lead. It is not
yet a production patch: the next step is to inspect the anonymous/COW/lazy
mapping fault path and formulate exactly one general hypothesis before any
feature-off change. After this diagnostic run the remote RISC-V64 production
artifact was rebuilt with diagnostics disabled; its SHA-256 is
`cc2bff815f8eb43ba33a6727791acfbb65b4f2894f02783db18cda9de98b3940`.

## Page-fault resolution classification (latest diagnostic sample)

`20260806-riscv64-diagnostics-smp8-fault-resolution-window300/window-run1/`
adds one further feature-gated, fixed-size aggregate: every successful
`MemorySet::handle_page_fault()` resolution is classified as COW, anonymous
demand, resident-file, clean-file-cache, existing-mapping, existing-frame, or
backing-read. It performs no allocation and emits only in the existing
10-second aggregate report. This tests the narrow hypothesis that the large
hardware-store total is mainly COW work.

The unmodified glibc image and official script reached `BUILDSTORM_BEGIN
mode=multi`; the runner measured `300.2047614300027 s` after that marker
(host elapsed `375.1018185770008 s`). The serial has 22 `Compiling` lines,
last `ax-posix-api v0.5.29`, two `Finished` lines, no panic, and no
`BUILDSTORM_COMPILE mode=multi ok=true`. The diagnostic kernel SHA-256 is
`6a94ee371fb6a53650b37a57724348309e728271e9d3c16ac025afe391bd8628`; serial
SHA-256 is
`9617cad8e7e522d71e0fe2302844991df4f7d62f793c585b4c7249340b3dab95`.
As with the other diagnostic runs, counters start at boot and include the
pre-marker setup period.

The sibling host wrapper status file records exit `2`: a malformed shell
process-check expression emitted `run_buildstorm_window.py: command not found`
after the runner had already saved its serial, launch metadata, and summary.
The saved runner summary independently records `marker_seen: true`, the
300.2047614300027-second marker window, and no panic. The raw serial is
preserved and remains useful for diagnostic attribution, but this run is not
used for an official result or any production progress comparison.

| Successful fault-resolution branch | Count | Aggregate handling time |
| --- | ---: | ---: |
| Anonymous demand | 118,182 | 77,321,090 us |
| COW | 12,603 | 9,704,786 us |
| Clean-file cache | 27,736 | 21,114,639 us |
| Existing mapping | 180 | 859 us |

At the same final snapshot, hardware store faults were 126,880 calls and
85,255,638 us. COW is therefore only about 10% of the successful store-fault
resolution count and 11% of its aggregate time; it is not the dominant first
hotspot. Anonymous demand resolution is the leading measured branch. This
rejects a COW-specific first production patch.

The next single production hypothesis is consequently narrow and general:
the current eight-page anonymous-demand provisioning window is too small for
the sequential anonymous writes made by the concurrent compiler processes;
increasing only that window may amortize trap, area-splitting, and page-table
work. It must be tested against the feature-off SMP8 300-second baseline and
retained only under the stated ≥15%, or 5--15% plus hotspot reduction,
threshold. The diagnostic 22-line result is not compared with the production
baseline. The remote production artifact was rebuilt without diagnostics
after the diagnostic run; its SHA-256 is
`364aef4290c60270fd47b356c68e33a20244862cd30a217b16a39d9f58c2f4d0`.

## Rejected production candidate: anonymous fault window 8 → 16 pages

After the classification report, exactly one feature-off production change
was tested: `ANONYMOUS_FAULT_WINDOW_PAGES` was increased from 8 to 16. This is
a general MM policy change, with no workload, crate, command, output, or score
branch. The unmodified glibc image and official script ran under the required
`-snapshot -m 8G -smp 8` configuration in
`20260806-riscv64-production-smp8-anonymous-fault-window16-window300/window-run1/`.

The valid marker window was `300.20156868800404 s` (host elapsed
`363.0791722970025 s`), with no panic and no compile-success marker. It
recorded 23 `Compiling` lines, last `ax-posix-api v0.5.29`, and two `Finished`
lines. The production candidate kernel SHA-256 is
`b65f475ab7d7d04ba717c10ecb7876e50732f6b45e49aece805ba330c288b442`; serial
SHA-256 is
`7d97eb118430dd6cf39912327e7ed586fcb23c8b0ac91ab9f766b12ffc9ec93e`.

The comparable feature-off SMP8 baseline had 23 `Compiling` lines in
`300.180 s`; the candidate also had 23 in `300.202 s`, for 0% progress
improvement by this coarse event metric. It therefore misses the 5% minimum
retention threshold and was immediately reverted to eight pages. No
cross-architecture or full-run gate was started for this rejected candidate.

## Anonymous fault phase attribution (latest diagnostic sample)

`20260806-riscv64-diagnostics-smp8-anonymous-fault-phase-window300/window-run1/`
adds only two feature-gated aggregate scopes within successful anonymous-demand
fault processing: frame allocation and the following VMA split/page-table
installation/coalescing portion. The same sample also records the actual
number of pages supplied per successful resolution. It allocates nothing in
the counting path and retains the existing ten-second reporting cadence.

The unmodified image and script reached `BUILDSTORM_BEGIN mode=multi`; the
marker window was `300.2014424200024 s` (host elapsed `371.8906044589967 s`).
It had 23 `Compiling` lines, last `ax-posix-api v0.5.29`, two `Finished`
lines, no panic, and no compile-success marker. The diagnostic kernel SHA-256
is `386fdfedf145f69372c9ff979f659754d8ed33b52bb29be49cba65ccb5eebd50`;
serial SHA-256 is
`4b6ebd147993f7d2c30164f51b30bd76ce2c4f4049520a55e63be357db0f71c7`.

| Anonymous-demand subpath | Count | Aggregate time |
| --- | ---: | ---: |
| Frame allocation | 124,117 | 8,808,951 us |
| VMA split, mapping, and coalescing | 124,117 | 60,704,142 us |

Anonymous demand resolved 255,711 pages in 124,117 calls, only 2.06 pages per
call on average. The `MemorySet` lock in the same final snapshot had
57,639,666 us aggregate hold time. This explains the failed 8→16-page trial:
the current workload often reaches the end of a small VMA, so increasing the
maximum window does not materially reduce its fault work. It also supports a
more specific, representation-level hypothesis: after a page-fault split, the
area vector is already ordered, so re-sorting and rebuilding the entire VMA
vector to merge only the changed area and its neighbors is unnecessary. That
single general production hypothesis is the next candidate; no other subsystem
is changed.

## Rejected production candidate: local anonymous VMA coalescing

Exactly one feature-off production candidate replaced the full
`coalesce_areas()` call only after anonymous-demand fault installation with a
neighbor-only merge. The intent was to avoid sorting and rebuilding the whole
already ordered VMA vector. No mapping flags, page-table operation, scheduler,
VFS, block cache, image, guest script, judge, marker, or clock behavior was
changed.

`20260806-riscv64-production-smp8-anonymous-local-coalesce-window300/window-run1/`
used the official glibc image/script and required `-snapshot -m 8G -smp 8`.
It reached the marker and ran `300.1922354480048 s` (host elapsed
`377.0852494659921 s`) without panic or compile-success marker. It recorded
23 `Compiling` lines, last `ax-posix-api v0.5.29`, and two `Finished` lines.
Candidate kernel SHA-256 is
`e4ea130cf127f87fd1d0acc01d3af1e058346c2c44998b0b1e4648394552941a`;
serial SHA-256 is
`fa74777b3a6d5d776de10bb05c143ff0acb7cf53e9a2587eaff9b43a8c75a40c`.

The feature-off baseline is 23 `Compiling` lines in `300.180 s`; this
candidate is also 23 in `300.192 s`, a 0% event-progress change. It is below
the mandatory 5% threshold and has been reverted immediately. No follow-up
cross-architecture, regression, full production BuildStorm, or judge run was
started for this rejected candidate.

## Detailed anonymous-fault attribution

The later diagnostic sample
`20260806-riscv64-diagnostics-smp8-anonymous-fault-install-detail-window300/window-run1/`
separates VMA splitting, page-table mapping, and VMA coalescing inside the
anonymous-demand installation path.  It is diagnostic-only, aggregate,
allocation-free on the event path, and does not alter the production kernel.

In its 300-second marker window, anonymous demand completed 123,278 times and
supplied 254,794 pages (2.07 pages per resolution).  Its aggregate time was
70,111,029 us: allocation 9,034,271 us; VMA split 1,373,906 us; page-table map
1,450,512 us; and VMA coalescing 57,798,240 us.  The detailed serial SHA-256 is
`f986e7bf6df26d792d986bca77eb2d2878365fee2e723642f5164dfeca3527ca`.

This identifies coalescing as the dominant *measured anonymous-fault subpath*,
but not as a retained throughput improvement: the one general local-coalescing
candidate above produced the same 23 `Compiling` lines as the feature-off
baseline and was reverted.  The result therefore remains evidence for further
diagnosis, not permission to claim a production optimization.

## Rejected production candidate: foreground empty-ready wait

Static audit found that `run_user_task_foreground()` repeatedly fetched the
global ready queue while it was empty.  A narrowly scoped, feature-off,
general scheduler candidate replaced only that empty iteration with the
existing WFI/idle protocol.  It did not inspect process names, commands,
paths, test output, scores, or markers, and it did not modify the official
image, script, judge, or guest clock.

`20260806-riscv64-production-smp8-foreground-idle-wait-window300/window-run1/`
used the unmodified glibc image/script with `-snapshot -m 8G -smp 8`.  Its
marker window was `300.186467469 s`; it recorded 23 `Compiling` lines, two
`Finished` lines, and last crate `ax-posix-api`, with no panic and no
compile-success marker.  This is the same coarse progress as the feature-off
baseline (23 lines in 300.180 s), or 0%.  The candidate was therefore reverted
immediately; no cross-architecture, regression, full BuildStorm, or judge gate
was started.  Candidate kernel SHA-256:
`59e7df3bb04d03289801d9acd6d6bfd58cb809923a5155c73115aec606ce2fdb`;
serial SHA-256:
`8cb63c710babaf57e52c8653dadc5a52c3b6f6a7db79e402412f24c654f6643a`.

## Corrected compiler-concurrency interpretation

The process-counting diagnostic explicitly counts a thread-group leader once,
separately from rustc worker threads.  In the detailed sample the maximum was
three live rustc processes and one runnable rustc process (snapshot 20), while
the maximum thread-level count was nine live rustc threads and three runnable
threads.  Later snapshots commonly show four live rustc threads belonging to
one rustc process, with only one runnable thread.  Future reports must use the
process metric for concurrent crate compilation and reserve the thread metric
for compiler-internal parallelism.

## User-entry duration diagnostic: incomplete coverage

`20260806-riscv64-diagnostics-smp8-user-run-boundary-window300/window-run1/`
was a valid 300.199-second official-image/script diagnostic window with no
panic or successful compile marker.  It reached 23 `Compiling` lines, two
`Finished` lines, and last crate `ax-posix-api`; its host elapsed time was
370.888 seconds.  The production kernel was rebuilt with diagnostics disabled
afterward (SHA-256
`ba0f8a8c29e2351cfa67d9ca81089a39f2f6398449b52f744ae09e5898453992`).

The newly added aggregate user-entry counter was zero on every CPU.  Static
call-path audit explains why: the foreground blocked-owner continuation can
run work through `run_current_user_task_one_boundary()`, while the new counter
was attached only to `run_current_user_task_until_reschedule()`.  The window
therefore does not distinguish user computation from kernel re-entry time and
must not be used to attribute a root cause or justify a production patch.  The
raw serial, launch metadata, and host samples are retained; a corrected
feature-gated counter must cover both user-entry helpers before repeating this
single timing hypothesis.

## User-entry duration diagnostic: corrected coverage

`window-run2` repeated the same diagnostic hypothesis after extending the
feature-gated aggregate counter to the blocked-owner single-boundary helper.
It used the unmodified image and official script, reached the marker, and ran
`300.205139941 s` without panic or compile success.  It recorded 22
`Compiling` lines, two `Finished` lines, and last crate `ax-posix-api`; this is
diagnostic attribution, not a production comparison.

Across CPUs 0--7, `run_user_task()` accounted for 428,865 returns and
600,117,519 us in total: 254,100 syscall returns, 14,911 timer returns, 1,102
IRQ returns, and 158,752 other returns.  The scope includes resumed user work
and its returning trap callback, so it is not a pure user-time measurement;
nevertheless its total is only about two CPU-equivalents over the 300-second
window.  At the final snapshot there was one runnable rustc TCB, 21 live user
tasks with 20 blocked, and an empty global ready queue.

Together with the host sample (all eight TCG threads busy but roughly 55% host
CPU idle), this rules out host CPU saturation and a first lock/device/MM/TLB
hotspot as the explanation for the missing compile marker.  The verified
throughput boundary is insufficient runnable Cargo/rustc work, especially the
late one-runnable-rustc phase.  The evidence does **not** yet establish why
Cargo/rustc has that dependency or synchronization shape, so no production
optimization is justified.  The next valid investigation is a narrowly scoped
jobserver/pipe/futex dependency-state diagnostic, not a scheduler, VFS, MM, or
block-cache rewrite.

## Cargo jobserver and pipe-FD lifecycle audit

`20260806-riscv64-diagnostics-smp8-ipc-jobserver-window300/window-run1/`
completed a `300.205731961 s` marker window on the unmodified official image
and script.  It recorded 23 `Compiling` lines, two `Finished` lines, last crate
`ax-posix-api`, no panic, and no successful compile marker.  Serial SHA-256:
`89e70ff4ec6f2ed6935c574c5d06e2ced1075aae2726099a6590f93446f4a5ed`.

The aggregate lifecycle evidence does not show a lost Cargo jobserver channel:
143 `pipe2` calls were made, 137 with `O_CLOEXEC`; 401 CLOEXEC pipe endpoints
were inherited at exec boundaries and all 401 were closed.  There were 318
`sched_getaffinity` calls, every observed maximum mask had eight CPUs, and the
sum of returned mask popcounts was exactly 2,544.  Pipe reads and writes made
forward progress (2,783,377 bytes read and 2,783,622 bytes written), with 382
read waits, 113 write waits, 1,584 readers woken, and 310 writers woken.

Static audit did find a general Linux OFD semantic defect: pipe `O_NONBLOCK`
was copied into each `FileDescriptor` during `dup`/fork instead of shared by
all aliases of the same open file description.  The workload exercised 70
pipe `F_SETFL` calls; 14 occurred while the endpoint had aliases in another fd
table.  This proves that the ABI defect is reachable, but not that it limits
BuildStorm throughput.

## Rejected production candidate: shared pipe OFD nonblocking state

The smallest general fix changed only pipe endpoint `O_NONBLOCK` storage to a
shared open-file-description value.  It did not inspect process or workload
names and did not touch scheduling, MM, VFS, block I/O, the image, script,
judge, marker, or guest time.

`20260806-riscv64-production-smp8-pipe-ofd-nonblock-window300/window-run2/`
ran production with diagnostics disabled for `300.198619910 s`.  It produced
23 `Compiling` lines, two `Finished` lines, and the same last crate
`ax-posix-api`, with no panic or compile-success marker.  This is 0% coarse
progress improvement, so the candidate was reverted immediately.  Kernel
SHA-256: `50708bf6e2e76082f685cefe7fbeac7f9f92e1ba5a91516d1dbcc4fe1408e180`;
serial SHA-256:
`34485c3f3385ad3f8edf8f56dc14273fc878a7d60a2631252a64283155372e2c`.
No cross-architecture, regression, full BuildStorm, or judge gate was started.

This rejects pipe/OFD state propagation as the first BuildStorm performance
hotspot.  The ABI defect remains a correctness issue for later independent
work, but it does not satisfy the performance retention rule in this campaign.

## Timed blocked-owner audit and rejected scheduler candidate

Static audit of the foreground scheduler found that timed waits retain a CPU
as a blocked-syscall owner and dispatch other ready tasks through
`run_current_user_task_one_boundary()`.  A feature-gated diagnostic added only
per-CPU totals for owner-loop duration and empty iterations.  Its run at
`20260806-riscv64-diagnostics-smp8-timed-block-owner-window300/window-run1/`
did not reach `BUILDSTORM_BEGIN`; it was stopped after the untimed `tg-xtask`
stage remained in one rustc phase for about 19 minutes.  Therefore it is
**unverified marker-window evidence** and must not be compared as a 300-second
result.  The retained pre-marker serial SHA-256 is
`1dae1f852ccaa466d8afe2549ee2885b6715e3d0b410bccab2a6302e91b8a351`.

Before termination, the final aggregate snapshot recorded 2,958 timed owner
loops, 968,346,596 us of overlapping owner-loop residence, 56,679,312 empty
iterations, and 126,045 ready-task single-boundary dispatches across CPUs 0--7.
This demonstrates substantial scheduler activity, but the owner-loop duration
includes user work dispatched on behalf of blocked tasks and cannot be treated
as pure wasted CPU time.

A production-only candidate then changed just the blocked-owner dispatch helper
to the normal `run_current_user_task_until_reschedule()` boundary.  The valid
production window
`20260806-riscv64-production-smp8-blocked-owner-normal-boundary-window300/window-run1/`
ran `300.182474262 s` and again produced 23 `Compiling` lines, two `Finished`
lines, and last crate `ax-posix-api`, with no panic or successful compile
marker.  This is 0% coarse progress improvement and the candidate was reverted.
Kernel SHA-256:
`7e7cd0b024c52b3ea3f361498b9061230c9e43eac1fa0aff779d3dc54ebb180f`;
serial SHA-256:
`93004e5ce111f36350734198a424473431e5b9aec353541d629b40263a971cc5`.

The current verified root-cause boundary is therefore narrower but not a
single retained production fix: Cargo sees eight CPUs, pipe jobserver traffic
and exec/CLOEXEC lifecycle make progress, wake-to-run delays are small, and
neither pipe OFD propagation nor blocked-owner trap granularity changes the
300-second production progress.  The observed late phase is genuinely limited
to one runnable rustc thread/process; while it runs, anonymous demand-fault
installation remains the largest measured kernel path.  Exact Cargo/rustc
dependency serialization versus compiler-internal single-thread work remains
unproven, so claiming a final kernel root cause or compile success would exceed
the evidence.

## Retained production candidate: range-local `mprotect` processing

Later timing coverage identified a separate, directly actionable MM hotspot.
In the valid diagnostic baseline
`20260806-riscv64-diagnostics-smp8-locks-window300-remote/window-run4/`,
55,650 `mprotect` calls consumed 31,569,748 microseconds in aggregate, or about
567 microseconds per call.  This was not TLB cost: the separately instrumented
address-space activation and shootdown sections were below one millisecond per
maximum sample and below one second in aggregate over a comparable window.

Static audit found the algorithmic cause in `MemorySet::protect_range()`.  The
VMA vector is maintained in address order and the syscall changes only the
VMAs overlapping its requested range.  Nevertheless, every non-no-op call
did both of the following while holding the task's `MemorySet` mutex:

1. scan every VMA after splitting the two range boundaries; and
2. call the global `coalesce_areas()`, which sorted the complete VMA vector,
   moved it through `mem::take`, allocated a replacement vector, and copied or
   merged every VMA.

With tens of thousands of calls, this converted a range-local permission
operation into repeated whole-address-space work.  It also lengthened the
critical section seen by user-entry activation, page-fault, mmap, and process
memory paths.  The root cause is therefore **O(all VMAs) global maintenance on
each local `mprotect` update, amplified by serialization under the per-process
MemorySet lock**.  The workload's repeated `mprotect` calls expose the issue;
no process, crate, command, artifact, output, or score identity is involved in
the diagnosis or fix.

The retained implementation starts at the first overlapping VMA, stops at the
end of the changed range, and coalesces only the changed interval plus its two
neighbour boundaries.  All operations that can add, remove, or reorder
arbitrary VMAs continue to use the global coalescer.  The local algorithm is
consistent with the existing sorted-vector invariant used by
`area_index_containing()` and `first_area_ending_after()`: splitting inserts
the right half immediately after the left half, changing flags does not change
addresses, and the local merge checks both the left and right boundary.

The comparable feature-gated diagnostic run is
`20260806-riscv64-diagnostics-smp8-mprotect-range-coalesce-window300/window-run1/`.
It recorded 55,737 `mprotect` calls in 2,050,299 microseconds, about 36.8
microseconds per call.  Aggregate `mprotect` time fell by approximately 93.5%.
Combined MemorySet lock hold time fell from 63,869,088 microseconds in the
baseline to 33,794,371 microseconds across the later split
`memory_set_activation` and `memory_set` classes; this comparison is
supporting evidence because the lock reporting layout changed between those
diagnostic builds.

The production, diagnostics-disabled timeline comparison uses:

* baseline:
  `20260806-riscv64-production-smp8-timeline-baseline-window300/window-run1/`;
* candidate:
  `20260806-riscv64-production-smp8-mprotect-range-coalesce-timeline-window300/window-run1/`.

Both windows reached the same 23rd `Compiling` event, `ax-posix-api`, but the
baseline reached it at 139.770461 seconds after the official begin marker and
the candidate at 123.749086 seconds.  That is an 11.46% earlier arrival.  The
first timed-batch crate moved from 130.157906 to 113.134767 seconds, and the
preceding `Finished dev` line moved from 20.425481 to 18.823232 seconds.  No
panic or correctness regression was observed.  This satisfies the stated
retention rule: a 5--15% progress improvement accompanied by a material fall
in the first measured hotspot.

The feature-off dual-architecture release builds, RISC-V64 and LoongArch64 SMP
regressions, and official toolchain/minibuild checks have passed.  Evidence is
retained under `20260806-mprotect-range-coalesce-gates/`.  The subsequent
complete production RISC-V64 run used the unmodified official image and
script, `-snapshot -m 8G -smp 8`, diagnostics disabled, and a 15,000-second
outer timeout.  Its complete copied evidence is
`20260806-riscv64-production-mprotect-range-coalesce-15000-official/`.

The runner exhausted the full outer allowance after `15000.071406268005`
seconds (`4:10:00` in GNU time) and exited with status 1.  It emitted
`BUILDSTORM_TOOLCHAIN ok`, `BUILDSTORM_MINIBUILD ok`, and
`BUILDSTORM_BEGIN mode=multi`, but no `BUILDSTORM_COMPILE` result.  There was
no exact `Kernel panic`, `panicked at`, or OOM marker.  The final serial log
contains 33 `Compiling` events and two `Finished` lines, ending at
`rustc-literal-escaper v0.0.7`.  Therefore this run is a verified
**15,000-second timeout with sustained computation**, not an official compile
pass; the compile result and compile-time score remain zero/unverified.

The official judge exited successfully and awarded 8 toolchain + 12 minibuild
= 20.0 / 180.0 scripted points, with compile `ok=missing`.  The kernel SHA-256
was `90b202ab13226a9b15d47001a663c28e66c156b82e6b5f2b17a48c9709643ffb`;
the image SHA-256 remained
`d74e436522f5946ca17280a7a25f17dbb6604b71fe675bb8a021ce8e849b334c`;
the copied serial-log SHA-256 is
`ceeef65c133fa0b66eb2933bc8bc167747ebe64082c7f3c0b30299065d7df117`.
The source commit was `4d6f10506d1bff10bb07e0f569daa86362459566` and
the official suite commit was `b5ec6ef8497e1818cbdec3b54bb722f036e57972`.

Host evidence rules out an environmental stop: GNU time reports 766% average
CPU and 3,301,060 KiB maximum RSS with zero swaps.  Across 14,998 vmstat
samples, maximum `swpd`, `si`, `so`, and I/O wait were all zero; average host
user/system/idle percentages were 43.55/4.52/51.85 on the 16-CPU host.  The
two NVMe devices averaged 0.054% and 0.125% utilization, with maxima of 6.4%
and 1.2%.  Live samples during the run showed all eight QEMU TCG threads
runnable at roughly 77--99% of a host CPU.  The saved pidstat file contains
headers but no process data because its command-name filter did not match the
truncated QEMU comm; this provenance defect is disclosed rather than filled
with reconstructed data.

After removing the `mprotect` amplification, the largest measured internal MM
subphase in the diagnostic candidate is anonymous-demand-fault coalescing
(61,031,528 microseconds over 129,069 samples).  A previous standalone local
coalescing experiment did not clear the 5% production retention threshold, so
it remains rejected rather than being combined speculatively with this patch.
It may be reconsidered only as the next single hypothesis against the retained
candidate baseline after the active complete run finishes.

The next-hypothesis audit is more specific.  In the retained-candidate
diagnostic window, anonymous allocation took 9,236,135 microseconds, VMA
splitting 1,596,541 microseconds, page mapping 1,469,803 microseconds, and the
subsequent global VMA coalescer 61,031,528 microseconds.  Coalescing therefore
accounts for approximately 94.8% of the measured anonymous-install path and
about 473 microseconds per sampled fault window.  The code splits one already
sorted VMA around an eight-page allocation and then calls `coalesce_areas()`,
which sorts and rebuilds the complete VMA vector even though only the inserted
window and its immediate neighbours can have become mergeable.

This is the next admissible single production hypothesis after the retained
candidate timed out.  The earlier production-only neighbour merge
run reached its 23rd crate at 138.165678 seconds versus 139.770461 seconds for
the original timeline baseline, only about 1.15% earlier and below the 5%
retention floor.  It also did not include the now-retained `mprotect` change,
so that result cannot establish the marginal benefit after the first hotspot
has been removed.  The next valid experiment is exactly one comparable
production window: preserve the retained `mprotect` patch, change only
anonymous-fault coalescing, and compare its timeline with the retained
candidate value of 123.749086 seconds for the 23rd crate.  No scheduler, VFS,
TLB, or block-cache change is included in that run.

### Deeper anonymous-fault root-cause audit

The representation explains why this cost scales badly.  `MapArea` stores
resident pages as one dense `frames` vector beginning at the VMA start.  It
cannot represent holes inside one area.  An anonymous demand fault therefore
has to split an empty area around the newly resident window, install the
frames in the middle fragment, and then attempt to merge fragments again.
`split_area_at()` inserts the right fragment next to the left fragment and
preserves address order.  Changing only the middle fragment's frame vector
cannot make any non-neighbouring pair mergeable.  Nevertheless,
`coalesce_areas()` sorts every area and moves the complete vector into a new
vector after every sampled anonymous window.  This is repeated global work
for a strictly local metadata change.

The ten-second diagnostic deltas show that the cost is not just a large boot
constant.  Before the main rustc phase, individual intervals varied widely;
once the rustc process was active, the coalescer commonly consumed about
423--768 microseconds per anonymous window.  The final cumulative sample was
57,798,240 microseconds for 123,278 windows in the install-detail run, and the
retained-mprotect diagnostic measured 61,031,528 microseconds for 129,069
windows.  This supports an algorithmic VMA-fragmentation cost rather than
frame allocation, page-table mapping, or device I/O as the immediate cause of
the anonymous-install time.

There is also a diagnostic coverage gap relevant to the observed SMP
behaviour.  The hardware page-fault path in `trap/mod.rs` acquires
`task.memory_set` directly with `spin::Mutex::lock()` and holds it through
`handle_page_fault()`.  It does not use the feature-gated
`buildstorm_memory_set_lock!` wrapper, so the reported MemorySet lock wait and
hold totals exclude this hottest acquisition path.  User-entry activation
does acquire the same per-process mutex through its separately measured lock
class.  A long anonymous-fault coalescer can therefore serialize sibling
threads of one process and make other vCPUs spin while waiting to reactivate
the address space.  That mechanism is consistent with the simultaneous high
host CPU use on all eight TCG threads and poor serial progress, but it is an
inference: the active production run has diagnostics disabled and cannot
directly distinguish useful guest user execution from guest spin-lock time.

The complete run has now failed to produce a successful compile marker, so the
next single production hypothesis is local anonymous-fault coalescing on top
of the retained `mprotect` patch.  A later diagnostic-only confirmation, if
needed, should route only the hardware page-fault acquisition through the
existing feature-gated lock wrapper and record current/max VMA count; it must
not alter the production lock type or scheduling behaviour.

### Rejected combined-baseline candidate: anonymous local coalescing

The exact one-line candidate was tested after the complete-run timeout: the
anonymous-demand branch called
`coalesce_changed_range(page_start, window_end)` instead of the global
coalescer.  No other production path changed.  The production,
diagnostics-disabled evidence is
`20260807-riscv64-production-smp8-mprotect-anonymous-local-coalesce-window300/window-run1/`.
It used the unmodified image and script with `-snapshot -m 8G -smp 8`, ran
`300.19798733301286` seconds after the official begin marker, reached 23
`Compiling` events, and emitted no panic or successful compile marker.  Its
kernel SHA-256 was
`b538e01edb4ce4fd8bc4e57bb03dfc8026e67ebfebd55610cac753b4bdc3b819`.

The 23rd crate arrived at `132.76079657900846` seconds versus
`123.74908572799177` seconds for the retained-mprotect baseline: 9.011711
seconds or **7.28% later**.  The first timed-batch crate likewise moved from
`113.13476717598678` to `123.3483340810053` seconds, 9.03% later.  This is
below the 5% retention floor and in the wrong direction, so the candidate was
immediately reverted locally and remotely.  The restored remote production
kernel was rebuilt without diagnostics; its SHA-256 is
`058828172e18a49868582a4fa691b98f3e164577797743546bee3ab678c0750f`.

The rejection refines the root cause.  The local helper still calls
`Vec::remove(index + 1)` for every merge.  Removing an element from the middle
of the VMA vector shifts the complete trailing suffix, so the attempted
"local" operation remains O(number of later VMAs) and can move almost as much
metadata as the global rebuild.  Eliminating the sort alone does not eliminate
the dominant vector movement.  A further production patch is not justified
until a feature-gated diagnostic measures whether anonymous windows are
usually adjacent to an already resident left or right area and records current
and maximum VMA count.  That evidence can distinguish an in-place extension
fast path from a larger sparse-residency representation change.

### Anonymous VMA adjacency diagnostic

The required feature-gated follow-up is retained under
`20260807-riscv64-diagnostics-smp8-anonymous-vma-adjacency-window300/window-run1/`.
It used the unmodified official image and guest script with
`-snapshot -m 8G -smp 8`.  The host runner observed the official begin marker,
then ran for `300.20609984100156` seconds and terminated QEMU.  It reached 23
`Compiling` events and two `Finished` lines, ending at `ax-posix-api`; there
was no `BUILDSTORM_COMPILE` result and no exact panic or OOM marker.

The diagnostic event path contains only fixed relaxed atomics and emits one
cumulative line through the existing ten-second reporter.  Immediately before
anonymous `split_area_at()` calls it classified whether the populated window
would be address-adjacent, permission-compatible, anonymous-backed, and next
to a fully resident left or right VMA.  The final emitted aggregate was:

* installs: 130,059;
* left mergeable: 71,653;
* right mergeable: 56,815;
* both mergeable: 6,189;
* neither mergeable: 7,780;
* maximum VMA count observed for one address space: 1,976;
* pages installed: 263,917, with a maximum of eight per window.

The disjoint distribution is 65,464 left-only, 50,626 right-only, 6,189 both,
and 7,780 neither.  Thus 122,279 of 130,059 sampled installations, **94.02%**,
had at least one usable resident neighbour.  This directly supports one next
production hypothesis: extend an adjacent fully resident anonymous VMA in
place and shrink the empty VMA, falling back to the existing split and global
coalescer only for the 5.98% neither-adjacent case.  It does not justify a
scheduler, VFS, TLB, block-cache, or general VMA-container rewrite in the same
candidate.

The diagnostic kernel SHA-256 was
`9a3e31561b8e57cdd40a8304792e72e027910f113ebb9823821a4433fa456d43`.
After evidence capture, the remote kernel was rebuilt without
`buildstorm-diagnostics`; the restored feature-off artifact SHA-256 is
`33d80cf18b1d4c776a65041090337c233ac61c883e7e4a85a939773e8bcd22fa`.

### Rejected adjacent-resident extension candidate

The adjacency result justified one production candidate, retained only long
enough for the required comparable window.  The anonymous demand-fault branch
preferred a compatible fully resident left VMA, otherwise a compatible right
VMA, extending it in place and shrinking or removing the empty VMA.  The
existing split plus global coalescer remained the fallback.  No scheduler,
VFS, TLB, block-cache, window-size, or diagnostic behaviour was changed in
the production build.

Evidence is retained under
`20260807-riscv64-production-smp8-mprotect-anonymous-adjacent-extend-window300/window-run1/`.
The feature-off run used the unmodified official image and script with
`-snapshot -m 8G -smp 8`, ran `300.20164946099976` seconds after the official
begin marker, reached 23 `Compiling` events and two `Finished` lines, and had
no exact panic, OOM, or successful compile marker.

The retained-mprotect baseline reached the first timed crate at
`113.13476717598678` seconds and the 23rd crate at
`123.74908572799177` seconds.  The adjacent-extension candidate reached them
at `123.14731684100116` and `133.96255563500745` seconds respectively.  It was
therefore 8.85% slower at the first timed crate and 8.25% slower at the 23rd
crate.  This is below the 5% retention floor and in the wrong direction, so
the candidate was immediately reverted locally and remotely.

The restored `memory_set.rs` SHA-256 is
`8e8f81cd8a20cda2e204fa83043d841cf2ea09236c94ab396d825634fb89b757`,
and the rebuilt remote feature-off kernel SHA-256 is again
`33d80cf18b1d4c776a65041090337c233ac61c883e7e4a85a939773e8bcd22fa`.
The result shows that adjacency frequency alone is insufficient: moving or
growing dense frame vectors and the remaining VMA removal/shrink operations
can offset the avoided coalescer work.  A further production patch is not
justified without a new, single diagnostic hypothesis that distinguishes
frame-vector growth/copy cost from VMA metadata movement.

### Anonymous frame/VMA movement diagnostic

The next feature-gated window is
`20260807-riscv64-diagnostics-smp8-anonymous-move-shape-window300/window-run1/`.
It kept the original production anonymous-install algorithm and added only
fixed aggregate counters for `Vec` spare capacity, projected resident-frame
movement, empty-VMA shrink versus removal, and the length of the suffix that a
removal would shift.  The official-image SMP8 marker window lasted
`300.2081285180029` seconds, reached 23 `Compiling` events and two `Finished`
lines, and emitted no successful compile, panic, or OOM marker.

At the final report, 129,799 anonymous installations included 71,537
left-selected and 50,495 right-selected cases under the rejected candidate's
left-first policy.  Of the left-selected cases, 67,595 had enough existing
frame-vector capacity, but 59,013 would consume and remove the empty VMA;
those removals would shift 18,678,129 VMA elements cumulatively, with a
maximum suffix of 1,953.  The right-selected path would move 30,790,615
resident frame handles; only 837 had enough capacity without reallocating.
Only 10,996 events, 8.47% of all installations, combined left-side spare
capacity with a pure empty-VMA shrink and therefore avoided both frame-vector
relocation and VMA-suffix movement.

This evidence narrows the only admissible follow-up to that 8.47% zero-move
subset.  It also explains why the broader adjacent-extension candidate was
slower despite avoiding the coalescer: its metadata movement was not actually
eliminated.

### Rejected left zero-move candidate

The narrow production candidate is retained under
`20260807-riscv64-production-smp8-mprotect-anonymous-left-zero-move-window300/window-run1/`.
It used the fast path only when the left VMA was compatible and fully
resident, its frame vector had sufficient spare capacity, and the empty VMA
could be shrunk without removal.  Every other event used the original split
and global coalescer.  The feature-off official-image SMP8 window lasted
`300.19339819399465` seconds, reached the same 23 `Compiling` events and two
`Finished` lines, and had no successful compile, panic, or OOM marker.

The first timed crate arrived at `116.5328978089965` seconds versus
`113.13476717598678` for the retained baseline, 3.00% later.  The 23rd crate
arrived at `126.54621261799184` versus `123.74908572799177` seconds, 2.26%
later.  It therefore failed the 5% retention floor and was immediately
reverted.  The restored `memory_set.rs` SHA-256 is
`48ae16d88586d43511b63ce5a32073bf771761940ceada53743efaec512f5c54`,
and the rebuilt remote feature-off kernel SHA-256 is
`5b79fdd2e0c8a341695104239dd37e2d37540c57139bc0032aaab04b13658e2a`.

Both adjacency-derived production variants are now rejected by comparable
feature-off windows.  Further small branches around the same dense
`Vec<MapArea>`/`Vec<FrameTracker>` representation are not justified by the
measured retention rule; a later attempt would require a separately measured
representation change, not another neighbour special case.

### Anonymous coalescer phase decomposition

The feature-gated decomposition run is
`20260807-riscv64-diagnostics-smp8-anonymous-coalesce-detail-window300/window-run1/`.
It ran the unchanged global anonymous coalescer and measured only its sort and
scan/rebuild phases.  The official-image SMP8 marker window lasted
`300.2053506789962` seconds, reached 23 `Compiling` events and two `Finished`
lines, and emitted no successful compile, panic, or OOM marker.

The final aggregate covered 129,001 calls and 122,317,107 visited VMAs, an
average of about 948 VMAs per anonymous installation and a maximum address
space size of 1,987 VMAs.  Sorting consumed 2,530,508 microseconds, while the
scan/merge/rebuild phase consumed 59,834,257 microseconds out of 62,364,765
microseconds total.  Thus scan/rebuild accounted for **95.95%** of measured
coalescer time; sort was only 4.05%.  The scan performed 127,420 merges and
merged 31,073,756 frame handles.  Only 53,851 frame-vector reallocations were
predicted, relocating 921,460 existing handles, so frame-vector reallocation
was not the dominant aggregate operation.  The primary cost is repeatedly
visiting and rebuilding the complete VMA sequence.

### Rejected `VecDeque` VMA-container candidate

The corresponding representation candidate changed `MemorySet.areas` from
`Vec<MapArea>` to `VecDeque<MapArea>` so middle insertion/removal would move
the shorter side, and used neighbour-only anonymous coalescing.  It did not
change frame representation, fault-window size, scheduler, VFS, TLB, or block
cache.  Evidence is retained under
`20260807-riscv64-production-smp8-mprotect-vma-vecdeque-local-coalesce-window300/window-run1/`.

The feature-off official-image SMP8 marker window lasted
`300.20420965099765` seconds, reached 23 `Compiling` events and two `Finished`
lines, and had no successful compile, panic, or OOM marker.  The candidate
reached the first timed crate at `123.95370568800718` seconds versus
`113.13476717598678` for the retained baseline, 9.56% later.  It reached the
23rd crate at `133.76687096200476` versus `123.74908572799177`, 8.10% later.
The container's indexing and general-operation overhead outweighed the local
removal benefit, so the candidate was immediately reverted.

The restored `memory_set.rs` SHA-256 is
`622bbbac2a1a7165609d140ee09fb0a0a0da81e51fe5d5b364d753ecad1df59b`,
and the rebuilt remote feature-off kernel SHA-256 is
`ec6d54be21bc83a26c11c2374560bba2d396207b812de60033f32184eda35537`.
This rejects both the local-`Vec` and general-`VecDeque` variants.  Any next
representation attempt must preserve fast contiguous indexing while avoiding
full-sequence reconstruction; it cannot be justified as another container
swap without new evidence.

### Global VMA audit and rejected batching candidate

The architecture-wide follow-up is recorded in
`20260807-global-vma-audit.md`.  It audited VMA ordering/search, mmap/munmap/
mprotect, page faults and COW, fork/clone, shared file and System V mappings,
user-copy consumers, direct `areas` users, and the page-table/TLB lock
lifetime.

The zero-length tombstone-slot prototype was rejected before remote testing.
Although its local ordering could be made compatible with the binary searches,
invalid slots leaked through the public `areas` representation into fork,
diagnostics, counts, and exact-start lookup semantics, and it had no robust
growth bound.  It was removed locally and was never copied to the remote host.

The single selected candidate instead retains the ordinary sorted,
non-overlapping, non-empty `Vec<MapArea>` representation and batches anonymous
global coalescing once per 64 anonymous installations.  At most 63 events are
deferred, and two splits per event bound additional temporary entries at 126.
All existing global coalescers reset the batch earlier.  This directly targets
the measured 95.95% scan/rebuild component without changing mapping, backing,
TLB, scheduler, VFS, or block behaviour.

RISC-V64 and LoongArch64 production and `buildstorm-diagnostics` release builds
passed locally, followed by successful eight-CPU SMP/TLB/ASID regressions on
both architectures.

The feature-off official-image SMP8 window is retained under
`20260807-riscv64-production-smp8-anonymous-coalesce-batch64-window300/window-run1/`.
It ran 300.19659674599825 seconds after the official begin marker, reached 23
`Compiling` events and two `Finished` lines, and emitted no panic, OOM, or
successful compile marker.  The first timed crate arrived at
`124.7491725430009` seconds versus the retained baseline's
`113.13476717598678` seconds, 10.2660% slower.  The 23rd crate arrived at
`134.7625188959937` versus `123.74908572799177`, 8.8998% slower.

The batching candidate therefore failed the retention floor and was
immediately reverted locally and remotely.  The restored `memory_set.rs`
SHA-256 is
`622bbbac2a1a7165609d140ee09fb0a0a0da81e51fe5d5b364d753ecad1df59b`.
The result rejects deferred active-VMA fragmentation as well as another
batch-threshold experiment: avoiding scan/rebuild did not compensate for
additional live fragments and middle-insertion/search pressure.  There is
still no exact successful `BUILDSTORM_COMPILE mode=multi ok=true` marker.

### Rejected encapsulated bounded vacant-slot candidate

After the batching candidate established that deferred active-VMA
fragmentation was harmful, a single representation follow-up made
`MemorySet.areas` private and represented locally merged-away entries as
bounded internal vacant slots.  External consumers used filtered accessors;
fork/clone skipped vacancies; local merge preserved binary-search ordering;
and compaction required at least 64 vacancies occupying at least 25% of the
physical slots.  A one-million-transition random model audit and sequential
RISC-V64/LoongArch64 SMP/TLB/ASID/VMA regressions passed before performance
testing.

The diagnostic window is retained under
`20260807-riscv64-diagnostics-smp8-vma-vacant-slots-window300/window-run1/`.
Anonymous coalesce time fell from 62,560,949 to 3,296,169 microseconds
(94.73%), MemorySet lock wait fell 14.13%, and the comparable 23rd-crate
boundary improved 4.67%.  Slot growth remained bounded (`active_max=1955`,
`total_max=2103`, `vacant_max=526`), but only 32 of 194,622 splits reused a
vacant slot.  The candidate therefore still performed essentially every
middle insertion.

The feature-off window is retained under
`20260807-riscv64-production-smp8-vma-vacant-slots-window300/window-run1/`.
It used the unmodified official image and guest script with
`-snapshot -m 8G -smp 8`, ran 300.1950164370064 seconds after the begin
marker, reached 23 `Compiling` and two `Finished` lines, and emitted no panic,
OOM, or successful compile marker.  The first timed crate regressed from
`113.13476717598678` to `124.94568506799988` seconds (10.44% slower), and the
23rd crate regressed from `123.74908572799177` to `135.15931047900813`
seconds (9.22% slower).  It failed the retention floor and was immediately
reverted locally and remotely.  The restored `memory_set.rs` SHA-256 is
`622bbbac2a1a7165609d140ee09fb0a0a0da81e51fe5d5b364d753ecad1df59b`;
the rebuilt remote feature-off RISC-V64 kernel SHA-256 is
`5681d1c0d0be6f977c5a982ca466dda017a81dc9e6941b760c8e5e3f0ebe2ad6`.

This closes the contiguous-`Vec<MapArea>` tombstone/vacant-slot family: the
hot coalescer can be removed diagnostically, but retaining middle split
insertion and physical-slot growth still makes production progress roughly
9--10% worse.  No further threshold tuning is justified, and there is still
no exact `BUILDSTORM_COMPILE mode=multi ok=true` marker.

### 20260812 correctness-gate repair: vfork wait and nested-COW remap

The retained correctness changes are documented separately from performance
candidates. Diagnostics before the repair showed `CLONE_VM | CLONE_VFORK`
(`clone_flags=0x4100`) with a valid envp-slot VMA/PTE/resident entry but a
poisoned pointer at exec time. The parent released and reused its stack before
the vfork child completed exec. The repair defers the parent until child
exec/exit and wakes it through a dedicated deferred-block handshake. This is a
verified root cause from serial diagnostics, not a performance claim.

The second repair addresses nested private COW. After the first COW fault, a
later fork could see VMA-level COW already set and skip making the parent's
writable PTE read-only. The page state then became COW while the parent PTE
remained writable. The repair remaps the parent whenever a page transitions
into COW; the nested-fork regression checks that a subsequent parent write
separates the frame.

The repaired diagnostics run is retained remotely under
`20260812-riscv64-diagnostics-smp8-nested-cow-remap-timeout300/`; it passed
`resident-memory-lifecycle`, `user-memory-lifecycle`, and
`BUILDSTORM_MINIBUILD ok`, reached 22 `Compiling` lines, and had no stack
smashing or exec failure. It was a bounded diagnostic run, not an official
compile pass.

The comparable production window is archived at
`20260812-riscv64-production-smp8-nested-cow-remap-window300/`. It used the
official image, production release kernel, diagnostics disabled, `-snapshot
-m 8G -smp 8`, and QEMU 11.0.3. The marker-window duration was
`300.183398145 s`; it reached 23 `Compiling` lines, ended at
`ax-posix-api v0.5.29`, and had two `Finished` lines. There was no panic or
OOM. The only result markers were `BUILDSTORM_TOOLCHAIN ok`,
`BUILDSTORM_MINIBUILD ok`, and `BUILDSTORM_BEGIN mode=multi`. QEMU exit code
`-9` is intentional window termination. The comparable baseline also reached
23 events, so measured progress is **0% improvement**; no full compile success
is claimed.

Window provenance: source commit `50c9bd348f13cd845880bdcf1ccf3a1e84eac0ea`,
dirty-diff SHA-256
`3b070754d619d33381ad991ec099b293d8ddb65bfd76c48905dfacdc70841c8f`, kernel
SHA-256 `a86c8e958861404f4b5ff60ab4e4f4ac065c0e3c5a5d6d351919a52a44fc9f26`,
image SHA-256
`d74e436522f5946ca17280a7a25f17dbb6604b71fe675bb8a021ce8e849b334c`.
Complete launch arguments and host `pidstat`, `iostat`, `vmstat`, and QEMU
thread samples are in the archived window directory. The guest aggregate file
is empty because production diagnostics were disabled.

### 20260812 production complete run: repeated 33-crate high-CPU stall

The post-correctness production complete run is archived at
`20260812-riscv64-production-complete-15000-official/`. It used the unmodified
official glibc image and suite, a production release kernel with diagnostics
disabled, and `-snapshot -m 8G -smp 8`. The outer runner was allowed its full
15,000-second deadline and terminated normally through the evidence wrapper.

The raw serial contains `BUILDSTORM_TOOLCHAIN ok`, `BUILDSTORM_MINIBUILD ok`,
and `BUILDSTORM_BEGIN mode=multi`, but no `BUILDSTORM_COMPILE` result. It has
33 `Compiling` lines, two `Finished` lines, and ends at
`rustc-literal-escaper v0.0.7`. There is no exact kernel panic or OOM marker.
The serial stopped growing near the 33rd event and remained unchanged for the
rest of the four-hour run. This repeats the prior 33-event long-baseline
boundary and is therefore classified as a **deterministic high-CPU
stall/livelock boundary**, not successful compilation and not demonstrated
slow forward progress. Production evidence does not identify the internal
lock or loop, so the mechanism remains unverified.

`runner.json` records `host_elapsed_seconds=15000.108831100002`, QEMU 11.0.3,
and the complete arguments. GNU time records `4:10:07`, 773% aggregate CPU,
maximum RSS 3,304,856 KiB, zero swaps, and exit status 1. Late `vmstat`
samples show `swpd/si/so=0`, `wa=0`, and six to eight runnable host tasks;
late `iostat` samples show effectively zero device utilization. During live
observation all eight TCG vCPU threads were busy. The host therefore did not
swap, saturate storage, or reduce execution to one QEMU thread. The requested
`host-pidstat.log` exists but contains only headers because `pidstat -C`
failed to match the truncated Linux QEMU comm; CPU evidence comes from GNU
time, `vmstat`, and the recorded live thread samples. This sampler defect does
not change the guest result but must be fixed in the runner before another
long run.

Official judge output is 20.0/180.0 scripted points: toolchain 8, minibuild
12, compile 0, compile-time 0. Provenance is source commit
`50c9bd348f13cd845880bdcf1ccf3a1e84eac0ea`, kernel SHA-256
`433a501d926119de1adf17eb8683c6e4ffa70fc6c52b95ecec92a73b6705f8e8`, image
SHA-256 `d74e436522f5946ca17280a7a25f17dbb6604b71fe675bb8a021ce8e849b334c`,
suite commit `b5ec6ef8497e1818cbdec3b54bb722f036e57972`, and serial SHA-256
`a5efe1cc04bbb6efc0597b4796f7f2c74d512e56ab7fc3b24a9026e0344d463a`.
There is still no official compile pass.
