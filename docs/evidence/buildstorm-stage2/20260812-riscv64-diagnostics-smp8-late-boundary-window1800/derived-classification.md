# Late-boundary diagnostic classification

This file derives a compact classification from `guest-aggregates.json`,
`serial.log`, `launch.json`, and the host sampler logs. The raw files remain
the authority. Snapshot 100 through snapshot 181 covers 835,852,201 us and
begins around the final visible crate transition.

## Run boundary

- Official image SHA-256: `d74e436522f5946ca17280a7a25f17dbb6604b71fe675bb8a021ce8e849b334c`.
- Source commit: `50c9bd348f13cd845880bdcf1ccf3a1e84eac0ea` plus dirty diagnostic diff
  SHA-256 `3b070754d619d33381ad991ec099b293d8ddb65bfd76c48905dfacdc70841c8f`.
- Diagnostic kernel SHA-256:
  `6697a8971432d34a801c51be0a95ae5a1c7b782081a560bd59fe6d284489be7f`.
- QEMU 11.0.3, `-snapshot -m 8G -smp 8`; marker window 1,800 seconds.
- 32 `Compiling` lines, two `Finished` lines, last crate `cfg-if v1.0.4`.
- No exact `BUILDSTORM_COMPILE mode=multi ok=true`, panic, or OOM marker.
  This diagnostic run is `unverified` for official compile scoring.

## Concurrency and host classification

- Maximum concurrent rustc population was two process groups, 20 rustc
  tasks/threads live, and ten rustc tasks runnable. The final snapshot had
  two rustc processes, 17 rustc tasks, and eight runnable rustc tasks.
- All eight guest CPUs accumulated substantial late user-run time. Snapshot
  100-to-181 deltas were 703,022,199; 712,791,688; 700,325,322;
  429,369,292; 694,963,730; 709,309,670; 719,113,934; and 725,256,909 us.
- Host samplers show no swap or storage saturation. All eight TCG vCPU
  threads were active. The boundary is not a one-vCPU or host-I/O condition.

## Late lock and work deltas

The dominant lock was `memory_set_activation`: 1,196,513 acquisitions,
1,014,749 contended acquisitions, 1,327,840,468 us aggregate wait, and only
3,190,936 us hold time. Aggregate `memory_set` was second with 1,120,439,870
us wait and 755,811,003 us hold. The largest attributed sites were:

| site | acquisitions | contended | wait us | hold us |
| --- | ---: | ---: | ---: | ---: |
| user-entry activation | 1,196,503 | 1,014,747 | 1,327,814,365 | 3,190,916 |
| mmap | 907,870 | 798,711 | 694,396,637 | 214,541,694 |
| file writeback | 204,986 | 181,917 | 194,731,069 | 1,496,983 |
| munmap | 204,946 | 174,013 | 124,441,372 | 414,282,435 |
| hardware page fault | 194,585 | 117,583 | 106,797,451 | 107,701,269 |

The same interval recorded 666,717 `mmap` calls taking 910,373,898 us and
204,951 `munmap` calls taking 735,686,314 us. Source audit confirms each
`WorkScope` is constructed once at its syscall entry. `mmap` intentionally
locks once to select an address and again to validate/commit, so its larger
lock-acquisition count is real and is not diagnostic double-counting.

Direct activation and TLB work is not the first cost: address-space
activation used 1,838,678 us and shootdown used 9,763,750 us, about 11.6
seconds total over the 835.85-second interval. The MM deltas were 204,832
user-root activations, 203,390 kernel-root activations, 408,222 page-table
writes, 203,390 local flushes, 129,565 remote shootdowns, and 687,085 remote
targets.

The largest blocking category was pipe readiness at 1,787,141,247 us,
followed by futex at 1,378,572,146 us and poll at 1,298,551,025 us. These are
aggregate waits across tasks; eight rustc tasks remained runnable, so they do
not supersede the MemorySet lock as the first actionable kernel hotspot.

## Device classification

Final totals were 96,957 virtio requests, 22,559 us queue time, 6,244,499 us
completion time, 1,575,117 block-cache hits, and 151,029 misses. Late
block-cache and virtio lock wait were both zero. The guest is neither waiting
on the host device nor serialized on a block/virtio lock.

## Verified versus inferred

Verified by the saved diagnostic evidence: multiple runnable rustc threads,
user execution on all eight CPUs, dominant `memory_set_activation` and
`memory_set` wait, high real mmap/munmap volume, small direct activation/TLB
time, and absence of host swap or I/O saturation.

Inferred from current source, not yet proven by a candidate run: removing the
shared MemorySet lock from the user-entry identity read is the first coherent
performance hypothesis. A raw TCB root/ASID cache is not yet safe. `exec`
marks peer threads Zombie but does not wait for their `running_cpu` ownership
to drain before replacing and dropping the shared MemorySet, so root, ASID,
page-table, and resident-frame lifetime must be closed before reusing the
historical lockless design. The exact reason the compiler fails to complete
after the visible crate boundary remains unverified.
