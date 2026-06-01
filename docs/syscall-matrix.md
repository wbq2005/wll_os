# Syscall Matrix

Status legend:

- `real`: implemented with kernel state or real backing objects.
- `partial`: useful subset or compatible behavior, but not full Linux semantics.
- `stub-ok`: fixed success or simplified value that is enough for current suites.
- `stub-risk`: currently unblocks simple cases but likely fails broader suites.
- `missing`: not dispatched or returns `ENOSYS`.

## Current Foundation

| Area | Status | Main files | Notes |
| --- | --- | --- | --- |
| Process lifecycle: `clone`, `execve`, `exit`, `exit_group`, `wait4`, `yield` | `partial` | `os/src/syscall/process.rs`, `os/src/task/mod.rs` | Process-style clone and some thread flags exist. Full thread group, signal, and robust futex semantics are still incomplete. |
| Memory: `brk`, `mmap`, `munmap`, `mprotect` | `partial` | `os/src/syscall/mm.rs`, `os/src/mm/*` | Anonymous and file-backed mappings work for common libc paths. Shared file writeback and advanced flags are not complete. |
| Filesystem path ops: `openat`, `mkdirat`, `unlinkat`, `renameat2`, `linkat`, `symlinkat`, `readlinkat`, `truncate`, `statx`, `newfstatat` | `partial` | `os/src/fs/vfs.rs`, `os/src/syscall/fs.rs`, `os/src/fs/ext4_vol.rs` | Syscalls now enter VFS for path metadata and open. FD layer still owns read/write/seek/getdents for opened objects. |
| FD I/O: `read`, `write`, `readv`, `writev`, `pread64`, `lseek`, `getdents64`, `sendfile`, `dup`, `dup3`, `fcntl` | `partial` | `os/src/fs/fd.rs`, `os/src/syscall/fs.rs` | Regular files, dirs, console, and pipes are covered. `fcntl(F_SETFL)` does not mutate descriptor flags yet. |
| Pipes and polling: `pipe2`, `ppoll`, `pselect6` | `partial` | `os/src/fs/fd.rs`, `os/src/syscall/fs.rs`, `os/src/task/wait_queue.rs` | Blocking now goes through reason-tagged wait queues. Pipe capacity/backpressure is still simplified. |
| Signals: `rt_sigaction`, `rt_sigprocmask`, `rt_sigreturn`, `kill`, `tkill`, `tgkill` | `partial` | `os/src/syscall/signal.rs` | Basic delivery and user trampoline exist. `sigsuspend`, `sigtimedwait`, process groups, and full default actions are incomplete. |
| Futex: `futex WAIT/WAKE` | `partial` | `os/src/syscall/other.rs`, `os/src/task/wait_queue.rs` | Address/key matching and timeout exist. PI, requeue, robust list, and shared-process details are not implemented. |
| Time/system info: `nanosleep`, `clock_gettime`, `gettimeofday`, `times`, `uname`, `sysinfo`, `getrandom` | `partial` | `os/src/syscall/other.rs`, `os/src/timer.rs` | Enough for libc startup and basic utilities. Accuracy and clock IDs are simplified. |
| Scheduler/rlimit/identity stubs | `stub-ok` to `stub-risk` | `os/src/syscall/other.rs`, `os/src/syscall/mod.rs` | UID/GID are fixed zero. Scheduler calls mostly return success. |
| Networking: socket family | `missing` | `os/src/syscall/mod.rs` | Syscall numbers are declared but not dispatched. |

## Test Group Matrix

| Test group | Current expected level | Real / partial coverage | Stub-risk / missing pressure |
| --- | --- | --- | --- |
| `basic` | Pass-oriented | `clone`, `wait4`, `pipe2`, `read/write`, `brk`, `mmap`, `yield`, basic stat/open are `real`/`partial`. | Foreground scheduling remains cooperative. Pipe capacity and wait timing are simplified. |
| `busybox` | Mostly usable | Shell launch through BusyBox, ext4 read/write/create, directory ops, `poll/select`, `sendfile`, symlink/link/rename are `partial`. | `fcntl(F_SETFL)`, terminal/ioctl semantics, procfs breadth, and shell job-control calls are `stub-risk`. |
| `lua` | Not enabled by default | Needs libc startup, `open/read/write/lseek/stat`, `mmap/brk`, time calls, `getrandom`, `futex` for libc. | Floating-point signal edge cases, `mprotect`, `rt_sig*`, and allocator-heavy mmap behavior are likely pressure points. |
| `libc-test` | Not enabled by default | Broad syscall ABI surface exists for many startup and FS cases. | High risk in signals, pthread/futex semantics, `fcntl`, `madvise`, `clock_nanosleep`, `setitimer`, `sigsuspend`, `sigtimedwait`, `getgroups`, `rlimit`. |
| `iozone` | Not enabled by default | Sequential file create/read/write/truncate/lseek on ext4 is `partial`. | `fsync` is `stub-ok` for correctness-light tests but `stub-risk` for persistence semantics. Sparse files, mmap writeback, and large I/O paths need hardening. |
| `UnixBench` | Not enabled by default | Process creation, pipe, exec, time, file I/O are `partial`. | Fork/exec throughput will expose scheduler fairness, wait queues, pipe buffering, `times/getrusage`, and shell workload gaps. |
| `LTP` | Not enabled by default | Some smoke-level process, memory, file, and signal calls exist. | Many cases are `missing`/`stub-risk`: namespaces, mount variants, sockets, process groups, ptrace, robust futex, permissions, uid/gid, timers, advanced signals. |
| `iperf/netperf` | Not enabled by default | Time, process, poll scaffolding exists. | Socket syscalls are `missing`: `socket`, `bind`, `listen`, `accept`, `connect`, `sendto`, `recvfrom`, `setsockopt`, `getsockopt`, `shutdown`. |

## High-Priority Gaps

1. Harden FD/VFS contract before more suites: `os/src/fs/vfs.rs`, `os/src/fs/fd.rs`, `os/src/syscall/fs.rs`.
   Implement mutable `F_SETFL` for `O_NONBLOCK`, consolidate legacy fd open helper, and add clearer metadata/open tests around MemFS overlay plus ext4.

2. Make blocking semantics observable and less foreground-specific: `os/src/task/wait_queue.rs`, `os/src/task/mod.rs`, `os/src/syscall/process.rs`, `os/src/syscall/other.rs`.
   Extend `BlockReason` into debug logs and replace remaining direct blocking entry points as new waits appear.

3. Finish pthread/libc basics: `os/src/syscall/process.rs`, `os/src/syscall/other.rs`, `os/src/syscall/signal.rs`.
   Prioritize `clone` thread semantics, `futex` shared keys, `set_robust_list`, `rt_sigsuspend`, and `rt_sigtimedwait`.

4. File workload support for `iozone` and UnixBench: `os/src/fs/ext4_vol.rs`, `os/src/mm/memory_set.rs`, `os/src/syscall/mm.rs`.
   Add mmap shared writeback, larger buffered pipe/file paths, and less stubby `fsync`/`fdatasync` behavior.

5. Network suites last: add a socket layer under `os/src/net` or `os/src/syscall/net.rs`, then dispatch the socket syscalls declared in `os/src/syscall/mod.rs`.
   Start with loopback TCP/UDP or a minimal virtio-net-backed path, then implement `poll` readiness over sockets.

## Suggested Enable Order

1. Keep `basic` and `busybox` as the default harness groups.
2. Enable `lua` next; it exercises libc and file paths without demanding a huge syscall surface.
3. Enable `iozone` after ext4 write/truncate/fsync and mmap behavior are stable.
4. Enable `UnixBench` after scheduler, pipe, wait, and timing behavior are less synthetic.
5. Enable `libc-test` in small categories, starting with file/process/memory, then pthread/signal.
6. Enable `LTP` only after the matrix is split by LTP subsystem.
7. Enable `iperf/netperf` after the socket layer exists.
