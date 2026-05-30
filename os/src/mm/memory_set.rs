use alloc::vec::Vec;
use polyhal::pagetable::{MappingFlags, MappingSize, PageTableWrapper};
use polyhal::{PhysAddr, VirtAddr};

use super::frame_allocator::{self, FrameTracker};
use super::map_area::MapArea;
use super::page_table::{self, PTEFlags};
use crate::config::PAGE_SIZE;

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
    ) {
        let start_vpn = start_va.raw() / PAGE_SIZE;
        let end_vpn = (end_va.raw() + PAGE_SIZE - 1) / PAGE_SIZE;
        let mut area = MapArea::new(start_va, end_va, permission);

        for vpn in start_vpn..end_vpn {
            if let Some(frame) = frame_allocator::alloc_frame() {
                let ppn = frame.ppn();
                let vaddr = VirtAddr::new(vpn * PAGE_SIZE);
                let paddr = PhysAddr::new(ppn.addr());
                let mf: MappingFlags = permission.into();
                self.page_table
                    .map_page(vaddr, paddr, mf, MappingSize::Page4KB);
                area.frames.push(frame);
            }
        }

        self.areas.push(area);
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
        let start_vpn = start_va.raw() / PAGE_SIZE;
        let end_vpn = (end_va.raw() + PAGE_SIZE - 1) / PAGE_SIZE;

        let area = MapArea::new(start_va, end_va, permission);
        self.areas.push(area);

        let mut frames: Vec<FrameTracker> = Vec::new();
        for vpn in start_vpn..end_vpn {
            if let Some(frame) = frame_allocator::alloc_frame() {
                let ppn = frame.ppn();
                let vaddr = VirtAddr::new(vpn * PAGE_SIZE);
                let paddr = PhysAddr::new(ppn.addr());
                let mf: MappingFlags = permission.into();
                self.page_table
                    .map_page(vaddr, paddr, mf, MappingSize::Page4KB);
                frames.push(frame);
            }
        }
        frames
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
        if let Some(area) = self
            .areas
            .iter()
            .find(|a| a.start_va.raw() == start_va.raw())
        {
            let start_vpn = area.start_va.raw() / PAGE_SIZE;
            let end_vpn = (area.end_va.raw() + PAGE_SIZE - 1) / PAGE_SIZE;

            for vpn in start_vpn..end_vpn {
                let vaddr = VirtAddr::new(vpn * PAGE_SIZE);
                self.page_table.unmap_page(vaddr);
            }
        }
        self.remove_area(start_va);
    }

    pub fn translate(&self, vaddr: VirtAddr) -> Option<PhysAddr> {
        self.page_table.translate(vaddr).map(|(paddr, _)| paddr)
    }

    pub fn is_mapped(&self, vaddr: VirtAddr) -> bool {
        self.translate(vaddr).is_some()
    }
}

impl Clone for MemorySet {
    fn clone(&self) -> Self {
        let mut new_ms = Self::from_kernel();

        for area in &self.areas {
            let start_vpn = area.start_va.raw() / PAGE_SIZE;
            let end_vpn = (area.end_va.raw() + PAGE_SIZE - 1) / PAGE_SIZE;
            let mut new_area = MapArea::new(area.start_va, area.end_va, area.flags);

            for vpn in start_vpn..end_vpn {
                if let Some(frame) = frame_allocator::alloc_frame() {
                    let new_ppn = frame.ppn();
                    let new_paddr = PhysAddr::new(new_ppn.addr());
                    let vaddr = VirtAddr::new(vpn * PAGE_SIZE);

                    if let Some(src_paddr) = self.translate(vaddr) {
                        unsafe {
                            core::ptr::copy_nonoverlapping(
                                src_paddr.raw() as *const u8,
                                new_paddr.raw() as *mut u8,
                                PAGE_SIZE,
                            );
                        }
                    } else {
                        new_paddr.clear_len(PAGE_SIZE);
                    }

                    let mf: MappingFlags = area.flags.into();
                    new_ms
                        .page_table
                        .map_page(vaddr, new_paddr, mf, MappingSize::Page4KB);
                    new_area.frames.push(frame);
                }
            }

            new_ms.areas.push(new_area);
        }

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
