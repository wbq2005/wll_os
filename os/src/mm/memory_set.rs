use alloc::vec::Vec;
use core::mem;
use polyhal::pagetable::{MappingFlags, MappingSize, PageTableWrapper};
use polyhal::{PhysAddr, VirtAddr};

use super::frame_allocator::{self, FrameTracker};
use super::map_area::{MapArea, MapAreaBacking};
use super::page_table::{self, PTEFlags};
use crate::config::PAGE_SIZE;
use crate::utils::error::SysErrNo;

fn align_down(value: usize) -> usize {
    value / PAGE_SIZE * PAGE_SIZE
}

fn align_up(value: usize) -> Option<usize> {
    value
        .checked_add(PAGE_SIZE - 1)
        .map(|value| value / PAGE_SIZE * PAGE_SIZE)
}

fn has_leaf_permission(flags: PTEFlags) -> bool {
    flags.intersects(PTEFlags::R | PTEFlags::W | PTEFlags::X)
}

fn ranges_overlap(
    left_start: usize,
    left_end: usize,
    right_start: usize,
    right_end: usize,
) -> bool {
    left_start < right_end && right_start < left_end
}

fn map_area_pages(page_table: &PageTableWrapper, area: &MapArea) {
    if !has_leaf_permission(area.flags) {
        return;
    }

    let mf: MappingFlags = area.flags.into();
    for (idx, frame) in area.frames.iter().enumerate() {
        let vaddr = VirtAddr::new(area.start_va.raw() + idx * PAGE_SIZE);
        let paddr = PhysAddr::new(frame.ppn().addr());
        page_table.map_page(vaddr, paddr, mf, MappingSize::Page4KB);
    }
}

fn unmap_area_pages(page_table: &PageTableWrapper, area: &MapArea, flags: PTEFlags) {
    if !has_leaf_permission(flags) || !area.has_frames() {
        return;
    }

    for idx in 0..area.frames.len() {
        let vaddr = VirtAddr::new(area.start_va.raw() + idx * PAGE_SIZE);
        page_table.unmap_page(vaddr);
    }
}

fn remap_area_pages(page_table: &PageTableWrapper, area: &MapArea, old_flags: PTEFlags) {
    unmap_area_pages(page_table, area, old_flags);
    map_area_pages(page_table, area);
}

fn populate_area_frames(area: &mut MapArea) -> Result<(), SysErrNo> {
    if area.has_frames() {
        return Ok(());
    }

    let mut frames = Vec::new();
    for _ in 0..area.page_count() {
        let Some(frame) = frame_allocator::alloc_frame() else {
            return Err(SysErrNo::ENOMEM);
        };
        frames.push(frame);
    }
    area.frames = frames;
    Ok(())
}

/// Per-process address space.
pub struct MemorySet {
    pub page_table: PageTableWrapper,
    /// User VMAs. Framed areas own their user data frames through `MapArea`.
    pub areas: Vec<MapArea>,
}

impl MemorySet {
    /// Create a fresh address space with an independent root page table.
    pub fn new_bare() -> Self {
        Self {
            page_table: PageTableWrapper::alloc_new(),
            areas: Vec::new(),
        }
    }

    /// Map a framed user range and keep ownership of every allocated data page.
    pub fn insert_framed_area(
        &mut self,
        start_va: VirtAddr,
        end_va: VirtAddr,
        permission: PTEFlags,
    ) -> Result<(), SysErrNo> {
        self.insert_framed_area_with_backing(
            start_va,
            end_va,
            permission,
            MapAreaBacking::Anonymous,
        )
    }

    pub fn insert_framed_area_with_backing(
        &mut self,
        start_va: VirtAddr,
        end_va: VirtAddr,
        permission: PTEFlags,
        backing: MapAreaBacking,
    ) -> Result<(), SysErrNo> {
        let start = align_down(start_va.raw());
        let end = align_up(end_va.raw()).ok_or(SysErrNo::EINVAL)?;
        if start >= end {
            return Err(SysErrNo::EINVAL);
        }

        let mut area = MapArea::with_backing(
            VirtAddr::new(start),
            VirtAddr::new(end),
            permission,
            backing,
        );
        if !has_leaf_permission(permission) {
            self.areas.push(area);
            self.coalesce_areas();
            return Ok(());
        }

        let start_vpn = start / PAGE_SIZE;
        let end_vpn = end / PAGE_SIZE;
        for vpn in start_vpn..end_vpn {
            let Some(frame) = frame_allocator::alloc_frame() else {
                for mapped_vpn in start_vpn..vpn {
                    self.page_table
                        .unmap_page(VirtAddr::new(mapped_vpn * PAGE_SIZE));
                }
                return Err(SysErrNo::ENOMEM);
            };
            let ppn = frame.ppn();
            let vaddr = VirtAddr::new(vpn * PAGE_SIZE);
            let paddr = PhysAddr::new(ppn.addr());
            if has_leaf_permission(permission) {
                let mf: MappingFlags = permission.into();
                self.page_table
                    .map_page(vaddr, paddr, mf, MappingSize::Page4KB);
            }
            area.frames.push(frame);
        }

        self.areas.push(area);
        self.coalesce_areas();
        Ok(())
    }

    /// Map a framed range and return the data frames to the caller.
    ///
    /// This is kept for callers that need external frame ownership. The normal
    /// process-memory path should use `insert_framed_area`.
    pub fn insert_framed_area_with_frames(
        &mut self,
        start_va: VirtAddr,
        end_va: VirtAddr,
        permission: PTEFlags,
    ) -> Vec<FrameTracker> {
        let start = align_down(start_va.raw());
        let end = align_up(end_va.raw()).unwrap_or(start);
        let start_vpn = start / PAGE_SIZE;
        let end_vpn = end / PAGE_SIZE;

        self.areas.push(MapArea::new(
            VirtAddr::new(start),
            VirtAddr::new(end),
            permission,
        ));

        let mut frames: Vec<FrameTracker> = Vec::new();
        for vpn in start_vpn..end_vpn {
            if let Some(frame) = frame_allocator::alloc_frame() {
                let ppn = frame.ppn();
                let vaddr = VirtAddr::new(vpn * PAGE_SIZE);
                let paddr = PhysAddr::new(ppn.addr());
                if has_leaf_permission(permission) {
                    let mf: MappingFlags = permission.into();
                    self.page_table
                        .map_page(vaddr, paddr, mf, MappingSize::Page4KB);
                }
                frames.push(frame);
            }
        }
        frames
    }

    pub fn insert_shared_framed_area(
        &mut self,
        start_va: VirtAddr,
        end_va: VirtAddr,
        permission: PTEFlags,
        backing: MapAreaBacking,
        frames: &[FrameTracker],
    ) -> Result<(), SysErrNo> {
        let start = align_down(start_va.raw());
        let end = align_up(end_va.raw()).ok_or(SysErrNo::EINVAL)?;
        if start >= end || frames.len() != (end - start) / PAGE_SIZE {
            return Err(SysErrNo::EINVAL);
        }
        if self.range_overlaps(start, end) {
            return Err(SysErrNo::ENOMEM);
        }

        let mut area = MapArea::with_backing(
            VirtAddr::new(start),
            VirtAddr::new(end),
            permission,
            backing,
        );
        area.frames = frames.to_vec();
        if has_leaf_permission(permission) {
            let mf: MappingFlags = permission.into();
            for (idx, frame) in area.frames.iter().enumerate() {
                let vaddr = VirtAddr::new(start + idx * PAGE_SIZE);
                let paddr = PhysAddr::new(frame.ppn().addr());
                self.page_table
                    .map_page(vaddr, paddr, mf, MappingSize::Page4KB);
            }
        }

        self.areas.push(area);
        self.coalesce_areas();
        Ok(())
    }

    /// Build a user address space seeded with the kernel/device mappings needed
    /// while the kernel runs on a user task's page table.
    pub fn from_kernel() -> Self {
        let mut ms = Self::new_bare();
        #[cfg(target_arch = "riscv64")]
        {
            let device_flags = PTEFlags::R | PTEFlags::W | PTEFlags::V;
            for paddr in [
                0x0200_0000usize,
                0x0c00_0000usize,
                0x1000_0000usize,
                0x1000_1000usize,
            ] {
                ms.page_table.map_page(
                    VirtAddr::new(paddr),
                    PhysAddr::new(paddr),
                    device_flags.into(),
                    MappingSize::Page4KB,
                );
            }
        }
        ms
    }

    pub fn activate(&self) {
        self.page_table.change();
    }

    pub fn satp_token(&self) -> usize {
        let root_ppn = self.page_table.root().raw() >> 12;
        (8usize << 60) | root_ppn
    }

    pub fn remove_area(&mut self, start_va: VirtAddr) {
        self.areas
            .retain(|area| area.start_va.raw() != start_va.raw());
    }

    pub fn unmap_area(&mut self, start_va: VirtAddr) {
        let Some(end_va) = self
            .areas
            .iter()
            .find(|a| a.start_va.raw() == start_va.raw())
            .map(|area| area.end_va)
        else {
            return;
        };
        let _ = self.unmap_range(start_va, end_va);
    }

    pub fn translate(&self, vaddr: VirtAddr) -> Option<PhysAddr> {
        self.page_table.translate(vaddr).map(|(paddr, _)| paddr)
    }

    pub fn is_mapped(&self, vaddr: VirtAddr) -> bool {
        self.translate(vaddr).is_some()
    }

    pub fn range_overlaps(&self, start: usize, end: usize) -> bool {
        self.areas.iter().any(|area| area.overlaps(start, end))
    }

    pub fn range_covered(&self, start: usize, end: usize) -> bool {
        let mut cursor = start;
        while cursor < end {
            let mut next = cursor;
            for area in &self.areas {
                let area_start = area.start_va.raw();
                let area_end = area.end_va.raw();
                if area_start <= cursor && cursor < area_end && area_end > next {
                    next = area_end;
                }
            }
            if next == cursor {
                return false;
            }
            cursor = next;
        }
        true
    }

    pub fn find_free_area(&self, hint: usize, length: usize, limit: usize) -> Option<usize> {
        if length == 0 {
            return None;
        }
        let mut candidate = align_up(hint.max(PAGE_SIZE))?;
        loop {
            let end = candidate.checked_add(length)?;
            if end > limit {
                return None;
            }

            let mut bumped = false;
            for area in &self.areas {
                if ranges_overlap(candidate, end, area.start_va.raw(), area.end_va.raw()) {
                    candidate = align_up(area.end_va.raw())?;
                    bumped = true;
                    break;
                }
            }
            if !bumped {
                return Some(candidate);
            }
        }
    }

    pub fn unmap_range(&mut self, start_va: VirtAddr, end_va: VirtAddr) -> Result<(), SysErrNo> {
        let start = start_va.raw();
        let end = end_va.raw();
        if start >= end || start % PAGE_SIZE != 0 || end % PAGE_SIZE != 0 {
            return Err(SysErrNo::EINVAL);
        }

        self.split_area_at(start);
        self.split_area_at(end);

        let mut kept = Vec::new();
        for area in mem::take(&mut self.areas) {
            if area.overlaps(start, end) {
                unmap_area_pages(&self.page_table, &area, area.flags);
            } else {
                kept.push(area);
            }
        }
        self.areas = kept;
        self.coalesce_areas();
        Ok(())
    }

    pub fn protect_range(
        &mut self,
        start_va: VirtAddr,
        end_va: VirtAddr,
        flags: PTEFlags,
    ) -> Result<(), SysErrNo> {
        let start = start_va.raw();
        let end = end_va.raw();
        if start >= end || start % PAGE_SIZE != 0 || end % PAGE_SIZE != 0 {
            return Err(SysErrNo::EINVAL);
        }
        if !self.range_covered(start, end) {
            return Err(SysErrNo::ENOMEM);
        }

        self.split_area_at(start);
        self.split_area_at(end);

        for area in &mut self.areas {
            if area.start_va.raw() >= start && area.end_va.raw() <= end {
                let old_flags = area.flags;
                if has_leaf_permission(flags) {
                    populate_area_frames(area)?;
                }
                area.flags = flags;
                remap_area_pages(&self.page_table, area, old_flags);
            }
        }
        self.coalesce_areas();
        Ok(())
    }

    pub fn write_bytes(&mut self, dst: usize, src: &[u8]) -> Result<(), SysErrNo> {
        let mut copied = 0usize;
        while copied < src.len() {
            let addr = dst.checked_add(copied).ok_or(SysErrNo::EFAULT)?;
            let Some(area) = self
                .areas
                .iter()
                .find(|area| area.contains(VirtAddr::new(addr)))
            else {
                return Err(SysErrNo::EFAULT);
            };

            let page_idx = (align_down(addr) - area.start_va.raw()) / PAGE_SIZE;
            let page_off = addr % PAGE_SIZE;
            let copy_len = (src.len() - copied).min(PAGE_SIZE - page_off);
            let frame = area.frames.get(page_idx).ok_or(SysErrNo::EFAULT)?;
            let dst_ptr = (frame.ppn().addr() + page_off) as *mut u8;
            unsafe {
                core::ptr::copy_nonoverlapping(src[copied..].as_ptr(), dst_ptr, copy_len);
            }
            copied += copy_len;
        }
        Ok(())
    }

    fn split_area_at(&mut self, addr: usize) {
        if addr % PAGE_SIZE != 0 {
            return;
        }

        let mut split = Vec::new();
        for mut area in mem::take(&mut self.areas) {
            if let Some(right) = area.split_at(VirtAddr::new(addr)) {
                split.push(area);
                split.push(right);
            } else {
                split.push(area);
            }
        }
        self.areas = split;
        self.sort_areas();
    }

    fn sort_areas(&mut self) {
        self.areas
            .sort_by(|left, right| left.start_va.raw().cmp(&right.start_va.raw()));
    }

    fn coalesce_areas(&mut self) {
        self.sort_areas();
        let mut merged: Vec<MapArea> = Vec::new();
        for area in mem::take(&mut self.areas) {
            if let Some(last) = merged.last_mut() {
                if last.can_merge_with(&area) {
                    last.merge_with(area);
                    continue;
                }
            }
            merged.push(area);
        }
        self.areas = merged;
    }
}

impl Clone for MemorySet {
    fn clone(&self) -> Self {
        let mut new_ms = Self::from_kernel();

        for area in &self.areas {
            let mut new_area =
                MapArea::with_backing(area.start_va, area.end_va, area.flags, area.backing.clone());

            if matches!(area.backing, MapAreaBacking::SharedMemory { .. }) {
                new_area.frames = area.frames.clone();
                map_area_pages(&new_ms.page_table, &new_area);
                new_ms.areas.push(new_area);
                continue;
            }

            for (idx, src_frame) in area.frames.iter().enumerate() {
                if let Some(frame) = frame_allocator::alloc_frame() {
                    let new_paddr = PhysAddr::new(frame.ppn().addr());
                    let src_paddr = PhysAddr::new(src_frame.ppn().addr());
                    unsafe {
                        core::ptr::copy_nonoverlapping(
                            src_paddr.raw() as *const u8,
                            new_paddr.raw() as *mut u8,
                            PAGE_SIZE,
                        );
                    }

                    if has_leaf_permission(area.flags) {
                        let vaddr = VirtAddr::new(area.start_va.raw() + idx * PAGE_SIZE);
                        let mf: MappingFlags = area.flags.into();
                        new_ms
                            .page_table
                            .map_page(vaddr, new_paddr, mf, MappingSize::Page4KB);
                    }
                    new_area.frames.push(frame);
                }
            }

            new_ms.areas.push(new_area);
        }
        new_ms.coalesce_areas();
        new_ms
    }
}

use lazy_static::lazy_static;
use spin::Mutex;

lazy_static! {
    pub static ref KERNEL_SPACE: Mutex<MemorySet> = Mutex::new(MemorySet::new_bare());
}

pub fn init_kernel_space() {
    if let Some(ref kpt) = *page_table::kernel_page_table().lock() {
        log::info!("[mm] Kernel space initialized using boot page table");
        log::info!("[mm] Boot page table root: {:?}", kpt.root());
        log::info!("[mm] SV39 page table setup complete");
    } else {
        log::error!("[mm] KERNEL_PAGE_TABLE not initialized!");
    }
}
