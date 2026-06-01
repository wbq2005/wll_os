use super::frame_allocator::FrameTracker;
use crate::mm::page_table::PTEFlags;
use crate::{config::PAGE_SIZE, fs::fd::FileDescriptor};
use alloc::vec::Vec;
use polyhal::VirtAddr;

#[derive(Clone)]
pub enum MapAreaBacking {
    Anonymous,
    File {
        file: FileDescriptor,
        offset: usize,
        shared: bool,
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
        }
    }

    fn same_file(left: &FileDescriptor, right: &FileDescriptor) -> bool {
        match (left, right) {
            (
                FileDescriptor::MemFile { name: left, .. },
                FileDescriptor::MemFile { name: right, .. },
            ) => left == right,
            (
                FileDescriptor::Ext4Regular { ino: left, .. },
                FileDescriptor::Ext4Regular { ino: right, .. },
            ) => left == right,
            _ => false,
        }
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
            _ => false,
        }
    }
}

/// Virtual memory area (VMA).
pub struct MapArea {
    pub start_va: VirtAddr,
    pub end_va: VirtAddr,
    pub flags: PTEFlags,
    pub frames: Vec<FrameTracker>,
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
            frames: Vec::new(),
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

    pub fn overlaps(&self, start: usize, end: usize) -> bool {
        self.start_va.raw() < end && start < self.end_va.raw()
    }

    pub fn can_merge_with(&self, next: &Self) -> bool {
        self.end_va.raw() == next.start_va.raw()
            && self.flags.bits() == next.flags.bits()
            && self.backing.can_merge_with(self.size(), &next.backing)
            && self.frames.len() == self.page_count()
            && next.frames.len() == next.page_count()
    }

    pub fn merge_with(&mut self, mut next: Self) {
        self.end_va = next.end_va;
        self.frames.append(&mut next.frames);
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
        if right_idx > self.frames.len() {
            return None;
        }

        let right_frames = self.frames.split_off(right_idx);
        let old_end = self.end_va;
        let right_backing = self.backing.split_right(split - self.start_va.raw());
        self.end_va = split_va;

        Some(Self {
            start_va: split_va,
            end_va: old_end,
            flags: self.flags,
            frames: right_frames,
            backing: right_backing,
        })
    }
}
