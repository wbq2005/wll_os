use super::frame_allocator::FrameTracker;
use crate::mm::page_table::PTEFlags;
use alloc::vec::Vec;
use polyhal::VirtAddr;

/// 虚拟内存区域 (VMA)
pub struct MapArea {
    pub start_va: VirtAddr,
    pub end_va: VirtAddr,
    pub flags: PTEFlags,
    pub frames: Vec<FrameTracker>,
}

impl MapArea {
    pub fn new(start_va: VirtAddr, end_va: VirtAddr, flags: PTEFlags) -> Self {
        Self {
            start_va,
            end_va,
            flags,
            frames: Vec::new(),
        }
    }

    pub fn size(&self) -> usize {
        self.end_va.raw() - self.start_va.raw()
    }

    pub fn contains(&self, va: VirtAddr) -> bool {
        va.raw() >= self.start_va.raw() && va.raw() < self.end_va.raw()
    }
}
