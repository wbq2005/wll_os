use alloc::vec::Vec;
use buddy_system_allocator::FrameAllocator;
#[cfg(feature = "buildstorm-diagnostics")]
use core::sync::atomic::AtomicU8;
use core::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use lazy_static::lazy_static;
use spin::{Mutex, Once};

use crate::config::PAGE_SIZE;

const FRAME_CACHE_CAPACITY: usize = 256;
const FRAME_CACHE_REFILL: usize = 64;
const FRAME_CACHE_FLUSH: usize = 64;

struct LocalFrameCache {
    frames: [usize; FRAME_CACHE_CAPACITY],
    len: usize,
}

impl LocalFrameCache {
    const fn empty() -> Self {
        Self {
            frames: [0; FRAME_CACHE_CAPACITY],
            len: 0,
        }
    }

    #[inline]
    fn pop(&mut self) -> Option<usize> {
        if self.len == 0 {
            return None;
        }
        self.len -= 1;
        Some(self.frames[self.len])
    }

    #[inline]
    fn push(&mut self, ppn: usize) -> bool {
        if self.len == FRAME_CACHE_CAPACITY {
            return false;
        }
        self.frames[self.len] = ppn;
        self.len += 1;
        true
    }

    fn drain(&mut self, output: &mut [usize]) -> usize {
        let mut count = 0;
        while count < output.len() {
            let Some(ppn) = self.pop() else {
                break;
            };
            output[count] = ppn;
            count += 1;
        }
        count
    }
}

static FRAME_CACHES: [Mutex<LocalFrameCache>; crate::config::MAX_CPUS] =
    [const { Mutex::new(LocalFrameCache::empty()) }; crate::config::MAX_CPUS];

struct FrameCacheInterruptGuard {
    restore_enabled: bool,
}

impl FrameCacheInterruptGuard {
    #[inline]
    fn new() -> Self {
        let restore_enabled = crate::trap::interrupts::is_interrupt_enabled();
        if restore_enabled {
            crate::trap::interrupts::disable_interrupt();
        }
        Self { restore_enabled }
    }
}

impl Drop for FrameCacheInterruptGuard {
    #[inline]
    fn drop(&mut self) {
        if self.restore_enabled {
            crate::trap::interrupts::enable_interrupt();
        }
    }
}

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
const FRAME_REF_CHUNK_FRAMES: usize = 1 << 18;

struct FrameRefChunk {
    refs: Vec<AtomicU32>,
    #[cfg(feature = "buildstorm-diagnostics")]
    owners: Vec<AtomicU8>,
}

struct FrameRefRegion {
    start_ppn: usize,
    end_ppn: usize,
    chunks: Vec<FrameRefChunk>,
}

impl FrameRefRegion {
    fn new(start_ppn: usize, end_ppn: usize) -> Self {
        let pages = end_ppn - start_ppn;
        let mut chunks = Vec::with_capacity(pages.div_ceil(FRAME_REF_CHUNK_FRAMES));
        let mut remaining = pages;
        while remaining != 0 {
            let chunk_pages = remaining.min(FRAME_REF_CHUNK_FRAMES);
            chunks.push(FrameRefChunk {
                refs: (0..chunk_pages).map(|_| AtomicU32::new(0)).collect(),
                #[cfg(feature = "buildstorm-diagnostics")]
                owners: (0..chunk_pages)
                    .map(|_| AtomicU8::new(DIAG_OWNER_FREE))
                    .collect(),
            });
            remaining -= chunk_pages;
        }
        Self {
            start_ppn,
            end_ppn,
            chunks,
        }
    }

    fn slot(&self, ppn: usize) -> Option<(&FrameRefChunk, usize)> {
        if ppn < self.start_ppn || ppn >= self.end_ppn {
            return None;
        }
        let offset = ppn - self.start_ppn;
        let chunk = self.chunks.get(offset / FRAME_REF_CHUNK_FRAMES)?;
        Some((chunk, offset % FRAME_REF_CHUNK_FRAMES))
    }

    fn counter(&self, ppn: usize) -> Option<&AtomicU32> {
        let (chunk, slot) = self.slot(ppn)?;
        chunk.refs.get(slot)
    }

    #[cfg(feature = "buildstorm-diagnostics")]
    fn owner(&self, ppn: usize) -> Option<&AtomicU8> {
        let (chunk, slot) = self.slot(ppn)?;
        chunk.owners.get(slot)
    }
}

pub struct FrameTracker {
    ppn: PhysPageNum,
}

impl FrameTracker {
    fn new(ppn: PhysPageNum) -> Self {
        let old = frame_ref_counter(ppn.0).fetch_add(1, Ordering::Relaxed);
        assert_eq!(old, 0, "allocated frame already has owners");
        #[cfg(feature = "buildstorm-diagnostics")]
        diagnostic_transition_owner(ppn.0, DIAG_OWNER_FREE, DIAG_OWNER_TRACKED);
        Self { ppn }
    }

    pub fn ppn(&self) -> PhysPageNum {
        self.ppn
    }

    pub fn ref_count(&self) -> usize {
        frame_ref_counter(self.ppn.0).load(Ordering::Acquire) as usize
    }

    pub fn into_raw_ppn(self) -> PhysPageNum {
        let ppn = self.ppn;
        let old = frame_ref_counter(ppn.0).fetch_sub(1, Ordering::Release);
        assert_eq!(old, 1, "shared frame cannot become a raw owner");
        #[cfg(feature = "buildstorm-diagnostics")]
        diagnostic_transition_owner(ppn.0, DIAG_OWNER_TRACKED, DIAG_OWNER_PAGE_TABLE);
        core::mem::forget(self);
        ppn
    }
}

impl Clone for FrameTracker {
    fn clone(&self) -> Self {
        let old = frame_ref_counter(self.ppn.0).fetch_add(1, Ordering::Relaxed);
        assert!(old != 0 && old != u32::MAX, "invalid frame owner count");
        Self { ppn: self.ppn }
    }
}

impl Drop for FrameTracker {
    fn drop(&mut self) {
        let old = frame_ref_counter(self.ppn.0).fetch_sub(1, Ordering::Release);
        assert!(old != 0, "frame owner count underflow");
        if old == 1 {
            core::sync::atomic::fence(Ordering::Acquire);
            #[cfg(feature = "buildstorm-diagnostics")]
            diagnostic_transition_owner(self.ppn.0, DIAG_OWNER_TRACKED, DIAG_OWNER_FREE);
            dealloc_frame_inner(self.ppn);
        }
    }
}

lazy_static! {
    static ref FRAME_ALLOCATOR: Mutex<FrameAllocator> = Mutex::new(FrameAllocator::new());
    static ref MEM_REGIONS: Mutex<Vec<(usize, usize)>> = Mutex::new(Vec::new());
}
static FRAME_REF_REGIONS: Once<Vec<FrameRefRegion>> = Once::new();

#[cfg(feature = "buildstorm-diagnostics")]
const DIAG_OWNER_FREE: u8 = 0;
#[cfg(feature = "buildstorm-diagnostics")]
const DIAG_OWNER_TRACKED: u8 = 1;
#[cfg(feature = "buildstorm-diagnostics")]
const DIAG_OWNER_PAGE_TABLE: u8 = 2;
#[cfg(feature = "buildstorm-diagnostics")]
const DIAG_OWNER_CONTIGUOUS: u8 = 3;

#[cfg(feature = "buildstorm-diagnostics")]
static DIAG_TRACKED_FRAMES: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "buildstorm-diagnostics")]
static DIAG_PAGE_TABLE_FRAMES: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "buildstorm-diagnostics")]
static DIAG_CONTIGUOUS_FRAMES: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "buildstorm-diagnostics")]
static DIAG_OWNER_TRANSITIONS: AtomicUsize = AtomicUsize::new(0);

static TOTAL_MANAGED_FRAMES: AtomicUsize = AtomicUsize::new(0);
static FREE_MANAGED_FRAMES: AtomicUsize = AtomicUsize::new(0);

#[cfg(feature = "buildstorm-diagnostics")]
static FRAME_CACHE_HITS: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "buildstorm-diagnostics")]
static FRAME_CACHE_MISSES: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "buildstorm-diagnostics")]
static FRAME_CACHE_REFILL_PAGES: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "buildstorm-diagnostics")]
static FRAME_CACHE_FREES: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "buildstorm-diagnostics")]
static FRAME_CACHE_DRAINED_PAGES: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "buildstorm-diagnostics")]
static FRAME_CENTRAL_ALLOC_RUNS: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "buildstorm-diagnostics")]
static FRAME_CENTRAL_ALLOC_PAGES: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "buildstorm-diagnostics")]
static FRAME_CENTRAL_FREE_BATCHES: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "buildstorm-diagnostics")]
static FRAME_CENTRAL_FREE_PAGES: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "buildstorm-diagnostics")]
static FRAME_ZERO_CALLS: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "buildstorm-diagnostics")]
static FRAME_ZERO_SAMPLE_COUNT: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "buildstorm-diagnostics")]
static FRAME_ZERO_SAMPLE_TOTAL_US: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "buildstorm-diagnostics")]
static FRAME_ZERO_SAMPLE_MAX_US: AtomicUsize = AtomicUsize::new(0);

#[cfg(feature = "buildstorm-diagnostics")]
const FRAME_ZERO_SAMPLE_SHIFT: usize = 10;

#[cfg(feature = "buildstorm-diagnostics")]
#[derive(Clone, Copy)]
#[repr(usize)]
pub(crate) enum FrameAllocationClass {
    PageTable,
    Anonymous,
    SharedMemory,
    PrivateCopy,
    CleanFile,
    FileWrite,
    EagerMapping,
    BackingFallback,
}

#[cfg(feature = "buildstorm-diagnostics")]
const FRAME_ALLOCATION_CLASS_NAMES: [&str; 8] = [
    "page_table",
    "anonymous",
    "shared_memory",
    "private_copy",
    "clean_file",
    "file_write",
    "eager_mapping",
    "backing_fallback",
];

#[cfg(feature = "buildstorm-diagnostics")]
static FRAME_ALLOCATION_CLASSES: [AtomicUsize; FRAME_ALLOCATION_CLASS_NAMES.len()] =
    [const { AtomicUsize::new(0) }; FRAME_ALLOCATION_CLASS_NAMES.len()];

#[cfg(feature = "buildstorm-diagnostics")]
#[inline]
pub(crate) fn diagnostic_note_allocation(class: FrameAllocationClass) {
    FRAME_ALLOCATION_CLASSES[class as usize].fetch_add(1, Ordering::Relaxed);
}

/// 初始化帧分配器
pub fn init_frame_allocator() {
    // 将在启动时由main.rs添加内存区域
}

/// 添加可用物理内存区域
pub fn add_frames_range(start: usize, end: usize) {
    let start_ppn = ((start + PAGE_SIZE - 1) / PAGE_SIZE).max(1);
    let end_ppn = end / PAGE_SIZE;
    if start_ppn < end_ppn {
        #[cfg(feature = "buildstorm-diagnostics")]
        crate::buildstorm_diagnostics::lock(
            crate::buildstorm_diagnostics::LockClass::FrameAllocator,
            &FRAME_ALLOCATOR,
        )
        .add_frame(start_ppn, end_ppn);
        #[cfg(not(feature = "buildstorm-diagnostics"))]
        FRAME_ALLOCATOR.lock().add_frame(start_ppn, end_ppn);
        MEM_REGIONS.lock().push((start_ppn, end_ppn));
        let pages = end_ppn - start_ppn;
        TOTAL_MANAGED_FRAMES.fetch_add(pages, Ordering::Relaxed);
        FREE_MANAGED_FRAMES.fetch_add(pages, Ordering::Relaxed);
    }
}

pub fn finalize_frame_refcounts() {
    FRAME_REF_REGIONS.call_once(|| {
        MEM_REGIONS
            .lock()
            .iter()
            .map(|&(start, end)| FrameRefRegion::new(start, end))
            .collect()
    });
}

#[cfg(feature = "buildstorm-diagnostics")]
fn frame_owner_counter(ppn: usize) -> &'static AtomicU8 {
    FRAME_REF_REGIONS
        .get()
        .expect("frame ownership table not initialized")
        .iter()
        .find_map(|region| region.owner(ppn))
        .expect("frame outside ownership table")
}

#[cfg(feature = "buildstorm-diagnostics")]
fn diagnostic_owner_total(owner: u8) -> &'static AtomicUsize {
    match owner {
        DIAG_OWNER_TRACKED => &DIAG_TRACKED_FRAMES,
        DIAG_OWNER_PAGE_TABLE => &DIAG_PAGE_TABLE_FRAMES,
        DIAG_OWNER_CONTIGUOUS => &DIAG_CONTIGUOUS_FRAMES,
        _ => unreachable!("free frames have no active-owner total"),
    }
}

#[cfg(feature = "buildstorm-diagnostics")]
fn diagnostic_transition_owner(ppn: usize, expected: u8, next: u8) {
    let state = frame_owner_counter(ppn);
    if let Err(actual) = state.compare_exchange(expected, next, Ordering::AcqRel, Ordering::Acquire)
    {
        panic!(
            "frame ownership violation ppn={:#x} expected={} actual={} next={} refs={}",
            ppn,
            expected,
            actual,
            next,
            frame_ref_counter(ppn).load(Ordering::Acquire)
        );
    }
    if expected != DIAG_OWNER_FREE {
        let old = diagnostic_owner_total(expected).fetch_sub(1, Ordering::Relaxed);
        assert!(old != 0, "frame ownership total underflow");
    }
    if next != DIAG_OWNER_FREE {
        diagnostic_owner_total(next).fetch_add(1, Ordering::Relaxed);
    }
    DIAG_OWNER_TRANSITIONS.fetch_add(1, Ordering::Relaxed);
}

#[cfg(feature = "buildstorm-diagnostics")]
pub(crate) fn diagnostic_ownership_snapshot() -> (usize, usize, usize, usize) {
    (
        DIAG_TRACKED_FRAMES.load(Ordering::Relaxed),
        DIAG_PAGE_TABLE_FRAMES.load(Ordering::Relaxed),
        DIAG_CONTIGUOUS_FRAMES.load(Ordering::Relaxed),
        DIAG_OWNER_TRANSITIONS.load(Ordering::Relaxed),
    )
}

fn frame_ref_counter(ppn: usize) -> &'static AtomicU32 {
    FRAME_REF_REGIONS
        .get()
        .expect("frame reference table not initialized")
        .iter()
        .find_map(|region| region.counter(ppn))
        .expect("frame outside reference table")
}

fn is_managed_range(start_ppn: usize, pages: usize) -> bool {
    if pages == 0 {
        return true;
    }
    let Some(end_ppn) = start_ppn.checked_add(pages) else {
        return false;
    };
    if let Some(regions) = FRAME_REF_REGIONS.get() {
        return regions
            .iter()
            .any(|region| start_ppn >= region.start_ppn && end_ppn <= region.end_ppn);
    }
    MEM_REGIONS
        .lock()
        .iter()
        .any(|(start, end)| start_ppn >= *start && end_ppn <= *end)
}

#[inline]
fn frame_cache_cpu() -> usize {
    crate::platform::current_cpu_index().min(crate::config::MAX_CPUS - 1)
}

fn central_alloc_run(max_pages: usize) -> Option<(usize, usize)> {
    if max_pages == 0 {
        return None;
    }
    let mut pages = 1usize << (usize::BITS as usize - 1 - max_pages.leading_zeros() as usize);
    #[cfg(feature = "buildstorm-diagnostics")]
    let mut allocator = crate::buildstorm_diagnostics::lock(
        crate::buildstorm_diagnostics::LockClass::FrameAllocator,
        &FRAME_ALLOCATOR,
    );
    #[cfg(not(feature = "buildstorm-diagnostics"))]
    let mut allocator = FRAME_ALLOCATOR.lock();

    while pages != 0 {
        if let Some(ppn) = allocator.alloc(pages) {
            if is_managed_range(ppn, pages) {
                #[cfg(feature = "buildstorm-diagnostics")]
                {
                    FRAME_CENTRAL_ALLOC_RUNS.fetch_add(1, Ordering::Relaxed);
                    FRAME_CENTRAL_ALLOC_PAGES.fetch_add(pages, Ordering::Relaxed);
                }
                return Some((ppn, pages));
            }
            log::warn!(
                "[frame] discard unmanaged allocation ppn={:#x}, pages={}",
                ppn,
                pages
            );
            continue;
        }
        pages >>= 1;
    }
    None
}

fn central_dealloc_pages(pages: &[usize]) {
    if pages.is_empty() {
        return;
    }
    #[cfg(feature = "buildstorm-diagnostics")]
    let mut allocator = crate::buildstorm_diagnostics::lock(
        crate::buildstorm_diagnostics::LockClass::FrameAllocator,
        &FRAME_ALLOCATOR,
    );
    #[cfg(not(feature = "buildstorm-diagnostics"))]
    let mut allocator = FRAME_ALLOCATOR.lock();
    for &ppn in pages {
        allocator.dealloc(ppn, 1);
    }
    #[cfg(feature = "buildstorm-diagnostics")]
    {
        FRAME_CENTRAL_FREE_BATCHES.fetch_add(1, Ordering::Relaxed);
        FRAME_CENTRAL_FREE_PAGES.fetch_add(pages.len(), Ordering::Relaxed);
    }
}

fn drain_all_frame_caches() {
    let _interrupt_guard = FrameCacheInterruptGuard::new();
    #[cfg(feature = "buildstorm-diagnostics")]
    let mut drained = 0usize;
    for cache in &FRAME_CACHES {
        let mut pages = [0usize; FRAME_CACHE_CAPACITY];
        let count = cache.lock().drain(&mut pages);
        if count != 0 {
            central_dealloc_pages(&pages[..count]);
            #[cfg(feature = "buildstorm-diagnostics")]
            {
                drained += count;
            }
        }
    }
    #[cfg(feature = "buildstorm-diagnostics")]
    FRAME_CACHE_DRAINED_PAGES.fetch_add(drained, Ordering::Relaxed);
}

fn alloc_cached_frame() -> Option<usize> {
    let _interrupt_guard = FrameCacheInterruptGuard::new();
    let cpu = frame_cache_cpu();
    if let Some(ppn) = FRAME_CACHES[cpu].lock().pop() {
        #[cfg(feature = "buildstorm-diagnostics")]
        FRAME_CACHE_HITS.fetch_add(1, Ordering::Relaxed);
        return Some(ppn);
    }
    #[cfg(feature = "buildstorm-diagnostics")]
    FRAME_CACHE_MISSES.fetch_add(1, Ordering::Relaxed);

    let mut run = central_alloc_run(FRAME_CACHE_REFILL);
    if run.is_none() {
        drain_all_frame_caches();
        run = central_alloc_run(FRAME_CACHE_REFILL);
    }
    let (start_ppn, pages) = run?;
    #[cfg(feature = "buildstorm-diagnostics")]
    FRAME_CACHE_REFILL_PAGES.fetch_add(pages, Ordering::Relaxed);

    let mut overflow = [0usize; FRAME_CACHE_REFILL];
    let mut overflow_count = 0;
    {
        let mut cache = FRAME_CACHES[cpu].lock();
        for ppn in start_ppn + 1..start_ppn + pages {
            if !cache.push(ppn) {
                overflow[overflow_count] = ppn;
                overflow_count += 1;
            }
        }
    }
    central_dealloc_pages(&overflow[..overflow_count]);
    Some(start_ppn)
}

fn cache_deallocated_frame(ppn: usize) {
    let _interrupt_guard = FrameCacheInterruptGuard::new();
    let cpu = frame_cache_cpu();
    let mut flushed = [0usize; FRAME_CACHE_FLUSH];
    let flushed_count = {
        let mut cache = FRAME_CACHES[cpu].lock();
        if cache.push(ppn) {
            0
        } else {
            let count = cache.drain(&mut flushed);
            let inserted = cache.push(ppn);
            debug_assert!(inserted);
            count
        }
    };
    #[cfg(feature = "buildstorm-diagnostics")]
    FRAME_CACHE_FREES.fetch_add(1, Ordering::Relaxed);
    central_dealloc_pages(&flushed[..flushed_count]);
}

/// 分配一个物理页帧
pub fn alloc_frame() -> Option<FrameTracker> {
    let ppn = alloc_cached_frame()?;
    FREE_MANAGED_FRAMES.fetch_sub(1, Ordering::Relaxed);
    let tracker = FrameTracker::new(PhysPageNum(ppn));
    #[cfg(feature = "buildstorm-diagnostics")]
    let zero_sequence = FRAME_ZERO_CALLS.fetch_add(1, Ordering::Relaxed);
    #[cfg(feature = "buildstorm-diagnostics")]
    let zero_started = if zero_sequence & ((1 << FRAME_ZERO_SAMPLE_SHIFT) - 1) == 0 {
        Some(crate::timer::get_time_us())
    } else {
        None
    };
    unsafe {
        let ptr = PhysPageNum(ppn).addr() as *mut u8;
        crate::platform::zero_phys_range(ptr, PAGE_SIZE);
    }
    #[cfg(feature = "buildstorm-diagnostics")]
    if let Some(started) = zero_started {
        let elapsed = crate::timer::get_time_us().saturating_sub(started);
        FRAME_ZERO_SAMPLE_COUNT.fetch_add(1, Ordering::Relaxed);
        FRAME_ZERO_SAMPLE_TOTAL_US.fetch_add(elapsed, Ordering::Relaxed);
        FRAME_ZERO_SAMPLE_MAX_US.fetch_max(elapsed, Ordering::Relaxed);
    }
    Some(tracker)
}

/// 分配连续的 `pages` 个物理页（用于 virtio DMA）。
/// 返回起始页帧号。
pub fn alloc_contiguous_frames(pages: usize) -> Option<usize> {
    if pages == 0 {
        return Some(0);
    }
    let mut drained_caches = false;
    loop {
        #[cfg(feature = "buildstorm-diagnostics")]
        let allocation = crate::buildstorm_diagnostics::lock(
            crate::buildstorm_diagnostics::LockClass::FrameAllocator,
            &FRAME_ALLOCATOR,
        )
        .alloc(pages);
        #[cfg(not(feature = "buildstorm-diagnostics"))]
        let allocation = FRAME_ALLOCATOR.lock().alloc(pages);
        let Some(ppn) = allocation else {
            if drained_caches {
                return None;
            }
            drain_all_frame_caches();
            drained_caches = true;
            continue;
        };
        if is_managed_range(ppn, pages) {
            FREE_MANAGED_FRAMES.fetch_sub(pages, Ordering::Relaxed);
            #[cfg(feature = "buildstorm-diagnostics")]
            for page in ppn..ppn + pages {
                diagnostic_transition_owner(page, DIAG_OWNER_FREE, DIAG_OWNER_CONTIGUOUS);
            }
            return Some(ppn);
        }
        log::warn!(
            "[frame] discard unmanaged contiguous allocation ppn={:#x}, pages={}",
            ppn,
            pages
        );
    }
}

/// 释放连续的 `pages` 个物理页。
pub fn dealloc_contiguous_frames(start_ppn: usize, pages: usize) {
    if pages != 0 {
        if !is_managed_range(start_ppn, pages) {
            log::warn!(
                "[frame] ignore unmanaged contiguous free ppn={:#x}, pages={}",
                start_ppn,
                pages
            );
            return;
        }
        #[cfg(feature = "buildstorm-diagnostics")]
        for page in start_ppn..start_ppn + pages {
            diagnostic_transition_owner(page, DIAG_OWNER_CONTIGUOUS, DIAG_OWNER_FREE);
        }
        #[cfg(feature = "buildstorm-diagnostics")]
        crate::buildstorm_diagnostics::lock(
            crate::buildstorm_diagnostics::LockClass::FrameAllocator,
            &FRAME_ALLOCATOR,
        )
        .dealloc(start_ppn, pages);
        #[cfg(not(feature = "buildstorm-diagnostics"))]
        FRAME_ALLOCATOR.lock().dealloc(start_ppn, pages);
        FREE_MANAGED_FRAMES.fetch_add(pages, Ordering::Relaxed);
    }
}

/// 释放一个物理页帧
pub fn dealloc_frame(ppn: PhysPageNum) {
    assert_eq!(
        frame_ref_counter(ppn.0).load(Ordering::Acquire),
        0,
        "deallocating an owned frame"
    );
    #[cfg(feature = "buildstorm-diagnostics")]
    diagnostic_transition_owner(ppn.0, DIAG_OWNER_PAGE_TABLE, DIAG_OWNER_FREE);
    dealloc_frame_inner(ppn);
}

fn dealloc_frame_inner(ppn: PhysPageNum) {
    if !is_managed_range(ppn.0, 1) {
        log::warn!(
            "[frame] ignore unmanaged free ppn={:#x} paddr={:#x}",
            ppn.0,
            ppn.addr()
        );
        return;
    }
    FREE_MANAGED_FRAMES.fetch_add(1, Ordering::Relaxed);
    cache_deallocated_frame(ppn.0);
}

#[cfg(feature = "buildstorm-diagnostics")]
pub(crate) fn report_cache_diagnostics() {
    let _interrupt_guard = FrameCacheInterruptGuard::new();
    let cached: usize = FRAME_CACHES.iter().map(|cache| cache.lock().len).sum();
    crate::println!(
        "BUILDSTORM_DIAG frame_cache cached={} hits={} misses={} refill_pages={} cached_frees={} drained_pages={} central_alloc_runs={} central_alloc_pages={} central_free_batches={} central_free_pages={} zero_block_bytes={}",
        cached,
        FRAME_CACHE_HITS.load(Ordering::Relaxed),
        FRAME_CACHE_MISSES.load(Ordering::Relaxed),
        FRAME_CACHE_REFILL_PAGES.load(Ordering::Relaxed),
        FRAME_CACHE_FREES.load(Ordering::Relaxed),
        FRAME_CACHE_DRAINED_PAGES.load(Ordering::Relaxed),
        FRAME_CENTRAL_ALLOC_RUNS.load(Ordering::Relaxed),
        FRAME_CENTRAL_ALLOC_PAGES.load(Ordering::Relaxed),
        FRAME_CENTRAL_FREE_BATCHES.load(Ordering::Relaxed),
        FRAME_CENTRAL_FREE_PAGES.load(Ordering::Relaxed),
        crate::platform::zero_block_bytes(),
    );
    crate::println!(
        "BUILDSTORM_DIAG frame_zero calls={} sample_count={} sample_total_us={} sample_max_us={} sample_shift={}",
        FRAME_ZERO_CALLS.load(Ordering::Relaxed),
        FRAME_ZERO_SAMPLE_COUNT.load(Ordering::Relaxed),
        FRAME_ZERO_SAMPLE_TOTAL_US.load(Ordering::Relaxed),
        FRAME_ZERO_SAMPLE_MAX_US.load(Ordering::Relaxed),
        FRAME_ZERO_SAMPLE_SHIFT,
    );
    for (name, count) in FRAME_ALLOCATION_CLASS_NAMES
        .iter()
        .zip(FRAME_ALLOCATION_CLASSES.iter())
    {
        crate::println!(
            "BUILDSTORM_DIAG frame_allocation class={} count={}",
            name,
            count.load(Ordering::Relaxed),
        );
    }
}

/// 获取剩余可用页帧数
pub fn remaining_frames() -> usize {
    FREE_MANAGED_FRAMES.load(Ordering::Relaxed)
}

pub fn total_frames() -> usize {
    TOTAL_MANAGED_FRAMES.load(Ordering::Relaxed)
}
