//! LoongArch QEMU virt 上的 VirtIO-Blk PCI（ECAM 枚举）。
//!
//! 参考: T202510008995695-2720-master/os/src/drivers/virtio/blk.rs (enumerate_pci / PciRangeAllocator)
//! 平台常量校验: T202510003995291-2331-master/arceos/configs/platforms/loongarch64-qemu-virt.toml
#![cfg(target_arch = "loongarch64")]

use alloc::sync::Arc;
use alloc::vec;

use virtio_drivers::device::blk::{VirtIOBlk, SECTOR_SIZE};
use virtio_drivers::transport::pci::bus::{BarInfo, Cam, Command, MemoryBarType, PciRoot};
use virtio_drivers::transport::pci::{virtio_device_type, PciTransport};
use virtio_drivers::transport::{DeviceType, Transport};

use crate::drivers::hal::{phys_to_virt_mmio, VirtHal};
use crate::fs::block_dev::RawBlockDevice;
use crate::utils::error::SysErrNo;

/// QEMU LoongArch virt: PCI ECAM 物理基址
/// 参考: 2331 loongarch64-qemu-virt.toml  pci-ecam-base = 0x2000_0000
/// 参考: 2720 os/src/drivers/mod.rs       VIRTIO0 = 0x2000_0000
const PCI_ECAM_PHYS: usize = 0x2000_0000;

/// PCI BAR 内存分配区域（物理地址，写入设备 BAR 寄存器）
/// 参考: 2331 loongarch64-qemu-virt.toml  pci-ranges [0x4000_0000, 0x0002_0000]
/// 参考: 2720 os/src/drivers/virtio/blk.rs VIRT_PCI_BASE / VIRT_PCI_SIZE
const PCI_BAR_BASE: usize = 0x4000_0000;
const PCI_BAR_SIZE: usize = 0x0002_0000;

struct PciBarAllocator {
    end: usize,
    current: usize,
}

impl PciBarAllocator {
    const fn new(base: usize, size: usize) -> Self {
        Self {
            end: base + size,
            current: base,
        }
    }

    fn alloc(&mut self, size: usize) -> Option<usize> {
        if !size.is_power_of_two() {
            return None;
        }
        let aligned = (self.current + size - 1) & !(size - 1);
        if aligned + size > self.end {
            return None;
        }
        self.current = aligned + size;
        Some(aligned)
    }
}

pub struct VirtioPciBlock {
    blk: spin::Mutex<VirtIOBlk<VirtHal, PciTransport>>,
}

unsafe impl Sync for VirtioPciBlock {}
unsafe impl Send for VirtioPciBlock {}

impl VirtioPciBlock {
    fn read_phys(&self, offset: usize, buf: &mut [u8]) -> Result<(), ()> {
        if buf.is_empty() {
            return Ok(());
        }
        let sector = offset / SECTOR_SIZE;
        let skip = offset % SECTOR_SIZE;
        let sectors_needed = skip.saturating_add(buf.len()).div_ceil(SECTOR_SIZE);
        let mut tmp = vec![0u8; sectors_needed * SECTOR_SIZE];
        self.blk
            .lock()
            .read_blocks(sector, &mut tmp)
            .map_err(|_| ())?;
        buf.copy_from_slice(&tmp[skip..skip + buf.len()]);
        Ok(())
    }

    fn write_phys(&self, offset: usize, data: &[u8]) -> Result<(), ()> {
        if data.is_empty() {
            return Ok(());
        }
        let sector = offset / SECTOR_SIZE;
        let skip = offset % SECTOR_SIZE;
        let end = skip + data.len();
        let sectors_needed = end.div_ceil(SECTOR_SIZE);
        let mut tmp = vec![0u8; sectors_needed * SECTOR_SIZE];

        if skip != 0 || end % SECTOR_SIZE != 0 {
            let _ = self.read_phys(sector * SECTOR_SIZE, &mut tmp);
        }

        tmp[skip..end].copy_from_slice(data);
        self.blk.lock().write_blocks(sector, &tmp).map_err(|_| ())?;
        Ok(())
    }
}

/// BlockDevice impl 与 riscv64 的 VirtioMmioBlock 保持同构
/// 参考: os_contest/os/src/drivers/virtio_mmio_blk.rs (riscv64 接口形状)
impl RawBlockDevice for VirtioPciBlock {
    fn read_at(&self, offset: usize, buf: &mut [u8]) -> Result<(), SysErrNo> {
        self.read_phys(offset, buf).map_err(|_| SysErrNo::EIO)
    }

    fn write_at(&self, offset: usize, data: &[u8]) -> Result<(), SysErrNo> {
        self.write_phys(offset, data).map_err(|_| SysErrNo::EIO)
    }

    fn size_bytes(&self) -> Option<usize> {
        let sectors = self.blk.lock().capacity();
        (sectors as usize).checked_mul(SECTOR_SIZE)
    }
}

/// 枚举 PCI 总线 0，找到第一个 VirtIO Block 设备并返回。
///
/// 参考: T202510008995695-2720-master/os/src/drivers/virtio/blk.rs (enumerate_pci)
pub fn probe_pci_virtio_blk() -> Option<Arc<dyn RawBlockDevice>> {
    let ecam_virt = phys_to_virt_mmio(PCI_ECAM_PHYS);
    log::info!(
        "[virtio-pci] Enumerating PCI bus, ECAM phys {:#x} virt {:p}",
        PCI_ECAM_PHYS,
        ecam_virt
    );
    let mut pci_root = unsafe { PciRoot::new(ecam_virt, Cam::Ecam) };

    for (dev_fn, info) in pci_root.enumerate_bus(0) {
        let Some(vtype) = virtio_device_type(&info) else {
            continue;
        };
        if vtype != DeviceType::Block {
            log::debug!(
                "[virtio-pci] skip non-block VirtIO {:?} @ {}",
                vtype,
                dev_fn
            );
            continue;
        }
        log::info!("[virtio-pci] Found VirtIO Block @ {}", dev_fn);

        let mut bar_alloc = PciBarAllocator::new(PCI_BAR_BASE, PCI_BAR_SIZE);
        let mut bar_idx: u8 = 0;
        while bar_idx < 6 {
            let bar_info = pci_root.bar_info(dev_fn, bar_idx).unwrap();
            if let BarInfo::Memory {
                address_type,
                address,
                size,
                ..
            } = bar_info
            {
                if address == 0 && size != 0 {
                    let addr = bar_alloc.alloc(size as usize).unwrap();
                    match address_type {
                        MemoryBarType::Width64 => {
                            pci_root.set_bar_64(dev_fn, bar_idx, addr as u64);
                        }
                        MemoryBarType::Width32 => {
                            pci_root.set_bar_32(dev_fn, bar_idx, addr as u32);
                        }
                        _ => {}
                    }
                }
            }
            bar_idx += 1;
            if bar_info.takes_two_entries() {
                bar_idx += 1;
            }
        }

        pci_root.set_command(
            dev_fn,
            Command::IO_SPACE | Command::MEMORY_SPACE | Command::BUS_MASTER,
        );

        let transport = match PciTransport::new::<VirtHal>(&mut pci_root, dev_fn) {
            Ok(transport) => transport,
            Err(err) => {
                log::warn!("[virtio-pci] PciTransport init failed: {:?}", err);
                return None;
            }
        };
        let blk = match VirtIOBlk::new(transport) {
            Ok(blk) => blk,
            Err(err) => {
                log::warn!("[virtio-pci] VirtIOBlk init failed: {:?}", err);
                return None;
            }
        };
        let cap = blk.capacity();
        log::info!("[virtio-pci] VirtIO blk ready, {} sectors × 512B", cap);

        return Some(Arc::new(VirtioPciBlock {
            blk: spin::Mutex::new(blk),
        }));
    }

    log::warn!("[virtio-pci] No VirtIO Block device found on PCI bus 0");
    None
}
