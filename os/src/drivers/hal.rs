//! VirtIO 驱动所用的 [`virtio_drivers::Hal`] 实现。
//!
//! LoongArch 在 PG=1 + DMW 模式下，裸物理地址不可直接解引用：
//! - DMA 缓冲区（RAM）需通过 DMW1 (0x9000..., cached) 访问
//! - MMIO 寄存器需通过 DMW0 (0x8000..., uncached) 访问
//! - 传给设备的 DMA 地址必须是物理地址（剥除 DMW 前缀）
//!
//! RISC-V 启动阶段无分页或有恒等映射，物理地址可直接使用。
//!
//! 参考: T202510008995695-2720-master/os/src/drivers/virtio/mod.rs (VirtIoHalImpl)

use core::ptr::NonNull;

use virtio_drivers::{BufferDirection, Hal, PhysAddr, PAGE_SIZE as VIRT_PAGE};

use crate::mm::frame_allocator::{
    alloc_contiguous_frames, dealloc_contiguous_frames,
};

/// LoongArch DMW1: cached, 用于普通 RAM / DMA 缓冲区
#[cfg(target_arch = "loongarch64")]
const DMW_RAM: usize = 0x9000_0000_0000_0000;

/// LoongArch DMW0: uncached, 用于 MMIO 设备寄存器
#[cfg(target_arch = "loongarch64")]
const DMW_MMIO: usize = 0x8000_0000_0000_0000;

/// 物理地址 → 可解引用的虚拟指针（RAM，cached）
#[inline]
pub fn phys_to_virt_ram(paddr: usize) -> *mut u8 {
    #[cfg(target_arch = "loongarch64")]
    { (paddr | DMW_RAM) as *mut u8 }
    #[cfg(not(target_arch = "loongarch64"))]
    { paddr as *mut u8 }
}

/// 物理地址 → 可解引用的虚拟指针（MMIO，uncached）
#[inline]
pub fn phys_to_virt_mmio(paddr: usize) -> *mut u8 {
    #[cfg(target_arch = "loongarch64")]
    { (paddr | DMW_MMIO) as *mut u8 }
    #[cfg(not(target_arch = "loongarch64"))]
    { paddr as *mut u8 }
}

/// 虚拟指针 → 物理地址（剥除 DMW 前缀）
#[inline]
pub fn virt_to_phys(vaddr: usize) -> usize {
    #[cfg(target_arch = "loongarch64")]
    { vaddr & 0x0FFF_FFFF_FFFF_FFFF }
    #[cfg(not(target_arch = "loongarch64"))]
    { vaddr }
}

#[derive(Clone, Copy)]
pub enum VirtHal {}

unsafe impl Hal for VirtHal {
    fn dma_alloc(pages: usize, _direction: BufferDirection) -> (PhysAddr, NonNull<u8>) {
        let Some(ppn_start) = alloc_contiguous_frames(pages) else {
            return (0, NonNull::dangling());
        };
        let paddr = ppn_start * VIRT_PAGE;
        let ptr = phys_to_virt_ram(paddr);
        unsafe {
            core::ptr::write_bytes(ptr, 0, pages * VIRT_PAGE);
        }
        (
            paddr,
            NonNull::new(ptr).expect("VirtHal::dma_alloc: null vaddr"),
        )
    }

    unsafe fn dma_dealloc(paddr: PhysAddr, _vaddr: NonNull<u8>, pages: usize) -> i32 {
        let start_ppn = paddr / VIRT_PAGE;
        dealloc_contiguous_frames(start_ppn, pages);
        0
    }

    unsafe fn mmio_phys_to_virt(paddr: PhysAddr, _size: usize) -> NonNull<u8> {
        NonNull::new_unchecked(phys_to_virt_mmio(paddr))
    }

    unsafe fn share(buffer: NonNull<[u8]>, _direction: BufferDirection) -> PhysAddr {
        virt_to_phys(buffer.as_ptr().cast::<u8>() as usize)
    }

    unsafe fn unshare(_paddr: PhysAddr, _buffer: NonNull<[u8]>, _direction: BufferDirection) {}
}
