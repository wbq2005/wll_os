use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use ext4_rs::{BlockDevice, BLOCK_SIZE as EXT4_BLOCK_SIZE};
use lazy_static::lazy_static;
use spin::Mutex;

use crate::utils::error::SysErrNo;

pub const DEV_VIRTIO_BLK_MAJOR: u32 = 254;
const SECTOR_SIZE: usize = 512;
const ROOT_DISK: &str = "/dev/vda";

pub trait RawBlockDevice: Send + Sync {
    fn read_at(&self, offset: usize, buf: &mut [u8]) -> Result<(), SysErrNo>;
    fn write_at(&self, offset: usize, data: &[u8]) -> Result<(), SysErrNo>;
    fn size_bytes(&self) -> Option<usize>;
}

#[derive(Clone)]
pub struct BlockRange {
    raw: Arc<dyn RawBlockDevice>,
    start: usize,
    len: Option<usize>,
}

impl BlockRange {
    fn new(raw: Arc<dyn RawBlockDevice>, start: usize, len: Option<usize>) -> Self {
        Self { raw, start, len }
    }

    fn checked_abs_range(&self, offset: usize, len: usize) -> Result<usize, SysErrNo> {
        let end = offset.checked_add(len).ok_or(SysErrNo::EINVAL)?;
        if let Some(limit) = self.len {
            if end > limit {
                return Err(SysErrNo::EINVAL);
            }
        }
        self.start.checked_add(offset).ok_or(SysErrNo::EINVAL)
    }

    pub fn read_at(&self, offset: usize, buf: &mut [u8]) -> Result<(), SysErrNo> {
        let abs = self.checked_abs_range(offset, buf.len())?;
        self.raw.read_at(abs, buf)
    }

    pub fn write_at(&self, offset: usize, data: &[u8]) -> Result<(), SysErrNo> {
        let abs = self.checked_abs_range(offset, data.len())?;
        self.raw.write_at(abs, data)
    }
}

impl BlockDevice for BlockRange {
    fn read_offset(&self, offset: usize) -> Vec<u8> {
        let mut buf = vec![0u8; EXT4_BLOCK_SIZE];
        if self.read_at(offset, &mut buf).is_err() {
            log::warn!("[block] read_offset failed @{:#x}", offset);
        }
        buf
    }

    fn write_offset(&self, offset: usize, data: &[u8]) {
        if self.write_at(offset, data).is_err() {
            log::warn!("[block] write_offset failed @{:#x}, len {}", offset, data.len());
        }
    }
}

#[derive(Clone)]
struct RegisteredBlockDevice {
    path: String,
    minor: u32,
    range: Arc<BlockRange>,
}

lazy_static! {
    static ref DEVICES: Mutex<Vec<RegisteredBlockDevice>> = Mutex::new(Vec::new());
    static ref ROOT_SOURCE: Mutex<String> = Mutex::new(ROOT_DISK.to_string());
}

fn read_u32_le(buf: &[u8], off: usize) -> Option<u32> {
    Some(u32::from_le_bytes([
        *buf.get(off)?,
        *buf.get(off + 1)?,
        *buf.get(off + 2)?,
        *buf.get(off + 3)?,
    ]))
}

fn read_u64_le(buf: &[u8], off: usize) -> Option<u64> {
    Some(u64::from_le_bytes([
        *buf.get(off)?,
        *buf.get(off + 1)?,
        *buf.get(off + 2)?,
        *buf.get(off + 3)?,
        *buf.get(off + 4)?,
        *buf.get(off + 5)?,
        *buf.get(off + 6)?,
        *buf.get(off + 7)?,
    ]))
}

#[derive(Clone, Copy)]
struct PartitionInfo {
    index: u32,
    start_lba: u64,
    sectors: u64,
}

fn read_sector(raw: &Arc<dyn RawBlockDevice>, lba: u64, out: &mut [u8; SECTOR_SIZE]) -> bool {
    let Some(offset) = (lba as usize).checked_mul(SECTOR_SIZE) else {
        return false;
    };
    raw.read_at(offset, out).is_ok()
}

fn parse_gpt_partitions(raw: &Arc<dyn RawBlockDevice>) -> Vec<PartitionInfo> {
    let mut header = [0u8; SECTOR_SIZE];
    if !read_sector(raw, 1, &mut header) || &header[0..8] != b"EFI PART" {
        return Vec::new();
    }

    let entries_lba = read_u64_le(&header, 72).unwrap_or(0);
    let entry_count = read_u32_le(&header, 80).unwrap_or(0).min(256);
    let entry_size = read_u32_le(&header, 84).unwrap_or(0) as usize;
    if entries_lba == 0 || entry_size < 128 || entry_size > 1024 || entry_count == 0 {
        return Vec::new();
    }

    let bytes = (entry_count as usize).saturating_mul(entry_size);
    let mut table = vec![0u8; bytes];
    let Some(offset) = (entries_lba as usize).checked_mul(SECTOR_SIZE) else {
        return Vec::new();
    };
    if raw.read_at(offset, &mut table).is_err() {
        return Vec::new();
    }

    let mut parts = Vec::new();
    for idx in 0..entry_count as usize {
        let base = idx * entry_size;
        let entry = &table[base..base + entry_size];
        if entry[..16].iter().all(|b| *b == 0) {
            continue;
        }
        let first = read_u64_le(entry, 32).unwrap_or(0);
        let last = read_u64_le(entry, 40).unwrap_or(0);
        if first == 0 || last < first {
            continue;
        }
        parts.push(PartitionInfo {
            index: idx as u32 + 1,
            start_lba: first,
            sectors: last - first + 1,
        });
    }
    parts
}

fn parse_mbr_partitions(raw: &Arc<dyn RawBlockDevice>) -> Vec<PartitionInfo> {
    let mut mbr = [0u8; SECTOR_SIZE];
    if !read_sector(raw, 0, &mut mbr) || mbr[510] != 0x55 || mbr[511] != 0xaa {
        return Vec::new();
    }

    if (0..4).any(|i| mbr[446 + i * 16 + 4] == 0xee) {
        let gpt = parse_gpt_partitions(raw);
        if !gpt.is_empty() {
            return gpt;
        }
    }

    let mut parts = Vec::new();
    for i in 0..4 {
        let base = 446 + i * 16;
        let part_type = mbr[base + 4];
        let start = read_u32_le(&mbr, base + 8).unwrap_or(0);
        let sectors = read_u32_le(&mbr, base + 12).unwrap_or(0);
        if part_type == 0 || sectors == 0 {
            continue;
        }
        parts.push(PartitionInfo {
            index: i as u32 + 1,
            start_lba: start as u64,
            sectors: sectors as u64,
        });
    }
    parts
}

fn ext4_magic(range: &BlockRange) -> bool {
    let mut magic = [0u8; 2];
    range.read_at(0x438, &mut magic).is_ok() && u16::from_le_bytes(magic) == 0xef53
}

pub fn register_virtio_disk(raw: Arc<dyn RawBlockDevice>) -> Arc<dyn BlockDevice> {
    let whole_size = raw.size_bytes();
    let whole = Arc::new(BlockRange::new(raw.clone(), 0, whole_size));
    let mut devices = Vec::new();
    devices.push(RegisteredBlockDevice {
        path: ROOT_DISK.to_string(),
        minor: 0,
        range: whole.clone(),
    });

    let mut root_source = ROOT_DISK.to_string();
    let mut root_range = whole.clone();
    let partitions = parse_mbr_partitions(&raw);
    for part in partitions.iter() {
        let Some(start) = (part.start_lba as usize).checked_mul(SECTOR_SIZE) else {
            continue;
        };
        let Some(len) = (part.sectors as usize).checked_mul(SECTOR_SIZE) else {
            continue;
        };
        let range = Arc::new(BlockRange::new(raw.clone(), start, Some(len)));
        let path = alloc::format!("{}{}", ROOT_DISK, part.index);
        if root_source == ROOT_DISK && !ext4_magic(&whole) && ext4_magic(&range) {
            root_source = path.clone();
            root_range = range.clone();
        }
        devices.push(RegisteredBlockDevice {
            path,
            minor: part.index,
            range,
        });
    }

    if ext4_magic(&whole) {
        root_source = ROOT_DISK.to_string();
        root_range = whole.clone();
    }

    log::info!(
        "[block] registered {} virtio block node(s), root candidate {}",
        devices.len(),
        root_source
    );
    *DEVICES.lock() = devices;
    *ROOT_SOURCE.lock() = root_source;
    root_range
}

pub fn list_device_paths() -> Vec<String> {
    DEVICES
        .lock()
        .iter()
        .map(|entry| entry.path.clone())
        .collect()
}

pub fn minor_for_path(path: &str) -> Option<u32> {
    DEVICES
        .lock()
        .iter()
        .find(|entry| entry.path == path)
        .map(|entry| entry.minor)
}

pub fn range_for_path(path: &str) -> Option<Arc<BlockRange>> {
    DEVICES
        .lock()
        .iter()
        .find(|entry| entry.path == path)
        .map(|entry| entry.range.clone())
}

pub fn is_root_source(path: &str) -> bool {
    *ROOT_SOURCE.lock() == path
}
