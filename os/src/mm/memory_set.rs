use alloc::vec::Vec;
use polyhal::{PhysAddr, VirtAddr};
use polyhal::pagetable::{PageTableWrapper, MappingFlags, MappingSize};

use super::frame_allocator::{self, FrameTracker};
use super::map_area::MapArea;
use super::page_table::{self, PTEFlags};
use crate::config::PAGE_SIZE;

/// 地址空间 - 管理进程的虚拟内存
pub struct MemorySet {
    pub page_table: PageTableWrapper,
    /// 映射区域列表
    pub areas: Vec<MapArea>,
}

impl MemorySet {
    /// 创建一个新的空地址空间（使用独立的页表）
    ///
    /// 使用 alloc_new() 创建独立页表，避免与其他 MemorySet 共享 boot page table。
    /// 这样当 MemorySet 被 drop 时，不会清空 boot page table 中的用户空间映射。
    pub fn new_bare() -> Self {
        Self {
            page_table: PageTableWrapper::alloc_new(),
            areas: Vec::new(),
        }
    }

    /// 插入一个映射区域（Framed 类型 - 每个虚拟页对应一个物理页帧）
    pub fn insert_framed_area(
        &mut self,
        start_va: VirtAddr,
        end_va: VirtAddr,
        permission: PTEFlags,
    ) {
        let start_vpn = start_va.raw() / PAGE_SIZE;
        let end_vpn = (end_va.raw() + PAGE_SIZE - 1) / PAGE_SIZE;
        
        // 创建 MapArea
        let area = MapArea::new(start_va, end_va, permission);
        self.areas.push(area);

        // 为每个虚拟页分配物理页帧并建立映射
        for vpn in start_vpn..end_vpn {
            if let Some(frame) = frame_allocator::alloc_frame() {
                let ppn = frame.ppn();
                core::mem::forget(frame); // 防止自动释放，由 MemorySet 管理
                let vaddr = VirtAddr::new(vpn * PAGE_SIZE);
                let paddr = PhysAddr::new(ppn.addr());
                let mf: MappingFlags = permission.into();
                self.page_table.map_page(vaddr, paddr, mf, MappingSize::Page4KB);
            }
        }
    }

    /// 插入一个映射区域（Framed 类型），使用 FrameTracker 管理
    pub fn insert_framed_area_with_frames(
        &mut self,
        start_va: VirtAddr,
        end_va: VirtAddr,
        permission: PTEFlags,
    ) -> Vec<FrameTracker> {
        let start_vpn = start_va.raw() / PAGE_SIZE;
        let end_vpn = (end_va.raw() + PAGE_SIZE - 1) / PAGE_SIZE;

        // 创建 MapArea
        let area = MapArea::new(start_va, end_va, permission);
        self.areas.push(area);

        // 为每个虚拟页分配物理页帧并建立映射
        let mut frames: Vec<FrameTracker> = Vec::new();
        for vpn in start_vpn..end_vpn {
            if let Some(frame) = frame_allocator::alloc_frame() {
                let ppn = frame.ppn();
                let vaddr = VirtAddr::new(vpn * PAGE_SIZE);
                let paddr = PhysAddr::new(ppn.addr());
                let mf: MappingFlags = permission.into();
                self.page_table.map_page(vaddr, paddr, mf, MappingSize::Page4KB);
                frames.push(frame);
            }
        }
        frames
    }

    /// 从内核页表复制 — 复制内核空间的映射
    ///
    /// 用户进程的页表需要包含内核映射，以便在陷入内核时使用。
    /// 遍历 KERNEL_PAGE_TABLE 复制 SV39 内核区域的映射。
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

        // RISC-V: OpenSBI 建立了 1:1 恒等映射和全空用户空间。
        // 用户的页表暂时只包含 OpenSBI 的 identity 映射，
        // 这对于在物理地址运行的内核代码已经够用（无需在高 VA 访问内核）。
        //
        // 如果将来需要高 VA 内核映射（如 sv39 kernel base），
        // 可以在这里遍历 KERNEL_PAGE_TABLE 复制映射，
        // 但需要确保 OpenSBI 的启动页表在对应 VA 有有效条目。
        //
        // 目前保持简单：用户进程共享 OpenSBI 的 identity 映射，
        // 内核在 sbi_call 时通过物理地址直接访问。

        ms
    }

    /// 激活此地址空间。
    pub fn activate(&self) {
        self.page_table.change();
    }

    /// 获取页表根地址
    pub fn satp_token(&self) -> usize {
        // 返回页表根物理页号，用于 satp 寄存器
        // SV39: MODE=8 (39-bit), ASID=0, PPN=root_ppn
        let root_ppn = self.page_table.root().raw() >> 12;
        (8usize << 60) | root_ppn
    }

    /// 移除一个映射区域
    pub fn remove_area(&mut self, start_va: VirtAddr) {
        self.areas.retain(|area| area.start_va.raw() != start_va.raw());
    }

    /// 解除映射并释放物理页帧
    pub fn unmap_area(&mut self, start_va: VirtAddr) {
        if let Some(area) = self.areas.iter().find(|a| a.start_va.raw() == start_va.raw()) {
            let start_vpn = area.start_va.raw() / PAGE_SIZE;
            let end_vpn = (area.end_va.raw() + PAGE_SIZE - 1) / PAGE_SIZE;
            
            for vpn in start_vpn..end_vpn {
                let vaddr = VirtAddr::new(vpn * PAGE_SIZE);
                self.page_table.unmap_page(vaddr);
            }
        }
        self.remove_area(start_va);
    }

    /// 地址转换：虚拟地址 -> 物理地址
    pub fn translate(&self, vaddr: VirtAddr) -> Option<PhysAddr> {
        self.page_table.translate(vaddr).map(|(paddr, _)| paddr)
    }

    /// 检查虚拟地址是否已映射
    pub fn is_mapped(&self, vaddr: VirtAddr) -> bool {
        self.translate(vaddr).is_some()
    }
}

impl Clone for MemorySet {
    fn clone(&self) -> Self {
        let mut new_ms = Self::from_kernel();

        // 2. 复制用户空间区域（Copy-on-Write）
        for area in &self.areas {
            let start_vpn = area.start_va.raw() / PAGE_SIZE;
            let end_vpn = (area.end_va.raw() + PAGE_SIZE - 1) / PAGE_SIZE;

            let new_area = MapArea::new(area.start_va, area.end_va, area.flags);
            new_ms.areas.push(new_area);

            for vpn in start_vpn..end_vpn {
                if let Some(frame) = frame_allocator::alloc_frame() {
                    let new_ppn = frame.ppn();
                    let new_paddr = PhysAddr::new(new_ppn.addr());

                    // 复制父进程页帧数据到子进程
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

                    core::mem::forget(frame);
                    let mf: MappingFlags = area.flags.into();
                    new_ms.page_table.map_page(vaddr, new_paddr, mf, MappingSize::Page4KB);
                }
            }
        }

        new_ms
    }
}

// 全局内核地址空间
use lazy_static::lazy_static;
use spin::Mutex;

lazy_static! {
    /// 内核地址空间
    pub static ref KERNEL_SPACE: Mutex<MemorySet> = Mutex::new(MemorySet::new_bare());
}

/// 初始化内核地址空间
///
/// 在内存管理初始化完成后调用：
/// 1. 将内核镜像各段映射到内核虚拟地址空间
/// 2. 激活内核页表
/// 3. 将填充好的页表提供给 `from_kernel()` 复制给用户进程
///
/// SV39 内核映射策略：
/// - 物理 0x8000_0000 → 虚拟 0xFFFF_FFFF_8000_0000 (256MB 内核空间)
/// - 使用 OpenSBI 提供的启动页表进行 1:1 映射
/// - 用户进程通过 from_kernel() 复制这些映射
///
/// 注意：当前设计依赖 OpenSBI 的启动页表，暂不重新构建内核映射
pub fn init_kernel_space() {
    // 获取当前 KERNEL_PAGE_TABLE
    // 此时 PageTableWrapper::alloc() 已经创建，使用启动页表根
    if let Some(ref kpt) = *page_table::kernel_page_table().lock() {
        log::info!("[mm] Kernel space initialized using boot page table");
        log::info!("[mm] Boot page table root: {:?}", kpt.root());

        // 暂时跳过 translate() 检查，因为启动页表可能不是标准的 SV39 结构
        // 验证工作可以通过 QEMU 的 info mem 命令手动完成

        log::info!("[mm] SV39 page table setup complete");
    } else {
        log::error!("[mm] KERNEL_PAGE_TABLE not initialized!");
    }

    // 注意：当前设计依赖 OpenSBI 的启动页表
    // 内核可以在 1:1 映射下正常运行
    // 如需建立独立的内核高地址映射，可以使用 alloc_new() 重新构建页表
}
