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
| Process lifecycle: `clone`, `execve`, `exit`, `exit_group`, `wait4`, `yield` | `partial` | `os/src/syscall/process.rs`, `os/src/task/mod.rs`, `os/src/task/wait_queue.rs` | `wait4` reaps process-zombie children across the caller's thread group; `exit_group` terminates the whole TGID; raw `exit` terminates the calling thread and marks the process zombie only when the last user thread exits. `execve` closes `FD_CLOEXEC` descriptors and kills peer threads before replacing the shared address space. Process groups, job control, ptrace-style wait options, robust futex, and complete pthread semantics remain incomplete. |
| Memory: `brk`, `mmap`, `munmap`, `mprotect` | `partial` | `os/src/syscall/mm.rs`, `os/src/mm/*` | Anonymous and file-backed mappings work for common libc paths. Writable file-backed `MAP_SHARED` writes the unmapped range back on `munmap`; dirty tracking, `msync`, and full shared-page coherence are still missing. |
| Filesystem path ops: `openat`, `mkdirat`, `unlink`, `unlinkat`, `rmdir`, `rename`, `renameat`, `renameat2`, `link`, `linkat`, `symlink`, `symlinkat`, `readlinkat`, `truncate`, `statx`, `newfstatat`, `access`, `faccessat`, `faccessat2` | `partial` | `os/src/fs/vfs.rs`, `os/src/syscall/fs.rs`, `os/src/syscall/mod.rs`, `os/src/fs/ext4_vol.rs` | Path metadata/open/readlink/truncate and path mutation enter VFS and share missing-path/type errno handling for common `ENOENT`/`ENOTDIR`/`EISDIR`/`EEXIST`/`ENOTEMPTY`/`EXDEV`/`ELOOP` cases. Old asm-generic path syscalls are compatibility wrappers over the same VFS operations. Real ext4 rename/link/symlink/unlink/rmdir are implemented for simple same-filesystem cases; MemFS overlay removals use VFS whiteouts. Cross-backend hard links/renames, full directory-replacement semantics, full permission/credential checks, `O_PATH`, and full symlink traversal remain partial. |
| FS metadata/sync: `statfs`, `fstatfs`, `fsync`, `fdatasync` | `partial` to `stub-risk` | `os/src/fs/vfs.rs`, `os/src/fs/ext4_vol.rs`, `os/src/syscall/fs.rs`, `os/src/syscall/mod.rs` | `statfs/fstatfs` validate path/fd and return ext4 superblock-derived block/inode counts when ext4 is mounted, with a MemFS-derived fallback when no ext4 root exists. It is still a single-root view, not per-mount accounting. `fsync/fdatasync` validate fd and reject pipes, but persistence is still immediate/no-op for current file backends. |
| FD I/O: `read`, `write`, `readv`, `writev`, `pread64`, `lseek`, `getdents64`, `sendfile`, `dup`, `dup3`, `fcntl` | `partial` | `os/src/fs/fd.rs`, `os/src/syscall/fs.rs` | Regular files, dirs, console, and pipes are covered. Per-fd `FD_CLOEXEC` is tracked for `openat(O_CLOEXEC)`, `pipe2(O_CLOEXEC)`, `dup3(O_CLOEXEC)`, `fcntl(F_GETFD/F_SETFD/F_DUPFD_CLOEXEC)`, and `execve` close. `fcntl(F_SETFL)` mutates pipe `O_NONBLOCK` and regular-file `O_APPEND`; broader descriptor-shared flag semantics remain partial. |
| Pipes and polling: `pipe2`, `ppoll`, `pselect6` | `partial` | `os/src/fs/fd.rs`, `os/src/syscall/fs.rs`, `os/src/task/wait_queue.rs` | Regular file read/write readiness is immediate, including EOF. Pipe read HUP reports `POLLHUP`; blocking goes through reason-tagged wait queues. Pipe capacity/backpressure is still simplified. |
| Signals: `rt_sigaction`, `rt_sigprocmask`, `rt_sigreturn`, `kill`, `tkill`, `tgkill` | `partial` | `os/src/syscall/signal.rs`, `os/src/task/mod.rs`, `os/src/task/wait_queue.rs` | Linux-style `rt_sigaction` handler/flags/restorer/mask layout is supported; `SA_RESTORER`, `SA_NODEFER`, `SA_RESETHAND`, per-task blocked masks, pending bits, `rt_sigreturn` trap-frame restore, and EINTR wakeups for wait-queue sleeps are real. `SA_SIGINFO` gets a minimal siginfo pointer, but the third argument is not a full Linux `ucontext_t`. `SA_RESTART`, `SA_ONSTACK`, `SA_NOCLDSTOP`, `SA_NOCLDWAIT`, `SA_INTERRUPT`, and `SA_EXPOSE_TAGBITS` are accepted as compatibility flags with partial/no effect. `sigsuspend`, `sigtimedwait`, process groups, alt signal stacks, shared process-pending queues, syscall restart, and full default stop/continue/core actions remain incomplete. |
| Futex: `futex WAIT/WAKE` | `partial` | `os/src/syscall/other.rs`, `os/src/task/wait_queue.rs` | Address/key matching and timeout exist; unsupported futex ops return `ENOSYS` instead of fake success. PI, requeue, robust list, and shared-process details are `missing`. |
| Time/system info: `nanosleep`, `clock_gettime`, `gettimeofday`, `times`, `uname`, `sysinfo`, `getrandom` | `partial` | `os/src/syscall/other.rs`, `os/src/timer.rs` | Enough for libc startup and basic utilities. Accuracy and clock IDs are simplified. |
| Scheduler/rlimit/identity stubs | `stub-ok` to `stub-risk` | `os/src/syscall/other.rs`, `os/src/syscall/mod.rs` | UID/GID are fixed zero. Scheduler calls mostly return success. |
| Networking: socket family | `missing` | `os/src/syscall/mod.rs` | Syscall numbers are declared but not dispatched. |

## Process Lifecycle Details

| Syscall / area | Status | Current semantics | Remaining risk |
| --- | --- | --- | --- |
| `wait4` child selection | `partial` | `pid == -1` and `pid == 0` wait for any child process in the caller's thread group; positive `pid` matches child TGID. A live child with `WNOHANG` returns `0`; no matching child returns `ECHILD`. Threads created with `CLONE_THREAD` are not waitable children. | `pid < -1` process-group waits are `missing`; stopped/continued states and Linux wait option filtering are not modeled. |
| `exit` / `exit_group` | `partial` | Raw `exit` makes only the current task zombie, clears `clear_child_tid`, wakes futex waiters, and marks the process zombie when all user threads are gone. `exit_group` applies that termination to every user thread in the TGID and wakes child waiters once. | Signal default actions and robust-list cleanup are incomplete. |
| `clone` flags | `partial` | Accepts `CLONE_VM`, `CLONE_FS`, `CLONE_FILES`, `CLONE_SIGHAND`, `CLONE_THREAD`, `CLONE_SYSVSEM`, `CLONE_SETTLS`, `CLONE_PARENT_SETTID`, `CLONE_CHILD_SETTID`, and `CLONE_CHILD_CLEARTID`. Enforces `CLONE_SIGHAND -> CLONE_VM` and `CLONE_THREAD -> CLONE_SIGHAND`. `getpid()` returns TGID; `gettid()` returns task PID; `getppid()` returns parent TGID. | Unsupported clone bits return `EINVAL`; `CLONE_SYSVSEM` is accepted as compatibility state only. `CLONE_PARENT`, namespaces, signal-group details, TLS arch edge cases, and full pthread/futex semantics remain `missing`/`stub-risk`. |
| `execve` lifecycle | `partial` | Replaces the current address space, resets signal handlers, updates `exec_path`, terminates peer threads in the TGID, and closes descriptors marked `FD_CLOEXEC`. | Linux's full exec credential, dumpability, robust futex, and process-group/session side effects are not modeled. |

## Signal Details

| Syscall / area | Status | Current semantics | Remaining risk |
| --- | --- | --- | --- |
| `rt_sigaction` ABI and flags | `partial` | Uses the common Linux kernel layout `{ handler, flags, restorer, mask }`. Unsupported/probing bits are cleared from stored actions; `SIGKILL` and `SIGSTOP` cannot be caught or ignored. `SA_RESTORER`, `SA_NODEFER`, and `SA_RESETHAND` affect delivery. `SA_SIGINFO` provides a minimal 128-byte siginfo with signo/code/sender pid. | No full `ucontext_t`, alternate signal stack, or syscall restart. `SA_RESTART`, `SA_ONSTACK`, `SA_NOCLDSTOP`, `SA_NOCLDWAIT`, `SA_INTERRUPT`, and `SA_EXPOSE_TAGBITS` are compatibility-accepted but not fully modeled. |
| `rt_sigprocmask` / pending relation | `partial` | Each task has a real blocked mask and pending bitset; `SIGKILL`/`SIGSTOP` are removed from user masks. Unblocking exposes pending signals, and deliverable pending signals interrupt wait-queue sleeps with `EINTR`. | Process-wide shared pending queues and realtime queued-signal multiplicity are missing. |
| `rt_sigreturn` | `partial` | Restores the saved arch trap frame and previous blocked mask from the user signal frame, then returns to the interrupted PC without advancing the syscall PC. | The frame is kernel-private, not a complete Linux `ucontext_t` ABI. Corrupt frames fail with `EINVAL`. |
| `kill` / `tkill` / `tgkill` targets | `partial` | Positive `kill(pid, sig)` targets the TGID and prefers a live thread that is not blocking the signal; `tkill` targets a TID; `tgkill` verifies TID membership in TGID. Signal 0 validates targets without delivery. `kill(-1, sig)` targets all user tasks. | Process groups (`pid == 0` as caller process group and `pid < -1`) are not real; the BusyBox-compatible "seen TGID" success path remains for exited positive TGIDs. |
| `sigsuspend` / `sigtimedwait` | `missing` | Syscall numbers are declared but not dispatched. | Needed for broader libc-test/LTP signal coverage. |

## Test Group Matrix

| Test group | Current expected level | Real / partial coverage | Stub-risk / missing pressure |
| --- | --- | --- | --- |
| `basic` | Pass-oriented | `clone`, `wait4`, `pipe2`, `read/write`, `brk`, `mmap`, `yield`, basic stat/open are `real`/`partial`; `wait4(WNOHANG)` and child zombie reaping now use task/thread-group state instead of harness markers. | Foreground scheduling remains cooperative. Pipe capacity and wait timing are simplified. |
| `busybox` | Mostly usable | Shell launch through BusyBox, ext4 read/write/create/truncate, directory ops, `poll/select`, `sendfile`, symlink/link/rename, pipe `F_SETFL(O_NONBLOCK)`, `FD_CLOEXEC`, `access`, `readlinkat`, and ext4-backed `statfs` are `partial`. | Terminal/ioctl semantics, procfs breadth, full descriptor-shared flags, cross-backend symlink/link/rename behavior, and shell job-control calls are `stub-risk`. |
| `lua` | Not enabled by default | Needs libc startup, `open/read/write/lseek/stat`, `mmap/brk`, time calls, `getrandom`, `futex` for libc. | Floating-point signal edge cases, `mprotect`, `rt_sig*`, and allocator-heavy mmap behavior are likely pressure points. |
| `libc-test` | Not enabled by default | Broad syscall ABI surface exists for many startup and FS cases; process basics now include TGID/TID distinction, `FD_CLOEXEC`, and process-zombie `wait4`. | High risk in signals, pthread/futex semantics, advanced `fcntl`, `madvise`, `clock_nanosleep`, `setitimer`, `sigsuspend`, `sigtimedwait`, `getgroups`, `rlimit`. |
| `iozone` | Not enabled by default | Sequential file create/read/write/truncate/lseek and simple `MAP_SHARED` munmap writeback on ext4 are `partial`. | `fsync` is `stub-ok` for correctness-light tests but `stub-risk` for persistence semantics. Sparse files, dirty tracking, `msync`, and large I/O paths need hardening. |
| `UnixBench` | Not enabled by default | Process creation, pipe, exec, time, file I/O are `partial`. | Fork/exec throughput will expose scheduler fairness, wait queues, pipe buffering, `times/getrusage`, and shell workload gaps. |
| `LTP` | Not enabled by default | Some smoke-level process, memory, file, and signal calls exist. | Many cases are `missing`/`stub-risk`: namespaces, mount variants, sockets, process groups, ptrace, robust futex, permissions, uid/gid, timers, advanced signals. |
| `iperf/netperf` | Not enabled by default | Time, process, poll scaffolding exists. | Socket syscalls are `missing`: `socket`, `bind`, `listen`, `accept`, `connect`, `sendto`, `recvfrom`, `setsockopt`, `getsockopt`, `shutdown`. |

## High-Priority Gaps

1. Harden FD/VFS contract before more suites: `os/src/fs/vfs.rs`, `os/src/fs/fd.rs`, `os/src/syscall/fs.rs`.
   Broaden descriptor-shared status flags beyond the current pipe `O_NONBLOCK` and regular `O_APPEND` subset, then finish full rename/link/symlink semantics and clearer metadata/open tests around MemFS overlay plus ext4.

2. Make blocking semantics observable and less foreground-specific: `os/src/task/wait_queue.rs`, `os/src/task/mod.rs`, `os/src/syscall/process.rs`, `os/src/syscall/other.rs`.
   Extend `BlockReason` into debug logs and replace remaining direct blocking entry points as new waits appear.

3. Finish pthread/libc basics: `os/src/syscall/process.rs`, `os/src/syscall/other.rs`, `os/src/syscall/signal.rs`.
   Prioritize `clone` thread semantics, `futex` shared keys, `set_robust_list`, `rt_sigsuspend`, and `rt_sigtimedwait`.

4. File workload support for `iozone` and UnixBench: `os/src/fs/ext4_vol.rs`, `os/src/mm/memory_set.rs`, `os/src/syscall/mm.rs`.
   Add dirty tracking/`msync` for shared mappings, larger buffered pipe/file paths, and less stubby `fsync`/`fdatasync` behavior.

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
