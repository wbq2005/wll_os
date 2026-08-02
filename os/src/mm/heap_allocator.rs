use buddy_system_allocator::LockedHeap;

/// 内核堆大小: 128MB（预加载的测试用例和大 ELF exec 缓冲可能占数十 MB）
const KERNEL_HEAP_SIZE: usize = 0x800_0000;
const MAX_DYNAMIC_HEAP_SIZE: usize = 0x4000_0000;

/// 内核堆空间
static mut HEAP_SPACE: [u8; KERNEL_HEAP_SIZE] = [0; KERNEL_HEAP_SIZE];

/// 全局堆分配器
#[global_allocator]
static HEAP_ALLOCATOR: LockedHeap<32> = LockedHeap::empty();

/// 初始化内核堆
pub fn init_heap() {
    unsafe {
        let heap_start = core::ptr::addr_of!(HEAP_SPACE) as usize;
        HEAP_ALLOCATOR.lock().init(heap_start, KERNEL_HEAP_SIZE);
    }
}

/// Reserve a memory-scaled physical range for transient kernel allocations.
///
/// User pages remain owned by the frame allocator. This permanently removes
/// one contiguous chunk (at most 1 GiB, and normally 1/8 of RAM) and adds it
/// to the kernel buddy heap so parallel exec, VFS writeback, and scheduler
/// metadata do not have to fit inside the fixed early-boot heap alone.
pub fn grow_from_frame_allocator(total_memory_bytes: usize) -> usize {
    // Large user-space builds can require a single high-order allocation.
    // Reserving 1/8 of RAM keeps a complete 512 MiB buddy block available on
    // the official 8 GiB runs while remaining capped at the existing 1 GiB.
    let requested = (total_memory_bytes / 8).min(MAX_DYNAMIC_HEAP_SIZE);
    let requested = if requested.is_power_of_two() {
        requested
    } else {
        requested.next_power_of_two() / 2
    };
    if requested < crate::config::PAGE_SIZE {
        return 0;
    }
    let pages = requested / crate::config::PAGE_SIZE;
    let Some(start_ppn) = crate::mm::frame_allocator::alloc_contiguous_frames(pages) else {
        log::warn!(
            "[heap] unable to reserve dynamic heap extension of {} MiB",
            requested / 1024 / 1024
        );
        return 0;
    };
    let start_phys = start_ppn * crate::config::PAGE_SIZE;
    #[cfg(target_arch = "loongarch64")]
    let start = polyhal::PhysAddr::new(start_phys).get_mut_ptr::<u8>() as usize;
    #[cfg(not(target_arch = "loongarch64"))]
    let start = start_phys;
    let end = start + requested;
    unsafe {
        HEAP_ALLOCATOR.lock().add_to_heap(start, end);
    }
    requested
}

/// 内存不足处理
#[alloc_error_handler]
pub fn handle_alloc_error(layout: core::alloc::Layout) -> ! {
    let heap = HEAP_ALLOCATOR.lock();
    panic!(
        "Heap allocation error, layout = {:?}, heap_user={}, heap_actual={}, heap_total={}",
        layout,
        heap.stats_alloc_user(),
        heap.stats_alloc_actual(),
        heap.stats_total_bytes(),
    );
}
