# Blocking Semantics Audit

This document records the current implementation facts for blocking and wakeup
paths. It is an audit snapshot, not a target design.

## Common Mechanism

Blocking user tasks use status plus wait tokens:

1. A syscall records a waiter, usually through `WaitQueue::sleep_until`.
2. `block_current_for(reason)` stores `TaskControlBlock.block_reason` and calls
   `block_current_and_run_next`.
3. `block_current_and_run_next` saves the current trap frame, sets the task to
   `TaskStatus::Blocked`, clears `CURRENT_TASK`, and then runs ready tasks while
   the original task remains blocked.
4. Wakeup calls `wake_task_token(task, token)`. If the token still matches and
   the task is still `Blocked`, it changes the task to `Ready`, clears
   `block_reason`, and pushes it into the global ready queue.
5. The blocked syscall resumes inside `block_current_and_run_next`; it removes
   its own ready-queue notification, changes `Ready` back to `Running`, restores
   the address space, and returns to the syscall body.

The ready-queue entry created by wakeup is therefore a notification as well as a
scheduling candidate. If the blocked task resumes in the original syscall loop,
that entry is removed before returning to user mode.

`timer::wake_expired_timers()` is called from timer traps, idle, foreground
harness loops, and the blocking loop itself. Stale timer wakeups are rejected by
the wait token.

Signals wake blocked tasks directly in `signal::wake_for_signal` by setting
`Ready` and adding the task to the ready queue. After a wait returns, the wait
code checks `current_has_unblocked_pending()`; deliverable pending signals turn
the syscall result into `EINTR`.

## Blocking Classes

| Class | Who sets `Blocked` | Who wakes | Ready queue re-entry | Signal interrupt | Timeout |
| --- | --- | --- | --- | --- | --- |
| Child wait | `sys_wait4`/`sys_waitid` -> `sleep_on_child_exit_if()` -> child wait queue | `finish_process_exit()` -> `wake_child_waiters()`; signals can also wake | `wake_task_token` pushes `Ready`; resuming waiter removes its own queued notification | Yes, if a deliverable pending signal exists after wake | No wait timeout; `WNOHANG` is immediate nonblocking return |
| IO wait | `sys_read` on empty blocking pipe, `sys_ppoll`, `sys_pselect6` -> `sleep_on_io()` | Pipe write, pipe endpoint drop, poll timeout timer, signals | Same token wake path; poll/read loops recheck readiness after wake | Yes for deliverable pending signals | Real for `ppoll`/`pselect6`; none for blocking `read` |
| Timer sleep | `sys_nanosleep` / dispatched `clock_nanosleep` -> `timer::sleep_until_us()` | `timer::wake_expired_timers()`; signals can also wake | `wake_task_token` pushes `Ready`; syscall resumes when deadline or signal wins | Yes, returns `EINTR`; `rem` is not filled on interruption | Real relative deadline |
| Futex wait | `sys_futex_stub(FUTEX_WAIT)` -> local futex waiter table -> `block_current_for(Futex)` | `FUTEX_WAKE`, `clear_child_tid` exit wake, timeout timer, signals | `futex_wake_addr` removes the waiter then calls `wake_task_token`; timeout leaves waiter for post-wake cleanup | Yes for deliverable pending signals | Real relative deadline; returns `ETIMEDOUT` if still waiting at deadline |
| Foreground harness | Harness itself does not block via wait queues; user tasks block through the normal paths | Normal wait queues, timers, signals; harness timeout aborts to `Zombie` instead of waking | Harness initially queues the root user task and requeues runnable tasks after each trap return; blocked tasks only re-enter through normal wake | Yes; foreground dispatch calls `handle_pending_for_user` before running a task | Harness has a synthetic loop budget (`TIMEOUT_TICKS`), while syscall timer deadlines remain real |

## Child Wait

Path:

- `sys_wait4` first tries to reap a process-zombie child from the `children`
  lists owned by any user thread in the caller's thread group.
- `WNOHANG` returns `0` only when a matching live child still exists; no
  matching child returns `ECHILD`.
- The blocking loops for `wait4` and `waitid` register the waiter first, then
  recheck non-consumingly for a matching zombie, stopped/continued event, or
  disappearance of the matching child. This closes the child-exit race between
  an earlier state check and wait registration.
- `sleep_on_child_exit_if()` uses the global `CHILD_WAIT_QUEUE` with
  `BlockReason::ChildExit`; the next loop iteration performs the actual reap
  or `waitid` event consumption.

Wake path:

- `finish_process_exit()` marks the thread group process-zombie once, reparents
  orphans, then calls `wake_child_waiters()`.
- `wake_child_waiters()` wakes all child waiters. The awakened `wait4` caller
  rechecks children and reaps the matching zombie.

Notes:

- `wait4` currently has no timeout. `WNOHANG` is not a sleep timeout; it bypasses
  blocking.
- In foreground mode the current code still uses the same child wait queue and
  blocking loop. While the parent is blocked, the blocking loop can run ready
  child tasks or spin if none are ready.

## IO Wait

Paths:

- `sys_read` blocks only when `FileDescriptor::PipeRead::read` returns
  `EAGAIN`, which happens for an empty pipe with at least one writer. If the pipe
  read end is nonblocking, `sys_read` returns `EAGAIN` instead of sleeping.
- `sys_ppoll` and `sys_pselect6` repeatedly call their readiness probes. If no
  descriptor is ready, they call `sleep_on_io(deadline)` or `sleep_on_io(None)`.
- `ppoll`/`pselect6` with `nfds == 0` use timer sleep directly rather than IO
  wait.

Wake path:

- Pipe writes append to the pipe buffer and call `wake_io_waiters()`.
- Dropping either pipe endpoint adjusts reader/writer counts and calls
  `wake_io_waiters()`.
- `wake_io_waiters()` wakes all tasks on the global IO wait queue. Each syscall
  rechecks its own fd state after wake.

Notes:

- Pipe writes do not currently block for capacity; the pipe buffer grows.
- The IO wait queue is global, so unrelated pipe events can cause harmless
  spurious wakeups. The syscall loops handle this by probing readiness again.
- `ppoll` and `pselect6` timeouts are real deadlines backed by timer waiters.

## Timer Sleep

Path:

- `sys_nanosleep` validates the user `timespec`, converts it to a relative
  microsecond duration, and calls `timer::sleep_until_us(deadline)`.
- `timer::sleep_until_us` uses a timer-specific wait queue with
  `BlockReason::Timer`.
- `WaitQueue::sleep_until` registers both a queue waiter and a timer waiter for
  the same token.

Wake path:

- `wake_expired_timers()` removes expired timer waiters and calls
  `wake_task_token`.
- The wait queue entry is removed by the sleeping task after it resumes.

Notes:

- `sys_nanosleep` ignores the `WaitOutcome` on success and writes zero to `rem`.
- If a deliverable signal interrupts the sleep, `EINTR` returns before `rem` is
  written.
- The syscall dispatch maps `clock_nanosleep` to `sys_nanosleep(args[2],
  args[3])`; clock IDs and absolute-time semantics are not implemented.

## Futex Wait

Path:

- `sys_futex_stub` implements `FUTEX_WAIT` and `FUTEX_WAKE` after masking the
  operation with `0x7f`.
- `FUTEX_WAIT` verifies `uaddr != 0` and that the user word still equals `val`;
  otherwise it returns `EFAULT` or `EAGAIN`.
- The futex key is `Arc::as_ptr(&task.memory_set)`, so it is shared by
  `CLONE_VM` tasks but not by independent address spaces.
- Waiters are stored in the local `FUTEX_WAITERS` table, then the task blocks
  with `BlockReason::Futex`.

Wake path:

- `FUTEX_WAKE` calls `futex_wake_addr`, finds waiters with matching
  `(uaddr, key)`, removes them from `FUTEX_WAITERS`, and calls
  `wake_task_token`.
- `finish_task_exit` handles `clear_child_tid` by writing zero to the user word
  and calling `futex_wake_addr(clear_child_tid, usize::MAX)`.
- Timeout wakeups do not remove the futex waiter first; the resumed waiter calls
  `remove_futex_waiter` and returns `ETIMEDOUT` if it was still present and the
  deadline has passed.

Notes:

- Timeout is a real relative `timespec` deadline.
- PI, requeue, robust-list behavior, and shared mapping keys are not implemented.
- Signals can interrupt with `EINTR` when deliverable. Non-deliverable signal
  wakeups can appear as spurious success, which futex callers must already
  tolerate.

## Foreground Harness

Path:

- The runtime test harness is a kernel task.
- For each script, `run_user_program_spec_foreground` creates a user task,
  enables `FOREGROUND_MODE`, and calls `run_user_task_foreground`.
- `run_user_task_foreground` puts the root user task in the ready queue, then
  repeatedly fetches ready tasks, runs one trap-return slice with
  `run_user_task`, restores the kernel page table, and calls
  `requeue_after_user_run`.
- `requeue_after_user_run` requeues only `Running` or `Ready` tasks. `Blocked`
  and `Zombie` tasks are not requeued.

Wake and timeout:

- Blocked foreground user tasks use the same wait queues, timer waiters, futex
  waiters, and signal wakeups as normal scheduler mode.
- Foreground timer traps call `wake_expired_timers()` and program a longer next
  foreground tick; they do not call `suspend_current_and_run_next`.
- If the foreground loop reaches `TIMEOUT_TICKS`, it marks the root task tree
  and queued non-kernel tasks as `Zombie` with exit code `-2`. This is an abort,
  not a wait-queue wake.

Notes:

- The harness kernel task is restored as `CURRENT_TASK` after the foreground run;
  it is not woken through a wait queue.
- Blocking syscalls invoked by foreground user tasks can still run other ready
  tasks inside `block_current_and_run_next`.

## Next Minimal Implementation Cut

The smallest useful implementation cut is to make signal wakeups more precise
without changing the scheduler shape:

1. Expose a small helper that answers whether a specific task has a deliverable
   pending signal.
2. In `wake_for_signal`, only move a blocked task to `Ready` when the queued
   signal is deliverable under that task's current mask/action.
3. Keep the existing wait-token and ready-queue mechanics unchanged.
4. After that, fill `nanosleep` remaining time on `EINTR` as a contained syscall
   semantic improvement.

This targets a real semantic gap while avoiding scheduler rewrites and should
have low risk for the current `basic` and `busybox` default harness groups.
