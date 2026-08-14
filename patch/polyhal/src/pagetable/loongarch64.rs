use loongArch64::register::{asid, pgdl};

use super::{MappingFlags, PageTable, PTE, TLB};
use crate::{PhysAddr, VirtAddr};

impl PTE {
    #[inline]
    pub const fn is_valid(&self) -> bool {
        self.0 != 0
    }

    #[inline]
    pub const fn flags(&self) -> PTEFlags {
        PTEFlags::from_bits_truncate(self.0)
    }

    #[inline]
    pub fn address(&self) -> PhysAddr {
        PhysAddr::new((self.0) & 0xffff_ffff_f000)
    }

    #[inline]
    pub fn is_table(&self) -> bool {
        // Directory entries are plain page-table physical addresses. Leaf
        // mappings carry LoongArch's present bit, so do not recurse into them.
        self.0 != 0 && !self.flags().contains(PTEFlags::P)
    }

    #[inline]
    pub(crate) fn new_table(paddr: PhysAddr) -> Self {
        Self(paddr.raw())
    }

    #[inline]
    pub(crate) fn new_page(paddr: PhysAddr, flags: PTEFlags) -> Self {
        Self(paddr.raw() | flags.bits())
    }
}

impl From<MappingFlags> for PTEFlags {
    fn from(value: MappingFlags) -> Self {
        let mut flags = PTEFlags::V | PTEFlags::P;
        if value.contains(MappingFlags::W) {
            flags |= PTEFlags::W | PTEFlags::D;
        }

        // if !value.contains(MappingFlags::X) {
        //     flags |= PTEFlags::NX;
        // }

        if value.contains(MappingFlags::U) {
            flags |= PTEFlags::PLV_USER;
        }
        flags
    }
}

impl From<PTEFlags> for MappingFlags {
    fn from(val: PTEFlags) -> Self {
        let mut flags = MappingFlags::empty();
        if val.contains(PTEFlags::V) && val.contains(PTEFlags::P) {
            flags |= MappingFlags::P;
        }
        if val.contains(PTEFlags::W) {
            flags |= MappingFlags::W;
        }

        if val.contains(PTEFlags::D) {
            flags |= MappingFlags::D;
        }

        // if !self.contains(PTEFlags::NX) {
        //     flags |= MappingFlags::X;
        // }

        if val.contains(PTEFlags::PLV_USER) {
            flags |= MappingFlags::U;
        }
        flags
    }
}

bitflags::bitflags! {
    /// Possible flags for a page table entry.
    pub struct PTEFlags: usize {
        /// Page Valid
        const V = bit!(0);
        /// Dirty, The page has been writed.
        const D = bit!(1);

        const PLV_USER = 0b11 << 2;

        const MAT_NOCACHE = 0b01 << 4;

        /// Designates a global mapping OR Whether the page is huge page.
        const GH = bit!(6);

        /// Page is existing.
        const P = bit!(7);
        /// Page is writeable.
        const W = bit!(8);
        /// Is a Global Page if using huge page(GH bit).
        const G = bit!(10);
        /// Page is not readable.
        const NR = bit!(11);
        /// Page is not executable.
        /// FIXME: Is it just for a huge page?
        /// Linux related url: https://github.com/torvalds/linux/blob/master/arch/loongarch/include/asm/pgtable-bits.h
        const NX = bit!(12);
        /// Whether the privilege Level is restricted. When RPLV is 0, the PTE
        /// can be accessed by any program with privilege Level highter than PLV.
        const RPLV = bit!(63);
    }
}

impl PageTable {
    /// The size of the page for this platform.
    pub const PAGE_SIZE: usize = 0x1000;
    pub const PAGE_LEVEL: usize = 3;
    pub const PTE_NUM_IN_PAGE: usize = 0x200;
    pub(crate) const GLOBAL_ROOT_PTE_RANGE: usize = 0x100;
    pub(crate) const VADDR_BITS: usize = 39;
    pub(crate) const USER_VADDR_END: usize = (1 << Self::VADDR_BITS) - 1;
    pub(crate) const KERNEL_VADDR_START: usize = !Self::USER_VADDR_END;

    #[inline]
    pub(crate) const fn is_user_root_entry(index: usize) -> bool {
        index < Self::GLOBAL_ROOT_PTE_RANGE
    }

    #[inline]
    pub fn restore(&self) {
        let current = Self::current().0;
        let arr = Self::get_pte_list(self.0);
        arr.fill(PTE(0));
        if current.raw() == 0 {
            TLB::flush_all();
            return;
        }
        let current_arr = Self::get_pte_list(current);
        for i in 0..Self::PTE_NUM_IN_PAGE {
            if !Self::is_user_root_entry(i) {
                arr[i] = current_arr[i];
            }
        }
        TLB::flush_all();
    }

    #[inline]
    pub fn current() -> Self {
        Self(PhysAddr::new(pgdl::read().base()))
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
    pub fn change_with_asid(&self, address_space_id: usize) {
        pgdl::set_base(self.0.raw());
        if Self::current_asid() != address_space_id {
            asid::set_asid(address_space_id);
        }
        unsafe { core::arch::asm!("dbar 0") }
    }

    #[inline]
    pub fn current_asid() -> usize {
        asid::read().asid()
    }

    pub fn max_asid() -> usize {
        let width = asid::read().asid_width().min(10);
        if width == 0 {
            0
        } else {
            (1usize << width) - 1
        }
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
            // Page-table edits happen while the kernel uses ASID 0, but the
            // changed leaf can belong to a nonzero user ASID. Operation 0x06
            // invalidates this virtual address for both global and non-global
            // entries regardless of the current ASID.
            core::arch::asm!("dbar 0; invtlb 0x06, $r0, {reg}", reg = in(reg) vaddr.raw());
        }
    }

    /// flush all tlb entry
    ///
    /// how to use ?
    /// just
    /// TLB::flush_all();
    #[inline]
    pub fn flush_all() {
        unsafe {
            core::arch::asm!("dbar 0; invtlb 0x00, $r0, $r0");
        }
    }
}

pub fn boot_page_table() -> PageTable {
    // FIXME: This should return a valid page table.
    // ref solution: create a blank page table in boot stage.
    PageTable(PhysAddr::new(0))
}
