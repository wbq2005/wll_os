use super::frame_allocator::FrameTracker;
use crate::mm::page_table::PTEFlags;
use crate::{config::PAGE_SIZE, fs::fd::FileDescriptor};
use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use core::mem;
use core::ops::Deref;
use polyhal::VirtAddr;

/// Page-level ownership state.  VMA flags describe policy; this state describes
/// the ownership and sharing of an actually resident leaf.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PageState {
    Private,
    Cow,
    Shared,
    FileClean,
    FileDirty,
}

/// One resident user page.  The frame is owned only by its ResidentSet entry.
#[derive(Clone)]
pub struct ResidentPage {
    frame: FrameTracker,
    state: PageState,
}

impl ResidentPage {
    pub fn new(frame: FrameTracker) -> Self {
        Self {
            frame,
            state: PageState::Private,
        }
    }

    pub fn with_state(frame: FrameTracker, state: PageState) -> Self {
        Self { frame, state }
    }

    pub fn state(&self) -> PageState {
        self.state
    }

    pub fn set_state(&mut self, state: PageState) {
        self.state = state;
    }

    pub fn frame(&self) -> &FrameTracker {
        &self.frame
    }

    pub fn into_frame(self) -> FrameTracker {
        self.frame
    }
}

impl Deref for ResidentPage {
    type Target = FrameTracker;

    fn deref(&self) -> &Self::Target {
        &self.frame
    }
}

#[derive(Clone, Default)]
struct ResidentRun {
    pages: Vec<ResidentPage>,
}

/// The sole owner of resident data frames for one logical VMA.
///
/// Runs are keyed by VMA-relative VPN and contain only contiguous resident
/// pages.  A demand fault can therefore insert a hole without changing VMA
/// topology or scanning unrelated areas.
#[derive(Clone, Default)]
pub struct ResidentSet {
    runs: BTreeMap<usize, ResidentRun>,
    count: usize,
}

impl ResidentSet {
    pub const fn new() -> Self {
        Self {
            runs: BTreeMap::new(),
            count: 0,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    pub fn len(&self) -> usize {
        self.count
    }

    pub fn capacity(&self) -> usize {
        self.runs.values().map(|run| run.pages.capacity()).sum()
    }

    /// Return the page at a VMA-relative VPN.
    pub fn lookup(&self, vpn: usize) -> Option<&ResidentPage> {
        let (&start, run) = self.runs.range(..=vpn).next_back()?;
        let offset = vpn.checked_sub(start)?;
        run.pages.get(offset)
    }

    pub fn lookup_mut(&mut self, vpn: usize) -> Option<&mut ResidentPage> {
        let (&start, run) = self.runs.range_mut(..=vpn).next_back()?;
        let offset = vpn.checked_sub(start)?;
        run.pages.get_mut(offset)
    }

    pub fn iter(&self) -> impl Iterator<Item = &ResidentPage> {
        self.runs.values().flat_map(|run| run.pages.iter())
    }

    pub(crate) fn iter_mut(&mut self) -> impl Iterator<Item = &mut ResidentPage> {
        self.runs.values_mut().flat_map(|run| run.pages.iter_mut())
    }

    /// Insert one VMA-relative page.  Existing entries are never overwritten.
    pub fn insert(&mut self, vpn: usize, page: FrameTracker) -> Result<(), FrameTracker> {
        if self.lookup(vpn).is_some() {
            return Err(page);
        }
        self.insert_page(ResidentPage::new(page), vpn)
            .unwrap_or_else(|_| unreachable!("resident run insertion after collision check"));
        Ok(())
    }

    pub(crate) fn insert_page_with_state(
        &mut self,
        vpn: usize,
        page: ResidentPage,
    ) -> Result<(), FrameTracker> {
        match self.insert_page(page, vpn) {
            Ok(()) => Ok(()),
            Err(page) => Err(page.into_frame()),
        }
    }

    /// Insert a contiguous run.  On overlap, all input frames remain owned by
    /// the caller and no resident metadata changes.
    pub fn insert_run(
        &mut self,
        start_vpn: usize,
        pages: Vec<FrameTracker>,
    ) -> Result<(), Vec<FrameTracker>> {
        if pages
            .iter()
            .enumerate()
            .any(|(offset, _)| self.lookup(start_vpn.saturating_add(offset)).is_some())
        {
            return Err(pages);
        }
        let resident_pages = pages.into_iter().map(ResidentPage::new).collect();
        self.insert_run_pages(start_vpn, resident_pages)
            .unwrap_or_else(|_| unreachable!("resident run insertion after collision check"));
        Ok(())
    }

    pub(crate) fn insert_run_with_state(
        &mut self,
        start_vpn: usize,
        pages: Vec<FrameTracker>,
        state: PageState,
    ) -> Result<(), Vec<FrameTracker>> {
        if pages
            .iter()
            .enumerate()
            .any(|(offset, _)| self.lookup(start_vpn.saturating_add(offset)).is_some())
        {
            return Err(pages);
        }
        let resident_pages = pages
            .into_iter()
            .map(|page| ResidentPage::with_state(page, state))
            .collect();
        match self.insert_run_pages(start_vpn, resident_pages) {
            Ok(()) => Ok(()),
            Err(pages) => Err(pages.into_iter().map(ResidentPage::into_frame).collect()),
        }
    }

    /// Extract [start_vpn, end_vpn), shifting returned keys to zero.
    pub fn extract_range(&mut self, start_vpn: usize, end_vpn: usize) -> Option<Self> {
        if start_vpn > end_vpn {
            return None;
        }
        let mut extracted = Self::new();
        let old_runs = mem::take(&mut self.runs);
        self.count = 0;
        for (run_start, run) in old_runs {
            let run_end = run_start.saturating_add(run.pages.len());
            let overlap_start = start_vpn.max(run_start);
            let overlap_end = end_vpn.min(run_end);
            if overlap_start >= overlap_end {
                self.insert_run_pages(run_start, run.pages)
                    .unwrap_or_else(|_| unreachable!("resident untouched run cannot overlap"));
                continue;
            }
            let mut pages = run.pages;
            let left_len = overlap_start - run_start;
            if left_len != 0 {
                let left_pages: Vec<_> = pages.drain(..left_len).collect();
                self.insert_run_pages(run_start, left_pages)
                    .unwrap_or_else(|_| unreachable!("resident left split cannot overlap"));
            }
            let middle_len = overlap_end - overlap_start;
            let middle: Vec<_> = pages.drain(..middle_len).collect();
            extracted
                .insert_run_pages(overlap_start - start_vpn, middle)
                .unwrap_or_else(|_| unreachable!("resident extracted split cannot overlap"));
            if !pages.is_empty() {
                self.insert_run_pages(overlap_end, pages)
                    .unwrap_or_else(|_| unreachable!("resident right split cannot overlap"));
            }
        }
        Some(extracted)
    }

    pub fn split_off(&mut self, vpn: usize) -> Option<Self> {
        if vpn == 0 {
            return Some(self.drain_all());
        }
        let end = self
            .runs
            .iter()
            .next_back()
            .map(|(&start, run)| start.saturating_add(run.pages.len()))
            .unwrap_or(vpn)
            .max(vpn);
        self.extract_range(vpn, end)
    }

    pub fn merge_from(&mut self, mut other: Self) {
        let offset = self
            .runs
            .iter()
            .next_back()
            .map(|(&start, run)| start.saturating_add(run.pages.len()))
            .unwrap_or(0);
        self.merge_from_at(offset, &mut other);
    }

    pub fn merge_from_at(&mut self, offset: usize, other: &mut Self) {
        let runs = mem::take(&mut other.runs);
        other.count = 0;
        for (start, run) in runs {
            self.insert_run_pages(start.saturating_add(offset), run.pages)
                .unwrap_or_else(|_| {
                    unreachable!("resident merge cannot overlap after VMA topology validation")
                });
        }
    }

    pub fn iter_range(
        &self,
        start_vpn: usize,
        end_vpn: usize,
    ) -> impl Iterator<Item = (usize, &ResidentPage)> {
        self.runs
            .range(..end_vpn)
            .flat_map(move |(&run_start, run)| {
                let first = start_vpn.saturating_sub(run_start);
                let last = end_vpn.saturating_sub(run_start).min(run.pages.len());
                run.pages
                    .iter()
                    .enumerate()
                    .skip(first.min(run.pages.len()))
                    .take(last.saturating_sub(first.min(run.pages.len())))
                    .map(move |(offset, page)| (run_start + offset, page))
            })
    }

    pub fn drain_all(&mut self) -> Self {
        Self {
            runs: mem::take(&mut self.runs),
            count: mem::replace(&mut self.count, 0),
        }
    }

    pub fn into_frames(self) -> Vec<FrameTracker> {
        self.runs
            .into_values()
            .flat_map(|run| run.pages.into_iter().map(ResidentPage::into_frame))
            .collect()
    }

    pub fn replace_all(&mut self, pages: Vec<FrameTracker>) -> Self {
        let old = self.drain_all();
        let _ = self.insert_run(0, pages);
        old
    }

    fn insert_page(&mut self, page: ResidentPage, vpn: usize) -> Result<(), ResidentPage> {
        match self.insert_run_pages(vpn, alloc::vec![page]) {
            Ok(()) => Ok(()),
            Err(mut pages) => Err(pages.pop().unwrap()),
        }
    }

    fn insert_run_pages(
        &mut self,
        start_vpn: usize,
        pages: Vec<ResidentPage>,
    ) -> Result<(), Vec<ResidentPage>> {
        if pages.is_empty()
            || pages
                .iter()
                .enumerate()
                .any(|(offset, _)| self.lookup(start_vpn.saturating_add(offset)).is_some())
        {
            return Err(pages);
        }
        self.count = self.count.saturating_add(pages.len());
        self.runs.insert(start_vpn, ResidentRun { pages });
        self.coalesce_adjacent(start_vpn);
        Ok(())
    }

    fn coalesce_adjacent(&mut self, start_vpn: usize) {
        let Some(mut run) = self.runs.remove(&start_vpn) else {
            return;
        };
        let mut key = start_vpn;
        if let Some((&left_start, left)) = self.runs.range(..start_vpn).next_back() {
            if left_start.saturating_add(left.pages.len()) == start_vpn {
                let mut left = self.runs.remove(&left_start).unwrap();
                left.pages.append(&mut run.pages);
                run = left;
                key = left_start;
            }
        }
        let right_start = key.saturating_add(run.pages.len());
        if let Some(right) = self.runs.remove(&right_start) {
            run.pages.extend(right.pages);
        }
        self.runs.insert(key, run);
    }
}

#[derive(Clone)]
pub enum MapAreaBacking {
    Anonymous,
    File {
        file: FileDescriptor,
        offset: usize,
        shared: bool,
    },
    SharedMemory {
        shmid: usize,
        base: usize,
        offset: usize,
    },
}

impl MapAreaBacking {
    fn split_right(&self, delta: usize) -> Self {
        match self {
            Self::Anonymous => Self::Anonymous,
            Self::File {
                file,
                offset,
                shared,
            } => Self::File {
                file: file.clone(),
                offset: offset.saturating_add(delta),
                shared: *shared,
            },
            Self::SharedMemory {
                shmid,
                base,
                offset,
            } => Self::SharedMemory {
                shmid: *shmid,
                base: *base,
                offset: offset.saturating_add(delta),
            },
        }
    }

    fn same_file(left: &FileDescriptor, right: &FileDescriptor) -> bool {
        left.same_file_identity(right)
    }

    fn can_merge_with(&self, left_len: usize, right: &Self) -> bool {
        match (self, right) {
            (Self::Anonymous, Self::Anonymous) => true,
            (
                Self::File {
                    file: left_file,
                    offset: left_offset,
                    shared: left_shared,
                },
                Self::File {
                    file: right_file,
                    offset: right_offset,
                    shared: right_shared,
                },
            ) => {
                left_shared == right_shared
                    && Self::same_file(left_file, right_file)
                    && left_offset.saturating_add(left_len) == *right_offset
            }
            (
                Self::SharedMemory {
                    shmid: left_shmid,
                    base: left_base,
                    offset: left_offset,
                },
                Self::SharedMemory {
                    shmid: right_shmid,
                    base: right_base,
                    offset: right_offset,
                },
            ) => {
                left_shmid == right_shmid
                    && left_base == right_base
                    && left_offset.saturating_add(left_len) == *right_offset
            }
            _ => false,
        }
    }
}

/// Virtual memory area (VMA).
pub struct MapArea {
    pub start_va: VirtAddr,
    pub end_va: VirtAddr,
    pub flags: PTEFlags,
    /// The compatibility representation physically colocates the resident
    /// owner with its VMA.  Keep this private: `MemorySet` must use the
    /// `ResidentSet` API below rather than changing an owner field directly.
    resident: ResidentSet,
    pub backing: MapAreaBacking,
}

impl MapArea {
    pub fn new(start_va: VirtAddr, end_va: VirtAddr, flags: PTEFlags) -> Self {
        Self::with_backing(start_va, end_va, flags, MapAreaBacking::Anonymous)
    }

    pub fn with_backing(
        start_va: VirtAddr,
        end_va: VirtAddr,
        flags: PTEFlags,
        backing: MapAreaBacking,
    ) -> Self {
        Self {
            start_va,
            end_va,
            flags,
            resident: ResidentSet::new(),
            backing,
        }
    }

    pub fn size(&self) -> usize {
        self.end_va.raw() - self.start_va.raw()
    }

    pub fn contains(&self, va: VirtAddr) -> bool {
        va.raw() >= self.start_va.raw() && va.raw() < self.end_va.raw()
    }

    pub fn page_count(&self) -> usize {
        self.size() / PAGE_SIZE
    }

    pub fn has_frames(&self) -> bool {
        !self.resident.is_empty()
    }

    /// Borrow the sole resident-frame owner for a read-only operation.
    ///
    /// The caller must derive PTE permissions from this area's policy; this
    /// method neither changes VMA topology nor publishes a PTE.
    pub(crate) fn resident(&self) -> &ResidentSet {
        &self.resident
    }

    /// Transfer `page` to this VMA's resident owner as its dense suffix.
    ///
    /// On `Err`, ownership remains with the caller and no PTE changed.  On
    /// success, this VMA is the sole owner; the coordinator must either publish
    /// the corresponding PTE or extract the page again on its rollback path.
    pub(crate) fn append_resident(&mut self, page: FrameTracker) -> Result<(), FrameTracker> {
        self.resident.insert(self.resident.len(), page)
    }

    pub(crate) fn insert_resident_page(
        &mut self,
        vpn: usize,
        page: FrameTracker,
        state: PageState,
    ) -> Result<(), FrameTracker> {
        self.resident
            .insert_page_with_state(vpn, ResidentPage::with_state(page, state))
    }

    pub(crate) fn insert_resident_run(
        &mut self,
        start_vpn: usize,
        pages: Vec<FrameTracker>,
        state: PageState,
    ) -> Result<(), Vec<FrameTracker>> {
        self.resident.insert_run_with_state(start_vpn, pages, state)
    }

    /// Atomically replace one already-resident VMA-relative page owner.
    ///
    /// The returned frame is still owned by the caller and must be retired only
    /// after the replacement PTE has been published and the relevant TLBs have
    /// been invalidated.  If the page is absent, `frame` is returned unchanged
    /// and neither resident metadata nor PTE state is modified.
    pub(crate) fn replace_resident_page(
        &mut self,
        vpn: usize,
        frame: FrameTracker,
    ) -> Result<FrameTracker, FrameTracker> {
        let Some(old) = self.resident.lookup_mut(vpn) else {
            return Err(frame);
        };
        Ok(mem::replace(&mut old.frame, frame))
    }

    pub(crate) fn set_resident_state(&mut self, vpn: usize, state: PageState) -> bool {
        let Some(page) = self.resident.lookup_mut(vpn) else {
            return false;
        };
        page.set_state(state);
        true
    }

    pub(crate) fn set_all_resident_state(&mut self, state: PageState) {
        for page in self.resident.iter_mut() {
            page.set_state(state);
        }
    }

    /// Replace this VMA's resident owner and return the previous sole owner.
    /// No PTE is changed here, so callers use this only before publication or
    /// alongside their explicit PTE/TLB rollback sequence.
    pub(crate) fn replace_resident(&mut self, pages: Vec<FrameTracker>) -> ResidentSet {
        self.resident.replace_all(pages)
    }

    /// Clone the frame references owned by `source` for a sharing operation.
    /// The source keeps ownership and the clone increments frame references;
    /// caller code must publish the destination PTEs only after this succeeds.
    pub(crate) fn clone_resident_from(&mut self, source: &Self) {
        self.resident = source.resident.clone();
    }

    /// Fixed-size data for the one-shot BuildStorm terminal-trap record.
    /// Resident lookup uses the VMA-relative key convention used by every
    /// ownership operation.
    #[cfg(feature = "buildstorm-diagnostics")]
    pub(crate) fn diagnostic_page_snapshot(&self, addr: usize) -> (usize, usize, usize) {
        let vpn = addr / PAGE_SIZE;
        let start_vpn = self.start_va.raw() / PAGE_SIZE;
        let page = vpn
            .checked_sub(start_vpn)
            .and_then(|index| self.resident.lookup(index));
        let resident = page.is_some() as usize;
        let backing = match self.backing {
            MapAreaBacking::Anonymous => 1,
            MapAreaBacking::File { shared: false, .. } => 2,
            MapAreaBacking::File { shared: true, .. } => 3,
            MapAreaBacking::SharedMemory { .. } => 4,
        };
        let page_state = page
            .map(|page| match page.state() {
                PageState::Private => 1,
                PageState::Cow => 2,
                PageState::Shared => 3,
                PageState::FileClean => 4,
                PageState::FileDirty => 5,
            })
            .unwrap_or(0);
        (backing, resident, page_state)
    }

    fn has_full_frames(&self) -> bool {
        self.resident.len() == self.page_count()
    }

    pub fn overlaps(&self, start: usize, end: usize) -> bool {
        self.start_va.raw() < end && start < self.end_va.raw()
    }

    pub fn can_merge_with(&self, next: &Self) -> bool {
        self.end_va.raw() == next.start_va.raw()
            && self.flags.bits() == next.flags.bits()
            && self.backing.can_merge_with(self.size(), &next.backing)
    }

    pub fn merge_with(&mut self, next: Self) {
        // Resident VPNs in `next` are relative to its own VMA start.  Record
        // the old left extent before extending the VMA so the transfer keeps
        // every resident page at its virtual address.
        let offset = self.page_count();
        self.end_va = next.end_va;
        let mut next_resident = next.resident;
        self.resident.merge_from_at(offset, &mut next_resident);
    }

    pub fn split_at(&mut self, split_va: VirtAddr) -> Option<Self> {
        let split = split_va.raw();
        if split <= self.start_va.raw() || split >= self.end_va.raw() {
            return None;
        }
        if split % PAGE_SIZE != 0 {
            return None;
        }

        let right_idx = (split - self.start_va.raw()) / PAGE_SIZE;
        let right_resident = if self.resident.is_empty() {
            ResidentSet::new()
        } else {
            self.resident.split_off(right_idx)?
        };
        let old_end = self.end_va;
        let right_backing = self.backing.split_right(split - self.start_va.raw());
        self.end_va = split_va;

        Some(Self {
            start_va: split_va,
            end_va: old_end,
            flags: self.flags,
            resident: right_resident,
            backing: right_backing,
        })
    }
}
