//!
//! 平台相关驱动入口。
//!
//! - riscv64:  VirtIO-MMIO（设备树枚举）
//! - loongarch64: VirtIO-PCI（ECAM 枚举）

pub mod hal;

#[cfg(target_arch = "riscv64")]
pub mod virtio_mmio_blk;

#[cfg(target_arch = "loongarch64")]
pub mod virtio_pci_blk;
