use alloc::vec::Vec;
use core::mem;
use polyhal::pagetable::{MappingFlags, MappingSize, PageTable, PageTableWrapper, TLB};
use polyhal::{PhysAddr, VirtAddr};

use super::frame_allocator::{self, FrameTracker};
use super::map_area::{MapArea, MapAreaBacking, PageState, ResidentPage};
use super::page_table::{self, PTEFlags};
use crate::config::PAGE_SIZE;
use crate::fs::fd::FileDescriptor;
use crate::utils::error::SysErrNo;

const CLEAN_FILE_FAULT_WINDOW_PAGES: usize = 64;
const CLEAN_FILE_FAULT_READ_AHEAD_PAGES: usize = 16;
const ANONYMOUS_FAULT_WINDOW_PAGES: usize = 8;
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

fn effective_mapping_flags_for_page(flags: PTEFlags, page: &ResidentPage) -> MappingFlags {
    let mut effective = flags;
    if page.state() == PageState::Cow {
        effective.remove(PTEFlags::W);
    }
    effective.into()
}

fn is_private_cow_candidate(area: &MapArea) -> bool {
    matches!(
        area.backing,
        MapAreaBacking::Anonymous | MapAreaBacking::File { shared: false, .. }
    ) && area.has_frames()
        && area.flags.contains(PTEFlags::W)
        && has_leaf_permission(area.flags)
}

fn is_readonly_share_candidate(area: &MapArea) -> bool {
    matches!(
        area.backing,
        MapAreaBacking::Anonymous | MapAreaBacking::File { .. }
    ) && area.has_frames()
        && !area.flags.contains(PTEFlags::W)
        && has_leaf_permission(area.flags)
}

fn is_shared_mapping(area: &MapArea) -> bool {
    matches!(
        area.backing,
        MapAreaBacking::SharedMemory { .. } | MapAreaBacking::File { shared: true, .. }
    )
}

fn map_area_pages(page_table: &PageTableWrapper, area: &MapArea) {
    if !has_leaf_permission(area.flags) {
        return;
    }

    for (idx, page) in area.resident().iter_range(0, area.page_count()) {
        let mf = effective_mapping_flags_for_page(area.flags, page);
        let vaddr = VirtAddr::new(area.start_va.raw() + idx * PAGE_SIZE);
        let paddr = PhysAddr::new(page.ppn().addr());
        page_table.map_page(vaddr, paddr, mf, MappingSize::Page4KB);
    }
}

fn map_area_page(page_table: &PageTableWrapper, area: &MapArea, vpn: usize) {
    let Some(page) = area.resident().lookup(vpn) else {
        return;
    };
    if !has_leaf_permission(area.flags) {
        return;
    }
    let mf = effective_mapping_flags_for_page(area.flags, page);
    let vaddr = VirtAddr::new(area.start_va.raw() + vpn * PAGE_SIZE);
    let paddr = PhysAddr::new(page.ppn().addr());
    page_table.map_page(vaddr, paddr, mf, MappingSize::Page4KB);
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

    let end_idx = start_idx.saturating_add(page_count).min(area.page_count());
    for (idx, page) in area.resident().iter_range(start_idx, end_idx) {
        let mf = effective_mapping_flags_for_page(area.flags, page);
        let vaddr = VirtAddr::new(area.start_va.raw() + idx * PAGE_SIZE);
        let paddr = PhysAddr::new(page.ppn().addr());
        page_table.map_page(vaddr, paddr, mf, MappingSize::Page4KB);
    }
}

fn unmap_area_pages_without_shootdown(
    page_table: &PageTableWrapper,
    area: &MapArea,
    flags: PTEFlags,
) {
    if !has_leaf_permission(flags) || !area.has_frames() {
        return;
    }

    for (idx, _) in area.resident().iter_range(0, area.page_count()) {
        let vaddr = VirtAddr::new(area.start_va.raw() + idx * PAGE_SIZE);
        page_table.unmap_page(vaddr);
    }
}

fn unmap_area_pages(page_table: &PageTableWrapper, area: &MapArea, flags: PTEFlags) {
    unmap_area_pages_without_shootdown(page_table, area, flags);
    if !has_leaf_permission(flags) || !area.has_frames() {
        return;
    }
    crate::platform::tlb_shootdown(page_table.root().raw());
}

fn remap_area_pages(page_table: &PageTableWrapper, area: &MapArea, old_flags: PTEFlags) {
    unmap_area_pages(page_table, area, old_flags);
    map_area_pages(page_table, area);
}

fn copy_area_frames(area: &mut MapArea) -> Result<Vec<FrameTracker>, SysErrNo> {
    if area.resident().is_empty() {
        return Ok(Vec::new());
    }

    let mut frames = Vec::new();
    for src_page in area.resident().iter() {
        let Some(frame) = frame_allocator::alloc_frame() else {
            return Err(SysErrNo::ENOMEM);
        };
        unsafe {
            core::ptr::copy_nonoverlapping(
                src_page.ppn().addr() as *const u8,
                frame.ppn().addr() as *mut u8,
                PAGE_SIZE,
            );
        }
        frames.push(frame);
    }
    Ok(area.replace_resident(frames).into_frames())
}

fn read_backing_page(backing: &MapAreaBacking, page_offset: usize) -> Result<Vec<u8>, SysErrNo> {
    let mut data = alloc::vec![0u8; PAGE_SIZE];
    match backing {
        MapAreaBacking::File { file, offset, .. } => {
            let mut file = file.clone();
            let _ = file.read_at(page_offset.saturating_add(*offset), &mut data)?;
        }
        MapAreaBacking::Anonymous | MapAreaBacking::SharedMemory { .. } => {}
    }
    Ok(data)
}

/// Per-process address space.
pub struct MemorySet {
    pub page_table: PageTableWrapper,
    address_space_id: usize,
    /// User VMAs own resident pages through `MapArea::resident`.
    pub areas: Vec<MapArea>,
}

impl MemorySet {
    #[cfg(feature = "buildstorm-diagnostics")]
    pub(crate) fn diagnostic_drop_after_exec(self) {
        let this = mem::ManuallyDrop::new(self);

        let started_at = crate::timer::get_time_us();
        retire_address_space_id(this.address_space_id);
        crate::buildstorm_diagnostics::note_phase(
            47,
            crate::timer::get_time_us().saturating_sub(started_at),
        );

        // Match Rust's field-drop order after MemorySet::drop: page table
        // ownership is released before the VMA resident-frame owners.
        let started_at = crate::timer::get_time_us();
        unsafe {
            drop(core::ptr::read(&this.page_table));
        }
        crate::buildstorm_diagnostics::note_phase(
            48,
            crate::timer::get_time_us().saturating_sub(started_at),
        );

        let started_at = crate::timer::get_time_us();
        unsafe {
            drop(core::ptr::read(&this.areas));
        }
        crate::buildstorm_diagnostics::note_phase(
            49,
            crate::timer::get_time_us().saturating_sub(started_at),
        );
    }

    /// Create a fresh address space with an independent root page table.
    pub fn new_bare() -> Self {
        Self {
            page_table: PageTableWrapper::alloc_new(),
            address_space_id: allocate_address_space_id(),
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
            area.append_resident(frame).map_err(|_| SysErrNo::EFAULT)?;
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
        let _ = area.replace_resident(frames.to_vec());
        if has_leaf_permission(permission) {
            let mf = effective_mapping_flags(permission);
            for (idx, frame) in area.resident().iter_range(0, area.page_count()) {
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

    /// Build a user address space seeded with the shared kernel mappings.
    pub fn from_kernel() -> Self {
        Self::new_bare()
    }

    pub fn activate(&self) {
        #[cfg(feature = "buildstorm-diagnostics")]
        let _diag = crate::buildstorm_diagnostics::WorkScope::new(
            crate::buildstorm_diagnostics::WorkClass::AddressSpaceActivation,
        );
        crate::perf_counters::note_user_page_table_activation();
        // Callers hold the shared MemorySet lock while activating. Publishing
        // the root before changing the hardware page table makes a concurrent
        // page-table editor observe this CPU only after the new root is live;
        // an editor that finished earlier leaves a generation handled here.
        crate::platform::mark_current_address_space(self.address_space_root());
        #[cfg(target_arch = "riscv64")]
        let already_active = PageTable::current().root() == self.page_table.root()
            && PageTable::current_asid() == self.address_space_id;
        #[cfg(not(target_arch = "riscv64"))]
        let already_active = false;
        if !already_active {
            #[cfg(feature = "buildstorm-diagnostics")]
            crate::buildstorm_diagnostics::note_root_activation(true, false);
            self.page_table.change_with_asid(self.address_space_id);
        } else {
            #[cfg(feature = "buildstorm-diagnostics")]
            crate::buildstorm_diagnostics::note_root_activation(true, true);
        }
        // LoongArch page-table edits currently occur under kernel ASID 0 and
        // need a conservative local invalidation before re-entering user mode.
        // RISC-V can retain its ASID-tagged entries across the transition.
        // RISC-V keeps the shared kernel root permanently on ASID 0. User page
        // table edits do not modify that root, and explicit kernel mapping
        // changes already invalidate their affected entries, so restoring the
        // same root needs no sfence.vma. LoongArch still requires the proven
        // conservative full invalidation on every user-root activation.
        #[cfg(target_arch = "loongarch64")]
        {
            #[cfg(feature = "buildstorm-diagnostics")]
            crate::buildstorm_diagnostics::note_local_tlb_flush();
            TLB::flush_all();
        }
    }

    pub fn address_space_id(&self) -> usize {
        self.address_space_id
    }

    pub fn address_space_root(&self) -> usize {
        self.page_table.root().raw()
    }

    pub fn satp_token(&self) -> usize {
        let root_ppn = self.page_table.root().raw() >> 12;
        (8usize << 60) | (self.address_space_id << 44) | root_ppn
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

    /// Fixed-size read-only snapshot for a one-shot diagnostic failure record.
    #[cfg(feature = "buildstorm-diagnostics")]
    pub(crate) fn diagnostic_user_page_snapshot(
        &self,
        addr: usize,
    ) -> (usize, usize, usize, usize, usize, usize, usize) {
        let pte_pa = self
            .translate(VirtAddr::new(addr))
            .map(|pa| pa.raw())
            .unwrap_or(0);
        self.areas
            .iter()
            .find(|area| area.contains(VirtAddr::new(addr)))
            .map(|area| {
                let (backing, resident, page_state) = area.diagnostic_page_snapshot(addr);
                (
                    area.start_va.raw(),
                    area.end_va.raw(),
                    area.flags.bits() as usize,
                    backing,
                    resident,
                    page_state,
                    pte_pa,
                )
            })
            .unwrap_or((0, 0, 0, 0, 0, 0, pte_pa))
    }

    pub fn is_mapped(&self, vaddr: VirtAddr) -> bool {
        self.translate(vaddr).is_some()
    }

    #[cfg(feature = "smp-regression")]
    pub(crate) fn regression_mapping_is_writable(&self, vaddr: VirtAddr) -> bool {
        self.page_table
            .translate(vaddr)
            .map(|(_, flags)| flags.contains(MappingFlags::W))
            .unwrap_or(false)
    }

    pub fn is_shared_mapping_at(&self, addr: usize) -> bool {
        self.area_index_containing(addr)
            .map(|index| is_shared_mapping(&self.areas[index]))
            .unwrap_or(false)
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

        let mut index = self.first_area_ending_after(start);
        let mut already_protected = true;
        while index < self.areas.len() && self.areas[index].start_va.raw() < end {
            if self.areas[index].flags.difference(PTEFlags::COW).bits() != flags.bits() {
                already_protected = false;
                break;
            }
            index += 1;
        }
        if already_protected {
            return Ok(());
        }

        self.split_area_at(start);
        self.split_area_at(end);

        let mut changed_mappings = false;
        let mut index = self.first_area_ending_after(start);
        while index < self.areas.len() && self.areas[index].start_va.raw() < end {
            let area = &mut self.areas[index];
            let old_flags = area.flags;
            if old_flags.difference(PTEFlags::COW).bits() == flags.bits() {
                index += 1;
                continue;
            }
            let private_file_backed =
                matches!(area.backing, MapAreaBacking::File { shared: false, .. });
            let anonymous = matches!(area.backing, MapAreaBacking::Anonymous);
            let mut new_flags = flags;
            if (anonymous || private_file_backed)
                && flags.contains(PTEFlags::W)
                && area.has_frames()
                && (area.flags.contains(PTEFlags::COW)
                    || area.resident().iter().any(|frame| frame.ref_count() > 1))
            {
                // Linux does not eagerly duplicate a whole private mapping
                // merely because mprotect made it writable.  Keep shared
                // clean/COW frames read-only and resolve at the first
                // store fault; otherwise one mprotect over a large rustc
                // mapping can monopolize the kernel for seconds/minutes.
                new_flags |= PTEFlags::COW;
            }
            area.flags = new_flags;
            if area.has_frames()
                && (has_leaf_permission(old_flags) || has_leaf_permission(new_flags))
            {
                if has_leaf_permission(new_flags) {
                    // map_page overwrites the existing leaf and performs
                    // the required local invalidation.
                    map_area_pages(&self.page_table, area);
                } else {
                    unmap_area_pages_without_shootdown(&self.page_table, area, old_flags);
                }
                changed_mappings = true;
            }
            index += 1;
        }
        if changed_mappings {
            crate::platform::tlb_shootdown(self.page_table.root().raw());
        }
        self.coalesce_changed_range(start, end);
        Ok(())
    }

    fn resolve_cow_page(&mut self, page_start: usize) -> Result<usize, SysErrNo> {
        let Some(source_index) = self.area_index_containing(page_start) else {
            return Err(SysErrNo::EFAULT);
        };
        let page_idx = (page_start - self.areas[source_index].start_va.raw()) / PAGE_SIZE;
        let (flags, backing, resident_page) = {
            let area = &self.areas[source_index];
            (
                area.flags,
                area.backing.clone(),
                area.resident().lookup(page_idx).cloned(),
            )
        };
        if !matches!(
            backing,
            MapAreaBacking::Anonymous | MapAreaBacking::File { shared: false, .. }
        ) || !flags.contains(PTEFlags::COW)
            || !flags.contains(PTEFlags::W)
        {
            return Err(SysErrNo::EFAULT);
        }
        let mut replacement = None;
        if let Some(source_page) = resident_page {
            if source_page.ref_count() > 1 {
                let Some(frame) = frame_allocator::alloc_frame() else {
                    return Err(SysErrNo::ENOMEM);
                };
                unsafe {
                    core::ptr::copy_nonoverlapping(
                        source_page.ppn().addr() as *const u8,
                        frame.ppn().addr() as *mut u8,
                        PAGE_SIZE,
                    );
                }
                replacement = Some(frame);
            }
        } else {
            let Some(frame) = frame_allocator::alloc_frame() else {
                return Err(SysErrNo::ENOMEM);
            };
            let backing_offset = page_start.saturating_sub(self.areas[source_index].start_va.raw());
            let data = read_backing_page(&backing, backing_offset)?;
            unsafe {
                core::ptr::copy_nonoverlapping(
                    data.as_ptr(),
                    frame.ppn().addr() as *mut u8,
                    PAGE_SIZE,
                );
            }
            self.areas[source_index]
                .insert_resident_page(page_idx, frame, PageState::Private)
                .map_err(|_| SysErrNo::EFAULT)?;
        }

        let old_frame = if let Some(frame) = replacement {
            Some(
                self.areas[source_index]
                    .replace_resident_page(page_idx, frame)
                    .map_err(|_| SysErrNo::EFAULT)?,
            )
        } else {
            None
        };
        self.areas[source_index].set_resident_state(page_idx, PageState::Private);
        let vaddr = VirtAddr::new(page_start);
        self.page_table.unmap_page(vaddr);
        crate::platform::tlb_shootdown(self.page_table.root().raw());
        map_area_page(&self.page_table, &self.areas[source_index], page_idx);
        crate::platform::tlb_shootdown(self.page_table.root().raw());
        drop(old_frame);
        Ok(1)
    }

    fn ensure_page_writable(&mut self, page_start: usize) -> Result<(), SysErrNo> {
        let Some(index) = self.area_index_containing(page_start) else {
            #[cfg(feature = "buildstorm-diagnostics")]
            crate::buildstorm_diagnostics::note_first_page_fault_failure(
                crate::buildstorm_diagnostics::PAGE_FAULT_FAILURE_PREPARE_WRITE_NO_VMA,
                SysErrNo::EFAULT as usize,
                page_start,
            );
            return Err(SysErrNo::EFAULT);
        };
        let flags = self.areas[index].flags;
        if !has_leaf_permission(flags) || !flags.contains(PTEFlags::W) {
            #[cfg(feature = "buildstorm-diagnostics")]
            crate::buildstorm_diagnostics::note_first_page_fault_failure(
                crate::buildstorm_diagnostics::PAGE_FAULT_FAILURE_PREPARE_WRITE_PERM,
                SysErrNo::EFAULT as usize,
                page_start,
            );
            return Err(SysErrNo::EFAULT);
        }
        if flags.contains(PTEFlags::COW) {
            self.resolve_cow_page(page_start)?;
            return Ok(());
        }
        if self.translate(VirtAddr::new(page_start)).is_none() {
            #[cfg(feature = "buildstorm-diagnostics")]
            let _fault_source = crate::buildstorm_diagnostics::PageFaultSourceScope::new(
                crate::buildstorm_diagnostics::PageFaultSource::PrepareWrite,
            );
            self.handle_page_fault(page_start, true, false)?;
        }
        Ok(())
    }

    fn ensure_page_readable(&mut self, page_start: usize) -> Result<(), SysErrNo> {
        let Some(index) = self.area_index_containing(page_start) else {
            #[cfg(feature = "buildstorm-diagnostics")]
            crate::buildstorm_diagnostics::note_first_page_fault_failure(
                crate::buildstorm_diagnostics::PAGE_FAULT_FAILURE_PREPARE_READ_NO_VMA,
                SysErrNo::EFAULT as usize,
                page_start,
            );
            return Err(SysErrNo::EFAULT);
        };
        let flags = self.areas[index].flags;
        if !has_leaf_permission(flags) || !flags.contains(PTEFlags::R) {
            #[cfg(feature = "buildstorm-diagnostics")]
            crate::buildstorm_diagnostics::note_first_page_fault_failure(
                crate::buildstorm_diagnostics::PAGE_FAULT_FAILURE_PREPARE_READ_PERM,
                SysErrNo::EFAULT as usize,
                page_start,
            );
            return Err(SysErrNo::EFAULT);
        }
        if self.translate(VirtAddr::new(page_start)).is_none() {
            #[cfg(feature = "buildstorm-diagnostics")]
            let _fault_source = crate::buildstorm_diagnostics::PageFaultSourceScope::new(
                crate::buildstorm_diagnostics::PageFaultSource::PrepareRead,
            );
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
            let frame = area.resident().lookup(page_idx).ok_or(SysErrNo::EFAULT)?;
            let shared_file = matches!(area.backing, MapAreaBacking::File { shared: true, .. });
            let dst_ptr = (frame.ppn().addr() + page_off) as *mut u8;
            unsafe {
                core::ptr::copy_nonoverlapping(src[copied..].as_ptr(), dst_ptr, copy_len);
            }
            if shared_file {
                self.areas[index].set_resident_state(page_idx, PageState::FileDirty);
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
        #[cfg(feature = "buildstorm-diagnostics")]
        let _diag = crate::buildstorm_diagnostics::WorkScope::new(if is_exec {
            crate::buildstorm_diagnostics::WorkClass::PageFaultExec
        } else if is_store {
            crate::buildstorm_diagnostics::WorkClass::PageFaultStore
        } else {
            crate::buildstorm_diagnostics::WorkClass::PageFaultLoad
        });
        #[cfg(feature = "buildstorm-diagnostics")]
        let fault_resolution = crate::buildstorm_diagnostics::PageFaultResolutionScope::new();
        crate::perf_counters::note_page_fault();
        let page_start = align_down(fault_addr);
        let Some(area_index) = self.area_index_containing(fault_addr) else {
            #[cfg(feature = "buildstorm-diagnostics")]
            crate::buildstorm_diagnostics::note_first_page_fault_failure(
                crate::buildstorm_diagnostics::PAGE_FAULT_FAILURE_HANDLER_NO_VMA,
                SysErrNo::EFAULT as usize,
                page_start,
            );
            return Err(SysErrNo::EFAULT);
        };
        let area = &self.areas[area_index];

        if !has_leaf_permission(area.flags) {
            #[cfg(feature = "buildstorm-diagnostics")]
            crate::buildstorm_diagnostics::note_first_page_fault_failure(
                crate::buildstorm_diagnostics::PAGE_FAULT_FAILURE_HANDLER_NO_LEAF,
                SysErrNo::EFAULT as usize,
                page_start,
            );
            return Err(SysErrNo::EFAULT);
        }
        if is_exec {
            if !area.flags.contains(PTEFlags::X) {
                #[cfg(feature = "buildstorm-diagnostics")]
                crate::buildstorm_diagnostics::note_first_page_fault_failure(
                    crate::buildstorm_diagnostics::PAGE_FAULT_FAILURE_HANDLER_EXEC_PERM,
                    SysErrNo::EFAULT as usize,
                    page_start,
                );
                return Err(SysErrNo::EFAULT);
            }
        } else if is_store {
            if !area.flags.contains(PTEFlags::W) {
                #[cfg(feature = "buildstorm-diagnostics")]
                crate::buildstorm_diagnostics::note_first_page_fault_failure(
                    crate::buildstorm_diagnostics::PAGE_FAULT_FAILURE_HANDLER_STORE_PERM,
                    SysErrNo::EFAULT as usize,
                    page_start,
                );
                return Err(SysErrNo::EFAULT);
            }
        } else if !area.flags.contains(PTEFlags::R) {
            #[cfg(feature = "buildstorm-diagnostics")]
            crate::buildstorm_diagnostics::note_first_page_fault_failure(
                crate::buildstorm_diagnostics::PAGE_FAULT_FAILURE_HANDLER_READ_PERM,
                SysErrNo::EFAULT as usize,
                page_start,
            );
            return Err(SysErrNo::EFAULT);
        }
        if is_store && area.flags.contains(PTEFlags::COW) {
            let _resolved_pages = self.resolve_cow_page(page_start).map_err(|error| {
                #[cfg(feature = "buildstorm-diagnostics")]
                crate::buildstorm_diagnostics::note_first_page_fault_failure(
                    crate::buildstorm_diagnostics::PAGE_FAULT_FAILURE_HANDLER_COW,
                    error as usize,
                    page_start,
                );
                error
            })?;
            #[cfg(feature = "buildstorm-diagnostics")]
            fault_resolution.finish(
                crate::buildstorm_diagnostics::PageFaultResolution::Cow,
                _resolved_pages,
            );
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
            #[cfg(feature = "buildstorm-diagnostics")]
            fault_resolution.finish(
                crate::buildstorm_diagnostics::PageFaultResolution::ExistingMapping,
                0,
            );
            return Ok(());
        }
        let page_idx = (page_start - area.start_va.raw()) / PAGE_SIZE;
        if area.resident().lookup(page_idx).is_some() {
            map_area_page(&self.page_table, area, page_idx);
            #[cfg(feature = "buildstorm-diagnostics")]
            fault_resolution.finish(
                crate::buildstorm_diagnostics::PageFaultResolution::FileResident,
                1,
            );
            return Ok(());
        }

        if matches!(area.backing, MapAreaBacking::Anonymous)
            && !area.flags.contains(PTEFlags::COW)
            && !area.flags.contains(PTEFlags::X)
        {
            #[cfg(feature = "buildstorm-diagnostics")]
            let anonymous_allocate = crate::buildstorm_diagnostics::WorkScope::new(
                crate::buildstorm_diagnostics::WorkClass::PageFaultAnonymousAllocate,
            );
            let pages_to_end = (area.end_va.raw() - page_start) / PAGE_SIZE;
            let max_window_pages = pages_to_end.min(ANONYMOUS_FAULT_WINDOW_PAGES);
            let mut window_pages = 0usize;
            while window_pages < max_window_pages
                && area.resident().lookup(page_idx + window_pages).is_none()
            {
                window_pages += 1;
            }
            let mut frames = Vec::new();
            for page in 0..window_pages {
                match frame_allocator::alloc_frame() {
                    Some(frame) => frames.push(frame),
                    None if page == 0 => return Err(SysErrNo::ENOMEM),
                    None => break,
                }
            }
            #[cfg(feature = "buildstorm-diagnostics")]
            drop(anonymous_allocate);
            #[cfg(feature = "buildstorm-diagnostics")]
            let _anonymous_install = crate::buildstorm_diagnostics::WorkScope::new(
                crate::buildstorm_diagnostics::WorkClass::PageFaultAnonymousInstall,
            );
            #[cfg(feature = "buildstorm-diagnostics")]
            {
                crate::buildstorm_diagnostics::note_anonymous_vma_install(
                    false,
                    false,
                    self.areas.len(),
                    frames.len(),
                );
            }

            if frames.is_empty() {
                return Err(SysErrNo::EFAULT);
            }
            let _mapped_pages = frames.len();
            let idx = self
                .area_index_containing(page_start)
                .ok_or(SysErrNo::EFAULT)?;
            self.areas[idx]
                .insert_resident_run(page_idx, frames, PageState::Private)
                .map_err(|_| SysErrNo::EFAULT)?;
            #[cfg(feature = "buildstorm-diagnostics")]
            let anonymous_map = crate::buildstorm_diagnostics::WorkScope::new(
                crate::buildstorm_diagnostics::WorkClass::PageFaultAnonymousMap,
            );
            map_area_page_window(&self.page_table, &self.areas[idx], page_idx, _mapped_pages);
            #[cfg(feature = "buildstorm-diagnostics")]
            drop(anonymous_map);
            #[cfg(feature = "buildstorm-diagnostics")]
            fault_resolution.finish(
                crate::buildstorm_diagnostics::PageFaultResolution::AnonymousDemand,
                _mapped_pages,
            );
            return Ok(());
        }

        if let Some((ino, file_offset)) = clean_cache_page {
            crate::perf_counters::note_clean_file_fault();
            let pages_to_end = (area.end_va.raw() - page_start) / PAGE_SIZE;
            let max_window_pages = pages_to_end.min(CLEAN_FILE_FAULT_WINDOW_PAGES);
            let mut window_pages = 0usize;
            while window_pages < max_window_pages
                && area.resident().lookup(page_idx + window_pages).is_none()
            {
                window_pages += 1;
            }
            if window_pages == 0 {
                return Err(SysErrNo::EFAULT);
            }
            let cache_frames = match crate::fs::ext4_vol::clean_page_cache_frames(
                ino,
                file_offset,
                window_pages,
                CLEAN_FILE_FAULT_READ_AHEAD_PAGES,
            ) {
                Ok(frames) => frames,
                Err(error) => {
                    #[cfg(feature = "buildstorm-diagnostics")]
                    crate::buildstorm_diagnostics::note_first_page_fault_failure(
                        crate::buildstorm_diagnostics::PAGE_FAULT_FAILURE_CLEAN_CACHE,
                        error as usize,
                        page_start,
                    );
                    return Err(error);
                }
            };
            if cache_frames.is_empty() {
                #[cfg(feature = "buildstorm-diagnostics")]
                crate::buildstorm_diagnostics::note_first_page_fault_failure(
                    crate::buildstorm_diagnostics::PAGE_FAULT_FAILURE_CLEAN_EMPTY,
                    SysErrNo::ENOMEM as usize,
                    page_start,
                );
                return Err(SysErrNo::ENOMEM);
            }
            let Some(idx) = self.area_index_containing(page_start) else {
                #[cfg(feature = "buildstorm-diagnostics")]
                crate::buildstorm_diagnostics::note_first_page_fault_failure(
                    crate::buildstorm_diagnostics::PAGE_FAULT_FAILURE_CLEAN_SPLIT_LOOKUP,
                    SysErrNo::EFAULT as usize,
                    page_start,
                );
                return Err(SysErrNo::EFAULT);
            };
            let _mapped_pages = cache_frames.len();
            self.areas[idx]
                .insert_resident_run(page_idx, cache_frames, PageState::FileClean)
                .map_err(|_| SysErrNo::EFAULT)?;
            map_area_page_window(&self.page_table, &self.areas[idx], page_idx, _mapped_pages);
            #[cfg(feature = "buildstorm-diagnostics")]
            fault_resolution.finish(
                crate::buildstorm_diagnostics::PageFaultResolution::CleanFileCache,
                _mapped_pages,
            );
            return Ok(());
        }

        let Some(idx) = self.area_index_containing(page_start) else {
            #[cfg(feature = "buildstorm-diagnostics")]
            crate::buildstorm_diagnostics::note_first_page_fault_failure(
                crate::buildstorm_diagnostics::PAGE_FAULT_FAILURE_FALLBACK_SPLIT_LOOKUP,
                SysErrNo::EFAULT as usize,
                page_start,
            );
            return Err(SysErrNo::EFAULT);
        };
        if self.areas[idx].resident().lookup(page_idx).is_some() {
            map_area_page(&self.page_table, &self.areas[idx], page_idx);
            #[cfg(feature = "buildstorm-diagnostics")]
            fault_resolution.finish(
                crate::buildstorm_diagnostics::PageFaultResolution::ExistingFrame,
                1,
            );
            return Ok(());
        }

        let Some(frame) = frame_allocator::alloc_frame() else {
            #[cfg(feature = "buildstorm-diagnostics")]
            crate::buildstorm_diagnostics::note_first_page_fault_failure(
                crate::buildstorm_diagnostics::PAGE_FAULT_FAILURE_FALLBACK_ALLOC,
                SysErrNo::ENOMEM as usize,
                page_start,
            );
            return Err(SysErrNo::ENOMEM);
        };
        let backing_offset = page_start.saturating_sub(self.areas[idx].start_va.raw());
        let data = match read_backing_page(&self.areas[idx].backing, backing_offset) {
            Ok(data) => data,
            Err(error) => {
                #[cfg(feature = "buildstorm-diagnostics")]
                crate::buildstorm_diagnostics::note_first_page_fault_failure(
                    crate::buildstorm_diagnostics::PAGE_FAULT_FAILURE_BACKING_READ,
                    error as usize,
                    page_start,
                );
                return Err(error);
            }
        };
        unsafe {
            core::ptr::copy_nonoverlapping(data.as_ptr(), frame.ppn().addr() as *mut u8, PAGE_SIZE);
        }
        let state = match self.areas[idx].backing {
            MapAreaBacking::File { shared: true, .. } | MapAreaBacking::SharedMemory { .. } => {
                PageState::Shared
            }
            MapAreaBacking::File { shared: false, .. } => PageState::FileClean,
            MapAreaBacking::Anonymous => PageState::Private,
        };
        self.areas[idx]
            .insert_resident_page(page_idx, frame, state)
            .map_err(|_| {
                #[cfg(feature = "buildstorm-diagnostics")]
                crate::buildstorm_diagnostics::note_first_page_fault_failure(
                    crate::buildstorm_diagnostics::PAGE_FAULT_FAILURE_RESIDENT_INSERT,
                    SysErrNo::EFAULT as usize,
                    page_start,
                );
                SysErrNo::EFAULT
            })?;
        map_area_page(&self.page_table, &self.areas[idx], page_idx);
        #[cfg(feature = "buildstorm-diagnostics")]
        fault_resolution.finish(
            crate::buildstorm_diagnostics::PageFaultResolution::BackingRead,
            1,
        );
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
        #[cfg(feature = "buildstorm-diagnostics")]
        crate::buildstorm_diagnostics::note_vma_count(self.areas.len());
    }

    #[cfg(feature = "buildstorm-diagnostics")]
    fn coalesce_areas_anonymous_diagnostic(&mut self) {
        let total_started = crate::timer::get_time_us();
        let sort_started = total_started;
        self.sort_areas();
        let after_sort = crate::timer::get_time_us();
        let old_areas = mem::take(&mut self.areas);
        let area_count = old_areas.len();
        let mut merged: Vec<MapArea> = Vec::new();
        let mut merges = 0usize;
        let mut merged_frames = 0usize;
        let mut frame_reallocs = 0usize;
        let mut frame_relocate = 0usize;
        for area in old_areas {
            if let Some(last) = merged.last_mut() {
                if last.can_merge_with(&area) {
                    let incoming_frames = area.resident().len();
                    if last
                        .resident()
                        .capacity()
                        .saturating_sub(last.resident().len())
                        < incoming_frames
                    {
                        frame_reallocs = frame_reallocs.saturating_add(1);
                        frame_relocate = frame_relocate.saturating_add(last.resident().len());
                    }
                    merges = merges.saturating_add(1);
                    merged_frames = merged_frames.saturating_add(incoming_frames);
                    last.merge_with(area);
                    continue;
                }
            }
            merged.push(area);
        }
        self.areas = merged;
        crate::buildstorm_diagnostics::note_vma_count(self.areas.len());
        let finished = crate::timer::get_time_us();
        crate::buildstorm_diagnostics::note_anonymous_coalesce_detail(
            area_count,
            after_sort.saturating_sub(sort_started),
            finished.saturating_sub(after_sort),
            finished.saturating_sub(total_started),
            merges,
            merged_frames,
            frame_reallocs,
            frame_relocate,
        );
    }

    /// Coalesce only the part of the already-sorted VMA vector whose flags may
    /// have changed.  The global variant remains for operations that can add,
    /// remove, or reorder arbitrary areas.
    fn coalesce_changed_range(&mut self, start: usize, end: usize) {
        if self.areas.len() < 2 {
            return;
        }
        let mut index = self.first_area_ending_after(start).saturating_sub(1);
        while index + 1 < self.areas.len() {
            if self.areas[index].start_va.raw() >= end {
                break;
            }
            if self.areas[index].can_merge_with(&self.areas[index + 1]) {
                let next = self.areas.remove(index + 1);
                self.areas[index].merge_with(next);
            } else {
                index += 1;
            }
        }
        #[cfg(feature = "buildstorm-diagnostics")]
        crate::buildstorm_diagnostics::note_vma_count(self.areas.len());
    }

    pub fn fork_cow(&mut self) -> Result<Self, SysErrNo> {
        let mut new_ms = Self::from_kernel();

        for area in &mut self.areas {
            let mut new_area =
                MapArea::with_backing(area.start_va, area.end_va, area.flags, area.backing.clone());

            if is_shared_mapping(area) {
                area.set_all_resident_state(PageState::Shared);
                new_area.clone_resident_from(area);
                map_area_pages(&new_ms.page_table, &new_area);
                new_ms.areas.push(new_area);
                continue;
            }

            if is_private_cow_candidate(area) {
                let old_flags = area.flags;
                // A prior COW fault can leave this VMA's policy marked COW
                // while individual Private pages are mapped writable again.
                // A later fork transitions those pages back to Cow, so the
                // parent leaf must be republished read-only even when the VMA
                // flags themselves do not change.
                let needs_parent_remap = !old_flags.contains(PTEFlags::COW)
                    || area
                        .resident()
                        .iter()
                        .any(|page| page.state() != PageState::Cow);
                area.set_all_resident_state(PageState::Cow);
                area.flags |= PTEFlags::COW;
                if needs_parent_remap {
                    remap_area_pages(&self.page_table, area, old_flags);
                }
                new_area.flags = area.flags;
                new_area.clone_resident_from(area);
                map_area_pages(&new_ms.page_table, &new_area);
                new_ms.areas.push(new_area);
                continue;
            }

            if is_readonly_share_candidate(area) {
                area.set_all_resident_state(PageState::Shared);
                new_area.clone_resident_from(area);
                map_area_pages(&new_ms.page_table, &new_area);
                new_ms.areas.push(new_area);
                continue;
            }

            for (idx, src_frame) in area.resident().iter_range(0, area.page_count()) {
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
                    let mf = effective_mapping_flags_for_page(area.flags, src_frame);
                    new_ms
                        .page_table
                        .map_page(vaddr, new_paddr, mf, MappingSize::Page4KB);
                }
                new_area
                    .insert_resident_page(idx, frame, PageState::Private)
                    .map_err(|_| SysErrNo::EFAULT)?;
            }

            new_ms.areas.push(new_area);
        }
        new_ms.coalesce_areas();
        Ok(new_ms)
    }
}

impl Drop for MemorySet {
    fn drop(&mut self) {
        retire_address_space_id(self.address_space_id);
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
                new_area.set_all_resident_state(PageState::Shared);
                new_area.clone_resident_from(area);
                map_area_pages(&new_ms.page_table, &new_area);
                new_ms.areas.push(new_area);
                continue;
            }

            for (idx, src_frame) in area.resident().iter_range(0, area.page_count()) {
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
                        let mf = effective_mapping_flags_for_page(new_flags, src_frame);
                        new_ms
                            .page_table
                            .map_page(vaddr, new_paddr, mf, MappingSize::Page4KB);
                    }
                    let _ = new_area.insert_resident_page(idx, frame, PageState::Private);
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

struct AsidAllocator {
    max_asid: usize,
    next_asid: usize,
    reusable: Vec<usize>,
    retired: Vec<usize>,
}

impl AsidAllocator {
    fn new() -> Self {
        let hardware_max_asid = PageTable::max_asid();
        #[cfg(feature = "smp-regression")]
        let hardware_max_asid = hardware_max_asid.min(8);
        Self {
            max_asid: hardware_max_asid,
            next_asid: 1,
            reusable: Vec::new(),
            retired: Vec::new(),
        }
    }

    fn allocate_without_recycling(&mut self) -> Option<usize> {
        if let Some(asid) = self.reusable.pop() {
            return Some(asid);
        }
        if self.next_asid <= self.max_asid {
            let asid = self.next_asid;
            self.next_asid += 1;
            return Some(asid);
        }
        None
    }
}

lazy_static! {
    static ref ASID_ALLOCATOR: Mutex<AsidAllocator> = Mutex::new(AsidAllocator::new());
    pub static ref KERNEL_SPACE: Mutex<MemorySet> = Mutex::new(MemorySet::new_bare());
}

fn allocate_address_space_id() -> usize {
    let retired = {
        let mut allocator = ASID_ALLOCATOR.lock();
        if let Some(asid) = allocator.allocate_without_recycling() {
            return asid;
        }
        mem::take(&mut allocator.retired)
    };

    if retired.is_empty() {
        // ASID 0 is shared with the kernel and therefore requires a full
        // local flush on every activation. This preserves correctness if the
        // implementation exposes no ASID bits or all live IDs are occupied.
        return 0;
    }

    crate::platform::flush_tlb_all_cpus();
    let mut allocator = ASID_ALLOCATOR.lock();
    allocator.reusable.extend(retired);
    allocator.allocate_without_recycling().unwrap_or(0)
}

fn retire_address_space_id(asid: usize) {
    if asid != 0 {
        ASID_ALLOCATOR.lock().retired.push(asid);
    }
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
