use alloc::vec::Vec;
use core::mem;
use polyhal::pagetable::{MappingFlags, MappingSize, PageTableWrapper};
use polyhal::{PhysAddr, VirtAddr};

use super::frame_allocator::{self, FrameTracker};
use super::map_area::{MapArea, MapAreaBacking};
use super::page_table::{self, PTEFlags};
use crate::config::PAGE_SIZE;
use crate::fs::fd::FileDescriptor;
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

fn effective_mapping_flags(flags: PTEFlags) -> MappingFlags {
    let mut effective = flags;
    if effective.contains(PTEFlags::COW) {
        effective.remove(PTEFlags::W);
    }
    effective.into()
}

fn is_anonymous_cow_candidate(area: &MapArea) -> bool {
    matches!(area.backing, MapAreaBacking::Anonymous)
        && area.has_frames()
        && area.flags.contains(PTEFlags::W)
        && has_leaf_permission(area.flags)
}

fn is_anonymous_readonly_share_candidate(area: &MapArea) -> bool {
    matches!(area.backing, MapAreaBacking::Anonymous)
        && area.has_frames()
        && !area.flags.contains(PTEFlags::W)
        && has_leaf_permission(area.flags)
}

fn map_area_pages(page_table: &PageTableWrapper, area: &MapArea) {
    if !has_leaf_permission(area.flags) {
        return;
    }

    let mf = effective_mapping_flags(area.flags);
    for (idx, frame) in area.frames.iter().enumerate() {
        let vaddr = VirtAddr::new(area.start_va.raw() + idx * PAGE_SIZE);
        let paddr = PhysAddr::new(frame.ppn().addr());
        page_table.map_page(vaddr, paddr, mf, MappingSize::Page4KB);
    }
}

fn map_area_page_window(
    page_table: &PageTableWrapper,
    area: &MapArea,
    start_idx: usize,
    page_count: usize,
) {
    if !has_leaf_permission(area.flags) {
        return;
    }

    let mf = effective_mapping_flags(area.flags);
    let end_idx = start_idx.saturating_add(page_count).min(area.frames.len());
    for idx in start_idx..end_idx {
        let frame = &area.frames[idx];
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

fn copy_area_frames(area: &mut MapArea) -> Result<(), SysErrNo> {
    if area.frames.is_empty() {
        return Ok(());
    }

    let mut frames = Vec::new();
    for src_frame in &area.frames {
        let Some(frame) = frame_allocator::alloc_frame() else {
            return Err(SysErrNo::ENOMEM);
        };
        unsafe {
            core::ptr::copy_nonoverlapping(
                src_frame.ppn().addr() as *const u8,
                frame.ppn().addr() as *mut u8,
                PAGE_SIZE,
            );
        }
        frames.push(frame);
    }
    area.frames = frames;
    Ok(())
}

fn read_file_page(backing: &MapAreaBacking) -> Result<Vec<u8>, SysErrNo> {
    let MapAreaBacking::File { file, offset, .. } = backing else {
        return Ok(alloc::vec![0u8; PAGE_SIZE]);
    };

    let mut data = alloc::vec![0u8; PAGE_SIZE];
    let mut file = file.clone();
    crate::trap::restore_kernel_page_table();
    let _ = file.read_at(*offset, &mut data)?;
    Ok(data)
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
                let mf = effective_mapping_flags(permission);
                self.page_table
                    .map_page(vaddr, paddr, mf, MappingSize::Page4KB);
            }
            area.frames.push(frame);
        }

        self.areas.push(area);
        self.coalesce_areas();
        Ok(())
    }

    pub fn insert_lazy_area_with_backing(
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

        self.areas.push(MapArea::with_backing(
            VirtAddr::new(start),
            VirtAddr::new(end),
            permission,
            backing,
        ));
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
        self.sort_areas();

        let mut frames: Vec<FrameTracker> = Vec::new();
        for vpn in start_vpn..end_vpn {
            if let Some(frame) = frame_allocator::alloc_frame() {
                let ppn = frame.ppn();
                let vaddr = VirtAddr::new(vpn * PAGE_SIZE);
                let paddr = PhysAddr::new(ppn.addr());
                if has_leaf_permission(permission) {
                    let mf = effective_mapping_flags(permission);
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
            let mf = effective_mapping_flags(permission);
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
        if start >= end {
            return false;
        }
        let index = self.first_area_ending_after(start);
        index < self.areas.len() && self.areas[index].start_va.raw() < end
    }

    pub fn range_covered(&self, start: usize, end: usize) -> bool {
        let mut cursor = start;
        while cursor < end {
            let Some(index) = self.area_index_containing(cursor) else {
                return false;
            };
            cursor = self.areas[index].end_va.raw();
        }
        true
    }

    pub fn find_free_area(&self, hint: usize, length: usize, limit: usize) -> Option<usize> {
        if length == 0 {
            return None;
        }
        let mut candidate = align_up(hint.max(PAGE_SIZE))?;
        let mut index = self.first_area_ending_after(candidate);
        loop {
            let end = candidate.checked_add(length)?;
            if end > limit {
                return None;
            }

            while index < self.areas.len() && self.areas[index].end_va.raw() <= candidate {
                index += 1;
            }

            if index == self.areas.len() || self.areas[index].start_va.raw() >= end {
                return Some(candidate);
            }

            candidate = align_up(self.areas[index].end_va.raw())?;
            index += 1;
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

    pub fn release_user_areas(&mut self) {
        for area in &self.areas {
            unmap_area_pages(&self.page_table, area, area.flags);
        }
        self.areas.clear();
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
                let file_backed = matches!(area.backing, MapAreaBacking::File { .. });
                let anonymous = matches!(area.backing, MapAreaBacking::Anonymous);
                let mut new_flags = flags;
                if file_backed && flags.contains(PTEFlags::W) && area.has_frames() {
                    copy_area_frames(area)?;
                }
                if anonymous
                    && flags.contains(PTEFlags::W)
                    && area.has_frames()
                    && area.flags.contains(PTEFlags::COW)
                {
                    new_flags |= PTEFlags::COW;
                } else if anonymous
                    && flags.contains(PTEFlags::W)
                    && area.has_frames()
                    && area.frames.iter().any(|frame| frame.ref_count() > 1)
                {
                    copy_area_frames(area)?;
                }
                if has_leaf_permission(new_flags) && !file_backed {
                    populate_area_frames(area)?;
                }
                area.flags = new_flags;
                remap_area_pages(&self.page_table, area, old_flags);
            }
        }
        self.coalesce_areas();
        Ok(())
    }

    fn resolve_cow_page(&mut self, page_start: usize) -> Result<(), SysErrNo> {
        let page_end = page_start.checked_add(PAGE_SIZE).ok_or(SysErrNo::EFAULT)?;
        self.split_area_at(page_start);
        self.split_area_at(page_end);

        let Some(index) = self.area_index_containing(page_start) else {
            return Err(SysErrNo::EFAULT);
        };
        if self.areas[index].start_va.raw() != page_start
            || self.areas[index].end_va.raw() != page_end
        {
            return Err(SysErrNo::EFAULT);
        }

        let old_flags = self.areas[index].flags;
        if !matches!(self.areas[index].backing, MapAreaBacking::Anonymous)
            || !old_flags.contains(PTEFlags::COW)
            || !old_flags.contains(PTEFlags::W)
        {
            return Err(SysErrNo::EFAULT);
        }
        let shared = self.areas[index]
            .frames
            .first()
            .map(|frame| frame.ref_count() > 1)
            .ok_or(SysErrNo::EFAULT)?;

        let replacement = if shared {
            let src_frame = self.areas[index].frames[0].clone();
            let Some(frame) = frame_allocator::alloc_frame() else {
                return Err(SysErrNo::ENOMEM);
            };
            unsafe {
                core::ptr::copy_nonoverlapping(
                    src_frame.ppn().addr() as *const u8,
                    frame.ppn().addr() as *mut u8,
                    PAGE_SIZE,
                );
            }
            Some(frame)
        } else {
            None
        };

        {
            let area = &mut self.areas[index];
            if let Some(frame) = replacement {
                area.frames[0] = frame;
            }
            area.flags.remove(PTEFlags::COW);
        }
        remap_area_pages(&self.page_table, &self.areas[index], old_flags);
        Ok(())
    }

    fn ensure_page_writable(&mut self, page_start: usize) -> Result<(), SysErrNo> {
        let Some(index) = self.area_index_containing(page_start) else {
            return Err(SysErrNo::EFAULT);
        };
        let flags = self.areas[index].flags;
        if !has_leaf_permission(flags) || !flags.contains(PTEFlags::W) {
            return Err(SysErrNo::EFAULT);
        }
        if flags.contains(PTEFlags::COW) {
            return self.resolve_cow_page(page_start);
        }
        if self.translate(VirtAddr::new(page_start)).is_none() {
            self.handle_page_fault(page_start, true, false)?;
        }
        Ok(())
    }

    fn ensure_page_readable(&mut self, page_start: usize) -> Result<(), SysErrNo> {
        let Some(index) = self.area_index_containing(page_start) else {
            return Err(SysErrNo::EFAULT);
        };
        let flags = self.areas[index].flags;
        if !has_leaf_permission(flags) || !flags.contains(PTEFlags::R) {
            return Err(SysErrNo::EFAULT);
        }
        if self.translate(VirtAddr::new(page_start)).is_none() {
            self.handle_page_fault(page_start, false, false)?;
        }
        Ok(())
    }

    pub fn prepare_read(&mut self, src: usize, len: usize) -> Result<(), SysErrNo> {
        let mut checked = 0usize;
        while checked < len {
            let addr = src.checked_add(checked).ok_or(SysErrNo::EFAULT)?;
            self.ensure_page_readable(align_down(addr))?;
            checked += (PAGE_SIZE - addr % PAGE_SIZE).min(len - checked);
        }
        Ok(())
    }

    pub fn prepare_write(&mut self, dst: usize, len: usize) -> Result<(), SysErrNo> {
        let mut checked = 0usize;
        while checked < len {
            let addr = dst.checked_add(checked).ok_or(SysErrNo::EFAULT)?;
            self.ensure_page_writable(align_down(addr))?;
            checked += (PAGE_SIZE - addr % PAGE_SIZE).min(len - checked);
        }
        Ok(())
    }

    pub fn write_bytes(&mut self, dst: usize, src: &[u8]) -> Result<(), SysErrNo> {
        let mut copied = 0usize;
        while copied < src.len() {
            let addr = dst.checked_add(copied).ok_or(SysErrNo::EFAULT)?;
            let Some(index) = self.area_index_containing(addr) else {
                return Err(SysErrNo::EFAULT);
            };
            let area = &self.areas[index];

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

    pub fn invalidate_file_range(
        &mut self,
        start_va: VirtAddr,
        end_va: VirtAddr,
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
            if area.start_va.raw() >= start
                && area.end_va.raw() <= end
                && matches!(area.backing, MapAreaBacking::File { .. })
            {
                unmap_area_pages(&self.page_table, area, area.flags);
            }
        }
        self.coalesce_areas();
        Ok(())
    }

    pub fn handle_page_fault(
        &mut self,
        fault_addr: usize,
        is_store: bool,
        is_exec: bool,
    ) -> Result<(), SysErrNo> {
        let page_start = align_down(fault_addr);
        let page_end = page_start.checked_add(PAGE_SIZE).ok_or(SysErrNo::EFAULT)?;
        let Some(area_index) = self.area_index_containing(fault_addr) else {
            return Err(SysErrNo::EFAULT);
        };
        let area = &self.areas[area_index];

        if !has_leaf_permission(area.flags) {
            return Err(SysErrNo::EFAULT);
        }
        if is_exec {
            if !area.flags.contains(PTEFlags::X) {
                return Err(SysErrNo::EFAULT);
            }
        } else if is_store {
            if !area.flags.contains(PTEFlags::W) {
                return Err(SysErrNo::EFAULT);
            }
        } else if !area.flags.contains(PTEFlags::R) {
            return Err(SysErrNo::EFAULT);
        }
        if is_store && area.flags.contains(PTEFlags::COW) {
            self.resolve_cow_page(page_start)?;
            self.activate();
            return Ok(());
        }
        let clean_cache_page = if !is_store && !area.flags.contains(PTEFlags::W) {
            match &area.backing {
                MapAreaBacking::File {
                    file: FileDescriptor::Ext4Regular { ino, .. },
                    offset,
                    ..
                } if crate::fs::ext4_vol::can_use_clean_page_cache(*ino) => Some((
                    *ino,
                    offset.saturating_add(page_start - area.start_va.raw()),
                )),
                _ => None,
            }
        } else {
            None
        };
        if self.translate(VirtAddr::new(page_start)).is_some() {
            self.activate();
            return Ok(());
        }
        if matches!(area.backing, MapAreaBacking::File { .. }) && area.has_frames() {
            let page_idx = (page_start - area.start_va.raw()) / PAGE_SIZE;
            map_area_page_window(&self.page_table, area, page_idx, 1);
            self.activate();
            return Ok(());
        }

        self.split_area_at(page_start);
        self.split_area_at(page_end);

        let Some(idx) = self.area_index_containing(page_start) else {
            return Err(SysErrNo::EFAULT);
        };
        if self.areas[idx].start_va.raw() != page_start || self.areas[idx].end_va.raw() != page_end
        {
            return Err(SysErrNo::EFAULT);
        }

        if self.areas[idx].has_frames() {
            map_area_pages(&self.page_table, &self.areas[idx]);
            self.activate();
            return Ok(());
        }

        if let Some((ino, file_offset)) = clean_cache_page {
            crate::trap::restore_kernel_page_table();
            let cache_frame_result = crate::fs::ext4_vol::clean_page_cache_frame(ino, file_offset);
            if cache_frame_result.is_err() {
                self.activate();
            }
            let cache_frame = cache_frame_result?;
            self.areas[idx].frames.push(cache_frame);
            map_area_pages(&self.page_table, &self.areas[idx]);
            self.activate();
            return Ok(());
        }

        let Some(frame) = frame_allocator::alloc_frame() else {
            return Err(SysErrNo::ENOMEM);
        };
        crate::trap::restore_kernel_page_table();
        let data_result = read_file_page(&self.areas[idx].backing);
        if data_result.is_err() {
            self.activate();
        }
        let data = data_result?;
        unsafe {
            core::ptr::copy_nonoverlapping(data.as_ptr(), frame.ppn().addr() as *mut u8, PAGE_SIZE);
        }
        self.areas[idx].frames.push(frame);
        map_area_pages(&self.page_table, &self.areas[idx]);
        self.activate();
        Ok(())
    }

    fn split_area_at(&mut self, addr: usize) {
        if addr % PAGE_SIZE != 0 {
            return;
        }

        let Some(index) = self.area_index_containing(addr) else {
            return;
        };
        if let Some(right) = self.areas[index].split_at(VirtAddr::new(addr)) {
            self.areas.insert(index + 1, right);
        }
    }

    fn sort_areas(&mut self) {
        self.areas
            .sort_by(|left, right| left.start_va.raw().cmp(&right.start_va.raw()));
    }

    fn area_index_containing(&self, addr: usize) -> Option<usize> {
        let mut left = 0usize;
        let mut right = self.areas.len();
        while left < right {
            let mid = left + (right - left) / 2;
            let area = &self.areas[mid];
            if addr < area.start_va.raw() {
                right = mid;
            } else if addr >= area.end_va.raw() {
                left = mid + 1;
            } else {
                return Some(mid);
            }
        }
        None
    }

    fn first_area_ending_after(&self, addr: usize) -> usize {
        let mut left = 0usize;
        let mut right = self.areas.len();
        while left < right {
            let mid = left + (right - left) / 2;
            if self.areas[mid].end_va.raw() <= addr {
                left = mid + 1;
            } else {
                right = mid;
            }
        }
        left
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

    pub fn fork_cow(&mut self) -> Result<Self, SysErrNo> {
        let mut new_ms = Self::from_kernel();

        for area in &mut self.areas {
            let mut new_area =
                MapArea::with_backing(area.start_va, area.end_va, area.flags, area.backing.clone());

            if matches!(area.backing, MapAreaBacking::SharedMemory { .. }) {
                new_area.frames = area.frames.clone();
                map_area_pages(&new_ms.page_table, &new_area);
                new_ms.areas.push(new_area);
                continue;
            }

            if is_anonymous_cow_candidate(area) {
                let old_flags = area.flags;
                area.flags |= PTEFlags::COW;
                if old_flags.bits() != area.flags.bits() {
                    remap_area_pages(&self.page_table, area, old_flags);
                }
                new_area.flags = area.flags;
                new_area.frames = area.frames.clone();
                map_area_pages(&new_ms.page_table, &new_area);
                new_ms.areas.push(new_area);
                continue;
            }

            if is_anonymous_readonly_share_candidate(area) {
                new_area.frames = area.frames.clone();
                map_area_pages(&new_ms.page_table, &new_area);
                new_ms.areas.push(new_area);
                continue;
            }

            for (idx, src_frame) in area.frames.iter().enumerate() {
                let Some(frame) = frame_allocator::alloc_frame() else {
                    return Err(SysErrNo::ENOMEM);
                };
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
                    let mf = effective_mapping_flags(area.flags);
                    new_ms
                        .page_table
                        .map_page(vaddr, new_paddr, mf, MappingSize::Page4KB);
                }
                new_area.frames.push(frame);
            }

            new_ms.areas.push(new_area);
        }
        new_ms.coalesce_areas();
        Ok(new_ms)
    }
}

impl Clone for MemorySet {
    fn clone(&self) -> Self {
        let mut new_ms = Self::from_kernel();

        for area in &self.areas {
            let mut new_flags = area.flags;
            new_flags.remove(PTEFlags::COW);
            let mut new_area =
                MapArea::with_backing(area.start_va, area.end_va, new_flags, area.backing.clone());

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
                        let mf = effective_mapping_flags(new_flags);
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
