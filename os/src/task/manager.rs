use alloc::collections::VecDeque;
use alloc::sync::Arc;
use lazy_static::lazy_static;
use spin::Mutex;

use super::{TaskControlBlock, TaskStatus};

lazy_static! {
    /// 全局就绪队列
    static ref READY_QUEUE: Mutex<VecDeque<Arc<TaskControlBlock>>> = Mutex::new(VecDeque::new());
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

/// 返回就绪队列长度（诊断用）
pub fn queue_len() -> usize {
    READY_QUEUE.lock().len()
}
