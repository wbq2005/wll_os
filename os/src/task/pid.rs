use alloc::vec::Vec;
use lazy_static::lazy_static;
use spin::Mutex;

lazy_static! {
    static ref PID_ALLOCATOR: Mutex<PidAllocator> = Mutex::new(PidAllocator::new());
}

/// PID 分配器
pub struct PidAllocator {
    current: usize,
    recycled: Vec<usize>,
}

impl PidAllocator {
    pub fn new() -> Self {
        Self {
            current: 1,
            recycled: Vec::new(),
        }
    }

    pub fn alloc(&mut self) -> usize {
        if let Some(pid) = self.recycled.pop() {
            pid
        } else {
            let pid = self.current;
            self.current += 1;
            pid
        }
    }

    pub fn dealloc(&mut self, pid: usize) {
        let _ = pid;
    }
}

/// PID 包装类型
pub struct Pid(pub usize);

impl Pid {
    pub fn alloc() -> Self {
        Self(PID_ALLOCATOR.lock().alloc())
    }
}

pub fn has_ever_allocated(pid: usize) -> bool {
    let allocator = PID_ALLOCATOR.lock();
    pid > 0 && pid < allocator.current
}

impl Drop for Pid {
    fn drop(&mut self) {
        PID_ALLOCATOR.lock().dealloc(self.0);
    }
}
