use alloc::collections::VecDeque;
use alloc::sync::Arc;
use lazy_static::lazy_static;
use spin::Mutex;

use super::{block_current_for_reason_until, current_task, wake_task_token_with, TaskControlBlock};
use crate::utils::error::SysErrNo;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockReason {
    Io,
    ChildExit,
    Timer,
    Futex,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitOutcome {
    Woken,
    TimedOut,
    Interrupted,
}

#[derive(Clone)]
struct WaitEntry {
    task: Arc<TaskControlBlock>,
    token: usize,
}

pub struct WaitQueue {
    waiters: Mutex<VecDeque<WaitEntry>>,
    reason: BlockReason,
}

impl WaitQueue {
    pub fn new(reason: BlockReason) -> Self {
        Self {
            waiters: Mutex::new(VecDeque::new()),
            reason,
        }
    }

    pub fn sleep(&self) -> Result<WaitOutcome, SysErrNo> {
        self.sleep_until(None)
    }

    pub fn sleep_until(&self, deadline_us: Option<usize>) -> Result<WaitOutcome, SysErrNo> {
        let task = current_task().ok_or(SysErrNo::ESRCH)?;

        if let Some(deadline) = deadline_us {
            if crate::timer::get_time_us() >= deadline {
                return Ok(WaitOutcome::TimedOut);
            }
        }

        let token = task.next_wait_token();
        self.waiters.lock().push_back(WaitEntry {
            task: task.clone(),
            token,
        });

        if let Some(deadline) = deadline_us {
            crate::timer::add_timeout(deadline, task.clone(), token);
        }

        block_current_for(self.reason, deadline_us);

        let still_waiting = self.remove_waiter(task.pid.0, token);
        match finish_wait(&task, still_waiting, deadline_us) {
            WaitOutcome::Interrupted => Err(SysErrNo::EINTR),
            outcome => Ok(outcome),
        }
    }

    pub fn wake_one(&self) -> usize {
        self.wake_n(1)
    }

    pub fn wake_n(&self, n: usize) -> usize {
        let mut woke = 0usize;
        while woke < n {
            let entry = self.waiters.lock().pop_front();
            let Some(entry) = entry else {
                break;
            };
            if wake_task_token_with(&entry.task, entry.token, WaitOutcome::Woken) {
                woke += 1;
            }
        }
        woke
    }

    pub fn wake_all(&self) -> usize {
        let mut woke = 0usize;
        loop {
            let entry = self.waiters.lock().pop_front();
            let Some(entry) = entry else {
                break;
            };
            if wake_task_token_with(&entry.task, entry.token, WaitOutcome::Woken) {
                woke += 1;
            }
        }
        woke
    }

    fn remove_waiter(&self, pid: usize, token: usize) -> bool {
        let mut waiters = self.waiters.lock();
        if let Some(index) = waiters
            .iter()
            .position(|entry| entry.task.pid.0 == pid && entry.token == token)
        {
            waiters.remove(index);
            true
        } else {
            false
        }
    }
}

impl Default for WaitQueue {
    fn default() -> Self {
        Self::new(BlockReason::Io)
    }
}

lazy_static! {
    pub static ref IO_WAIT_QUEUE: WaitQueue = WaitQueue::new(BlockReason::Io);
    pub static ref CHILD_WAIT_QUEUE: WaitQueue = WaitQueue::new(BlockReason::ChildExit);
}

pub fn block_current_for(reason: BlockReason, deadline_us: Option<usize>) {
    block_current_for_reason_until(reason, deadline_us);
}

pub fn finish_wait(
    task: &Arc<TaskControlBlock>,
    still_waiting: bool,
    deadline_us: Option<usize>,
) -> WaitOutcome {
    if let Some(outcome) = task.wait_outcome.lock().take() {
        return outcome;
    }
    if crate::syscall::signal::current_has_unblocked_pending() {
        return WaitOutcome::Interrupted;
    }
    if still_waiting
        && deadline_us
            .map(|deadline| crate::timer::get_time_us() >= deadline)
            .unwrap_or(false)
    {
        WaitOutcome::TimedOut
    } else {
        WaitOutcome::Woken
    }
}

pub fn sleep_on_io(deadline_us: Option<usize>) -> Result<WaitOutcome, SysErrNo> {
    IO_WAIT_QUEUE.sleep_until(deadline_us)
}

pub fn wake_io_waiters() -> usize {
    IO_WAIT_QUEUE.wake_all()
}

pub fn sleep_on_child_exit() -> Result<WaitOutcome, SysErrNo> {
    CHILD_WAIT_QUEUE.sleep()
}

pub fn wake_child_waiters() -> usize {
    CHILD_WAIT_QUEUE.wake_all()
}
