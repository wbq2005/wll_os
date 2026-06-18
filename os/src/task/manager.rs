use alloc::collections::VecDeque;
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use core::sync::atomic::{AtomicUsize, Ordering};
use lazy_static::lazy_static;
use spin::Mutex;

use super::{TaskControlBlock, TaskStatus};

lazy_static! {
    static ref USER_READY_QUEUE: Mutex<VecDeque<Arc<TaskControlBlock>>> =
        Mutex::new(VecDeque::new());
    static ref KERNEL_READY_QUEUE: Mutex<VecDeque<Arc<TaskControlBlock>>> =
        Mutex::new(VecDeque::new());
    static ref TASK_REGISTRY: Mutex<Vec<Weak<TaskControlBlock>>> = Mutex::new(Vec::new());
    static ref EXITED_TASKS: Mutex<Vec<(usize, usize)>> = Mutex::new(Vec::new());
}

const RT_RUN_BUDGET: usize = 64;
static RT_RUNS_SINCE_NORMAL: AtomicUsize = AtomicUsize::new(0);

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

fn push_task_back(queue: &Mutex<VecDeque<Arc<TaskControlBlock>>>, task: Arc<TaskControlBlock>) {
    let mut queue = queue.lock();
    if queue
        .iter()
        .any(|queued| Arc::ptr_eq(queued, &task) || queued.pid.0 == task.pid.0)
    {
        return;
    }
    queue.push_back(task);
}

pub fn add_task_front(task: Arc<TaskControlBlock>) {
    if task.is_kernel {
        push_task_front(&KERNEL_READY_QUEUE, task);
    } else {
        push_task_front(&USER_READY_QUEUE, task);
    }
}

fn push_task_front(queue: &Mutex<VecDeque<Arc<TaskControlBlock>>>, task: Arc<TaskControlBlock>) {
    let mut queue = queue.lock();
    if queue
        .iter()
        .any(|queued| Arc::ptr_eq(queued, &task) || queued.pid.0 == task.pid.0)
    {
        return;
    }
    queue.push_front(task);
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

fn fetch_from_queue(
    queue: &Mutex<VecDeque<Arc<TaskControlBlock>>>,
) -> Option<Arc<TaskControlBlock>> {
    let mut queue = queue.lock();
    let mut best_index = None;
    let mut best_priority = 0;
    let mut first_normal_index = None;
    let mut index = 0;
    while index < queue.len() {
        let task = &queue[index];
        if matches!(task.status(), TaskStatus::Zombie | TaskStatus::Blocked) {
            queue.remove(index);
            continue;
        }
        let priority = task.effective_sched_priority();
        if priority == 0 && first_normal_index.is_none() {
            first_normal_index = Some(index);
        }
        if best_index.is_none() || priority > best_priority {
            best_index = Some(index);
            best_priority = priority;
        }
        index += 1;
    }
    let chosen_index = if best_priority > 0 {
        let rt_runs = RT_RUNS_SINCE_NORMAL.load(Ordering::Relaxed);
        if rt_runs >= RT_RUN_BUDGET {
            first_normal_index.or(best_index)
        } else {
            best_index
        }
    } else {
        best_index
    }?;
    let task = queue.remove(chosen_index)?;
    if best_priority > 0 && task.effective_sched_priority() > 0 {
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

fn has_runnable_task(queue: &Mutex<VecDeque<Arc<TaskControlBlock>>>) -> bool {
    queue
        .lock()
        .iter()
        .any(|task| !matches!(task.status(), TaskStatus::Zombie | TaskStatus::Blocked))
}

pub fn remove_task(pid: usize) -> Option<Arc<TaskControlBlock>> {
    remove_task_from_queue(&USER_READY_QUEUE, pid)
        .or_else(|| remove_task_from_queue(&KERNEL_READY_QUEUE, pid))
}

fn remove_task_from_queue(
    queue: &Mutex<VecDeque<Arc<TaskControlBlock>>>,
    pid: usize,
) -> Option<Arc<TaskControlBlock>> {
    let mut queue = queue.lock();
    let index = queue.iter().position(|task| task.pid.0 == pid)?;
    queue.remove(index)
}

pub fn remove_task_instances(task: &Arc<TaskControlBlock>) -> usize {
    remove_task_instances_from_queue(&USER_READY_QUEUE, task)
        + remove_task_instances_from_queue(&KERNEL_READY_QUEUE, task)
}

fn remove_task_instances_from_queue(
    queue: &Mutex<VecDeque<Arc<TaskControlBlock>>>,
    task: &Arc<TaskControlBlock>,
) -> usize {
    let mut removed = 0usize;
    let mut queue = queue.lock();
    let mut kept = VecDeque::new();
    while let Some(queued) = queue.pop_front() {
        if Arc::ptr_eq(&queued, task) || queued.pid.0 == task.pid.0 {
            removed += 1;
        } else {
            kept.push_back(queued);
        }
    }
    *queue = kept;
    removed
}

pub fn retain_tasks(mut keep: impl FnMut(&Arc<TaskControlBlock>) -> bool) {
    retain_queue(&USER_READY_QUEUE, &mut keep);
    retain_queue(&KERNEL_READY_QUEUE, &mut keep);
}

fn retain_queue(
    queue: &Mutex<VecDeque<Arc<TaskControlBlock>>>,
    keep: &mut impl FnMut(&Arc<TaskControlBlock>) -> bool,
) {
    let mut queue = queue.lock();
    let mut kept = VecDeque::new();
    while let Some(task) = queue.pop_front() {
        if keep(&task) {
            kept.push_back(task);
        }
    }
    *queue = kept;
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
