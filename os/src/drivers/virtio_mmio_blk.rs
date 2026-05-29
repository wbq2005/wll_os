//! RISC-V QEMU `virt,mmio` 上的 VirtIO-Blk MMIO（由设备树枚举）。
#![cfg(target_arch = "riscv64")]

use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use core::ptr::NonNull;

use ext4_rs::BLOCK_SIZE as EXT4_BLOCK_SIZE;
use ext4_rs::BlockDevice;
use virtio_drivers::device::blk::{VirtIOBlk, SECTOR_SIZE};
use virtio_drivers::transport::mmio::{MmioTransport, VirtIOHeader};
use virtio_drivers::transport::{DeviceType, Transport};

use crate::drivers::hal::VirtHal;

unsafe fn dtb_totalsize(dtb: usize) -> Option<usize> {
    let mag = unsafe { *(dtb as *const u32) };
    if u32::from_be(mag) != 0xd00d_feed {
        return None;
    }
    let sz = unsafe { *((dtb + 4) as *const u32) };
    Some(u32::from_be(sz) as usize)
}

/// 磁盘块：`ext4_rs` 期望 `read_offset` 返回整块（常见 4096）。
pub struct VirtioMmioBlock {
    blk: spin::Mutex<VirtIOBlk<VirtHal, MmioTransport>>,
}

unsafe impl Sync for VirtioMmioBlock {}
unsafe impl Send for VirtioMmioBlock {}

impl VirtioMmioBlock {
    /// 从给定物理起始地址（MMIO 窗口）附着 VirtIO blk 设备。
    pub unsafe fn attach(mmio_pa: usize) -> Option<Self> {
        let header_ptr = NonNull::new(mmio_pa as *mut VirtIOHeader)?;
        // Debug: read VirtIO magic at offset 0 (should be 0x74726976 = "virt")
        let magic_val = unsafe { core::ptr::read_volatile(mmio_pa as *const u32) };
        log::info!("[virtio] MMIO @{:#x}: magic = {:#x} (expect 0x74726976)", mmio_pa, magic_val);
        let transport = MmioTransport::new(header_ptr).ok()?;
        if transport.device_type() != DeviceType::Block {
            log::warn!(
                "[virtio] MMIO @{:#x}: unexpected device {:?}",
                mmio_pa,
                transport.device_type()
            );
            return None;
        }
        let blk = VirtIOBlk::new(transport).ok()?;
        let cap = blk.capacity();
        log::info!("[virtio] VirtIO blk @ {:#x}, {} sectors × 512B", mmio_pa, cap);
        Some(Self {
            blk: spin::Mutex::new(blk),
        })
    }

    fn read_phys(&self, offset: usize, buf: &mut [u8]) -> Result<(), ()> {
        if buf.is_empty() {
            return Ok(());
        }
        let sector = offset / SECTOR_SIZE;
        let skip = offset % SECTOR_SIZE;
        let sectors_needed =
            skip.saturating_add(buf.len()).div_ceil(SECTOR_SIZE);
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

impl BlockDevice for VirtioMmioBlock {
    fn read_offset(&self, offset: usize) -> Vec<u8> {
        let mut buf = vec![0u8; EXT4_BLOCK_SIZE];
        if self.read_phys(offset, &mut buf).is_err() {
            log::warn!("[virtio] read_offset failed @{:#x}", offset);
        }
        buf
    }

    fn write_offset(&self, offset: usize, data: &[u8]) {
        if self.write_phys(offset, data).is_err() {
            log::warn!(
                "[virtio] write_offset failed @{:#x}, len {}",
                offset,
                data.len()
            );
        }
    }
}

/// 枚举 DTB 中兼容 `virtio,mmio` 的节点并附着第一块 virtio-blk。
pub unsafe fn probe_first_virtio_disk_from_dt(dtb_ptr: usize) -> Option<Arc<dyn BlockDevice>> {
    let tot = dtb_totalsize(dtb_ptr)?;
    let blob =
        unsafe { core::slice::from_raw_parts(dtb_ptr as *const u8, tot) };
    let Ok(fdt) = flat_device_tree::Fdt::new(blob) else {
        log::warn!("[fdt] failed to parse DTB @{:#x}", dtb_ptr);
        return None;
    };

    let mut found_count = 0usize;
    let mut checked_addrs: [usize; 16] = [0; 16];
    for node in fdt.all_nodes() {
        let Some(compat) = node.compatible() else {
            continue;
        };
        if !compat.all().any(|s| s == "virtio,mmio") {
            continue;
        }
        let Some(reg) = node.reg().next() else {
            continue;
        };
        let mmio_pa = reg.starting_address as usize;
        // Debug: record this address
        if found_count < 16 {
            checked_addrs[found_count] = mmio_pa;
        }
        found_count += 1;

        // Debug: print the MMIO address before trying to attach
        log::info!("[virtio] Trying virtio,mmio @ {:#x}", mmio_pa);
        unsafe {
            if let Some(dev) = VirtioMmioBlock::attach(mmio_pa) {
                let arc: Arc<dyn BlockDevice> = Arc::new(dev);
                log::info!("[virtio] Successfully attached virtio blk @ {:#x}", mmio_pa);
                return Some(arc);
            }
        }
    }
    log::warn!("[virtio] no virtio,mmio block device enumerated from DTB (found {} virtio nodes)", found_count);
    None
}
