use buddy_system_allocator::Heap;
use core::alloc::{GlobalAlloc, Layout};
use core::ptr::NonNull;
#[cfg(feature = "buildstorm-diagnostics")]
use core::sync::atomic::{AtomicUsize, Ordering};
use spin::Mutex;

/// 内核堆大小: 128MB（预加载的测试用例和大 ELF exec 缓冲可能占数十 MB）
const KERNEL_HEAP_SIZE: usize = 0x800_0000;
const MAX_DYNAMIC_HEAP_SIZE: usize = 0x8000_0000;
const DYNAMIC_HEAP_MEMORY_FRACTION: usize = 4;

const MIN_CACHE_SHIFT: usize = 3;
const MAX_CACHE_SHIFT: usize = 12;
const SMALL_CLASS_COUNT: usize = MAX_CACHE_SHIFT - MIN_CACHE_SHIFT + 1;
const CACHE_CAPACITY: usize = 64;
const REFILL_BATCH: usize = 16;
const FLUSH_BATCH: usize = 16;

/// 内核堆空间
static mut HEAP_SPACE: [u8; KERNEL_HEAP_SIZE] = [0; KERNEL_HEAP_SIZE];

struct SmallCache {
    blocks: [[usize; CACHE_CAPACITY]; SMALL_CLASS_COUNT],
    lengths: [u8; SMALL_CLASS_COUNT],
}

impl SmallCache {
    const fn empty() -> Self {
        Self {
            blocks: [[0; CACHE_CAPACITY]; SMALL_CLASS_COUNT],
            lengths: [0; SMALL_CLASS_COUNT],
        }
    }

    #[inline]
    fn pop(&mut self, class: usize) -> Option<usize> {
        let len = self.lengths[class] as usize;
        if len == 0 {
            return None;
        }
        let next = len - 1;
        self.lengths[class] = next as u8;
        Some(self.blocks[class][next])
    }

    #[inline]
    fn push(&mut self, class: usize, ptr: usize) -> bool {
        let len = self.lengths[class] as usize;
        if len == CACHE_CAPACITY {
            return false;
        }
        self.blocks[class][len] = ptr;
        self.lengths[class] = (len + 1) as u8;
        true
    }

    fn drain_class(&mut self, class: usize, output: &mut [usize]) -> usize {
        let mut count = 0;
        while count < output.len() {
            let Some(ptr) = self.pop(class) else {
                break;
            };
            output[count] = ptr;
            count += 1;
        }
        count
    }
}

struct KernelHeapAllocator {
    central: Mutex<Heap<32>>,
    caches: [Mutex<SmallCache>; crate::config::MAX_CPUS],
}

struct InterruptGuard {
    restore_enabled: bool,
}

impl InterruptGuard {
    #[inline]
    fn new() -> Self {
        let restore_enabled = crate::trap::interrupts::is_interrupt_enabled();
        if restore_enabled {
            crate::trap::interrupts::disable_interrupt();
        }
        Self { restore_enabled }
    }
}

impl Drop for InterruptGuard {
    #[inline]
    fn drop(&mut self) {
        if self.restore_enabled {
            crate::trap::interrupts::enable_interrupt();
        }
    }
}

impl KernelHeapAllocator {
    const fn empty() -> Self {
        Self {
            central: Mutex::new(Heap::empty()),
            caches: [const { Mutex::new(SmallCache::empty()) }; crate::config::MAX_CPUS],
        }
    }

    #[inline]
    fn cache_class(layout: Layout) -> Option<usize> {
        let block_size = layout
            .size()
            .next_power_of_two()
            .max(layout.align())
            .max(core::mem::size_of::<usize>());
        let shift = block_size.trailing_zeros() as usize;
        (shift <= MAX_CACHE_SHIFT).then_some(shift - MIN_CACHE_SHIFT)
    }

    #[inline]
    fn class_layout(class: usize) -> Layout {
        let block_size = 1usize << (class + MIN_CACHE_SHIFT);
        unsafe { Layout::from_size_align_unchecked(block_size, block_size) }
    }

    #[inline]
    fn cpu_index() -> usize {
        crate::platform::current_cpu_index().min(crate::config::MAX_CPUS - 1)
    }

    fn central_alloc_batch(&self, layout: Layout, output: &mut [usize]) -> usize {
        #[cfg(feature = "buildstorm-diagnostics")]
        let mut central =
            crate::buildstorm_diagnostics::lock_heap(&self.central, layout.size(), true);
        #[cfg(not(feature = "buildstorm-diagnostics"))]
        let mut central = self.central.lock();

        let mut count = 0;
        while count < output.len() {
            let Ok(ptr) = central.alloc(layout) else {
                break;
            };
            output[count] = ptr.as_ptr() as usize;
            count += 1;
        }
        #[cfg(feature = "buildstorm-diagnostics")]
        CENTRAL_ALLOCS.fetch_add(count, Ordering::Relaxed);
        count
    }

    unsafe fn central_dealloc_batch(&self, layout: Layout, blocks: &[usize]) {
        if blocks.is_empty() {
            return;
        }
        #[cfg(feature = "buildstorm-diagnostics")]
        let mut central =
            crate::buildstorm_diagnostics::lock_heap(&self.central, layout.size(), false);
        #[cfg(not(feature = "buildstorm-diagnostics"))]
        let mut central = self.central.lock();

        for &ptr in blocks {
            central.dealloc(NonNull::new_unchecked(ptr as *mut u8), layout);
        }
        #[cfg(feature = "buildstorm-diagnostics")]
        CENTRAL_FREES.fetch_add(blocks.len(), Ordering::Relaxed);
    }

    fn drain_all_caches(&self) {
        #[cfg(feature = "buildstorm-diagnostics")]
        let mut drained_total = 0;
        for cpu in 0..crate::config::MAX_CPUS {
            for class in 0..SMALL_CLASS_COUNT {
                let mut blocks = [0usize; CACHE_CAPACITY];
                let count = {
                    let mut cache = self.caches[cpu].lock();
                    cache.drain_class(class, &mut blocks)
                };
                if count != 0 {
                    unsafe {
                        self.central_dealloc_batch(Self::class_layout(class), &blocks[..count]);
                    }
                    #[cfg(feature = "buildstorm-diagnostics")]
                    {
                        drained_total += count;
                    }
                }
            }
        }
        #[cfg(feature = "buildstorm-diagnostics")]
        DRAINED_BLOCKS.fetch_add(drained_total, Ordering::Relaxed);
    }

    fn alloc_small(&self, class: usize) -> *mut u8 {
        let cpu = Self::cpu_index();
        if let Some(ptr) = self.caches[cpu].lock().pop(class) {
            #[cfg(feature = "buildstorm-diagnostics")]
            CACHE_HITS.fetch_add(1, Ordering::Relaxed);
            return ptr as *mut u8;
        }
        #[cfg(feature = "buildstorm-diagnostics")]
        CACHE_MISSES.fetch_add(1, Ordering::Relaxed);

        let layout = Self::class_layout(class);
        let mut blocks = [0usize; REFILL_BATCH];
        let mut count = self.central_alloc_batch(layout, &mut blocks);
        if count == 0 {
            self.drain_all_caches();
            count = self.central_alloc_batch(layout, &mut blocks);
        }
        if count == 0 {
            return core::ptr::null_mut();
        }
        #[cfg(feature = "buildstorm-diagnostics")]
        REFILL_BLOCKS.fetch_add(count, Ordering::Relaxed);

        let result = blocks[0] as *mut u8;
        let mut returned = [0usize; REFILL_BATCH];
        let returned_count = {
            let mut cache = self.caches[cpu].lock();
            let mut returned_count = 0;
            for &ptr in &blocks[1..count] {
                if !cache.push(class, ptr) {
                    returned[returned_count] = ptr;
                    returned_count += 1;
                }
            }
            returned_count
        };
        if returned_count != 0 {
            unsafe {
                self.central_dealloc_batch(layout, &returned[..returned_count]);
            }
        }
        result
    }

    unsafe fn dealloc_small(&self, ptr: *mut u8, class: usize) {
        let cpu = Self::cpu_index();
        let mut flushed = [0usize; FLUSH_BATCH];
        let flushed_count = {
            let mut cache = self.caches[cpu].lock();
            if cache.push(class, ptr as usize) {
                0
            } else {
                let count = cache.drain_class(class, &mut flushed);
                let inserted = cache.push(class, ptr as usize);
                debug_assert!(inserted);
                count
            }
        };
        #[cfg(feature = "buildstorm-diagnostics")]
        CACHED_FREES.fetch_add(1, Ordering::Relaxed);
        if flushed_count != 0 {
            self.central_dealloc_batch(Self::class_layout(class), &flushed[..flushed_count]);
        }
    }
}

unsafe impl GlobalAlloc for KernelHeapAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let _interrupt_guard = InterruptGuard::new();
        if let Some(class) = Self::cache_class(layout) {
            return self.alloc_small(class);
        }

        let mut block = [0usize; 1];
        if self.central_alloc_batch(layout, &mut block) == 0 {
            self.drain_all_caches();
            if self.central_alloc_batch(layout, &mut block) == 0 {
                return core::ptr::null_mut();
            }
        }
        block[0] as *mut u8
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        let _interrupt_guard = InterruptGuard::new();
        if let Some(class) = Self::cache_class(layout) {
            self.dealloc_small(ptr, class);
        } else {
            self.central_dealloc_batch(layout, &[ptr as usize]);
        }
    }
}

/// 全局堆分配器
#[global_allocator]
static HEAP_ALLOCATOR: KernelHeapAllocator = KernelHeapAllocator::empty();

#[cfg(feature = "buildstorm-diagnostics")]
static CACHE_HITS: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "buildstorm-diagnostics")]
static CACHE_MISSES: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "buildstorm-diagnostics")]
static REFILL_BLOCKS: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "buildstorm-diagnostics")]
static CACHED_FREES: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "buildstorm-diagnostics")]
static CENTRAL_ALLOCS: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "buildstorm-diagnostics")]
static CENTRAL_FREES: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "buildstorm-diagnostics")]
static DRAINED_BLOCKS: AtomicUsize = AtomicUsize::new(0);

#[cfg(feature = "buildstorm-diagnostics")]
pub(crate) fn report_cache_diagnostics() {
    crate::println!(
        "BUILDSTORM_DIAG heap_cache hits={} misses={} refill_blocks={} cached_frees={} central_allocs={} central_frees={} drained_blocks={}",
        CACHE_HITS.load(Ordering::Relaxed),
        CACHE_MISSES.load(Ordering::Relaxed),
        REFILL_BLOCKS.load(Ordering::Relaxed),
        CACHED_FREES.load(Ordering::Relaxed),
        CENTRAL_ALLOCS.load(Ordering::Relaxed),
        CENTRAL_FREES.load(Ordering::Relaxed),
        DRAINED_BLOCKS.load(Ordering::Relaxed),
    );
}

/// 初始化内核堆
pub fn init_heap() {
    unsafe {
        let heap_start = core::ptr::addr_of!(HEAP_SPACE) as usize;
        HEAP_ALLOCATOR
            .central
            .lock()
            .init(heap_start, KERNEL_HEAP_SIZE);
    }
}

/// Reserve a memory-scaled physical range for transient kernel allocations.
///
/// User pages remain owned by the frame allocator. This permanently removes
/// one contiguous chunk (at most 2 GiB, and normally 1/4 of RAM) and adds it
/// to the kernel buddy heap. The larger boot-time reserve leaves high-order
/// blocks available after compiler metadata fragments the small-object heap;
/// it does not change allocator ownership or the IRQ/lock protocol.
pub fn grow_from_frame_allocator(total_memory_bytes: usize) -> usize {
    // Large user-space builds can require a single high-order allocation.
    // Reserving 1/4 of RAM keeps multi-hundred-MiB buddy blocks available on
    // the official 8 GiB runs while remaining capped at 2 GiB.
    let requested = (total_memory_bytes / DYNAMIC_HEAP_MEMORY_FRACTION)
        .min(MAX_DYNAMIC_HEAP_SIZE);
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
        HEAP_ALLOCATOR.central.lock().add_to_heap(start, end);
    }
    requested
}

/// 内存不足处理
#[alloc_error_handler]
pub fn handle_alloc_error(layout: Layout) -> ! {
    let _interrupt_guard = InterruptGuard::new();
    let heap = HEAP_ALLOCATOR.central.lock();
    panic!(
        "Heap allocation error, layout = {:?}, heap_user={}, heap_actual={}, heap_total={}",
        layout,
        heap.stats_alloc_user(),
        heap.stats_alloc_actual(),
        heap.stats_total_bytes(),
    );
}
