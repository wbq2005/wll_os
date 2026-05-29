pub mod frame_allocator;
pub mod heap_allocator;
pub mod page_table;
pub mod memory_set;
pub mod map_area;
pub mod elf_loader;

use polyhal::{PhysAddr};
use polyhal::common::PageAlloc;

/// 内核页分配器实现
pub struct KernelPageAlloc;

impl PageAlloc for KernelPageAlloc {
    fn alloc(&self) -> PhysAddr {
        frame_allocator::alloc_frame()
            .map(|frame| {
                let paddr: PhysAddr = frame.ppn().into();
                paddr.clear_len(crate::config::PAGE_SIZE);
                core::mem::forget(frame);
                paddr
            })
            .unwrap_or_else(|| PhysAddr::new(0))
    }

    fn dealloc(&self, paddr: PhysAddr) {
        if let Ok(ppn) = frame_allocator::PhysPageNum::try_from(paddr) {
            frame_allocator::dealloc_frame(ppn);
        }
    }
}

/// 初始化内存管理子系统
pub fn init() {
    heap_allocator::init_heap();
    frame_allocator::init_frame_allocator();
}
