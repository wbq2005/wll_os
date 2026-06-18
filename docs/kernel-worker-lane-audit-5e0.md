# Kernel worker lane / writeback worker audit 5E-0

## Conclusion

5E-0 is a design-only precheck. The scheduler now has a separate kernel lane, but
writeback worker startup must remain explicit and disabled by default in this
phase.

Go:

- Future kernel-only worker code may run through the kernel lane.
- The existing synchronous writeback path remains the source of truth for
  durability-sensitive syscalls.

No-go:

- Do not auto-start a writeback worker from mount, open, close, fsync,
  fdatasync, sync, or sync_all.
- Do not make fsync, fdatasync, sync, or sync_all mean "hand off to background."
- Do not do 5C here: no ordinary close drain and no iozone-sized automatic drain.

## Boundary Check

Worker lane:

- `TaskControlBlock::new_kernel_task(...)` is the required constructor for any
  future kernel-only worker task.
- `manager::add_task()` routes `task.is_kernel` into `KERNEL_READY_QUEUE`.
- `run_user_task_foreground()` fetches only from the user lane.
- `run_ready_task_once()` pumps only user tasks.
- `drain_kernel_ready_once()` is the explicit non-foreground kernel-lane pump.

Writeback worker state:

- `DEFAULT_WRITEBACK_WORKER_ENABLED` is `false`.
- `start_writeback_worker_explicit()` returns `ENOSYS`.
- `writeback_worker_main()` has no startup caller.
- Dirty cached writes may enqueue inode numbers, but the queue is not a worker
  lifecycle or completion guarantee.

Mount/open/close/sync state:

- `mount_block_device()` mounts ext4 and initializes caches; it does not start a
  worker.
- `open_regular_ino()` only increments the regular-file refcount.
- Ordinary close only drops refs through `close_regular_ino()`. The only close
  cleanup is delayed unlink finalization for an already unlinked last reference.
- `sys_fsync()` and `sys_fdatasync()` write back shared mappings, then call
  `sync_fd()`, which reaches `FileDescriptor::sync()` and `flush_cached_ino()`.
- `sys_sync()` writes back all shared file mappings, then calls `sync_all()`,
  which reaches `flush_all_cached()`.

## Guardrails For Later 5E Work

- If worker lifecycle is implemented later, create the worker with
  `TaskControlBlock::new_kernel_task(...)` and enqueue only through the kernel
  lane.
- Keep startup behind an explicit disabled-by-default gate until the lifecycle,
  shutdown, wait, logging, and dual-arch marker verification are complete.
- Preserve synchronous drain semantics for fsync, fdatasync, sync, sync_all,
  msync, truncate, unlink, and metadata-time update paths.
- Do not add benchmark-specific success paths, marker shortcuts, MemFS
  redirection, close drain, or iozone-sized automatic drain.

## Verification

Static checks for this precheck:

- Search worker startup callers and kernel task creation paths:
  `start_writeback_worker_explicit`, `writeback_worker_main`,
  `TaskControlBlock::new_kernel_task`, `add_kernel_task`,
  `drain_kernel_ready_once`.
- Search durability paths:
  `sys_fsync`, `sys_fdatasync`, `sys_sync`, `sync_fd`, `sync_all`,
  `flush_cached_ino`, `flush_all_cached`.

Build check after the comment/doc update:

```powershell
cargo +nightly-2025-01-18 check --locked --offline --release --target riscv64gc-unknown-none-elf
```

Runtime benchmark rerun is not required for this design-only checkpoint. If a
future patch wires worker lifecycle, require dual-arch filtered iozone plus
foreground marker greps before calling it safe.
