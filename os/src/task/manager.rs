use alloc::collections::VecDeque;
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use lazy_static::lazy_static;
use spin::Mutex;

use super::{TaskControlBlock, TaskStatus};

lazy_static! {
    /// 全局就绪队列
    static ref READY_QUEUE: Mutex<VecDeque<Arc<TaskControlBlock>>> = Mutex::new(VecDeque::new());
    static ref TASK_REGISTRY: Mutex<Vec<Weak<TaskControlBlock>>> = Mutex::new(Vec::new());
    static ref EXITED_TASKS: Mutex<Vec<(usize, usize)>> = Mutex::new(Vec::new());
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

/// 添加任务到就绪队列
pub fn add_task(task: Arc<TaskControlBlock>) {
    READY_QUEUE.lock().push_back(task);
}

/// 从就绪队列取出一个任务
pub fn fetch_task() -> Option<Arc<TaskControlBlock>> {
    let mut queue = READY_QUEUE.lock();
    while let Some(task) = queue.pop_front() {
        if !matches!(task.status(), TaskStatus::Zombie | TaskStatus::Blocked) {
            return Some(task);
        }
    }
    None
}

/// 检查就绪队列是否有任务
pub fn has_task() -> bool {
    READY_QUEUE
        .lock()
        .iter()
        .any(|task| !matches!(task.status(), TaskStatus::Zombie | TaskStatus::Blocked))
}

pub fn remove_task(pid: usize) -> Option<Arc<TaskControlBlock>> {
    let mut queue = READY_QUEUE.lock();
    let index = queue.iter().position(|task| task.pid.0 == pid)?;
    queue.remove(index)
}

pub fn retain_tasks(mut keep: impl FnMut(&Arc<TaskControlBlock>) -> bool) {
    let mut queue = READY_QUEUE.lock();
    let mut kept = VecDeque::new();
    while let Some(task) = queue.pop_front() {
        if keep(&task) {
            kept.push_back(task);
        }
    }
    *queue = kept;
}

/// Snapshot live tasks from the weak registry and compact stale entries.
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

/// 返回就绪队列长度（诊断用）
pub fn queue_len() -> usize {
    READY_QUEUE.lock().len()
}
