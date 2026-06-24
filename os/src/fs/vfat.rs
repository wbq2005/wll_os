use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;

use crate::utils::error::SysErrNo;

use super::block_dev::BlockRange;
use super::fd;

const SECTOR_SIZE: usize = 512;
const ATTR_READ_ONLY: u8 = 0x01;
const ATTR_DIRECTORY: u8 = 0x10;
const ATTR_VOLUME_ID: u8 = 0x08;
const ATTR_LONG_NAME: u8 = 0x0f;
const END_OF_CHAIN: u32 = 0x0fff_fff8;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FatKind {
    Fat16,
    Fat32,
}

#[derive(Clone, Debug)]
pub struct VfatMetadata {
    pub is_dir: bool,
    pub readonly: bool,
    pub size: u32,
}

#[derive(Clone, Debug)]
struct DirEntry {
    name: String,
    is_dir: bool,
    readonly: bool,
    cluster: u32,
    size: u32,
}

#[derive(Clone)]
pub struct VfatVolume {
    device: Arc<BlockRange>,
    bytes_per_sector: usize,
    sectors_per_cluster: usize,
    reserved_sectors: u32,
    root_entry_count: u32,
    root_cluster: u32,
    first_data_sector: u32,
    root_dir_sector: u32,
    fat_kind: FatKind,
}

fn read_u16_le(buf: &[u8], off: usize) -> Option<u16> {
    Some(u16::from_le_bytes([*buf.get(off)?, *buf.get(off + 1)?]))
}

fn read_u32_le(buf: &[u8], off: usize) -> Option<u32> {
    Some(u32::from_le_bytes([
        *buf.get(off)?,
        *buf.get(off + 1)?,
        *buf.get(off + 2)?,
        *buf.get(off + 3)?,
    ]))
}

fn valid_power_of_two(value: usize, min: usize, max: usize) -> bool {
    value >= min && value <= max && value.is_power_of_two()
}

fn short_name(raw: &[u8]) -> Option<String> {
    if raw.len() < 11 || raw[0] == 0x00 || raw[0] == 0xe5 {
        return None;
    }
    let base = core::str::from_utf8(&raw[0..8]).ok()?.trim_end();
    if base.is_empty() {
        return None;
    }
    let ext = core::str::from_utf8(&raw[8..11]).ok()?.trim_end();
    if ext.is_empty() {
        Some(base.to_ascii_lowercase())
    } else {
        Some(alloc::format!("{}.{}", base, ext).to_ascii_lowercase())
    }
}

impl VfatVolume {
    pub fn open(device: Arc<BlockRange>) -> Result<Self, SysErrNo> {
        let mut boot = [0u8; SECTOR_SIZE];
        device.read_at(0, &mut boot)?;
        if boot[510] != 0x55 || boot[511] != 0xaa {
            return Err(SysErrNo::EINVAL);
        }

        let bytes_per_sector = read_u16_le(&boot, 11).ok_or(SysErrNo::EINVAL)? as usize;
        let sectors_per_cluster = boot[13] as usize;
        let reserved_sectors = read_u16_le(&boot, 14).ok_or(SysErrNo::EINVAL)? as u32;
        let fat_count = boot[16] as u32;
        let root_entry_count = read_u16_le(&boot, 17).ok_or(SysErrNo::EINVAL)? as u32;
        let total_sectors_16 = read_u16_le(&boot, 19).ok_or(SysErrNo::EINVAL)? as u32;
        let fat_size_16 = read_u16_le(&boot, 22).ok_or(SysErrNo::EINVAL)? as u32;
        let total_sectors_32 = read_u32_le(&boot, 32).ok_or(SysErrNo::EINVAL)?;
        let fat_size_32 = read_u32_le(&boot, 36).ok_or(SysErrNo::EINVAL)?;
        let root_cluster = read_u32_le(&boot, 44).unwrap_or(2);

        if !valid_power_of_two(bytes_per_sector, 512, 4096)
            || !valid_power_of_two(sectors_per_cluster, 1, 128)
            || reserved_sectors == 0
            || fat_count == 0
        {
            return Err(SysErrNo::EINVAL);
        }

        let total_sectors = if total_sectors_16 != 0 {
            total_sectors_16
        } else {
            total_sectors_32
        };
        let fat_size_sectors = if fat_size_16 != 0 {
            fat_size_16
        } else {
            fat_size_32
        };
        if total_sectors == 0 || fat_size_sectors == 0 {
            return Err(SysErrNo::EINVAL);
        }

        let root_dir_sectors =
            ((root_entry_count * 32) + bytes_per_sector as u32 - 1) / bytes_per_sector as u32;
        let first_data_sector = reserved_sectors + fat_count * fat_size_sectors + root_dir_sectors;
        if total_sectors <= first_data_sector {
            return Err(SysErrNo::EINVAL);
        }
        let data_sectors = total_sectors - first_data_sector;
        let cluster_count = data_sectors / sectors_per_cluster as u32;
        let fat_kind = if cluster_count < 4085 {
            return Err(SysErrNo::EINVAL);
        } else if cluster_count < 65525 {
            FatKind::Fat16
        } else {
            FatKind::Fat32
        };
        if fat_kind == FatKind::Fat32 && root_cluster < 2 {
            return Err(SysErrNo::EINVAL);
        }

        Ok(Self {
            device,
            bytes_per_sector,
            sectors_per_cluster,
            reserved_sectors,
            root_entry_count,
            root_cluster,
            first_data_sector,
            root_dir_sector: reserved_sectors + fat_count * fat_size_sectors,
            fat_kind,
        })
    }

    pub fn metadata(&self, path: &str) -> Result<VfatMetadata, SysErrNo> {
        if super::normalize_path(path) == "/" {
            return Ok(VfatMetadata {
                is_dir: true,
                readonly: true,
                size: 0,
            });
        }
        let entry = self.lookup(path)?;
        Ok(VfatMetadata {
            is_dir: entry.is_dir,
            readonly: entry.readonly,
            size: entry.size,
        })
    }

    pub fn list_dir(&self, path: &str) -> Result<Vec<fd::DirEntryRecord>, SysErrNo> {
        let entries = self.read_dir_for_path(path)?;
        Ok(entries
            .into_iter()
            .filter(|entry| entry.name != "." && entry.name != "..")
            .map(|entry| fd::DirEntryRecord {
                name: entry.name,
                is_dir: entry.is_dir,
            })
            .collect())
    }

    pub fn read_file(&self, path: &str) -> Result<Vec<u8>, SysErrNo> {
        let entry = self.lookup(path)?;
        if entry.is_dir {
            return Err(SysErrNo::EISDIR);
        }
        if entry.cluster < 2 || entry.size == 0 {
            return Ok(Vec::new());
        }
        let mut data = self.read_cluster_chain(entry.cluster)?;
        data.truncate(entry.size as usize);
        Ok(data)
    }

    fn lookup(&self, path: &str) -> Result<DirEntry, SysErrNo> {
        let norm = super::normalize_path(path);
        let mut entries = self.read_root_dir()?;
        let mut components = norm.split('/').filter(|part| !part.is_empty()).peekable();
        while let Some(component) = components.next() {
            let entry = entries
                .iter()
                .find(|entry| entry.name.eq_ignore_ascii_case(component))
                .cloned()
                .ok_or(SysErrNo::ENOENT)?;
            if components.peek().is_none() {
                return Ok(entry);
            }
            if !entry.is_dir {
                return Err(SysErrNo::ENOTDIR);
            }
            entries = self.read_dir_entry(&entry)?;
        }
        Err(SysErrNo::ENOENT)
    }

    fn read_dir_for_path(&self, path: &str) -> Result<Vec<DirEntry>, SysErrNo> {
        let norm = super::normalize_path(path);
        if norm == "/" {
            return self.read_root_dir();
        }
        let entry = self.lookup(&norm)?;
        if !entry.is_dir {
            return Err(SysErrNo::ENOTDIR);
        }
        self.read_dir_entry(&entry)
    }

    fn read_dir_entry(&self, entry: &DirEntry) -> Result<Vec<DirEntry>, SysErrNo> {
        if entry.cluster < 2 {
            return Ok(Vec::new());
        }
        let data = self.read_cluster_chain(entry.cluster)?;
        Ok(parse_dir_entries(&data))
    }

    fn read_root_dir(&self) -> Result<Vec<DirEntry>, SysErrNo> {
        match self.fat_kind {
            FatKind::Fat32 => {
                let data = self.read_cluster_chain(self.root_cluster)?;
                Ok(parse_dir_entries(&data))
            }
            FatKind::Fat16 => {
                let bytes = self.root_entry_count as usize * 32;
                let mut data = vec![0u8; bytes];
                let offset = self.root_dir_sector as usize * self.bytes_per_sector;
                self.device.read_at(offset, &mut data)?;
                Ok(parse_dir_entries(&data))
            }
        }
    }

    fn cluster_offset(&self, cluster: u32) -> Result<usize, SysErrNo> {
        if cluster < 2 {
            return Err(SysErrNo::EINVAL);
        }
        let sector = self
            .first_data_sector
            .checked_add((cluster - 2).saturating_mul(self.sectors_per_cluster as u32))
            .ok_or(SysErrNo::EINVAL)?;
        (sector as usize)
            .checked_mul(self.bytes_per_sector)
            .ok_or(SysErrNo::EINVAL)
    }

    fn read_cluster(&self, cluster: u32) -> Result<Vec<u8>, SysErrNo> {
        let len = self.sectors_per_cluster * self.bytes_per_sector;
        let mut data = vec![0u8; len];
        let offset = self.cluster_offset(cluster)?;
        self.device.read_at(offset, &mut data)?;
        Ok(data)
    }

    fn read_cluster_chain(&self, start: u32) -> Result<Vec<u8>, SysErrNo> {
        let mut cluster = start;
        let mut data = Vec::new();
        let mut guard = 0usize;
        while cluster >= 2 && cluster < END_OF_CHAIN {
            data.extend_from_slice(&self.read_cluster(cluster)?);
            cluster = self.next_cluster(cluster)?;
            guard += 1;
            if guard > 1_000_000 {
                return Err(SysErrNo::ELOOP);
            }
        }
        Ok(data)
    }

    fn next_cluster(&self, cluster: u32) -> Result<u32, SysErrNo> {
        let fat_offset = match self.fat_kind {
            FatKind::Fat16 => cluster as usize * 2,
            FatKind::Fat32 => cluster as usize * 4,
        };
        let abs = self.reserved_sectors as usize * self.bytes_per_sector + fat_offset;
        match self.fat_kind {
            FatKind::Fat16 => {
                let mut raw = [0u8; 2];
                self.device.read_at(abs, &mut raw)?;
                let value = u16::from_le_bytes(raw) as u32;
                if value >= 0xfff8 {
                    Ok(END_OF_CHAIN)
                } else {
                    Ok(value)
                }
            }
            FatKind::Fat32 => {
                let mut raw = [0u8; 4];
                self.device.read_at(abs, &mut raw)?;
                Ok(u32::from_le_bytes(raw) & 0x0fff_ffff)
            }
        }
    }
}

fn parse_dir_entries(data: &[u8]) -> Vec<DirEntry> {
    let mut entries = Vec::new();
    for raw in data.chunks_exact(32) {
        if raw[0] == 0x00 {
            break;
        }
        if raw[0] == 0xe5 || raw[11] == ATTR_LONG_NAME || (raw[11] & ATTR_VOLUME_ID) != 0 {
            continue;
        }
        let Some(name) = short_name(&raw[0..11]) else {
            continue;
        };
        let high = u16::from_le_bytes([raw[20], raw[21]]) as u32;
        let low = u16::from_le_bytes([raw[26], raw[27]]) as u32;
        let cluster = (high << 16) | low;
        let size = u32::from_le_bytes([raw[28], raw[29], raw[30], raw[31]]);
        entries.push(DirEntry {
            name,
            is_dir: (raw[11] & ATTR_DIRECTORY) != 0,
            readonly: (raw[11] & ATTR_READ_ONLY) != 0,
            cluster,
            size,
        });
    }
    entries
}
