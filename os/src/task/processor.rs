/// CPU 执行状态管理

use alloc::sync::Arc;

use super::TaskControlBlock;

/// CPU 状态
pub struct Processor {
    current: Option<Arc<TaskControlBlock>>,
}

impl Processor {
    pub fn new() -> Self {
        Self { current: None }
    }

    pub fn take_current(&mut self) -> Option<Arc<TaskControlBlock>> {
        self.current.take()
    }

    pub fn current(&self) -> Option<Arc<TaskControlBlock>> {
        self.current.clone()
    }

    pub fn set_current(&mut self, task: Arc<TaskControlBlock>) {
        self.current = Some(task);
    }
}
