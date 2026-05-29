use core::arch::riscv64::sfence_vma;

use bitflags::bitflags;
use riscv::register::satp::{self, Satp};

use super::{MappingFlags, PageTable, PTE, TLB};
use crate::{PhysAddr, VirtAddr};

impl PTE {
    #[inline]
    pub const fn from_ppn(ppn: usize, flags: PTEFlags) -> Self {
        // let flags = flags.union(PTEFlags::D);
        let mut flags = flags;
        if flags.contains(PTEFlags::R) | flags.contains(PTEFlags::X) {
            flags = flags.union(PTEFlags::A)
        }
        if flags.contains(PTEFlags::W) {
            flags = flags.union(PTEFlags::D)
        }
        // TIPS: This is prepare for the extend bits of T-HEAD C906
        #[cfg(cpu_family = "c906")]
        if flags.contains(PTEFlags::G) && ppn == 0x8_0000 {
            Self(
                ppn << 10
                    | flags
                        .union(PTEFlags::C)
                        .union(PTEFlags::B)
                        .union(PTEFlags::K)
                        .bits() as usize,
            )
        } else if flags.contains(PTEFlags::G) && ppn == 0 {
            Self(ppn << 10 | flags.union(PTEFlags::SE).union(PTEFlags::SO).bits() as usize)
        } else {
            Self(ppn << 10 | flags.union(PTEFlags::C).bits() as usize)
        }

        #[cfg(not(cpu_family = "c906"))]
        Self(ppn << 10 | flags.bits() as usize)
    }

    #[inline]
    pub const fn from_addr(addr: usize, flags: PTEFlags) -> Self {
        Self::from_ppn(addr >> 12, flags)
    }

    #[inline]
    pub const fn flags(&self) -> PTEFlags {
        PTEFlags::from_bits_truncate((self.0 & 0xff) as u64)
    }

    #[inline]
    pub const fn is_valid(&self) -> bool {
        self.flags().contains(PTEFlags::V) && self.0 > u8::MAX as usize
    }

    #[inline]
    pub(crate) fn is_table(&self) -> bool {
        return self.flags().contains(PTEFlags::V)
            && !(self.flags().contains(PTEFlags::R)
                || self.flags().contains(PTEFlags::W)
                || self.flags().contains(PTEFlags::X));
    }

    #[inline]
    pub(crate) fn new_table(paddr: PhysAddr) -> Self {
        // SV39: PPN 存储在 bits[53:10]，所以需要右移 2 位（PA >> 12 得到 PPN，再左移 10 位放回）
        // paddr.raw() >> 12 得到物理页号 PPN
        // | V (Valid) = 1
        Self((paddr.raw() >> 2) | (PTEFlags::V).bits() as usize)
    }

    #[inline]
    pub(crate) fn new_page(paddr: PhysAddr, flags: PTEFlags) -> Self {
        // SV39: PPN 存储在 bits[53:10]
        // paddr.raw() >> 12 得到物理页号 PPN
        Self((paddr.raw() >> 2) | flags.bits() as usize)
    }

    #[inline]
    pub(crate) fn address(&self) -> PhysAddr {
        // SV39 PTE 格式: PPN[55:10] | Reserved[9:8] | Flags[7:0]
        // PPN 占 44 bits (bits 53:10)，存储时左移 10 位
        // 恢复时: 先右移 10 位得到 PPN，再左移 12 位得到物理地址
        let ppn = (self.0 >> 10) & 0x003F_FFFF_FFFF_FFFF;
        PhysAddr::new(ppn << 12)
    }
}

bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
    pub struct PTEFlags: u64 {
        const V = bit!(0);
        const R = bit!(1);
        const W = bit!(2);
        const X = bit!(3);
        const U = bit!(4);
        const G = bit!(5);
        const A = bit!(6);
        const D = bit!(7);

        #[cfg(cpu_family = "c906")]
        const SO = bit!(63);
        #[cfg(cpu_family = "c906")]
        const C = bit!(62);
        #[cfg(cpu_family = "c906")]
        const B = bit!(61);
        #[cfg(cpu_family = "c906")]
        const K = bit!(60);
        #[cfg(cpu_family = "c906")]
        const SE = bit!(59);

        const VRWX  = Self::V.bits() | Self::R.bits() | Self::W.bits() | Self::X.bits();
        const ADUVRX = Self::A.bits() | Self::D.bits() | Self::U.bits() | Self::V.bits() | Self::R.bits() | Self::X.bits();
        const ADVRWX = Self::A.bits() | Self::D.bits() | Self::VRWX.bits();
        const ADGVRWX = Self::G.bits() | Self::ADVRWX.bits();
    }
}

impl From<MappingFlags> for PTEFlags {
    fn from(flags: MappingFlags) -> Self {
        if flags.is_empty() {
            Self::empty()
        } else {
            let mut res = Self::V;
            if flags.contains(MappingFlags::R) {
                res |= PTEFlags::R | PTEFlags::A;
            }
            if flags.contains(MappingFlags::W) {
                res |= PTEFlags::W | PTEFlags::D;
            }
            if flags.contains(MappingFlags::X) {
                res |= PTEFlags::X;
            }
            if flags.contains(MappingFlags::U) {
                res |= PTEFlags::U;
            }
            res
        }
    }
}

impl From<PTEFlags> for MappingFlags {
    fn from(value: PTEFlags) -> Self {
        let mut mapping_flags = MappingFlags::empty();
        if value.contains(PTEFlags::V) {
            mapping_flags |= MappingFlags::P;
        }
        if value.contains(PTEFlags::R) {
            mapping_flags |= MappingFlags::R;
        }
        if value.contains(PTEFlags::W) {
            mapping_flags |= MappingFlags::W;
        }
        if value.contains(PTEFlags::X) {
            mapping_flags |= MappingFlags::X;
        }
        if value.contains(PTEFlags::U) {
            mapping_flags |= MappingFlags::U;
        }
        if value.contains(PTEFlags::A) {
            mapping_flags |= MappingFlags::A;
        }
        if value.contains(PTEFlags::D) {
            mapping_flags |= MappingFlags::D;
        }

        mapping_flags
    }
}

impl PageTable {
    /// The size of the page for this platform.
    pub const PAGE_SIZE: usize = 0x1000;
    pub const PAGE_LEVEL: usize = 3;
    pub const PTE_NUM_IN_PAGE: usize = 0x200;
    /// 内核映射区域：VPN[2] = 0x100 ~ 0x1FF (0x8000_0000 ~ 0xBFFF_FFFF)
    /// 这个范围覆盖内核低区（OpenSBI 1:1 映射）和链接内核高区
    pub(crate) const GLOBAL_ROOT_PTE_RANGE: usize = 0x200;
    pub(crate) const VADDR_BITS: usize = 39;
    pub(crate) const USER_VADDR_END: usize = (1 << Self::VADDR_BITS) - 1;
    pub(crate) const KERNEL_VADDR_START: usize = !Self::USER_VADDR_END;

    #[inline]
    pub fn current() -> Self {
        Self(PhysAddr::new(satp::read().ppn() << 12))
    }

    #[inline]
    pub fn kernel_pte_entry(&self) -> PhysAddr {
        self.0
    }

    #[inline]
    pub fn restore(&self) {
        let current = Self::current().0;
        let arr = Self::get_pte_list(self.0);
        if current.raw() == 0 {
            arr.fill(PTE(0));
            return;
        }

        // 获取当前（启动）页表的 PTE 列表
        let kernel_arr = Self::get_pte_list(current);

        // SV39 页表结构分析:
        // - 根页表索引 VPN[2] 范围 0-511
        // - VPN[2] = 0x000 ~ 0x07F: 低地址空间 (0x0000_0000 ~ 0x3FFF_FFFF)
        // - VPN[2] = 0x080 ~ 0x0FF: I/O 区域 (0x4000_0000 ~ 0x7FFF_FFFF)
        // - VPN[2] = 0x100 ~ 0x1FF: 内核低区 (0x8000_0000 ~ 0xBFFF_FFFF, OpenSBI 1:1 映射区)
        // - VPN[2] = 0x200 ~ 0x2FF: 内核高区 (0xC000_0000 ~ 0xFFFF_FFFF, 链接内核使用)
        // - VPN[2] = 0x300 ~ 0x3FF: 未使用
        //
        // 复制全局内核映射区域到新页表
        // GLOBAL_ROOT_PTE_RANGE = 0x100，覆盖 OpenSBI 区和链接内核区

        for i in 0..Self::GLOBAL_ROOT_PTE_RANGE {
            arr[i] = kernel_arr[i];
        }

        // 清零用户空间区域（索引 0x100 之后，保留给内核）
        // 注意：这里的清零是安全的，因为我们只复制了 0x100 个条目
        // 索引 0x100 ~ 0x1FF 已被内核映射覆盖
        for i in Self::GLOBAL_ROOT_PTE_RANGE..Self::PTE_NUM_IN_PAGE {
            arr[i] = PTE(0);
        }
    }

    #[inline]
    pub fn change(&self) {
        let satp_val = (8usize << 60) | (self.0.raw() >> 12);
        unsafe { satp::write(Satp::from_bits(satp_val)) }
        TLB::flush_all();
    }
}

/// TLB operations
impl TLB {
    /// flush the TLB entry by VirtualAddress
    /// just use it directly
    ///
    /// TLB::flush_vaddr(arg0); // arg0 is the virtual address(VirtAddr)
    #[inline]
    pub fn flush_vaddr(vaddr: VirtAddr) {
        unsafe {
            sfence_vma(vaddr.raw(), 0);
        }
    }

    /// flush all tlb entry
    ///
    /// how to use ?
    /// just
    /// TLB::flush_all();
    #[inline]
    pub fn flush_all() {
        riscv::asm::sfence_vma_all();
    }
}
