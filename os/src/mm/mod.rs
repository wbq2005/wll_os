pub mod elf_loader;
pub mod frame_allocator;
pub mod heap_allocator;
pub mod map_area;
pub mod memory_set;
pub mod page_table;

use polyhal::common::PageAlloc;
use polyhal::PhysAddr;

/// 内核页分配器实现
pub struct KernelPageAlloc;

impl PageAlloc for KernelPageAlloc {
    fn alloc(&self) -> PhysAddr {
        frame_allocator::alloc_frame()
            .map(|frame| {
                #[cfg(feature = "buildstorm-diagnostics")]
                frame_allocator::diagnostic_note_allocation(
                    frame_allocator::FrameAllocationClass::PageTable,
                );
                let paddr: PhysAddr = frame.into_raw_ppn().into();
                paddr.clear_len(crate::config::PAGE_SIZE);
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
