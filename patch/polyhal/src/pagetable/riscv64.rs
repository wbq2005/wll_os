use core::arch::riscv64::sfence_vma;

use bitflags::bitflags;
use riscv::register::satp::{self, Mode};

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
                res |= PTEFlags::X | PTEFlags::A;
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
    /// Root entries 0 and 1 are user space in this kernel
    /// (0x0000_0000..0x8000_0000). Kernel identity mappings start at
    /// 0x8000_0000, so fresh process page tables must not inherit low entries
    /// from the currently running user page table.
    pub(crate) const USER_ROOT_PTE_END: usize = 2;
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
        arr.fill(PTE(0));
        if current.raw() == 0 {
            arr[2] = PTE::new_page(
                PhysAddr::new(0x8000_0000),
                PTEFlags::V | PTEFlags::R | PTEFlags::W | PTEFlags::X | PTEFlags::G | PTEFlags::A | PTEFlags::D,
            );
            return;
        }

        // 获取当前（启动）页表的 PTE 列表
        let kernel_arr = Self::get_pte_list(current);

        // Copy only kernel/global root entries. Copying user entries from the
        // active task aliases its lower-level page tables; when the old address
        // space is dropped, the new one would keep dangling mappings.
        for i in Self::USER_ROOT_PTE_END..Self::PTE_NUM_IN_PAGE {
            arr[i] = kernel_arr[i];
        }
    }

    #[inline]
    pub fn change(&self) {
        self.change_with_asid(0);
        TLB::flush_all();
    }

    /// Activate this root under an address-space identifier.
    ///
    /// The caller owns ASID allocation and must invalidate stale entries
    /// before reusing an ASID for another root.
    #[inline]
    pub fn change_with_asid(&self, asid: usize) {
        if self.0.raw() == 0 {
            unsafe { satp::set(Mode::Bare, 0, 0) }
        } else {
            unsafe { satp::set(Mode::Sv39, asid, self.0.raw() >> 12) }
        }
    }

    #[inline]
    pub fn current_asid() -> usize {
        satp::read().asid()
    }

    /// Probe the implemented Sv39 ASID width through the WARL satp field.
    pub fn max_asid() -> usize {
        let original = satp::read();
        if original.mode() == Mode::Bare {
            return 0;
        }
        unsafe { satp::set(original.mode(), u16::MAX as usize, original.ppn()) }
        let max_asid = satp::read().asid();
        unsafe { satp::write(original) }
        TLB::flush_all();
        max_asid
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

    /// Flush translations tagged with one ASID while retaining other roots.
    ///
    /// RISC-V defines `sfence.vma x0, asid` as an ASID-scoped invalidation;
    /// this is sufficient when returning to the shared kernel root (ASID 0)
    /// and avoids discarding user-ASID translations on every trap return.
    #[inline]
    pub fn flush_asid(asid: usize) {
        unsafe { sfence_vma(0, asid) }
    }
}
