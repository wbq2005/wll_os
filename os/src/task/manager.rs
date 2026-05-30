use alloc::collections::VecDeque;
use alloc::sync::Arc;
use lazy_static::lazy_static;
use spin::Mutex;

use super::TaskControlBlock;

lazy_static! {
    /// 全局就绪队列
    static ref READY_QUEUE: Mutex<VecDeque<Arc<TaskControlBlock>>> = Mutex::new(VecDeque::new());
}

/// 添加任务到就绪队列
pub fn add_task(task: Arc<TaskControlBlock>) {
    log::info!("[task] add_task pid={}", task.pid.0);
    READY_QUEUE.lock().push_back(task);
}

/// 从就绪队列取出一个任务
pub fn fetch_task() -> Option<Arc<TaskControlBlock>> {
    READY_QUEUE.lock().pop_front()
}

/// 检查就绪队列是否有任务
pub fn has_task() -> bool {
    !READY_QUEUE.lock().is_empty()
}

/// 返回就绪队列长度（诊断用）
pub fn queue_len() -> usize {
    READY_QUEUE.lock().len()
}
