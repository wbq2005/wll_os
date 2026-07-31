use alloc::collections::{BTreeMap, BTreeSet};
use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use ext4_rs::{BlockDevice, BLOCK_SIZE as EXT4_BLOCK_SIZE};
use lazy_static::lazy_static;
use spin::Mutex;

use crate::utils::error::SysErrNo;

pub const DEV_VIRTIO_BLK_MAJOR: u32 = 254;
pub const DEV_LOOP_MAJOR: u32 = 7;
pub const DEV_LOOP_CONTROL_MAJOR: u32 = 10;
pub const DEV_LOOP_CONTROL_MINOR: u32 = 237;
pub const LOOP_DEVICE_COUNT: usize = 4;
const SECTOR_SIZE: usize = 512;
const BLOCK_CACHE_LIMIT: usize = 32 * 1024;
const ROOT_DISK: &str = "/dev/vda";
const LOOP_CONTROL: &str = "/dev/loop-control";

pub trait RawBlockDevice: Send + Sync {
    fn read_at(&self, offset: usize, buf: &mut [u8]) -> Result<(), SysErrNo>;
    fn write_at(&self, offset: usize, data: &[u8]) -> Result<(), SysErrNo>;
    fn size_bytes(&self) -> Option<usize>;
}

struct BlockCacheEntry {
    data: Vec<u8>,
    last_used: u64,
}

struct BlockCache {
    blocks: BTreeMap<usize, BlockCacheEntry>,
    ages: BTreeSet<(u64, usize)>,
    next_age: u64,
}

impl BlockCache {
    fn new() -> Self {
        Self {
            blocks: BTreeMap::new(),
            ages: BTreeSet::new(),
            next_age: 1,
        }
    }

    fn bump_age(&mut self) -> u64 {
        let age = self.next_age;
        self.next_age = self.next_age.wrapping_add(1).max(1);
        age
    }

    fn get(&mut self, block: usize) -> Option<Vec<u8>> {
        let age = self.bump_age();
        let entry = self.blocks.get_mut(&block)?;
        self.ages.remove(&(entry.last_used, block));
        entry.last_used = age;
        self.ages.insert((age, block));
        Some(entry.data.clone())
    }

    fn insert(&mut self, block: usize, data: Vec<u8>) {
        let age = self.bump_age();
        if let Some(entry) = self.blocks.get_mut(&block) {
            self.ages.remove(&(entry.last_used, block));
            entry.data = data;
            entry.last_used = age;
            self.ages.insert((age, block));
            return;
        }
        if self.blocks.len() >= BLOCK_CACHE_LIMIT {
            if let Some((old_age, old_block)) = self.ages.iter().next().copied() {
                self.ages.remove(&(old_age, old_block));
                self.blocks.remove(&old_block);
            }
        }
        self.blocks.insert(
            block,
            BlockCacheEntry {
                data,
                last_used: age,
            },
        );
        self.ages.insert((age, block));
    }

    fn invalidate_range(&mut self, start: usize, len: usize) {
        let Some(end) = start.checked_add(len) else {
            self.blocks.clear();
            self.ages.clear();
            return;
        };
        if start >= end {
            return;
        }
        let first = (start / EXT4_BLOCK_SIZE) * EXT4_BLOCK_SIZE;
        let last = ((end - 1) / EXT4_BLOCK_SIZE) * EXT4_BLOCK_SIZE;
        let mut block = first;
        loop {
            if let Some(entry) = self.blocks.remove(&block) {
                self.ages.remove(&(entry.last_used, block));
            }
            if block == last {
                break;
            }
            block = block.saturating_add(EXT4_BLOCK_SIZE);
        }
    }
}

#[derive(Clone)]
pub struct BlockRange {
    raw: Arc<dyn RawBlockDevice>,
    start: usize,
    len: Option<usize>,
    cache: Arc<Mutex<BlockCache>>,
    io_lock: Arc<Mutex<()>>,
}

impl BlockRange {
    fn new(raw: Arc<dyn RawBlockDevice>, start: usize, len: Option<usize>) -> Self {
        Self {
            raw,
            start,
            len,
            cache: Arc::new(Mutex::new(BlockCache::new())),
            io_lock: Arc::new(Mutex::new(())),
        }
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

    fn read_raw_at(&self, offset: usize, buf: &mut [u8]) -> Result<(), SysErrNo> {
        let abs = self.checked_abs_range(offset, buf.len())?;
        self.raw.read_at(abs, buf)
    }

    fn write_raw_at(&self, offset: usize, data: &[u8]) -> Result<(), SysErrNo> {
        let abs = self.checked_abs_range(offset, data.len())?;
        self.raw.write_at(abs, data)
    }

    pub fn read_at(&self, offset: usize, buf: &mut [u8]) -> Result<(), SysErrNo> {
        let _io = self.io_lock.lock();
        self.read_raw_at(offset, buf)
    }

    pub fn write_at(&self, offset: usize, data: &[u8]) -> Result<(), SysErrNo> {
        let _io = self.io_lock.lock();
        self.cache.lock().invalidate_range(offset, data.len());
        self.write_raw_at(offset, data)
    }

    fn read_cached_block(&self, offset: usize) -> Vec<u8> {
        if let Some(data) = self.cache.lock().get(offset) {
            return data;
        }

        let _io = self.io_lock.lock();
        if let Some(data) = self.cache.lock().get(offset) {
            return data;
        }
        let mut data = vec![0u8; EXT4_BLOCK_SIZE];
        if self.read_raw_at(offset, &mut data).is_err() {
            log::warn!("[block] read_offset failed @{:#x}", offset);
            return data;
        }
        self.cache.lock().insert(offset, data.clone());
        data
    }

    fn read_cached_blocks(&self, offsets: &[usize]) -> Result<Vec<Vec<u8>>, SysErrNo> {
        let mut blocks: Vec<Option<Vec<u8>>> = vec![None; offsets.len()];
        {
            let mut cache = self.cache.lock();
            for (index, offset) in offsets.iter().copied().enumerate() {
                blocks[index] = cache.get(offset);
            }
        }
        if blocks.iter().all(Option::is_some) {
            return Ok(blocks.into_iter().flatten().collect());
        }

        let _io = self.io_lock.lock();
        {
            let mut cache = self.cache.lock();
            for (index, offset) in offsets.iter().copied().enumerate() {
                if blocks[index].is_none() {
                    blocks[index] = cache.get(offset);
                }
            }
        }

        let mut index = 0usize;
        while index < offsets.len() {
            if blocks[index].is_some() {
                index += 1;
                continue;
            }
            let run_start = index;
            let mut run_end = index + 1;
            while run_end < offsets.len()
                && blocks[run_end].is_none()
                && offsets[run_end - 1].checked_add(EXT4_BLOCK_SIZE) == Some(offsets[run_end])
            {
                run_end += 1;
            }

            let run_bytes = (run_end - run_start)
                .checked_mul(EXT4_BLOCK_SIZE)
                .ok_or(SysErrNo::EFBIG)?;
            let mut data = vec![0u8; run_bytes];
            self.read_raw_at(offsets[run_start], &mut data)?;
            let mut cache = self.cache.lock();
            for block_index in run_start..run_end {
                let data_start = (block_index - run_start) * EXT4_BLOCK_SIZE;
                let block = data[data_start..data_start + EXT4_BLOCK_SIZE].to_vec();
                cache.insert(offsets[block_index], block.clone());
                blocks[block_index] = Some(block);
            }
            index = run_end;
        }

        blocks.into_iter().collect::<Option<Vec<_>>>().ok_or(SysErrNo::EIO)
    }
}

impl BlockDevice for BlockRange {
    fn read_offset(&self, offset: usize) -> Vec<u8> {
        if offset % EXT4_BLOCK_SIZE == 0 {
            return self.read_cached_block(offset);
        }

        let end = offset.saturating_add(EXT4_BLOCK_SIZE);
        let first = (offset / EXT4_BLOCK_SIZE) * EXT4_BLOCK_SIZE;
        let mut out = vec![0u8; EXT4_BLOCK_SIZE];
        let mut copied = 0usize;
        let mut block = first;
        while copied < EXT4_BLOCK_SIZE {
            let data = self.read_cached_block(block);
            let block_offset = offset.saturating_add(copied).saturating_sub(block);
            let available = EXT4_BLOCK_SIZE.saturating_sub(block_offset);
            let count = available.min(EXT4_BLOCK_SIZE - copied);
            if count == 0 {
                break;
            }
            out[copied..copied + count]
                .copy_from_slice(&data[block_offset..block_offset + count]);
            copied += count;
            block = block.saturating_add(EXT4_BLOCK_SIZE);
            if block >= end && copied < EXT4_BLOCK_SIZE {
                break;
            }
        }
        out
    }

    fn write_offset(&self, offset: usize, data: &[u8]) {
        if self.write_at(offset, data).is_err() {
            log::warn!(
                "[block] write_offset failed @{:#x}, len {}",
                offset,
                data.len()
            );
        }
    }
}

#[derive(Clone)]
struct RegisteredBlockDevice {
    path: String,
    major: u32,
    minor: u32,
    range: Arc<BlockRange>,
}

lazy_static! {
    static ref DEVICES: Mutex<Vec<RegisteredBlockDevice>> = Mutex::new(Vec::new());
    static ref ROOT_SOURCE: Mutex<String> = Mutex::new(ROOT_DISK.to_string());
    static ref LOOP_DEVICES: Mutex<Vec<LoopDeviceState>> = Mutex::new(
        (0..LOOP_DEVICE_COUNT)
            .map(|_| LoopDeviceState::new())
            .collect()
    );
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
        major: DEV_VIRTIO_BLK_MAJOR,
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
            major: DEV_VIRTIO_BLK_MAJOR,
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
    let mut paths: Vec<String> = DEVICES
        .lock()
        .iter()
        .map(|entry| entry.path.clone())
        .collect();
    paths.push(LOOP_CONTROL.to_string());
    for index in 0..LOOP_DEVICE_COUNT {
        paths.push(loop_device_path(index));
        paths.push(loop_device_alt_path(index));
        paths.push(loop_device_block_path(index));
    }
    paths
}

pub fn device_numbers_for_path(path: &str) -> Option<(u32, u32)> {
    if path == LOOP_CONTROL {
        return Some((DEV_LOOP_CONTROL_MAJOR, DEV_LOOP_CONTROL_MINOR));
    }
    if let Some(index) = loop_index_for_path(path) {
        return Some((DEV_LOOP_MAJOR, index as u32));
    }
    DEVICES
        .lock()
        .iter()
        .find(|entry| entry.path == path)
        .map(|entry| (entry.major, entry.minor))
}

pub fn minor_for_path(path: &str) -> Option<u32> {
    device_numbers_for_path(path).map(|(_, minor)| minor)
}

pub fn range_for_path(path: &str) -> Option<Arc<BlockRange>> {
    DEVICES
        .lock()
        .iter()
        .find(|entry| entry.path == path)
        .map(|entry| entry.range.clone())
}

pub fn block_device_available(path: &str) -> bool {
    if range_for_path(path).is_some() {
        return true;
    }
    if loop_index_for_path(path).is_some() {
        return true;
    }
    loop_index_for_path(path)
        .and_then(|index| loop_is_attached(index).ok())
        .unwrap_or(false)
}

pub fn is_root_source(path: &str) -> bool {
    *ROOT_SOURCE.lock() == path
}

pub fn root_source_path() -> String {
    ROOT_SOURCE.lock().clone()
}

pub fn root_device_numbers() -> Option<(u32, u32)> {
    let path = root_source_path();
    device_numbers_for_path(&path)
}

pub fn read_root_blocks(offsets: &[usize]) -> Result<Vec<Vec<u8>>, SysErrNo> {
    let path = root_source_path();
    let range = range_for_path(&path).ok_or(SysErrNo::ENODEV)?;
    range.read_cached_blocks(offsets)
}

#[derive(Clone, Debug)]
pub enum LoopBacking {
    MemFile { path: String },
    Ext4Regular { ino: u32 },
}

#[derive(Clone, Debug)]
struct LoopDeviceState {
    backing: Option<LoopBacking>,
    offset: usize,
}

impl LoopDeviceState {
    fn new() -> Self {
        Self {
            backing: None,
            offset: 0,
        }
    }
}

pub fn loop_control_path(path: &str) -> bool {
    path == LOOP_CONTROL
}

pub fn loop_device_path(index: usize) -> String {
    alloc::format!("/dev/loop{}", index)
}

pub fn loop_device_alt_path(index: usize) -> String {
    alloc::format!("/dev/loop/{}", index)
}

pub fn loop_device_block_path(index: usize) -> String {
    alloc::format!("/dev/block/loop{}", index)
}

pub fn loop_index_for_path(path: &str) -> Option<usize> {
    if let Some(tail) = path.strip_prefix("/dev/loop") {
        if !tail.is_empty() && tail.as_bytes().iter().all(|b| b.is_ascii_digit()) {
            return tail
                .parse::<usize>()
                .ok()
                .filter(|index| *index < LOOP_DEVICE_COUNT);
        }
    }
    if let Some(tail) = path.strip_prefix("/dev/loop/") {
        return tail
            .parse::<usize>()
            .ok()
            .filter(|index| *index < LOOP_DEVICE_COUNT);
    }
    if let Some(tail) = path.strip_prefix("/dev/block/loop") {
        return tail
            .parse::<usize>()
            .ok()
            .filter(|index| *index < LOOP_DEVICE_COUNT);
    }
    None
}

pub fn first_free_loop() -> Option<usize> {
    LOOP_DEVICES
        .lock()
        .iter()
        .enumerate()
        .find_map(|(index, state)| state.backing.is_none().then_some(index))
}

pub fn loop_is_attached(index: usize) -> Result<bool, SysErrNo> {
    LOOP_DEVICES
        .lock()
        .get(index)
        .map(|state| state.backing.is_some())
        .ok_or(SysErrNo::ENODEV)
}

pub fn attach_loop(index: usize, backing: LoopBacking) -> Result<(), SysErrNo> {
    let mut devices = LOOP_DEVICES.lock();
    let state = devices.get_mut(index).ok_or(SysErrNo::ENODEV)?;
    if state.backing.is_some() {
        return Err(SysErrNo::EBUSY);
    }
    if let LoopBacking::Ext4Regular { ino } = backing {
        crate::fs::ext4_vol::open_regular_ino(ino);
    }
    state.backing = Some(backing);
    state.offset = 0;
    Ok(())
}

pub fn detach_loop(index: usize) -> Result<(), SysErrNo> {
    let mut devices = LOOP_DEVICES.lock();
    let state = devices.get_mut(index).ok_or(SysErrNo::ENODEV)?;
    if state.backing.is_none() {
        return Err(SysErrNo::ENXIO);
    }
    if let Some(LoopBacking::Ext4Regular { ino }) = state.backing.as_ref() {
        crate::fs::ext4_vol::close_regular_ino(*ino);
    }
    state.backing = None;
    state.offset = 0;
    Ok(())
}

fn loop_backing(index: usize) -> Result<LoopBacking, SysErrNo> {
    LOOP_DEVICES
        .lock()
        .get(index)
        .ok_or(SysErrNo::ENODEV)?
        .backing
        .clone()
        .ok_or(SysErrNo::ENXIO)
}

pub fn loop_size(index: usize) -> Result<usize, SysErrNo> {
    match loop_backing(index)? {
        LoopBacking::MemFile { path } => crate::fs::MEM_FS
            .lock()
            .get_file(&path)
            .map(|file| file.size())
            .ok_or(SysErrNo::ENOENT),
        LoopBacking::Ext4Regular { ino } => crate::fs::ext4_vol::regular_file_size(ino),
    }
}

pub fn loop_read_at(index: usize, offset: usize, buf: &mut [u8]) -> Result<usize, SysErrNo> {
    match loop_backing(index)? {
        LoopBacking::MemFile { path } => crate::fs::MEM_FS.lock().read_file_at(&path, offset, buf),
        LoopBacking::Ext4Regular { ino } => crate::fs::ext4_vol::ext4_read_at(ino, offset, buf),
    }
}

pub fn loop_write_at(index: usize, offset: usize, buf: &[u8]) -> Result<usize, SysErrNo> {
    match loop_backing(index)? {
        LoopBacking::MemFile { path } => crate::fs::MEM_FS.lock().write_file_at(&path, offset, buf),
        LoopBacking::Ext4Regular { ino } => crate::fs::ext4_vol::ext4_write_at(ino, offset, buf),
    }
}
