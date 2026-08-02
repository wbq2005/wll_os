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

    fn push_back(&mut self, task: Arc<TaskControlBlock>) {
        if self.queued_pids.insert(task.pid.0) {
            self.tasks.push_back(task);
        }
    }

    fn push_front(&mut self, task: Arc<TaskControlBlock>) {
        if self.queued_pids.insert(task.pid.0) {
            self.tasks.push_front(task);
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

pub fn add_user_task(task: Arc<TaskControlBlock>) {
    debug_assert!(!task.is_kernel);
    push_task_back(&USER_READY_QUEUE, task);
}

pub fn add_kernel_task(task: Arc<TaskControlBlock>) {
    debug_assert!(task.is_kernel);
    push_task_back(&KERNEL_READY_QUEUE, task);
}

fn push_task_back(queue: &Mutex<ReadyQueue>, task: Arc<TaskControlBlock>) {
    if task.status() != TaskStatus::Ready {
        return;
    }
    queue.lock().push_back(task);
    crate::platform::notify_runnable();
}

pub fn add_task_front(task: Arc<TaskControlBlock>) {
    if task.is_kernel {
        push_task_front(&KERNEL_READY_QUEUE, task);
    } else {
        push_task_front(&USER_READY_QUEUE, task);
    }
}

fn push_task_front(queue: &Mutex<ReadyQueue>, task: Arc<TaskControlBlock>) {
    if task.status() != TaskStatus::Ready {
        return;
    }
    queue.lock().push_front(task);
    crate::platform::notify_runnable();
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
    let mut queue = queue.lock();
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
    let mut queue = queue.lock();
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

pub fn user_queue_len() -> usize {
    USER_READY_QUEUE.lock().len()
}

pub fn kernel_queue_len() -> usize {
    KERNEL_READY_QUEUE.lock().len()
}
