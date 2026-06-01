use alloc::collections::VecDeque;
use alloc::sync::Arc;
use lazy_static::lazy_static;
use spin::Mutex;

use super::{block_current_and_run_next, current_task, wake_task_token, TaskControlBlock};
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

        block_current_for(self.reason);

        let still_waiting = self.remove_waiter(task.pid.0, token);
        if crate::syscall::signal::current_has_unblocked_pending() {
            return Err(SysErrNo::EINTR);
        }
        if still_waiting
            && deadline_us
                .map(|deadline| crate::timer::get_time_us() >= deadline)
                .unwrap_or(false)
        {
            Ok(WaitOutcome::TimedOut)
        } else {
            Ok(WaitOutcome::Woken)
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
            if wake_task_token(&entry.task, entry.token) {
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
            if wake_task_token(&entry.task, entry.token) {
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

pub fn block_current_for(reason: BlockReason) {
    if let Some(task) = current_task() {
        *task.block_reason.lock() = Some(reason);
    }
    block_current_and_run_next();
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
