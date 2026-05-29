//! 运行时 ext4：`VirtIO-BLK` + `mount_block_device` 后与 MemFS 并列作为读路径后端。
//!
//! `ext4_rs::ext4_file_open` 在 crates.io 版中有误（打开类型被写成目录），此处不用它；
//! 目录项逐级 `ext4_dir_get_entries`/`compare_name`，文件内容 [`Ext4::read_at`]。

use alloc::collections::BTreeSet;
use alloc::format;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;

use ext4_rs::{Errno, Ext4, Ext4Error, InodeFileType};

/// ext4 标准根 inode 号（与 `ext4_rs` 内部一致，crate 根未再导出该常量）。
const ROOT_INODE: u32 = 2;

pub(crate) fn map_ext4_err(e: Ext4Error) -> SysErrNo {
    match e.error() {
        Errno::ENOENT => SysErrNo::ENOENT,
        Errno::EEXIST => SysErrNo::EEXIST,
        Errno::ENOTDIR => SysErrNo::ENOTDIR,
        Errno::EISDIR => SysErrNo::EISDIR,
        Errno::EINVAL => SysErrNo::EINVAL,
        Errno::ENOSPC => SysErrNo::ENOSPC,
        Errno::EROFS => SysErrNo::EROFS,
        Errno::EBADF => SysErrNo::EBADF,
        _ => SysErrNo::EIO,
    }
}

use lazy_static::lazy_static;
use spin::Mutex;

use crate::fs::normalize_path;
use crate::utils::error::SysErrNo;

lazy_static! {
    /// 挂载后的 Ext4（无盘或未探测到 virtio 时为 `None`）
    pub static ref ROOT_EXT4: Mutex<Option<Arc<Ext4>>> = Mutex::new(None);
}

pub fn is_ext4_mounted() -> bool {
    ROOT_EXT4.lock().is_some()
}

pub fn mount_block_device(device: Arc<dyn ext4_rs::BlockDevice>) {
    log::info!("[fs] Attempting to mount ext4 from block device...");

    // Read first block. For 4KB block ext4: block0 = boot(1024) + superblock(1024) + bgd(2048).
    // Superblock magic (0xEF53 LE) is at superblock offset 0x38 = absolute byte 1024+0x38 = 0x438.
    // In the returned block buffer, it's at index 0x438.
    let block0 = device.read_offset(0);
    log::info!("[fs] read_offset(0) returned {} bytes", block0.len());

    if block0.len() < 0x438 + 2 {
        log::error!("[fs] ext4: buffer too small ({} < {})", block0.len(), 0x438 + 2);
        return;
    }

    let magic = u16::from_le_bytes([block0[0x438], block0[0x439]]);
    log::info!("[fs] ext4: superblock magic = {:#x} (expect 0xef53)", magic);
    if magic != 0xef53 {
        log::error!(
            "[fs] ext4: INVALID superblock magic {:#x} != 0xef53. Image may not be a valid ext4 filesystem.",
            magic
        );
        return;
    }

    // block_size: stored as log2 at superblock offset 0x18 = block0[1024+0x18] = block0[0x418]
    let log_block_size = u32::from_le_bytes([
        block0[0x418],
        block0[0x419],
        block0[0x41a],
        block0[0x41b],
    ]) as usize;
    let block_size = 1024usize << log_block_size;
    log::info!("[fs] ext4: log_block_size={}, block_size={}", log_block_size, block_size);

    let fs = Ext4::open(device);
    let root_count = fs.ext4_dir_get_entries(ROOT_INODE).len();
    match root_count {
        0 => log::warn!("[fs] EXT4 root dir appears empty — expected test scripts here"),
        n => log::info!("[fs] EXT4 root dir has {} entries", n),
    }
    *ROOT_EXT4.lock() = Some(Arc::new(fs));
    log::info!("[fs] EXT4 mounted as runtime root backing");
}

/// 卸载运行时 ext4 根（`umount2` / 回退 MemFS）。
pub fn unmount_root() {
    *ROOT_EXT4.lock() = None;
    log::info!("[fs] EXT4 unmounted");
}

fn split_parent_name(norm: &str) -> Result<(String, String), SysErrNo> {
    let n = norm.trim_end_matches('/');
    if n.is_empty() || n == "/" {
        return Err(SysErrNo::EINVAL);
    }
    if let Some(pos) = n.rfind('/') {
        if pos == 0 {
            Ok((String::from("/"), n[1..].into()))
        } else {
            Ok((n[..pos].into(), n[pos + 1..].into()))
        }
    } else {
        Err(SysErrNo::EINVAL)
    }
}

/// 删除 ext4 上的普通文件（非目录）。
pub fn unlink_regular_file(path: &str) -> Result<(), SysErrNo> {
    let fs = ROOT_EXT4.lock().clone().ok_or(SysErrNo::ENOENT)?;
    let norm = normalize_path(path);
    let (parent_path, name) = split_parent_name(&norm)?;
    let Some((parent_ino, _)) = resolve_existing(&fs, &parent_path) else {
        return Err(SysErrNo::ENOENT);
    };
    let Some((child_ino, is_dir)) = resolve_existing(&fs, &norm) else {
        return Err(SysErrNo::ENOENT);
    };
    if is_dir {
        return Err(SysErrNo::EISDIR);
    }
    let mut parent_ref = fs.get_inode_ref(parent_ino);
    let mut child_ref = fs.get_inode_ref(child_ino);
    fs.unlink(&mut parent_ref, &mut child_ref, &name)
        .map_err(map_ext4_err)?;
    Ok(())
}

pub fn mkdir_ext4(path: &str) -> Result<(), SysErrNo> {
    let fs = ROOT_EXT4.lock().clone().ok_or(SysErrNo::ENOENT)?;
    fs.dir_mk(path).map_err(map_ext4_err)?;
    Ok(())
}

/// 创建普通文件（已存在则由 `generic_open` 语义处理）。
pub fn create_regular_ext4(path: &str) -> Result<u32, SysErrNo> {
    let fs = ROOT_EXT4.lock().clone().ok_or(SysErrNo::ENOENT)?;
    let norm = normalize_path(path);
    let mut parent = ROOT_INODE;
    let mut nameoff = 0u32;
    fs.generic_open(
        &norm,
        &mut parent,
        true,
        InodeFileType::S_IFREG.bits(),
        &mut nameoff,
    )
    .map_err(map_ext4_err)
}

pub fn truncate_regular_ext4(path: &str, size: u64) -> Result<(), SysErrNo> {
    let fs = ROOT_EXT4.lock().clone().ok_or(SysErrNo::ENOENT)?;
    let Some((ino, is_dir)) = resolve_existing(&fs, path) else {
        return Err(SysErrNo::ENOENT);
    };
    if is_dir {
        return Err(SysErrNo::EISDIR);
    }
    let mut iref = fs.get_inode_ref(ino);
    fs.truncate_inode(&mut iref, size).map_err(map_ext4_err)?;
    Ok(())
}

pub fn ext4_read_at(ino: u32, offset: usize, buf: &mut [u8]) -> Result<usize, SysErrNo> {
    let fs = ROOT_EXT4.lock().clone().ok_or(SysErrNo::ENOENT)?;
    fs.read_at(ino, offset, buf).map_err(map_ext4_err)
}

pub fn ext4_write_at(ino: u32, offset: usize, buf: &[u8]) -> Result<usize, SysErrNo> {
    let fs = ROOT_EXT4.lock().clone().ok_or(SysErrNo::ENOENT)?;
    fs.write_at(ino, offset, buf).map_err(map_ext4_err)
}

pub fn lookup_path(path: &str) -> Option<(u32, bool)> {
    let fs = ROOT_EXT4.lock().clone()?;
    resolve_existing(&fs, path)
}

pub fn regular_file_size(ino: u32) -> Result<usize, SysErrNo> {
    let fs = ROOT_EXT4.lock().clone().ok_or(SysErrNo::ENOENT)?;
    Ok(fs.get_inode_ref(ino).inode.size() as usize)
}

fn find_child_ino(fs: &Ext4, parent_ino: u32, name: &str) -> Option<u32> {
    for e in fs.ext4_dir_get_entries(parent_ino) {
        if e.unused() {
            continue;
        }
        if e.compare_name(name) {
            return Some(e.inode);
        }
    }
    None
}

/// 解析已存在的绝对路径 → (inode, 是否为目录)。不存在或非法则 `None`。
fn resolve_existing(fs: &Ext4, path: &str) -> Option<(u32, bool)> {
    let n = normalize_path(path);
    if !n.starts_with('/') {
        return None;
    }
    let tail = n.trim_matches('/');
    let parts: Vec<&str> = if tail.is_empty() {
        Vec::new()
    } else {
        tail.split('/').filter(|p| !p.is_empty()).collect()
    };

    if parts.is_empty() {
        let r = fs.get_inode_ref(ROOT_INODE);
        return Some((ROOT_INODE, r.inode.is_dir()));
    }

    let mut parent = ROOT_INODE;
    for (i, comp) in parts.iter().enumerate() {
        let ino = find_child_ino(fs, parent, comp)?;
        if i + 1 == parts.len() {
            let r = fs.get_inode_ref(ino);
            return Some((ino, r.inode.is_dir()));
        }
        if !fs.get_inode_ref(ino).inode.is_dir() {
            return None;
        }
        parent = ino;
    }
    None
}

/// 整块读入普通文件（用于 `execve` / harness）。目录或不存在返回 `None`。
pub fn slurp_regular_file(path: &str) -> Option<Vec<u8>> {
    let fs = ROOT_EXT4.lock().clone()?;
    let (ino, is_dir) = resolve_existing(&fs, path)?;
    if is_dir || !fs.get_inode_ref(ino).inode.is_file() {
        return None;
    }

    let mut out = Vec::new();
    let mut off = 0usize;
    loop {
        let mut chunk = [0u8; 4096];
        let n = fs.read_at(ino, off, &mut chunk).ok()?;
        if n == 0 {
            break;
        }
        out.extend_from_slice(&chunk[..n]);
        off += n;
    }
    Some(out)
}

pub fn ext4_regular_file_exists(path: &str) -> bool {
    let Some(fs) = ROOT_EXT4.lock().clone() else {
        return false;
    };
    resolve_existing(&fs, path)
        .map(|(ino, is_dir)| !is_dir && fs.get_inode_ref(ino).inode.is_file())
        .unwrap_or(false)
}

pub fn ext4_dir_path_exists(path: &str) -> bool {
    let Some(fs) = ROOT_EXT4.lock().clone() else {
        return false;
    };
    resolve_existing(&fs, path)
        .map(|(_, is_dir)| is_dir)
        .unwrap_or(false)
}

/// 枚举目录单层子项（文件名 + 是否为目录）。
pub fn ext4_list_dir(dir_path: &str) -> Result<Vec<(String, bool)>, SysErrNo> {
    let fs = ROOT_EXT4.lock().clone().ok_or(SysErrNo::ENOENT)?;
    let (ino, is_dir) = resolve_existing(&fs, dir_path).ok_or(SysErrNo::ENOENT)?;
    if !is_dir {
        return Err(SysErrNo::ENOTDIR);
    }

    let mut out = Vec::new();
    for e in fs.ext4_dir_get_entries(ino) {
        if e.unused() {
            continue;
        }
        let name = e.get_name();
        if name == "." || name == ".." {
            continue;
        }
        let child_ino = e.inode;
        let is_subdir = fs.get_inode_ref(child_ino).inode.is_dir();
        out.push((name, is_subdir));
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}

/// 枚举 ext4 目录项（按 inode 号），用于 `getdents64` 对 `Ext4Dir` fd 的支持。
/// 返回 `(child_ino, name, is_dir)` 元组的 `Vec`，跳过 `.` 和 `..`。
pub fn ext4_list_dir_by_ino(ino: u32) -> Result<Vec<(u32, String, bool)>, SysErrNo> {
    let fs = ROOT_EXT4.lock().clone().ok_or(SysErrNo::ENOENT)?;
    let mut out = Vec::new();
    for e in fs.ext4_dir_get_entries(ino) {
        if e.unused() {
            continue;
        }
        let name = e.get_name();
        if name == "." || name == ".." {
            continue;
        }
        let child_ino = e.inode;
        let is_subdir = fs.get_inode_ref(child_ino).inode.is_dir();
        out.push((child_ino, name, is_subdir));
    }
    Ok(out)
}

fn ext4_gather_file_paths(fs: &Ext4, dir_path: &str, parent_ino: u32, out: &mut Vec<String>) {
    for e in fs.ext4_dir_get_entries(parent_ino) {
        if e.unused() {
            continue;
        }
        let name = e.get_name();
        if name == "." || name == ".." {
            continue;
        }
        let child_ino = e.inode;
        let full_path = if dir_path == "/" {
            format!("/{}", name)
        } else {
            format!("{}/{}", dir_path, name)
        };

        let inode_ref = fs.get_inode_ref(child_ino);
        if inode_ref.inode.is_dir() {
            ext4_gather_file_paths(fs, &full_path, child_ino, out);
        } else if inode_ref.inode.is_file() {
            out.push(full_path);
        }
    }
}

/// 枚举卷上全部普通文件的绝对路径（用于 harness 发现 *_testcode.sh）。
pub fn ext4_list_all_file_paths() -> Vec<String> {
    let Some(fs) = ROOT_EXT4.lock().clone() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    ext4_gather_file_paths(&fs, "/", ROOT_INODE, &mut out);
    out.sort();
    out
}

/// MemFS + ext4：合并列出文件路径，`MEM_FS` 中的路径优先，避免重复。
pub fn merged_list_all_file_paths(mem_paths: Vec<String>) -> Vec<String> {
    let mut set: BTreeSet<String> = mem_paths.into_iter().collect();
    let mut v: Vec<String> = set.iter().cloned().collect();
    if is_ext4_mounted() {
        for p in ext4_list_all_file_paths() {
            if set.insert(p.clone()) {
                v.push(p);
            }
        }
    }
    v.sort();
    v
}
