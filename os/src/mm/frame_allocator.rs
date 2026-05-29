use alloc::vec::Vec;
use buddy_system_allocator::FrameAllocator;
use lazy_static::lazy_static;
use spin::Mutex;

use crate::config::PAGE_SIZE;

/// 物理页号
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct PhysPageNum(pub usize);

impl PhysPageNum {
    pub fn new(ppn: usize) -> Self {
        Self(ppn)
    }

    pub fn addr(&self) -> usize {
        self.0 * PAGE_SIZE
    }

    pub fn as_mut<T>(&self) -> &'static mut T {
        unsafe { &mut *(self.addr() as *mut T) }
    }
}

impl From<PhysPageNum> for polyhal::PhysAddr {
    fn from(ppn: PhysPageNum) -> Self {
        polyhal::PhysAddr::new(ppn.addr())
    }
}

impl TryFrom<polyhal::PhysAddr> for PhysPageNum {
    type Error = ();
    fn try_from(paddr: polyhal::PhysAddr) -> Result<Self, Self::Error> {
        if paddr.raw() % PAGE_SIZE == 0 {
            Ok(PhysPageNum(paddr.raw() / PAGE_SIZE))
        } else {
            Err(())
        }
    }
}

/// 页帧追踪器 - RAII自动释放
pub struct FrameTracker {
    pub ppn: PhysPageNum,
}

impl FrameTracker {
    pub fn new(ppn: PhysPageNum) -> Self {
        Self { ppn }
    }

    pub fn ppn(&self) -> PhysPageNum {
        self.ppn
    }
}

impl Drop for FrameTracker {
    fn drop(&mut self) {
        dealloc_frame(self.ppn);
    }
}

lazy_static! {
    static ref FRAME_ALLOCATOR: Mutex<FrameAllocator> = Mutex::new(FrameAllocator::new());
    static ref MEM_REGIONS: Mutex<Vec<(usize, usize)>> = Mutex::new(Vec::new());
}

/// 初始化帧分配器
pub fn init_frame_allocator() {
    // 将在启动时由main.rs添加内存区域
}

/// 添加可用物理内存区域
pub fn add_frames_range(start: usize, end: usize) {
    let start_ppn = (start + PAGE_SIZE - 1) / PAGE_SIZE;
    let end_ppn = end / PAGE_SIZE;
    if start_ppn < end_ppn {
        FRAME_ALLOCATOR.lock().add_frame(start_ppn, end_ppn);
        MEM_REGIONS.lock().push((start_ppn, end_ppn));
    }
}

/// 分配一个物理页帧
pub fn alloc_frame() -> Option<FrameTracker> {
    FRAME_ALLOCATOR
        .lock()
        .alloc(1)
        .map(|ppn| {
            let tracker = FrameTracker::new(PhysPageNum(ppn));
            unsafe {
                let ptr = PhysPageNum(ppn).addr() as *mut u8;
                core::ptr::write_bytes(ptr, 0, PAGE_SIZE);
            }
            tracker
        })
}

/// 分配连续的 `pages` 个物理页（用于 virtio DMA）。
/// 返回起始页帧号。
pub fn alloc_contiguous_frames(pages: usize) -> Option<usize> {
    if pages == 0 {
        return Some(0);
    }
    FRAME_ALLOCATOR.lock().alloc(pages)
}

/// 释放连续的 `pages` 个物理页。
pub fn dealloc_contiguous_frames(start_ppn: usize, pages: usize) {
    if pages != 0 {
        FRAME_ALLOCATOR.lock().dealloc(start_ppn, pages);
    }
}

/// 释放一个物理页帧
pub fn dealloc_frame(ppn: PhysPageNum) {
    FRAME_ALLOCATOR.lock().dealloc(ppn.0, 1);
}

/// 获取剩余可用页帧数
pub fn remaining_frames() -> usize {
    let _allocator = FRAME_ALLOCATOR.lock();
    // Buddy allocator doesn't have free_frames method, estimate from total - used
    // For now return 0 as placeholder
    0
}
