use alloc::vec::Vec;
use buddy_system_allocator::FrameAllocator;
#[cfg(feature = "buildstorm-diagnostics")]
use core::sync::atomic::AtomicU8;
use core::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use lazy_static::lazy_static;
use spin::{Mutex, Once};

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
    MEM_REGIONS
        .lock()
        .iter()
        .any(|(start, end)| start_ppn >= *start && end_ppn <= *end)
}

/// 分配一个物理页帧
pub fn alloc_frame() -> Option<FrameTracker> {
    loop {
        #[cfg(feature = "buildstorm-diagnostics")]
        let ppn = crate::buildstorm_diagnostics::lock(
            crate::buildstorm_diagnostics::LockClass::FrameAllocator,
            &FRAME_ALLOCATOR,
        )
        .alloc(1)?;
        #[cfg(not(feature = "buildstorm-diagnostics"))]
        let ppn = FRAME_ALLOCATOR.lock().alloc(1)?;
        if !is_managed_range(ppn, 1) {
            log::warn!(
                "[frame] discard unmanaged allocation ppn={:#x} paddr={:#x}",
                ppn,
                ppn * PAGE_SIZE
            );
            continue;
        }
        FREE_MANAGED_FRAMES.fetch_sub(1, Ordering::Relaxed);
        let tracker = FrameTracker::new(PhysPageNum(ppn));
        unsafe {
            let ptr = PhysPageNum(ppn).addr() as *mut u8;
            core::ptr::write_bytes(ptr, 0, PAGE_SIZE);
        }
        return Some(tracker);
    }
}

/// 分配连续的 `pages` 个物理页（用于 virtio DMA）。
/// 返回起始页帧号。
pub fn alloc_contiguous_frames(pages: usize) -> Option<usize> {
    if pages == 0 {
        return Some(0);
    }
    loop {
        #[cfg(feature = "buildstorm-diagnostics")]
        let ppn = crate::buildstorm_diagnostics::lock(
            crate::buildstorm_diagnostics::LockClass::FrameAllocator,
            &FRAME_ALLOCATOR,
        )
        .alloc(pages)?;
        #[cfg(not(feature = "buildstorm-diagnostics"))]
        let ppn = FRAME_ALLOCATOR.lock().alloc(pages)?;
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
    #[cfg(feature = "buildstorm-diagnostics")]
    crate::buildstorm_diagnostics::lock(
        crate::buildstorm_diagnostics::LockClass::FrameAllocator,
        &FRAME_ALLOCATOR,
    )
    .dealloc(ppn.0, 1);
    #[cfg(not(feature = "buildstorm-diagnostics"))]
    FRAME_ALLOCATOR.lock().dealloc(ppn.0, 1);
    FREE_MANAGED_FRAMES.fetch_add(1, Ordering::Relaxed);
}

/// 获取剩余可用页帧数
pub fn remaining_frames() -> usize {
    FREE_MANAGED_FRAMES.load(Ordering::Relaxed)
}

pub fn total_frames() -> usize {
    TOTAL_MANAGED_FRAMES.load(Ordering::Relaxed)
}
