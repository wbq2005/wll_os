use buddy_system_allocator::LockedHeap;

/// 内核堆大小: 64MB（预加载的测试用例文件可能占数十 MB）
const KERNEL_HEAP_SIZE: usize = 0x400_0000;

/// 内核堆空间
static mut HEAP_SPACE: [u8; KERNEL_HEAP_SIZE] = [0; KERNEL_HEAP_SIZE];

/// 全局堆分配器
#[global_allocator]
static HEAP_ALLOCATOR: LockedHeap<32> = LockedHeap::empty();

/// 初始化内核堆
pub fn init_heap() {
    unsafe {
        let heap_start = core::ptr::addr_of!(HEAP_SPACE) as usize;
        HEAP_ALLOCATOR.lock().init(
            heap_start,
            KERNEL_HEAP_SIZE,
        );
    }
}

/// 内存不足处理
#[alloc_error_handler]
pub fn handle_alloc_error(layout: core::alloc::Layout) -> ! {
    panic!("Heap allocation error, layout = {:?}", layout);
}
