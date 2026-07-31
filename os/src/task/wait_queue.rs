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
    Signal,
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
    key: Option<WaitKey>,
}

pub struct WaitQueue {
    waiters: Mutex<VecDeque<WaitEntry>>,
    reason: BlockReason,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WaitKey {
    object: usize,
    event: usize,
}

impl WaitKey {
    pub const fn new(object: usize, event: usize) -> Self {
        Self { object, event }
    }
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
        self.sleep_until_if(deadline_us, || Ok(true))
    }

    pub fn sleep_until_if<F>(
        &self,
        deadline_us: Option<usize>,
        should_sleep: F,
    ) -> Result<WaitOutcome, SysErrNo>
    where
        F: FnOnce() -> Result<bool, SysErrNo>,
    {
        self.sleep_until_key_if(None, deadline_us, should_sleep)
    }

    pub fn sleep_until_key_if<F>(
        &self,
        key: Option<WaitKey>,
        deadline_us: Option<usize>,
        should_sleep: F,
    ) -> Result<WaitOutcome, SysErrNo>
    where
        F: FnOnce() -> Result<bool, SysErrNo>,
    {
        match key {
            Some(key) => self.sleep_until_keys_if(&[key], deadline_us, should_sleep),
            None => self.sleep_until_keys_if(&[], deadline_us, should_sleep),
        }
    }

    pub fn sleep_until_keys_if<F>(
        &self,
        keys: &[WaitKey],
        deadline_us: Option<usize>,
        should_sleep: F,
    ) -> Result<WaitOutcome, SysErrNo>
    where
        F: FnOnce() -> Result<bool, SysErrNo>,
    {
        let task = current_task().ok_or(SysErrNo::ESRCH)?;

        // Foreground scheduling parks a blocking syscall and resumes it by
        // re-entering at the same PC.  The waker records the completed wait in
        // wait_outcome; consume that result before registering a new token.
        // Timer wakeups do not remove the queue entry themselves, so also
        // retire the old waiter/timeout here.
        let resumed_outcome = task.wait_outcome.lock().take();
        if let Some(outcome) = resumed_outcome {
            let token = task.current_wait_token();
            self.remove_waiters(task.pid.0, token);
            crate::timer::remove_timeout(task.pid.0, token);
            *task.block_reason.lock() = None;
            return match outcome {
                WaitOutcome::Interrupted => Err(SysErrNo::EINTR),
                outcome => Ok(outcome),
            };
        }

        if let Some(deadline) = deadline_us {
            if crate::timer::get_time_us() >= deadline {
                return Ok(WaitOutcome::TimedOut);
            }
        }

        // A TCB can execute only one blocking syscall at a time.  Bound the
        // queue to one registration per task even if an earlier restart path
        // was interrupted before it could perform normal cleanup.
        self.remove_task_waiters(&task);
        crate::timer::remove_task_timeouts(&task);
        let token = task.next_wait_token();
        *task.wait_outcome.lock() = None;
        *task.block_reason.lock() = Some(self.reason);
        {
            let mut waiters = self.waiters.lock();
            if keys.is_empty() {
                waiters.push_back(WaitEntry {
                    task: task.clone(),
                    token,
                    key: None,
                });
            } else {
                for key in keys {
                    waiters.push_back(WaitEntry {
                        task: task.clone(),
                        token,
                        key: Some(*key),
                    });
                }
            }
        }

        let sleep = match should_sleep() {
            Ok(sleep) => sleep,
            Err(err) => {
                self.remove_waiters(task.pid.0, token);
                *task.block_reason.lock() = None;
                return Err(err);
            }
        };
        if !sleep {
            self.remove_waiters(task.pid.0, token);
            *task.block_reason.lock() = None;
            return Ok(WaitOutcome::Woken);
        }
        if crate::syscall::signal::current_has_unblocked_pending() {
            self.remove_waiters(task.pid.0, token);
            *task.block_reason.lock() = None;
            return Err(SysErrNo::EINTR);
        }

        if let Some(deadline) = deadline_us {
            crate::timer::add_timeout(deadline, task.clone(), token);
        }

        block_current_for(self.reason, deadline_us);

        if crate::trap::syscall_parked() {
            return Err(SysErrNo::ERESTARTSYS);
        }

        *task.block_reason.lock() = None;
        let still_waiting = self.remove_waiters(task.pid.0, token);
        if deadline_us.is_some() {
            crate::timer::remove_timeout(task.pid.0, token);
        }
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

    pub fn wake_key(&self, key: WaitKey) -> usize {
        self.wake_matching(usize::MAX, |entry| entry.key == Some(key))
    }

    pub fn wake_key_n(&self, key: WaitKey, n: usize) -> usize {
        self.wake_matching(n, |entry| entry.key == Some(key))
    }

    pub fn wake_unkeyed(&self) -> usize {
        self.wake_matching(usize::MAX, |entry| entry.key.is_none())
    }

    fn wake_matching(&self, limit: usize, mut matches: impl FnMut(&WaitEntry) -> bool) -> usize {
        let mut woke = 0usize;
        while woke < limit {
            let entry = {
                let mut waiters = self.waiters.lock();
                let Some(index) = waiters.iter().position(|entry| matches(entry)) else {
                    break;
                };
                waiters.remove(index)
            };
            let Some(entry) = entry else {
                break;
            };
            if wake_task_token_with(&entry.task, entry.token, WaitOutcome::Woken) {
                woke += 1;
            }
        }
        woke
    }

    pub fn remove_task_waiters(&self, task: &Arc<TaskControlBlock>) -> usize {
        let mut removed = 0usize;
        let mut waiters = self.waiters.lock();
        let mut index = 0usize;
        while index < waiters.len() {
            if Arc::ptr_eq(&waiters[index].task, task) || waiters[index].task.pid.0 == task.pid.0 {
                waiters.remove(index);
                removed += 1;
            } else {
                index += 1;
            }
        }
        removed
    }

    fn remove_waiter(&self, pid: usize, token: usize) -> bool {
        self.remove_waiters(pid, token)
    }

    fn remove_waiters(&self, pid: usize, token: usize) -> bool {
        let mut waiters = self.waiters.lock();
        let mut removed = false;
        let mut index = 0usize;
        while index < waiters.len() {
            if waiters[index].task.pid.0 == pid && waiters[index].token == token {
                waiters.remove(index);
                removed = true;
            } else {
                index += 1;
            }
        }
        removed
    }

    #[cfg(feature = "buildstorm-diagnostics")]
    fn diagnostic_snapshot(&self) -> alloc::vec::Vec<(usize, usize, usize, Option<WaitKey>)> {
        self.waiters
            .lock()
            .iter()
            .map(|entry| {
                (
                    entry.task.pid.0,
                    entry.task.thread_group.tgid(),
                    entry.token,
                    entry.key,
                )
            })
            .collect()
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

pub fn sleep_on_io_if<F>(
    deadline_us: Option<usize>,
    should_sleep: F,
) -> Result<WaitOutcome, SysErrNo>
where
    F: FnOnce() -> Result<bool, SysErrNo>,
{
    IO_WAIT_QUEUE.sleep_until_if(deadline_us, should_sleep)
}

pub fn sleep_on_io_key_if<F>(
    key: WaitKey,
    deadline_us: Option<usize>,
    should_sleep: F,
) -> Result<WaitOutcome, SysErrNo>
where
    F: FnOnce() -> Result<bool, SysErrNo>,
{
    IO_WAIT_QUEUE.sleep_until_key_if(Some(key), deadline_us, should_sleep)
}

pub fn sleep_on_io_keys_if<F>(
    keys: &[WaitKey],
    deadline_us: Option<usize>,
    should_sleep: F,
) -> Result<WaitOutcome, SysErrNo>
where
    F: FnOnce() -> Result<bool, SysErrNo>,
{
    IO_WAIT_QUEUE.sleep_until_keys_if(keys, deadline_us, should_sleep)
}

pub fn wake_io_waiters() -> usize {
    IO_WAIT_QUEUE.wake_unkeyed()
}

pub fn wake_io_keyed_waiters(key: WaitKey) -> usize {
    IO_WAIT_QUEUE.wake_key(key) + IO_WAIT_QUEUE.wake_unkeyed()
}

pub fn wake_io_keyed_waiter(key: WaitKey) -> usize {
    IO_WAIT_QUEUE.wake_key_n(key, 1) + IO_WAIT_QUEUE.wake_unkeyed()
}

pub fn sleep_on_child_exit() -> Result<WaitOutcome, SysErrNo> {
    CHILD_WAIT_QUEUE.sleep()
}

pub fn wake_child_waiters() -> usize {
    CHILD_WAIT_QUEUE.wake_all()
}

pub(crate) fn remove_core_waiters_for_task(task: &Arc<TaskControlBlock>) -> usize {
    IO_WAIT_QUEUE.remove_task_waiters(task) + CHILD_WAIT_QUEUE.remove_task_waiters(task)
}

#[cfg(feature = "buildstorm-diagnostics")]
pub(crate) fn diagnostic_dump_waiters() {
    let io = IO_WAIT_QUEUE.diagnostic_snapshot();
    let child = CHILD_WAIT_QUEUE.diagnostic_snapshot();
    crate::println!(
        "[buildstorm-diag] wait-queues io={} child={}",
        io.len(),
        child.len()
    );
    for (pid, tgid, token, key) in io {
        crate::println!(
            "[buildstorm-diag] io-wait pid={} tgid={} token={} key={:?}",
            pid,
            tgid,
            token,
            key
        );
    }
    for (pid, tgid, token, key) in child {
        crate::println!(
            "[buildstorm-diag] child-wait pid={} tgid={} token={} key={:?}",
            pid,
            tgid,
            token,
            key
        );
    }
}
