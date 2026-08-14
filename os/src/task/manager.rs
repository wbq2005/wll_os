use alloc::collections::{BTreeSet, VecDeque};
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use core::sync::atomic::{AtomicUsize, Ordering};
use lazy_static::lazy_static;
use spin::Mutex;

use super::{TaskControlBlock, TaskStatus};

lazy_static! {
    static ref USER_READY_QUEUE: Mutex<ReadyQueue> = Mutex::new(ReadyQueue::new());
    static ref KERNEL_READY_QUEUE: Mutex<ReadyQueue> = Mutex::new(ReadyQueue::new());
    static ref TASK_REGISTRY: Mutex<Vec<Weak<TaskControlBlock>>> = Mutex::new(Vec::new());
    static ref EXITED_TASKS: Mutex<Vec<(usize, usize)>> = Mutex::new(Vec::new());
}

const RT_LOWER_RUN_BUDGET: usize = 1;
static RT_RUNS_SINCE_NORMAL: AtomicUsize = AtomicUsize::new(0);

// The deque keeps runnable order; the pid set mirrors its membership so repeated
// wakeups/requeues can be deduplicated without scanning the whole ready queue.
struct ReadyQueue {
    tasks: VecDeque<Arc<TaskControlBlock>>,
    queued_pids: BTreeSet<usize>,
}

impl ReadyQueue {
    fn new() -> Self {
        Self {
            tasks: VecDeque::new(),
            queued_pids: BTreeSet::new(),
        }
    }

    fn push_back(&mut self, task: Arc<TaskControlBlock>) -> bool {
        if self.queued_pids.insert(task.pid.0) {
            self.tasks.push_back(task);
            true
        } else {
            false
        }
    }

    fn push_front(&mut self, task: Arc<TaskControlBlock>) -> bool {
        if self.queued_pids.insert(task.pid.0) {
            self.tasks.push_front(task);
            true
        } else {
            false
        }
    }

    fn remove_at(&mut self, index: usize) -> Option<Arc<TaskControlBlock>> {
        let task = self.tasks.remove(index)?;
        self.queued_pids.remove(&task.pid.0);
        Some(task)
    }

    fn len(&self) -> usize {
        self.tasks.len()
    }
}

pub fn register_task(task: &Arc<TaskControlBlock>) {
    TASK_REGISTRY.lock().push(Arc::downgrade(task));
}

pub fn record_exited_task(pid: usize, tgid: usize) {
    let mut exited = EXITED_TASKS.lock();
    if !exited.iter().any(|entry| *entry == (pid, tgid)) {
        exited.push((pid, tgid));
    }
}

pub fn add_task(task: Arc<TaskControlBlock>) {
    if task.is_kernel {
        add_kernel_task(task);
    } else {
        add_user_task(task);
    }
}

/// Requeue work for the scheduler already running on this CPU.
///
/// The caller must continue into a scheduling decision after publishing the
/// task, so no remote idle CPU needs an IPI for this queue transition.
pub fn add_task_local(task: Arc<TaskControlBlock>) {
    let notify = !task.can_run_on_cpu(crate::platform::current_cpu_index());
    if task.is_kernel {
        push_task_back(&KERNEL_READY_QUEUE, task, notify);
    } else {
        push_task_back(&USER_READY_QUEUE, task, notify);
    }
}

pub fn add_user_task(task: Arc<TaskControlBlock>) {
    debug_assert!(!task.is_kernel);
    push_task_back(&USER_READY_QUEUE, task, true);
}

pub fn add_kernel_task(task: Arc<TaskControlBlock>) {
    debug_assert!(task.is_kernel);
    push_task_back(&KERNEL_READY_QUEUE, task, true);
}

fn push_task_back(queue: &Mutex<ReadyQueue>, task: Arc<TaskControlBlock>, notify: bool) {
    if task.status() != TaskStatus::Ready {
        return;
    }
    let affinity = task.affinity_mask();
    #[cfg(feature = "buildstorm-diagnostics")]
    let mut queue = crate::buildstorm_diagnostics::lock(
        crate::buildstorm_diagnostics::LockClass::TaskManager,
        queue,
    );
    #[cfg(not(feature = "buildstorm-diagnostics"))]
    let mut queue = queue.lock();
    let inserted = queue.push_back(task);
    #[cfg(feature = "buildstorm-diagnostics")]
    crate::buildstorm_diagnostics::note_runqueue_len(queue.len());
    drop(queue);
    if inserted && notify {
        crate::platform::notify_runnable_for(affinity);
    }
}

pub fn add_task_front(task: Arc<TaskControlBlock>) {
    if task.is_kernel {
        push_task_front(&KERNEL_READY_QUEUE, task, true);
    } else {
        push_task_front(&USER_READY_QUEUE, task, true);
    }
}

pub fn add_task_front_local(task: Arc<TaskControlBlock>) {
    let notify = !task.can_run_on_cpu(crate::platform::current_cpu_index());
    if task.is_kernel {
        push_task_front(&KERNEL_READY_QUEUE, task, notify);
    } else {
        push_task_front(&USER_READY_QUEUE, task, notify);
    }
}

fn push_task_front(queue: &Mutex<ReadyQueue>, task: Arc<TaskControlBlock>, notify: bool) {
    if task.status() != TaskStatus::Ready {
        return;
    }
    let affinity = task.affinity_mask();
    #[cfg(feature = "buildstorm-diagnostics")]
    let mut queue = crate::buildstorm_diagnostics::lock(
        crate::buildstorm_diagnostics::LockClass::TaskManager,
        queue,
    );
    #[cfg(not(feature = "buildstorm-diagnostics"))]
    let mut queue = queue.lock();
    let inserted = queue.push_front(task);
    #[cfg(feature = "buildstorm-diagnostics")]
    crate::buildstorm_diagnostics::note_runqueue_len(queue.len());
    drop(queue);
    if inserted && notify {
        crate::platform::notify_runnable_for(affinity);
    }
}

pub fn fetch_task() -> Option<Arc<TaskControlBlock>> {
    fetch_user_task_for_foreground().or_else(fetch_kernel_task)
}

pub fn fetch_user_task_for_foreground() -> Option<Arc<TaskControlBlock>> {
    fetch_from_queue(&USER_READY_QUEUE)
}

pub fn fetch_kernel_task() -> Option<Arc<TaskControlBlock>> {
    fetch_from_queue(&KERNEL_READY_QUEUE)
}

fn fetch_from_queue(queue: &Mutex<ReadyQueue>) -> Option<Arc<TaskControlBlock>> {
    #[cfg(feature = "buildstorm-diagnostics")]
    let mut queue = crate::buildstorm_diagnostics::lock(
        crate::buildstorm_diagnostics::LockClass::TaskManager,
        queue,
    );
    #[cfg(not(feature = "buildstorm-diagnostics"))]
    let mut queue = queue.lock();
    #[cfg(feature = "buildstorm-diagnostics")]
    crate::buildstorm_diagnostics::note_runqueue_len(queue.len());
    let current_cpu = crate::platform::current_cpu_index();
    let mut best_index = None;
    let mut best_priority = 0;
    let mut lower_rt_index = None;
    let mut lower_rt_priority = 0;
    let mut index = 0;
    while index < queue.tasks.len() {
        let task = &queue.tasks[index];
        if task.status() != TaskStatus::Ready {
            queue.remove_at(index);
            continue;
        }
        if !task.can_run_on_cpu(current_cpu) {
            index += 1;
            continue;
        }
        // Scheduling attributes may change while a task is queued, so choose
        // using the current effective priority instead of caching it at enqueue.
        let priority = task.effective_sched_priority();
        if priority > best_priority {
            if best_priority > 0 && best_priority > lower_rt_priority {
                lower_rt_index = best_index;
                lower_rt_priority = best_priority;
            }
            best_index = Some(index);
            best_priority = priority;
        } else if priority > 0 && priority < best_priority && priority > lower_rt_priority {
            lower_rt_index = Some(index);
            lower_rt_priority = priority;
        } else if best_index.is_none() {
            best_index = Some(index);
        }
        index += 1;
    }
    let chosen_index = if best_priority > 0 {
        let rt_runs = RT_RUNS_SINCE_NORMAL.load(Ordering::Relaxed);
        // Keep the existing bounded fairness behavior among runnable RT tasks:
        // occasionally run the next lower RT priority instead of the top one.
        if rt_runs >= RT_LOWER_RUN_BUDGET && lower_rt_index.is_some() {
            lower_rt_index
        } else {
            best_index
        }
    } else {
        best_index
    }?;
    let task = queue.remove_at(chosen_index)?;
    let chosen_priority = task.effective_sched_priority();
    if best_priority > 0 && chosen_priority > 0 && chosen_priority == best_priority {
        RT_RUNS_SINCE_NORMAL.fetch_add(1, Ordering::Relaxed);
    } else {
        RT_RUNS_SINCE_NORMAL.store(0, Ordering::Relaxed);
    }
    Some(task)
}

pub fn has_task() -> bool {
    has_user_task() || has_kernel_task()
}

pub fn has_user_task() -> bool {
    has_runnable_task(&USER_READY_QUEUE)
}

pub fn has_kernel_task() -> bool {
    has_runnable_task(&KERNEL_READY_QUEUE)
}

fn has_runnable_task(queue: &Mutex<ReadyQueue>) -> bool {
    #[cfg(feature = "buildstorm-diagnostics")]
    let mut queue = crate::buildstorm_diagnostics::lock(
        crate::buildstorm_diagnostics::LockClass::TaskManager,
        queue,
    );
    #[cfg(not(feature = "buildstorm-diagnostics"))]
    let mut queue = queue.lock();
    #[cfg(feature = "buildstorm-diagnostics")]
    crate::buildstorm_diagnostics::note_runqueue_len(queue.len());
    let current_cpu = crate::platform::current_cpu_index();
    let mut index = 0;
    while index < queue.tasks.len() {
        let task = &queue.tasks[index];
        if task.status() != TaskStatus::Ready {
            queue.remove_at(index);
            continue;
        }
        if task.can_run_on_cpu(current_cpu) {
            return true;
        }
        index += 1;
    }
    false
}

pub fn remove_task(pid: usize) -> Option<Arc<TaskControlBlock>> {
    remove_task_from_queue(&USER_READY_QUEUE, pid)
        .or_else(|| remove_task_from_queue(&KERNEL_READY_QUEUE, pid))
}

fn remove_task_from_queue(queue: &Mutex<ReadyQueue>, pid: usize) -> Option<Arc<TaskControlBlock>> {
    let mut queue = queue.lock();
    let index = queue.tasks.iter().position(|task| task.pid.0 == pid)?;
    queue.remove_at(index)
}

pub fn remove_task_instances(task: &Arc<TaskControlBlock>) -> usize {
    remove_task_instances_from_queue(&USER_READY_QUEUE, task)
        + remove_task_instances_from_queue(&KERNEL_READY_QUEUE, task)
}

fn remove_task_instances_from_queue(
    queue: &Mutex<ReadyQueue>,
    task: &Arc<TaskControlBlock>,
) -> usize {
    let mut removed = 0usize;
    let mut queue = queue.lock();
    let mut kept = VecDeque::new();
    while let Some(queued) = queue.tasks.pop_front() {
        if Arc::ptr_eq(&queued, task) || queued.pid.0 == task.pid.0 {
            queue.queued_pids.remove(&queued.pid.0);
            removed += 1;
        } else {
            kept.push_back(queued);
        }
    }
    queue.tasks = kept;
    removed
}

pub fn retain_tasks(mut keep: impl FnMut(&Arc<TaskControlBlock>) -> bool) {
    retain_queue(&USER_READY_QUEUE, &mut keep);
    retain_queue(&KERNEL_READY_QUEUE, &mut keep);
}

fn retain_queue(queue: &Mutex<ReadyQueue>, keep: &mut impl FnMut(&Arc<TaskControlBlock>) -> bool) {
    let mut queue = queue.lock();
    let mut kept = VecDeque::new();
    let mut kept_pids = BTreeSet::new();
    while let Some(task) = queue.tasks.pop_front() {
        if keep(&task) && kept_pids.insert(task.pid.0) {
            kept.push_back(task);
        }
    }
    queue.tasks = kept;
    queue.queued_pids = kept_pids;
}

fn live_tasks() -> Vec<Arc<TaskControlBlock>> {
    let mut registry = TASK_REGISTRY.lock();
    let mut tasks = Vec::new();
    let mut index = 0usize;
    while index < registry.len() {
        if let Some(task) = registry[index].upgrade() {
            tasks.push(task);
            index += 1;
        } else {
            registry.remove(index);
        }
    }
    tasks
}

pub fn find_task(pid: usize) -> Option<Arc<TaskControlBlock>> {
    live_tasks().into_iter().find(|task| task.pid.0 == pid)
}

pub fn find_thread_group(tgid: usize) -> Vec<Arc<TaskControlBlock>> {
    live_tasks()
        .into_iter()
        .filter(|task| task.thread_group.tgid() == tgid && !task.is_kernel)
        .collect()
}

pub fn find_process_group(pgid: usize) -> Vec<Arc<TaskControlBlock>> {
    live_tasks()
        .into_iter()
        .filter(|task| !task.is_kernel && task.inner.lock().pgid == pgid)
        .collect()
}

pub fn all_user_tasks() -> Vec<Arc<TaskControlBlock>> {
    live_tasks()
        .into_iter()
        .filter(|task| !task.is_kernel)
        .collect()
}

pub fn was_thread_group_seen(tgid: usize) -> bool {
    crate::task::pid::has_ever_allocated(tgid)
        || EXITED_TASKS
            .lock()
            .iter()
            .any(|(_, seen_tgid)| *seen_tgid == tgid)
}

pub fn queue_len() -> usize {
    user_queue_len() + kernel_queue_len()
}

/// Snapshot only.  It deliberately walks the weak registry in place instead of
/// building a task vector, because BuildStorm diagnostics must not allocate while
/// reporting scheduler state.
#[cfg(feature = "buildstorm-diagnostics")]
pub(crate) fn diagnostic_task_counts() -> (
    usize,
    usize,
    usize,
    usize,
    usize,
    usize,
    usize,
    usize,
    usize,
    usize,
) {
    let registry = TASK_REGISTRY.lock();
    let mut user_live = 0usize;
    let mut user_runnable = 0usize;
    let mut user_blocked = 0usize;
    let mut rustc_live = 0usize;
    let mut rustc_runnable = 0usize;
    let mut process_live = 0usize;
    let mut process_runnable = 0usize;
    let mut process_blocked = 0usize;
    let mut rustc_process_live = 0usize;
    let mut rustc_process_runnable = 0usize;
    for weak in registry.iter() {
        let Some(task) = weak.upgrade() else {
            continue;
        };
        if task.is_kernel {
            continue;
        }
        user_live += 1;
        let status = *task.status.lock();
        if matches!(status, TaskStatus::Ready | TaskStatus::Running) {
            user_runnable += 1;
        }
        if matches!(status, TaskStatus::Blocked) {
            user_blocked += 1;
        }
        let is_rustc = task
            .inner
            .try_lock()
            .map(|inner| inner.exec_path.contains("rustc"))
            .unwrap_or(false);
        if is_rustc {
            rustc_live += 1;
            if matches!(status, TaskStatus::Ready | TaskStatus::Running) {
                rustc_runnable += 1;
            }
        }
        // The thread-group leader is the process identity in this kernel.
        // Count it separately from TCBs so rustc worker threads do not inflate
        // the diagnostic process-concurrency result.  This reporting path is
        // feature-gated, allocation-free, and never changes scheduler policy.
        if task.pid.0 == task.thread_group.tgid() {
            process_live += 1;
            if matches!(status, TaskStatus::Ready | TaskStatus::Running) {
                process_runnable += 1;
            }
            if matches!(status, TaskStatus::Blocked) {
                process_blocked += 1;
            }
            if is_rustc {
                rustc_process_live += 1;
                if matches!(status, TaskStatus::Ready | TaskStatus::Running) {
                    rustc_process_runnable += 1;
                }
            }
        }
    }
    (
        user_live,
        user_runnable,
        user_blocked,
        rustc_live,
        rustc_runnable,
        process_live,
        process_runnable,
        process_blocked,
        rustc_process_live,
        rustc_process_runnable,
    )
}

/// Rate-limited caller-side diagnostic output only.  It deliberately uses the
/// existing weak registry and `try_lock` so it neither allocates nor blocks a
/// user task merely to print its command name.
#[cfg(feature = "buildstorm-diagnostics")]
pub(crate) fn diagnostic_dump_user_comm() {
    let registry = TASK_REGISTRY.lock();
    for weak in registry.iter() {
        let Some(task) = weak.upgrade() else {
            continue;
        };
        if task.is_kernel {
            continue;
        }
        let status = *task.status.lock();
        let reason = *task.block_reason.lock();
        let Some(inner) = task.inner.try_lock() else {
            continue;
        };
        crate::println!(
            "BUILDSTORM_DIAG comm pid={} tgid={} status={:?} block={:?} user_ticks={} kernel_ticks={} user_run_count={} user_run_total_us={} user_run_max_us={} block_count={} block_total_us={} vma_current={} vma_max={} comm={}",
            task.pid.0,
            task.thread_group.tgid(),
            status,
            reason,
            task.diagnostic_user_ticks.load(Ordering::Relaxed),
            task.diagnostic_kernel_ticks.load(Ordering::Relaxed),
            task.diagnostic_user_run_count.load(Ordering::Relaxed),
            task.diagnostic_user_run_total_us.load(Ordering::Relaxed),
            task.diagnostic_user_run_max_us.load(Ordering::Relaxed),
            task.diagnostic_block_count.load(Ordering::Relaxed),
            task.diagnostic_block_total_us.load(Ordering::Relaxed),
            task.diagnostic_vma_current.load(Ordering::Relaxed),
            task.diagnostic_vma_max.load(Ordering::Relaxed),
            inner.exec_path,
        );
        if task.pid.0 == task.thread_group.tgid() {
            crate::buildstorm_diagnostics::report_mmap_protocol(&task.thread_group);
        }
    }
}

pub fn user_queue_len() -> usize {
    USER_READY_QUEUE.lock().len()
}

pub fn kernel_queue_len() -> usize {
    KERNEL_READY_QUEUE.lock().len()
}
