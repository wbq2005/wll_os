use super::frame_allocator::FrameTracker;
use crate::mm::page_table::PTEFlags;
use crate::{config::PAGE_SIZE, fs::fd::FileDescriptor};
use alloc::vec::Vec;
use core::mem;
use polyhal::VirtAddr;

/// The sole owner of resident data frames for one logical VMA.
///
/// This first migration stage intentionally retains the legacy dense layout:
/// frames are indexed from the VMA start and the set is either empty or
/// contiguous.  Callers use this API rather than reaching into a `Vec`, so a
/// later sparse-run implementation can change the representation without
/// creating a second owner table.
#[derive(Clone, Default)]
pub struct ResidentSet {
    dense: Vec<FrameTracker>,
}

impl ResidentSet {
    pub const fn new() -> Self {
        Self { dense: Vec::new() }
    }

    pub fn is_empty(&self) -> bool {
        self.dense.is_empty()
    }

    pub fn len(&self) -> usize {
        self.dense.len()
    }

    pub fn capacity(&self) -> usize {
        self.dense.capacity()
    }

    /// Return the page at a VMA-relative VPN.
    pub fn lookup(&self, vpn: usize) -> Option<&FrameTracker> {
        self.dense.get(vpn)
    }

    pub fn lookup_mut(&mut self, vpn: usize) -> Option<&mut FrameTracker> {
        self.dense.get_mut(vpn)
    }

    pub fn iter(&self) -> impl Iterator<Item = &FrameTracker> {
        self.dense.iter()
    }

    /// Insert one VMA-relative page.  The dense compatibility form accepts
    /// only an append, which prevents silently shifting virtual-page identity.
    pub fn insert(&mut self, vpn: usize, page: FrameTracker) -> Result<(), FrameTracker> {
        if vpn != self.dense.len() {
            return Err(page);
        }
        self.dense.push(page);
        Ok(())
    }

    /// Insert a contiguous suffix.  This is the legacy equivalent of adding
    /// a run at `start_vpn`; sparse implementations will remove the suffix
    /// restriction while preserving the ownership contract.
    pub fn insert_run(
        &mut self,
        start_vpn: usize,
        mut pages: Vec<FrameTracker>,
    ) -> Result<(), Vec<FrameTracker>> {
        if start_vpn != self.dense.len() {
            return Err(pages);
        }
        self.dense.append(&mut pages);
        Ok(())
    }

    /// Extract a contiguous suffix into a new owner.  The current dense
    /// representation cannot preserve an interior hole, so callers must first
    /// split the logical VMA at the range boundary.
    pub fn extract_range(&mut self, start_vpn: usize, end_vpn: usize) -> Option<Self> {
        if start_vpn > end_vpn || end_vpn != self.dense.len() {
            return None;
        }
        Some(Self {
            dense: self.dense.split_off(start_vpn),
        })
    }

    pub fn split_off(&mut self, vpn: usize) -> Option<Self> {
        if vpn > self.dense.len() {
            return None;
        }
        Some(Self {
            dense: self.dense.split_off(vpn),
        })
    }

    pub fn merge_from(&mut self, mut other: Self) {
        self.dense.append(&mut other.dense);
    }

    pub fn iter_range(
        &self,
        start_vpn: usize,
        end_vpn: usize,
    ) -> impl Iterator<Item = (usize, &FrameTracker)> {
        self.dense
            .get(start_vpn..end_vpn)
            .unwrap_or(&[])
            .iter()
            .enumerate()
            .map(move |(index, page)| (start_vpn + index, page))
    }

    pub fn drain_all(&mut self) -> Self {
        Self {
            dense: mem::take(&mut self.dense),
        }
    }

    pub fn into_frames(self) -> Vec<FrameTracker> {
        self.dense
    }

    pub fn replace_all(&mut self, pages: Vec<FrameTracker>) -> Self {
        Self {
            dense: mem::replace(&mut self.dense, pages),
        }
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
    /// Resident pages are owned only here.  Keep the field crate-visible while
    /// the compatibility migration removes direct `MapArea.frames` access;
    /// all frame operations must go through `ResidentSet` methods.
    pub(crate) resident: ResidentSet,
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

    fn has_full_frames(&self) -> bool {
        self.resident.len() == self.page_count()
    }

    pub fn overlaps(&self, start: usize, end: usize) -> bool {
        self.start_va.raw() < end && start < self.end_va.raw()
    }

    pub fn can_merge_with(&self, next: &Self) -> bool {
        let compatible_frames = (self.resident.is_empty() && next.resident.is_empty())
            || (self.has_full_frames() && next.has_full_frames());
        self.end_va.raw() == next.start_va.raw()
            && self.flags.bits() == next.flags.bits()
            && self.backing.can_merge_with(self.size(), &next.backing)
            && compatible_frames
    }

    pub fn merge_with(&mut self, next: Self) {
        self.end_va = next.end_va;
        self.resident.merge_from(next.resident);
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
            if right_idx > self.resident.len() {
                return None;
            }
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
