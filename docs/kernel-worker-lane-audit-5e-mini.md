# Kernel worker lane / writeback worker audit 5E-mini

## Conclusion

5E-mini implements the minimal default-off explicit startup entry for the
background ext4 writeback worker. The default runtime path still does not start a
worker, and durability-sensitive syscalls still use synchronous writeback.

This checkpoint reaches completion level A:

- zero default-path worker startup;
- `start_writeback_worker_explicit()` creates a kernel-only worker task;
- the worker is enqueued through the kernel ready lane;
- no user ABI, mount/open/close/write/sync auto-start, close drain, or
  iozone-sized automatic drain was added.

Level B runtime proof is left for a later phase because there is currently no
user-reachable command or syscall that may safely call the explicit starter, and
this phase does not introduce one.

## Boundary Check

Worker startup:

- `DEFAULT_WRITEBACK_WORKER_ENABLED` remains `false`.
- `start_writeback_worker_explicit()` is the only startup entry.
- The entry creates the task with `TaskControlBlock::new_kernel_task(...)`.
- `manager::add_task()` routes the worker by `is_kernel` into
  `KERNEL_READY_QUEUE`.
- Duplicate explicit starts are idempotent.

Foreground safety:

- `run_user_task_foreground()` fetches only `fetch_user_task_for_foreground()`.
- `run_ready_task_once()` pumps only user tasks.
- `drain_kernel_ready_once()` refuses to run while the foreground driver is
  active.

Writeback semantics:

- The worker consumes existing `WRITEBACK_QUEUE` items with
  `drain_queued_writeback_ino()`.
- `fsync`, `fdatasync`, and `sync_all` still call the synchronous
  `flush_cached_ino()` / `flush_all_cached()` path.
- `flush_cached_ino()` cancels queued work for the inode before taking a
  snapshot, waits for active writeback progress, acknowledges `last_error`, and
  retries before returning success.
- Failed writeback keeps dirty state and records `last_error`.

No-go preserved:

- no default startup;
- no user ABI;
- no close drain;
- no benchmark-specific path;
- no foreground user ready-lane pollution.
