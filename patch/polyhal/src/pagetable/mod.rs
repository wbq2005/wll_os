cfg_if::cfg_if! {
    if #[cfg(target_arch = "loongarch64")] {
        mod loongarch64;
        pub use loongarch64::*;
    } else if #[cfg(target_arch = "aarch64")] {
        mod aarch64;
        pub use aarch64::*;
    } else if #[cfg(target_arch = "riscv64")] {
        mod riscv64;
        pub use riscv64::*;

    } else if #[cfg(target_arch = "x86_64")] {
        mod x86_64;
        pub use x86_64::*;
    } else {
        compile_error!("unsupported architecture!");
    }
}

use core::ops::Deref;

use crate::{components::common::frame_alloc, PhysAddr, VirtAddr};

use super::common::frame_dealloc;

/// The size of the page table.
pub const PAGE_SIZE: usize = PageTable::PAGE_SIZE;

/// Page table entry structure
///
/// Just define here. Should implement functions in specific architectures.
#[derive(Copy, Clone, Debug)]
pub struct PTE(pub usize);

impl PTE {
    pub const fn empty() -> Self {
        Self(0)
    }
}

/// Page Table
///
/// This is just the page table defination.
/// The implementation of the page table in the specific architecture mod.
/// Such as:
/// x86_64/page_table.rs
/// riscv64/page_table/sv39.rs
/// aarch64/page_table.rs
/// loongarch64/page_table.rs
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct PageTable(PhysAddr);

impl PageTable {
    /// Get the root Physical Page
    pub const fn root(&self) -> PhysAddr {
        self.0
    }
    /// Get the page table list through the physical address
    #[inline]
    pub(crate) fn get_pte_list(paddr: PhysAddr) -> &'static mut [PTE] {
        paddr.slice_mut_with_len::<PTE>(Self::PTE_NUM_IN_PAGE)
    }

    fn leaf_table_for_mapping(&self, vaddr: VirtAddr) -> PhysAddr {
        let mut table = self.0;
        if Self::PAGE_LEVEL == 4 {
            let pte = &mut Self::get_pte_list(table)[vaddr.pn_index(3)];
            if !pte.is_valid() {
                *pte = PTE::new_table(frame_alloc());
            }
            table = pte.address();
        }
        for level in (1..=2).rev() {
            let pte = &mut Self::get_pte_list(table)[vaddr.pn_index(level)];
            if !pte.is_valid() {
                *pte = PTE::new_table(frame_alloc());
            }
            table = pte.address();
        }
        table
    }

    fn leaf_table_for_unmapping(&self, vaddr: VirtAddr) -> Option<PhysAddr> {
        let mut table = self.0;
        if Self::PAGE_LEVEL == 4 {
            let pte = Self::get_pte_list(table)[vaddr.pn_index(3)];
            if !pte.is_table() {
                return None;
            }
            table = pte.address();
        }
        for level in (1..=2).rev() {
            let pte = Self::get_pte_list(table)[vaddr.pn_index(level)];
            if !pte.is_table() {
                return None;
            }
            table = pte.address();
        }
        Some(table)
    }

    /// Publish 4 KiB leaves without issuing a TLB invalidation.
    ///
    /// Adjacent virtual pages reuse their leaf-table walk. The caller must
    /// publish the writes with the architecture's ordering and TLB protocol
    /// before an active address space can observe them.
    pub fn map_pages_without_flush<I>(&self, pages: I) -> usize
    where
        I: IntoIterator<Item = (VirtAddr, PhysAddr, MappingFlags)>,
    {
        let mut leaf_tag = usize::MAX;
        let mut leaf_table = PhysAddr::new(0);
        let mut mapped = 0usize;
        for (vaddr, paddr, flags) in pages {
            let current_tag = vaddr.raw() >> 21;
            if current_tag != leaf_tag {
                leaf_table = self.leaf_table_for_mapping(vaddr);
                leaf_tag = current_tag;
            }
            Self::get_pte_list(leaf_table)[vaddr.pn_index(0)] =
                PTE::new_page(paddr, flags.into());
            mapped += 1;
        }
        mapped
    }

    /// Revoke 4 KiB leaves without issuing a TLB invalidation.
    ///
    /// The caller must retain every affected data-frame owner until it has
    /// completed the architecture's local and remote TLB retirement barrier.
    pub fn unmap_pages_without_flush<I>(&self, pages: I) -> usize
    where
        I: IntoIterator<Item = VirtAddr>,
    {
        let mut leaf_tag = usize::MAX;
        let mut leaf_table = None;
        let mut unmapped = 0usize;
        for vaddr in pages {
            let current_tag = vaddr.raw() >> 21;
            if current_tag != leaf_tag {
                leaf_table = self.leaf_table_for_unmapping(vaddr);
                leaf_tag = current_tag;
            }
            let Some(table) = leaf_table else {
                continue;
            };
            let pte = &mut Self::get_pte_list(table)[vaddr.pn_index(0)];
            if pte.is_valid() {
                *pte = PTE::empty();
                unmapped += 1;
            }
        }
        unmapped
    }

    /// Mapping a page to specific virtual page (user space address).
    ///
    /// Ensure that PageTable is which you want to map.
    /// vpn: Virtual page will be mapped.
    /// ppn: Physical page.
    /// flags: Mapping flags, include Read, Write, Execute and so on.
    /// size: MappingSize. Just support 4KB page currently.
    pub fn map_page(
        &self,
        vaddr: VirtAddr,
        paddr: PhysAddr,
        flags: MappingFlags,
        _size: MappingSize,
    ) {
        self.map_pages_without_flush(core::iter::once((vaddr, paddr, flags)));
        TLB::flush_vaddr(vaddr);
    }

    /// Mapping a page to specific address(kernel space address).
    ///
    /// TODO: This method is not implemented.
    /// TIPS: If we mapped to kernel, the page will be shared between different pagetable.
    ///
    /// Ensure that PageTable is which you want to map.
    /// vpn: Virtual page will be mapped.
    /// ppn: Physical page.
    /// flags: Mapping flags, include Read, Write, Execute and so on.
    /// size: MappingSize. Just support 4KB page currently.    
    ///
    /// How to implement shared.
    pub fn map_kernel(
        &self,
        vaddr: VirtAddr,
        paddr: PhysAddr,
        flags: MappingFlags,
        _size: MappingSize,
    ) {
        let mut pte_list = Self::get_pte_list(self.0);
        if Self::PAGE_LEVEL == 4 {
            let pte = &mut pte_list[vaddr.pn_index(3)];
            if !pte.is_valid() {
                *pte = PTE::new_table(frame_alloc());
            }
            pte_list = Self::get_pte_list(pte.address());
        }
        // level 3
        {
            let pte = &mut pte_list[vaddr.pn_index(2)];
            if !pte.is_valid() {
                *pte = PTE::new_table(frame_alloc());
            }
            pte_list = Self::get_pte_list(pte.address());
        }
        // level 2
        {
            let pte = &mut pte_list[vaddr.pn_index(1)];
            if !pte.is_valid() {
                *pte = PTE::new_table(frame_alloc());
            }
            pte_list = Self::get_pte_list(pte.address());
        }
        // level 1, map page
        pte_list[vaddr.pn_index(0)] = PTE::new_page(paddr, flags.into());
        TLB::flush_vaddr(vaddr);
    }

    /// Unmap a page from specific virtual page (user space address).
    ///
    /// Ensure the virtual page is exists.
    /// vpn: Virtual address.
    pub fn unmap_page(&self, vaddr: VirtAddr) {
        let mut pte_list = Self::get_pte_list(self.0);
        if Self::PAGE_LEVEL == 4 {
            let pte = &mut pte_list[vaddr.pn_index(3)];
            if !pte.is_table() {
                return;
            };
            pte_list = Self::get_pte_list(pte.address());
        }
        // level 3
        {
            let pte = &mut pte_list[vaddr.pn_index(2)];
            if !pte.is_table() {
                return;
            };
            pte_list = Self::get_pte_list(pte.address());
        }
        // level 2
        {
            let pte = &mut pte_list[vaddr.pn_index(1)];
            if !pte.is_table() {
                return;
            };
            pte_list = Self::get_pte_list(pte.address());
        }
        // level 1, map page
        pte_list[vaddr.pn_index(0)] = PTE(0);
        TLB::flush_vaddr(vaddr);
    }

    /// Translate a virtual adress to a physical address and mapping flags.
    ///
    /// Return None if the vaddr isn't mapped.
    /// vpn: The virtual address will be translated.
    ///
    /// SV39 页表结构 (3级页表 + 最终页):
    /// - PGD: 根页表 (VPN[2])
    /// - PMD: 中间页表 (VPN[1])
    /// - PTE: 页表 (VPN[0])
    /// - 最终页: 4KB 物理页
    pub fn translate(&self, vaddr: VirtAddr) -> Option<(PhysAddr, MappingFlags)> {
        let mut pte_list = Self::get_pte_list(self.0);

        // SV39 只有 3 级页表，不需要处理 PAGE_LEVEL == 4 的情况

        // Level 3: 遍历 VPN[2] 索引 (根页表 -> PMD)
        let vpn2_idx = vaddr.pn_index(2);
        if vpn2_idx >= Self::PTE_NUM_IN_PAGE {
            return None;
        }
        let pte = pte_list[vpn2_idx];
        if !pte.is_table() {
            // 如果不是中间页表，检查是否直接是有效页面（不应该发生在 VPN[2] 级别）
            return None;
        }
        pte_list = Self::get_pte_list(pte.address());

        // Level 2: 遍历 VPN[1] 索引 (PMD -> PTE)
        let vpn1_idx = vaddr.pn_index(1);
        if vpn1_idx >= Self::PTE_NUM_IN_PAGE {
            return None;
        }
        let pte = pte_list[vpn1_idx];
        if !pte.is_table() {
            return None;
        }
        pte_list = Self::get_pte_list(pte.address());

        // Level 1: 遍历 VPN[0] 索引 (PTE -> 最终页)
        let vpn0_idx = vaddr.pn_index(0);
        if vpn0_idx >= Self::PTE_NUM_IN_PAGE {
            return None;
        }
        let pte = pte_list[vpn0_idx];
        if !pte.is_valid() {
            return None;
        }

        // 计算物理地址：PPN * PAGE_SIZE + offset
        let paddr = pte.address();
        let offset = vaddr.raw() & 0xFFF;  // 页面内偏移
        Some((
            PhysAddr::new(paddr.raw() + offset),
            pte.flags().into(),
        ))
    }

    /// Release the page table entry.
    ///
    /// The page table entry in the user space address will be released.
    /// Only releases user-space mappings, keeps kernel mappings intact.
    ///
    /// Release every architecture-owned user branch while preserving shared
    /// kernel/global branches. User root entries need not be contiguous.
    pub fn release(&self) {
        // Skip if frame allocator is not initialized yet
        if self.0.raw() == 0 {
            return;
        }

        // Helper: recursively release sub-page-tables
        let release_sub_pt = |pte_list: &[PTE]| {
            for pte in pte_list {
                if pte.is_table() {
                    let sub_list = Self::get_pte_list(pte.address());
                    // 递归释放下一级
                    for sub_pte in sub_list {
                        if sub_pte.is_table() {
                            let sub_sub_list = Self::get_pte_list(sub_pte.address());
                            for sub_sub_pte in sub_sub_list {
                                if sub_sub_pte.is_table() {
                                    // L1 PTE - 这是实际的页表
                                    frame_dealloc(sub_sub_pte.address());
                                } else if sub_sub_pte.is_valid() && !sub_sub_pte.flags().contains(PTEFlags::G) {
                                    // 实际页面，可能需要处理（取决于实现）
                                }
                            }
                            // 释放 L1 页表页
                            frame_dealloc(sub_pte.address());
                        }
                    }
                    // 释放 L0 页表页
                    frame_dealloc(pte.address());
                }
            }
        };

        let pte_list = Self::get_pte_list(self.0);

        for index in 0..PageTable::PTE_NUM_IN_PAGE {
            if !PageTable::is_user_root_entry(index) {
                continue;
            }
            release_sub_pt(&pte_list[index..index + 1]);
            pte_list[index] = PTE(0);
        }
    }
}

bitflags::bitflags! {
    /// Mapping flags for page table.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
    pub struct MappingFlags: u64 {
        /// Persent
        const P = bit!(0);
        /// User Accessable Flag
        const U = bit!(1);
        /// Readable Flag
        const R = bit!(2);
        /// Writeable Flag
        const W = bit!(3);
        /// Executeable Flag
        const X = bit!(4);
        /// Accessed Flag
        const A = bit!(5);
        /// Dirty Flag, indicating that the page was written
        const D = bit!(6);
        /// Global Flag
        const G = bit!(7);
        /// Device Flag, indicating that the page was used for device memory
        const Device = bit!(8);
        /// Cache Flag, indicating that the page will be cached
        const Cache = bit!(9);

        /// Read | Write | Executeable Flags
        const RWX = Self::R.bits() | Self::W.bits() | Self::X.bits();
        /// User | Read | Write Flags
        const URW = Self::U.bits() | Self::R.bits() | Self::W.bits();
        /// User | Read | Executeable Flags
        const URX = Self::U.bits() | Self::R.bits() | Self::X.bits();
        /// User | Read | Write | Executeable Flags
        const URWX = Self::URW.bits() | Self::X.bits();
    }
}

/// This structure indicates size of the page that will be mapped.
///
/// TODO: Support More Page Size, 16KB or 32KB
/// Just support 4KB right now.
#[derive(Debug)]
pub enum MappingSize {
    Page4KB,
    // Page2MB,
    // Page1GB,
}

/// TLB Operation set.
/// Such as flush_vaddr, flush_all.
/// Just use it in the fn.
///
/// there are some methods in the TLB implementation
///
/// ### Flush the tlb entry through the specific virtual address
///
/// ```rust
/// TLB::flush_vaddr(arg0);  arg0 should be VirtAddr
/// ```
/// ### Flush all tlb entries
/// ```rust
/// TLB::flush_all();
/// ```
pub struct TLB;

/// Page Table Wrapper
///
/// You can use this wrapper to packing PageTable.
/// If you release the PageTableWrapper,
/// the PageTable will release its page table entry.
#[derive(Debug)]
pub struct PageTableWrapper(pub PageTable);

impl Deref for PageTableWrapper {
    type Target = PageTable;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

/// Allocate a new PageTableWrapper with new page table root
///
/// This operation will copy kernel page table space from booting page table.
impl PageTableWrapper {
    /// Alloc a new PageTableWrapper sharing the boot page table root
    ///
    /// This reuses the OpenSBI-established page table for compatibility.
    /// All processes will share the same underlying page table until
    /// proper process isolation is implemented.
    #[inline]
    pub fn alloc() -> Self {
        // 使用启动页表的根地址，保持与 OpenSBI 设置的 1:1 映射兼容
        let boot_pt_root = PageTable::current();
        Self(boot_pt_root)
    }

    /// Allocate a NEW independent page table with kernel mappings copied
    ///
    /// This creates a process-specific page table that:
    /// 1. Allocates a new page table root frame
    /// 2. Copies kernel mappings from the boot page table
    /// 3. Leaves user space unmapped (page fault on access)
    pub fn alloc_new() -> Self {
        // 分配新的页表根物理页
        let root_paddr = frame_alloc();
        let pt = PageTable(root_paddr);

        // 清零新页表根
        PageTable::get_pte_list(root_paddr).fill(PTE(0));

        // 调用 restore() 从启动页表复制内核映射
        pt.restore();

        Self(pt)
    }
}

/// Page Table Release.
///
/// You must implement this trait to release page table.
/// Include the page table entry and root page.
///
/// NOTE: This will NOT release the boot page table (satp current root)
/// to prevent kernel crashes.
impl Drop for PageTableWrapper {
    fn drop(&mut self) {
        // 检查是否是启动页表（satp 当前指向的页表）
        let boot_root = PageTable::current();
        if self.0.root().raw() == boot_root.root().raw() {
            // 这是启动页表，只清零条目但保留根
            self.0.release();
            // 不释放根页帧
        } else {
            // 普通页表，完整释放
            self.0.release();
            frame_dealloc(self.0.0);
        }
    }
}

impl VirtAddr {
    /// Get n level page table index of the given virtual address
    ///
    /// SV39 三级页表索引:
    /// - VPN[2] (level 2): bits[38:30] → 页目录索引
    /// - VPN[1] (level 1): bits[29:21] → 页中间目录索引
    /// - VPN[0] (level 0): bits[20:12] → 页表索引
    ///
    /// 注意：对于内核高地址 (bit[63]=1)，逻辑右移会保持符号位，
    /// 但我们只取低 9 位，所以可以直接使用 & 0x1ff 掩码
    #[inline]
    pub fn pn_index(&self, n: usize) -> usize {
        match n {
            0 => (self.raw() >> 12) & 0x1ff,      // VPN[0]: 页表索引
            1 => (self.raw() >> 21) & 0x1ff,      // VPN[1]: PMD 索引
            2 => (self.raw() >> 30) & 0x1ff,      // VPN[2]: PGD 索引
            3 => (self.raw() >> 39) & 0x1ff,      // VPN[3]: PUD 索引 (SV48+)
            _ => 0,
        }
    }

    /// Get n level page table offset of the given virtual address
    #[inline]
    pub fn pn_offest(&self, n: usize) -> usize {
        self.raw() % (1 << (12 + 9 * n))
    }

    /// Check if this is a kernel virtual address (SV39: bit[63] = 1)
    #[inline]
    pub fn is_kernel(&self) -> bool {
        (self.raw() as i64) < 0
    }

    /// Get VPN[2] index for SV39
    #[inline]
    pub fn vpn2(&self) -> usize {
        self.pn_index(2)
    }

    /// Get VPN[1] index for SV39
    #[inline]
    pub fn vpn1(&self) -> usize {
        self.pn_index(1)
    }

    /// Get VPN[0] index for SV39
    #[inline]
    pub fn vpn0(&self) -> usize {
        self.pn_index(0)
    }
}
