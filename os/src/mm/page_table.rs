use bitflags::bitflags;
use lazy_static::lazy_static;
use polyhal::pagetable::{MappingFlags, PageTableWrapper};
use polyhal::{PhysAddr, VirtAddr};
use spin::Mutex;

/// 页表项标志位
bitflags! {
    #[derive(Clone, Copy)]
    pub struct PTEFlags: u16 {
        const V = 1 << 0;   // Valid
        const R = 1 << 1;   // Readable
        const W = 1 << 2;   // Writable
        const X = 1 << 3;   // Executable
        const U = 1 << 4;   // User accessible
        const G = 1 << 5;   // Global
        const A = 1 << 6;   // Accessed
        const D = 1 << 7;   // Dirty
    }
}

impl From<PTEFlags> for MappingFlags {
    fn from(flags: PTEFlags) -> Self {
        let mut mf = MappingFlags::empty();
        if flags.contains(PTEFlags::R) {
            mf |= MappingFlags::R;
        }
        if flags.contains(PTEFlags::W) {
            mf |= MappingFlags::W;
        }
        if flags.contains(PTEFlags::X) {
            mf |= MappingFlags::X;
        }
        if flags.contains(PTEFlags::U) {
            mf |= MappingFlags::U;
        }
        mf
    }
}

lazy_static! {
    /// 内核页表实例 — 由 `init_kernel_page_table()` 创建并填充实际内核映射。
    /// 其他模块通过 `page_table::kernel_page_table()` 获取引用。
    static ref KERNEL_PAGE_TABLE: Mutex<Option<PageTableWrapper>> = Mutex::new(None);
}

/// 获取内核页表引用（供其他模块使用）
pub fn kernel_page_table() -> &'static Mutex<Option<PageTableWrapper>> {
    &KERNEL_PAGE_TABLE
}

/// 初始化内核页表
pub fn init_kernel_page_table() {
    let pt = PageTableWrapper::alloc();
    *KERNEL_PAGE_TABLE.lock() = Some(pt);
}

/// 映射页面
pub fn map_page(vpn: usize, ppn: crate::mm::frame_allocator::PhysPageNum, flags: PTEFlags) {
    let vaddr = VirtAddr::new(vpn * crate::config::PAGE_SIZE);
    let paddr = PhysAddr::new(ppn.addr());
    let mf: MappingFlags = flags.into();
    if let Some(ref pt) = KERNEL_PAGE_TABLE.lock().as_ref() {
        pt.map_page(vaddr, paddr, mf, polyhal::pagetable::MappingSize::Page4KB);
    }
}

/// 取消映射页面
pub fn unmap_page(vpn: usize) {
    let vaddr = VirtAddr::new(vpn * crate::config::PAGE_SIZE);
    if let Some(ref pt) = KERNEL_PAGE_TABLE.lock().as_ref() {
        pt.unmap_page(vaddr);
    }
}

/// 地址转换
pub fn translate(vaddr: VirtAddr) -> Option<PhysAddr> {
    KERNEL_PAGE_TABLE
        .lock()
        .as_ref()
        .and_then(|pt| pt.translate(vaddr).map(|(paddr, _)| paddr))
}
