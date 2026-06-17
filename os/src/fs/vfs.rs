//! 最薄 VFS：统一 MemFS + ext4 后端的查找与变更入口；`mount`/`umount2` 语义见 `syscall::fs`。

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::string::String;
use alloc::vec::Vec;
use lazy_static::lazy_static;
use spin::Mutex;

use crate::utils::error::SysErrNo;

use super::ext4_vol;
use super::fd;
use super::{normalize_path, MEM_FS};

const S_IFDIR: u32 = 0o040000;
const S_IFIFO: u32 = 0o010000;
const S_IFCHR: u32 = 0o020000;
const S_IFREG: u32 = 0o100000;
const S_IFSOCK: u32 = 0o140000;
const DEV_NULL_MAJOR: u32 = 1;
const DEV_NULL_MINOR: u32 = 3;
const DEV_ZERO_MAJOR: u32 = 1;
const DEV_ZERO_MINOR: u32 = 5;

lazy_static! {
    static ref WHITEOUTS: Mutex<BTreeSet<String>> = Mutex::new(BTreeSet::new());
}

#[derive(Clone, Copy, Debug)]
pub struct VfsStatFs {
    pub f_type: usize,
    pub f_bsize: usize,
    pub f_blocks: usize,
    pub f_bfree: usize,
    pub f_bavail: usize,
    pub f_files: usize,
    pub f_ffree: usize,
    pub f_namelen: usize,
    pub f_frsize: usize,
    pub f_flags: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VfsNodeKind {
    Regular,
    Directory,
    Symlink,
    Other,
}

#[derive(Clone, Copy, Debug)]
pub struct VfsMetadata {
    pub ino: u64,
    pub kind: VfsNodeKind,
    pub mode: u32,
    pub nlink: u32,
    pub uid: u32,
    pub gid: u32,
    pub rdev_major: u32,
    pub rdev_minor: u32,
    pub size: u64,
    pub blocks: u64,
    pub atime_sec: isize,
    pub atime_nsec: isize,
    pub mtime_sec: isize,
    pub mtime_nsec: isize,
    pub ctime_sec: isize,
    pub ctime_nsec: isize,
}

pub fn is_removed(name: &str) -> bool {
    WHITEOUTS.lock().contains(&normalize_path(name))
}

fn clear_whiteout(name: &str) {
    WHITEOUTS.lock().remove(&normalize_path(name));
}

fn mark_whiteout(name: &str) {
    WHITEOUTS.lock().insert(normalize_path(name));
}

fn parent_path(path: &str) -> String {
    let norm = normalize_path(path);
    let trimmed = norm.trim_end_matches('/');
    if trimmed.is_empty() || trimmed == "/" {
        return String::from("/");
    }
    match trimmed.rfind('/') {
        Some(0) => String::from("/"),
        Some(pos) => String::from(&trimmed[..pos]),
        None => String::from("/"),
    }
}

fn pseudo_inode(path: &str) -> u64 {
    let mut hash = 1469598103934665603u64;
    for &b in path.as_bytes() {
        hash ^= b as u64;
        hash = hash.wrapping_mul(1099511628211);
    }
    hash & 0x7fff_ffff
}

fn regular_blocks(size: u64) -> u64 {
    size.div_ceil(512)
}

fn current_times() -> (isize, isize) {
    let (sec, usec) = crate::timer::get_timeval();
    (sec as isize, (usec * 1000) as isize)
}

fn kind_from_ext4(kind: ext4_vol::Ext4NodeKind) -> VfsNodeKind {
    match kind {
        ext4_vol::Ext4NodeKind::Regular => VfsNodeKind::Regular,
        ext4_vol::Ext4NodeKind::Directory => VfsNodeKind::Directory,
        ext4_vol::Ext4NodeKind::Symlink => VfsNodeKind::Symlink,
        ext4_vol::Ext4NodeKind::Other => VfsNodeKind::Other,
    }
}

fn metadata_from_ext4(meta: ext4_vol::Ext4Metadata) -> VfsMetadata {
    let kind = match meta.mode & 0o170000 {
        S_IFREG => VfsNodeKind::Regular,
        S_IFDIR => VfsNodeKind::Directory,
        0o120000 => VfsNodeKind::Symlink,
        _ => VfsNodeKind::Other,
    };
    VfsMetadata {
        ino: meta.ino as u64,
        kind,
        mode: meta.mode,
        nlink: meta.nlink,
        uid: meta.uid,
        gid: meta.gid,
        rdev_major: 0,
        rdev_minor: 0,
        size: meta.size,
        blocks: meta.blocks.max(regular_blocks(meta.size)),
        atime_sec: meta.atime_sec,
        atime_nsec: meta.atime_nsec,
        mtime_sec: meta.mtime_sec,
        mtime_nsec: meta.mtime_nsec,
        ctime_sec: meta.ctime_sec,
        ctime_nsec: meta.ctime_nsec,
    }
}

fn synthetic_metadata(
    ino_key: &str,
    kind: VfsNodeKind,
    mode: u32,
    size: u64,
    nlink: u32,
) -> VfsMetadata {
    let (sec, nsec) = current_times();
    VfsMetadata {
        ino: pseudo_inode(ino_key),
        kind,
        mode,
        nlink,
        uid: 0,
        gid: 0,
        rdev_major: 0,
        rdev_minor: 0,
        size,
        blocks: regular_blocks(size),
        atime_sec: sec,
        atime_nsec: nsec,
        mtime_sec: sec,
        mtime_nsec: nsec,
        ctime_sec: sec,
        ctime_nsec: nsec,
    }
}

fn is_dev_null_path(path: &str) -> bool {
    matches!(path, "/dev/null" | "/glibc/dev/null" | "/musl/dev/null")
}

fn is_dev_zero_path(path: &str) -> bool {
    matches!(path, "/dev/zero" | "/glibc/dev/zero" | "/musl/dev/zero")
}

fn metadata_for_char_device(path: &str, major: u32, minor: u32) -> VfsMetadata {
    let mut meta = synthetic_metadata(path, VfsNodeKind::Other, S_IFCHR | 0o666, 0, 1);
    meta.rdev_major = major;
    meta.rdev_minor = minor;
    meta
}

fn metadata_for_mem_file(name: &str, len: usize, is_elf: bool) -> VfsMetadata {
    if is_dev_null_path(name) {
        return metadata_for_char_device(name, DEV_NULL_MAJOR, DEV_NULL_MINOR);
    }
    if is_dev_zero_path(name) {
        return metadata_for_char_device(name, DEV_ZERO_MAJOR, DEV_ZERO_MINOR);
    }
    let perm = if is_elf { 0o777 } else { 0o666 };
    synthetic_metadata(
        name,
        VfsNodeKind::Regular,
        S_IFREG | perm,
        len as u64,
        1,
    )
}

fn metadata_from_mem_file(file: &super::MemFile) -> VfsMetadata {
    let mut meta = metadata_for_mem_file(&file.name, file.content.len(), is_elf_image(&file.content));
    meta.atime_sec = file.times.atime_sec;
    meta.atime_nsec = file.times.atime_nsec;
    meta.mtime_sec = file.times.mtime_sec;
    meta.mtime_nsec = file.times.mtime_nsec;
    meta.ctime_sec = file.times.ctime_sec;
    meta.ctime_nsec = file.times.ctime_nsec;
    meta
}

fn metadata_from_mem_fd(
    name: &str,
    content: &fd::MemFileContent,
    times: super::FileTimes,
) -> VfsMetadata {
    let mut meta = metadata_for_mem_file(name, content.len(), content.is_elf_image());
    meta.atime_sec = times.atime_sec;
    meta.atime_nsec = times.atime_nsec;
    meta.mtime_sec = times.mtime_sec;
    meta.mtime_nsec = times.mtime_nsec;
    meta.ctime_sec = times.ctime_sec;
    meta.ctime_nsec = times.ctime_nsec;
    meta
}

pub fn metadata(path: &str, follow_symlink: bool) -> Result<VfsMetadata, SysErrNo> {
    let norm = normalize_path(path);
    if is_removed(&norm) {
        return Err(SysErrNo::ENOENT);
    }

    {
        let mem = MEM_FS.lock();
        if mem.is_dir(&norm) {
            let entries = mem.list_dir(&norm)?;
            let (sec, nsec) = current_times();
            return Ok(VfsMetadata {
                ino: pseudo_inode(&norm),
                kind: VfsNodeKind::Directory,
                mode: S_IFDIR | 0o755,
                nlink: 1,
                uid: 0,
                gid: 0,
                rdev_major: 0,
                rdev_minor: 0,
                size: entries.len() as u64,
                blocks: regular_blocks(entries.len() as u64),
                atime_sec: sec,
                atime_nsec: nsec,
                mtime_sec: sec,
                mtime_nsec: nsec,
                ctime_sec: sec,
                ctime_nsec: nsec,
            });
        }
        if let Some(file) = mem.get_file(&norm) {
            return Ok(metadata_from_mem_file(file));
        }
    }

    let ext_path = match ext4_vol::lookup_kind(&norm) {
        Some((_ino, ext4_vol::Ext4NodeKind::Symlink)) if follow_symlink => {
            ext4_vol::resolve_symlinks(&norm)?
        }
        Some((_ino, kind)) => {
            let meta = ext4_vol::metadata(&norm)?;
            let mut out = metadata_from_ext4(meta);
            out.kind = kind_from_ext4(kind);
            return Ok(out);
        }
        None => return Err(missing_path_errno(&norm)),
    };

    ext4_vol::metadata(&ext_path).map(metadata_from_ext4)
}

pub fn metadata_for_fd(file: &fd::FileDescriptor) -> Result<VfsMetadata, SysErrNo> {
    match file {
        fd::FileDescriptor::Stdin => Ok(synthetic_metadata(
            "stdin",
            VfsNodeKind::Other,
            S_IFIFO | 0o444,
            0,
            1,
        )),
        fd::FileDescriptor::Stdout => Ok(synthetic_metadata(
            "stdout",
            VfsNodeKind::Other,
            S_IFIFO | 0o222,
            0,
            1,
        )),
        fd::FileDescriptor::Stderr => Ok(synthetic_metadata(
            "stderr",
            VfsNodeKind::Other,
            S_IFIFO | 0o222,
            0,
            1,
        )),
        fd::FileDescriptor::MemFile {
            name,
            content,
            times,
            linked,
            ..
        } => {
            if *linked {
                let mem = MEM_FS.lock();
                if let Some(file) = mem.get_file(name) {
                    Ok(metadata_from_mem_file(file))
                } else {
                    Ok(metadata_from_mem_fd(name, content, *times))
                }
            } else {
                Ok(metadata_from_mem_fd(name, content, *times))
            }
        }
        fd::FileDescriptor::MemDir { path, entries, .. } => Ok(synthetic_metadata(
            path,
            VfsNodeKind::Directory,
            S_IFDIR | 0o755,
            entries.len() as u64,
            1,
        )),
        fd::FileDescriptor::Ext4Regular { ino, .. } | fd::FileDescriptor::Ext4Dir { ino, .. } => {
            ext4_vol::metadata_by_ino(*ino).map(metadata_from_ext4)
        }
        fd::FileDescriptor::PipeRead { .. } => Ok(synthetic_metadata(
            "pipe-read",
            VfsNodeKind::Other,
            S_IFIFO | 0o444,
            0,
            1,
        )),
        fd::FileDescriptor::PipeWrite { .. } => Ok(synthetic_metadata(
            "pipe-write",
            VfsNodeKind::Other,
            S_IFIFO | 0o222,
            0,
            1,
        )),
        fd::FileDescriptor::Socket { .. } => Ok(synthetic_metadata(
            "socket",
            VfsNodeKind::Other,
            S_IFSOCK | 0o666,
            0,
            1,
        )),
    }
}

pub fn check_metadata_access(meta: &VfsMetadata, access_mode: usize) -> Result<(), SysErrNo> {
    const R_OK: usize = 4;
    const W_OK: usize = 2;
    const X_OK: usize = 1;
    let perm = meta.mode & 0o777;
    if (access_mode & R_OK) != 0 && (perm & 0o444) == 0 {
        return Err(SysErrNo::EACCES);
    }
    if (access_mode & W_OK) != 0 && (perm & 0o222) == 0 {
        return Err(SysErrNo::EACCES);
    }
    if (access_mode & X_OK) != 0 && (perm & 0o111) == 0 {
        return Err(SysErrNo::EACCES);
    }
    Ok(())
}

pub fn check_access(path: &str, follow_symlink: bool, access_mode: usize) -> Result<(), SysErrNo> {
    let meta = metadata(path, follow_symlink)?;
    check_metadata_access(&meta, access_mode)
}

pub fn check_fd_access(file: &fd::FileDescriptor, access_mode: usize) -> Result<(), SysErrNo> {
    let meta = metadata_for_fd(file)?;
    check_metadata_access(&meta, access_mode)
}

pub fn read_file(name: &str) -> Option<Vec<u8>> {
    let norm = normalize_path(name);
    if is_removed(&norm) {
        return None;
    }
    let m = MEM_FS.lock();
    if let Some(f) = m.get_file(&norm) {
        return Some(f.content.clone());
    }
    drop(m);
    ext4_vol::slurp_regular_file(&norm)
}

fn is_elf_image(data: &[u8]) -> bool {
    data.len() >= 4 && &data[..4] == b"\x7fELF"
}

pub fn read_executable_file(name: &str) -> Option<Vec<u8>> {
    let norm = normalize_path(name);
    if is_removed(&norm) {
        return None;
    }

    let mem_data = MEM_FS.lock().get_file(&norm).map(|f| f.content.clone());
    if mem_data.as_ref().is_some_and(|data| is_elf_image(data)) {
        return mem_data;
    }

    let ext4_data = ext4_vol::slurp_regular_file(&norm);
    if let Some(data) = ext4_data {
        if is_elf_image(&data) {
            if mem_data.is_some() {
                log::warn!(
                    "[fs] executable overlay for '{}' is not ELF; using ext4 backing",
                    norm
                );
            }
            return Some(data);
        }
        return mem_data.or(Some(data));
    }

    mem_data
}

fn basename(path: &str) -> &str {
    path.rsplit('/')
        .find(|part| !part.is_empty())
        .unwrap_or(path)
}

fn read_interpreter_logical(root: &str, logical: &str) -> Option<(String, String, Vec<u8>)> {
    let logical = normalize_path(logical);
    let host = super::apply_root(root, &logical);
    read_executable_file(&host).map(|data| (logical, host, data))
}

fn read_interpreter_host(logical: &str, host: &str) -> Option<(String, String, Vec<u8>)> {
    let logical = normalize_path(logical);
    let host = normalize_path(host);
    read_executable_file(&host).map(|data| (logical, host, data))
}

pub fn read_interpreter(root: &str, interp: &str) -> Option<(String, String, Vec<u8>)> {
    let interp = normalize_path(interp);
    if let Some(found) = read_interpreter_logical(root, &interp) {
        return Some(found);
    }

    let name = basename(&interp);
    let lib_candidate = alloc::format!("/lib/{}", name);
    if interp.starts_with("/lib64/") {
        if let Some(found) = read_interpreter_logical(root, &lib_candidate) {
            return Some(found);
        }
    }

    #[cfg(target_arch = "loongarch64")]
    {
        if name == "ld-linux-loongarch-lp64d.so.1" {
            let host = alloc::format!("/glibc/lib/{}", name);
            if let Some(found) = read_interpreter_host(&lib_candidate, &host) {
                return Some(found);
            }
        }
    }

    read_interpreter_logical(root, "/lib/libc.so")
}

pub fn file_exists(name: &str) -> bool {
    let norm = normalize_path(name);
    if is_removed(&norm) {
        return false;
    }
    MEM_FS.lock().get_file(&norm).is_some() || ext4_vol::ext4_file_path_exists(&norm)
}

pub fn dir_exists(name: &str) -> bool {
    let norm = normalize_path(name);
    if is_removed(&norm) {
        return false;
    }
    MEM_FS.lock().is_dir(&norm) || ext4_vol::ext4_dir_path_exists(&norm)
}

pub fn filesystem_magic() -> usize {
    if ext4_vol::is_ext4_mounted() {
        0xef53
    } else {
        0x0102_1994
    }
}

fn mem_statfs() -> VfsStatFs {
    let mem = MEM_FS.lock();
    let entries = mem.entry_count().max(1);
    let used_blocks = mem.total_file_bytes().div_ceil(4096).max(1);
    let total_blocks = used_blocks.saturating_add(4096);
    VfsStatFs {
        f_type: 0x0102_1994,
        f_bsize: 4096,
        f_blocks: total_blocks,
        f_bfree: total_blocks.saturating_sub(used_blocks),
        f_bavail: total_blocks.saturating_sub(used_blocks),
        f_files: entries.saturating_add(1024),
        f_ffree: 1024,
        f_namelen: 255,
        f_frsize: 4096,
        f_flags: 0,
    }
}

fn current_statfs() -> VfsStatFs {
    if let Some(info) = ext4_vol::statfs_info() {
        return VfsStatFs {
            f_type: 0xef53,
            f_bsize: info.block_size,
            f_blocks: info.blocks,
            f_bfree: info.free_blocks,
            f_bavail: info.free_blocks,
            f_files: info.files,
            f_ffree: info.free_files,
            f_namelen: info.max_name_len,
            f_frsize: info.block_size,
            f_flags: 0,
        };
    }
    mem_statfs()
}

pub fn statfs_for_path(path: &str) -> Result<VfsStatFs, SysErrNo> {
    metadata(path, true)?;
    Ok(current_statfs())
}

pub fn statfs_for_fd(file: &fd::FileDescriptor) -> Result<VfsStatFs, SysErrNo> {
    metadata_for_fd(file)?;
    Ok(current_statfs())
}

pub fn sync_fd(file: &fd::FileDescriptor, data_only: bool) -> Result<(), SysErrNo> {
    file.sync(data_only)
}

pub fn sync_all() -> Result<(), SysErrNo> {
    ext4_vol::flush_all_cached()?;
    Ok(())
}

pub fn remove_file(path: &str) -> Result<(), SysErrNo> {
    let norm = normalize_path(path);
    if is_removed(&norm) {
        return Err(SysErrNo::ENOENT);
    }
    if norm == "/" {
        return Err(SysErrNo::EISDIR);
    }
    {
        let mut m = MEM_FS.lock();
        if m.is_dir(&norm) {
            return Err(SysErrNo::EISDIR);
        }
        if m.get_file(&norm).is_some() {
            m.remove_file(&norm)?;
            mark_whiteout(&norm);
            return Ok(());
        }
    }

    match ext4_vol::lookup_kind(&norm) {
        Some((_ino, ext4_vol::Ext4NodeKind::Directory)) => Err(SysErrNo::EISDIR),
        Some(_) => {
            ext4_vol::unlink_non_dir(&norm)?;
            Ok(())
        }
        None => Err(missing_path_errno(&norm)),
    }
}

pub fn remove_dir(path: &str) -> Result<(), SysErrNo> {
    let norm = normalize_path(path);
    if is_removed(&norm) {
        return Err(SysErrNo::ENOENT);
    }
    if norm == "/" {
        return Err(SysErrNo::EBUSY);
    }
    {
        let mut m = MEM_FS.lock();
        if m.get_file(&norm).is_some() {
            return Err(SysErrNo::ENOTDIR);
        }
        if m.is_dir(&norm) {
            m.remove_dir(&norm)?;
            mark_whiteout(&norm);
            return Ok(());
        }
    }

    match ext4_vol::lookup_kind(&norm) {
        Some((_ino, ext4_vol::Ext4NodeKind::Directory)) => {
            if !list_dir(&norm)?.is_empty() {
                return Err(SysErrNo::ENOTEMPTY);
            }
            ext4_vol::remove_empty_dir_ext4(&norm)?;
            Ok(())
        }
        Some(_) => Err(SysErrNo::ENOTDIR),
        None => Err(missing_path_errno(&norm)),
    }
}

pub fn rename_path(old: &str, new: &str, no_replace: bool) -> Result<(), SysErrNo> {
    let old = normalize_path(old);
    let new = normalize_path(new);
    if is_removed(&old) {
        return Err(SysErrNo::ENOENT);
    }
    if no_replace && (file_exists(&new) || dir_exists(&new)) {
        return Err(SysErrNo::EEXIST);
    }

    let mut m = MEM_FS.lock();
    if m.exists(&old) {
        if !no_replace
            && (ext4_vol::ext4_file_path_exists(&new) || ext4_vol::ext4_dir_path_exists(&new))
        {
            return Err(SysErrNo::EEXIST);
        }
        let new_parent = parent_path(&new);
        if !m.is_dir(&new_parent) {
            drop(m);
            if ext4_vol::ext4_dir_path_exists(&new_parent) {
                return Err(SysErrNo::EXDEV);
            }
            return Err(missing_path_errno(&new));
        }
        let result = m.rename_path(&old, &new);
        if result.is_ok() {
            clear_whiteout(&new);
            mark_whiteout(&old);
        }
        return result;
    }
    drop(m);

    if ext4_vol::lookup_kind(&old).is_some() {
        let new_parent = parent_path(&new);
        if MEM_FS.lock().exists(&new) {
            return if no_replace {
                Err(SysErrNo::EEXIST)
            } else {
                Err(SysErrNo::EXDEV)
            };
        }
        if path_exists_non_dir(&new_parent) {
            return Err(SysErrNo::ENOTDIR);
        }
        if MEM_FS.lock().is_dir(&new_parent) && !ext4_vol::ext4_dir_path_exists(&new_parent) {
            return Err(SysErrNo::EXDEV);
        }
        ext4_vol::rename_ext4(&old, &new, no_replace)?;
        clear_whiteout(&new);
        return Ok(());
    }

    Err(missing_path_errno(&old))
}

pub fn link_path(old: &str, new: &str, follow_old: bool) -> Result<(), SysErrNo> {
    let old = normalize_path(old);
    let new = normalize_path(new);
    if is_removed(&old) {
        return Err(SysErrNo::ENOENT);
    }
    if file_exists(&new) || dir_exists(&new) {
        return Err(SysErrNo::EEXIST);
    }
    if MEM_FS.lock().exists(&old) {
        return Err(SysErrNo::EXDEV);
    }
    if ext4_vol::lookup_kind(&old).is_none() && path_exists_non_dir(&parent_path(&old)) {
        return Err(SysErrNo::ENOTDIR);
    }
    let new_parent = parent_path(&new);
    if MEM_FS.lock().is_dir(&new_parent) && !ext4_vol::ext4_dir_path_exists(&new_parent) {
        return Err(SysErrNo::EXDEV);
    }
    if path_exists_non_dir(&new_parent) {
        return Err(SysErrNo::ENOTDIR);
    }
    if !ext4_vol::ext4_dir_path_exists(&new_parent) {
        return Err(SysErrNo::ENOENT);
    }
    ext4_vol::link_ext4(&old, &new, follow_old)?;
    clear_whiteout(&new);
    Ok(())
}

pub fn create_symlink(target: &str, link_path: &str) -> Result<(), SysErrNo> {
    let norm = normalize_path(link_path);
    if file_exists(&norm) || dir_exists(&norm) {
        return Err(SysErrNo::EEXIST);
    }
    let parent = parent_path(&norm);
    if ext4_vol::ext4_dir_path_exists(&parent) {
        clear_whiteout(&norm);
        return ext4_vol::create_symlink_ext4(target, &norm);
    }
    if MEM_FS.lock().is_dir(&parent) {
        return Err(SysErrNo::EXDEV);
    }
    if path_exists_non_dir(&parent) {
        return Err(SysErrNo::ENOTDIR);
    }
    Err(SysErrNo::ENOENT)
}

pub fn read_link(path: &str) -> Result<String, SysErrNo> {
    let norm = normalize_path(path);
    if is_removed(&norm) {
        return Err(SysErrNo::ENOENT);
    }
    if MEM_FS.lock().exists(&norm) {
        return Err(SysErrNo::EINVAL);
    }
    match ext4_vol::lookup_kind(&norm) {
        Some((_ino, ext4_vol::Ext4NodeKind::Symlink)) => ext4_vol::readlink_ext4(&norm),
        Some(_) => Err(SysErrNo::EINVAL),
        None => Err(missing_path_errno(&norm)),
    }
}

pub fn truncate_path(path: &str, size: u64) -> Result<(), SysErrNo> {
    let norm = normalize_path(path);
    {
        let mut mem = MEM_FS.lock();
        if mem.is_dir(&norm) {
            return Err(SysErrNo::EISDIR);
        }
        if mem.get_file(&norm).is_some() {
            return mem.truncate_file(&norm, size as usize);
        }
    }
    let ext_path = match ext4_vol::lookup_kind(&norm) {
        Some((_ino, ext4_vol::Ext4NodeKind::Symlink)) => ext4_vol::resolve_symlinks(&norm)?,
        Some(_) => norm.clone(),
        None => return Err(missing_path_errno(&norm)),
    };
    ext4_vol::truncate_regular_ext4(&ext_path, size)
}

pub fn truncate_fd(file: &mut fd::FileDescriptor, size: u64) -> Result<(), SysErrNo> {
    if size > usize::MAX as u64 {
        return Err(SysErrNo::EFBIG);
    }
    file.truncate(size as usize)
}

pub fn set_times_path(
    path: &str,
    follow_symlink: bool,
    atime: Option<(isize, isize)>,
    mtime: Option<(isize, isize)>,
) -> Result<(), SysErrNo> {
    let norm = normalize_path(path);
    {
        let mut mem = MEM_FS.lock();
        if mem.is_dir(&norm) {
            return Ok(());
        }
        if mem.get_file(&norm).is_some() {
            return mem.set_file_times(&norm, atime, mtime);
        }
    }
    let ext_path = match ext4_vol::lookup_kind(&norm) {
        Some((_ino, ext4_vol::Ext4NodeKind::Symlink)) if follow_symlink => {
            ext4_vol::resolve_symlinks(&norm)?
        }
        Some(_) => norm.clone(),
        None => return Err(missing_path_errno(&norm)),
    };
    ext4_vol::set_times_path(&ext_path, atime, mtime)
}

pub fn set_times_fd(
    file: &mut fd::FileDescriptor,
    atime: Option<(isize, isize)>,
    mtime: Option<(isize, isize)>,
) -> Result<(), SysErrNo> {
    match file {
        fd::FileDescriptor::MemFile { name, times, .. } => {
            let _ = MEM_FS.lock().set_file_times(name, atime, mtime);
            times.set_access_modify(atime, mtime);
            Ok(())
        }
        fd::FileDescriptor::MemDir { .. } => Ok(()),
        fd::FileDescriptor::Ext4Regular { ino, .. } | fd::FileDescriptor::Ext4Dir { ino, .. } => {
            ext4_vol::set_times_ino(*ino, atime, mtime)
        }
        _ => Err(SysErrNo::EINVAL),
    }
}

pub fn create_dir_with_mode(path: &str, mode: u32) -> Result<(), SysErrNo> {
    let norm = normalize_path(path);
    if file_exists(&norm) || dir_exists(&norm) {
        return Err(SysErrNo::EEXIST);
    }
    clear_whiteout(&norm);
    let parent = parent_path(&norm);
    let mem_parent = MEM_FS.lock().is_dir(&parent);
    let ext_parent = ext4_vol::ext4_dir_path_exists(&parent);
    if mem_parent && (super::is_memfs_volatile_dir(&parent) || !ext_parent) {
        MEM_FS.lock().add_dir(&norm);
        return Ok(());
    }
    if ext_parent {
        ext4_vol::mkdir_ext4_with_mode(&norm, mode)?;
        return Ok(());
    }
    if mem_parent {
        MEM_FS.lock().add_dir(&norm);
        return Ok(());
    }
    Err(SysErrNo::ENOENT)
}

pub fn create_dir(path: &str) -> Result<(), SysErrNo> {
    create_dir_with_mode(path, 0o755)
}

pub fn create_regular_file(path: &str, mode: u32) -> Result<u32, SysErrNo> {
    let norm = normalize_path(path);
    if file_exists(&norm) || dir_exists(&norm) {
        return Err(SysErrNo::EEXIST);
    }
    clear_whiteout(&norm);
    let parent = parent_path(&norm);
    let mem_parent = MEM_FS.lock().is_dir(&parent);
    let ext_parent = ext4_vol::ext4_dir_path_exists(&parent);
    if mem_parent && (super::is_memfs_volatile_dir(&parent) || !ext_parent) {
        MEM_FS.lock().add_file(&norm, Vec::new());
        return Ok(pseudo_inode(&norm) as u32);
    }
    if ext_parent {
        return ext4_vol::create_regular_ext4_with_mode(&norm, mode);
    }
    if mem_parent {
        MEM_FS.lock().add_file(&norm, Vec::new());
        return Ok(pseudo_inode(&norm) as u32);
    }
    Err(SysErrNo::ENOENT)
}

fn open_dir_descriptor(
    host_path: &str,
    logical_path: &str,
) -> Result<fd::FileDescriptor, SysErrNo> {
    if MEM_FS.lock().is_dir(host_path) {
        let entries = list_dir(host_path)?;
        return Ok(fd::FileDescriptor::MemDir {
            path: logical_path.into(),
            entries,
            offset: 0,
        });
    }

    let Some((ino, is_dir)) = ext4_vol::lookup_path(host_path) else {
        return Err(SysErrNo::ENOENT);
    };
    if !is_dir {
        return Err(SysErrNo::ENOTDIR);
    }
    Ok(fd::FileDescriptor::Ext4Dir {
        path: logical_path.into(),
        ino,
        offset: 0,
    })
}

fn path_exists_non_dir(path: &str) -> bool {
    let norm = normalize_path(path);
    if is_removed(&norm) {
        return false;
    }
    {
        let mem = MEM_FS.lock();
        if mem.get_file(&norm).is_some() {
            return true;
        }
        if mem.is_dir(&norm) {
            return false;
        }
    }
    matches!(
        ext4_vol::lookup_kind(&norm),
        Some((
            _,
            ext4_vol::Ext4NodeKind::Regular
                | ext4_vol::Ext4NodeKind::Symlink
                | ext4_vol::Ext4NodeKind::Other
        ))
    )
}

fn missing_path_errno(path: &str) -> SysErrNo {
    let parent = parent_path(path);
    if parent != normalize_path(path) && path_exists_non_dir(&parent) {
        SysErrNo::ENOTDIR
    } else {
        SysErrNo::ENOENT
    }
}

pub fn open_path(
    host_path: &str,
    logical_path: &str,
    flags: u32,
    mode: u32,
) -> Result<fd::FileDescriptor, SysErrNo> {
    use fd::open_flags::*;

    let path_norm = normalize_path(host_path);
    let logical_norm = normalize_path(logical_path);
    let accmode = flags & O_ACCMODE;
    if accmode == O_ACCMODE {
        return Err(SysErrNo::EINVAL);
    }
    let read_ok = accmode == O_RDONLY || accmode == O_RDWR;
    let write_ok = accmode == O_WRONLY || accmode == O_RDWR;
    let want_dir = (flags & O_DIRECTORY) != 0;
    let want_create = (flags & O_CREAT) != 0;
    let want_excl = (flags & O_EXCL) != 0;
    let want_trunc = (flags & O_TRUNC) != 0;
    let nofollow = (flags & O_NOFOLLOW) != 0;
    let append = (flags & O_APPEND) != 0;

    if want_dir && want_create {
        return Err(SysErrNo::EINVAL);
    }

    let removed = is_removed(&path_norm);
    if removed && !want_create {
        return Err(SysErrNo::ENOENT);
    }

    let mem_has_file = MEM_FS.lock().get_file(&path_norm).is_some();
    let ext_path_norm = if !removed {
        match ext4_vol::lookup_kind(&path_norm) {
            Some((_ino, ext4_vol::Ext4NodeKind::Symlink)) if nofollow => {
                return Err(SysErrNo::ELOOP);
            }
            Some((_ino, ext4_vol::Ext4NodeKind::Symlink)) => {
                ext4_vol::resolve_symlinks(&path_norm)?
            }
            _ => path_norm.clone(),
        }
    } else {
        path_norm.clone()
    };

    if mem_has_file {
        if want_dir {
            return Err(SysErrNo::ENOTDIR);
        }
        if want_excl && want_create {
            return Err(SysErrNo::EEXIST);
        }
        let source = MEM_FS
            .lock()
            .get_file(&path_norm)
            .map(|file| (fd::MemFileContent::from_slice(&file.content), file.times));
        let (mut content, mut times) =
            source.unwrap_or_else(|| (fd::MemFileContent::new(), super::FileTimes::now()));
        if want_trunc && write_ok {
            content.clear();
            MEM_FS.lock().truncate_file(&path_norm, 0)?;
            if let Some(file) = MEM_FS.lock().get_file(&path_norm) {
                times = file.times;
            }
        }
        let base_off = if append && write_ok { content.len() } else { 0 };
        return Ok(fd::FileDescriptor::MemFile {
                name: path_norm,
                content,
                times,
            offset: base_off,
            readable: read_ok,
            writable: write_ok,
            append,
            linked: true,
        });
    }

    if MEM_FS.lock().is_dir(&path_norm) || ext4_vol::ext4_dir_path_exists(&ext_path_norm) {
        if write_ok || want_trunc || want_create {
            return Err(SysErrNo::EISDIR);
        }
        let open_path = if MEM_FS.lock().is_dir(&path_norm) {
            path_norm.clone()
        } else {
            ext_path_norm.clone()
        };
        return open_dir_descriptor(&open_path, &logical_norm);
    }

    if want_dir {
        if ext4_vol::ext4_regular_file_exists(&ext_path_norm) || path_exists_non_dir(&path_norm) {
            return Err(SysErrNo::ENOTDIR);
        }
        return Err(missing_path_errno(&path_norm));
    }

    if !removed && ext4_vol::ext4_regular_file_exists(&ext_path_norm) {
        if want_excl && want_create {
            return Err(SysErrNo::EEXIST);
        }
        let Some((ino, is_dir)) = ext4_vol::lookup_path(&ext_path_norm) else {
            return Err(SysErrNo::ENOENT);
        };
        if is_dir {
            return Err(SysErrNo::EISDIR);
        }
        if want_trunc && write_ok {
            ext4_vol::truncate_regular_ext4(&ext_path_norm, 0)?;
        }
        let base_off = if append && write_ok {
            ext4_vol::regular_file_size(ino)?
        } else {
            0
        };
        ext4_vol::open_regular_ino(ino);
        return Ok(fd::FileDescriptor::Ext4Regular {
            ino,
            offset: base_off,
            readable: read_ok,
            writable: write_ok,
            append,
        });
    }

    if want_create {
        let parent = parent_path(&path_norm);
        let mem_parent = MEM_FS.lock().is_dir(&parent);
        let ext_parent = ext4_vol::ext4_dir_path_exists(&parent);
        if mem_parent && (super::is_memfs_volatile_dir(&parent) || !ext_parent) {
            MEM_FS.lock().add_file(&path_norm, Vec::new());
            clear_whiteout(&path_norm);
            let times = MEM_FS
                .lock()
                .get_file(&path_norm)
                .map(|file| file.times)
                .unwrap_or_else(super::FileTimes::now);
            return Ok(fd::FileDescriptor::MemFile {
                name: path_norm,
                content: fd::MemFileContent::new(),
                times,
                offset: 0,
                readable: read_ok,
                writable: write_ok,
                append,
                linked: true,
            });
        }
        if ext_parent {
            let ino = ext4_vol::create_regular_ext4_with_mode(&path_norm, mode)?;
            clear_whiteout(&path_norm);
            ext4_vol::open_regular_ino(ino);
            return Ok(fd::FileDescriptor::Ext4Regular {
                ino,
                offset: 0,
                readable: read_ok,
                writable: write_ok,
                append,
            });
        }
        if mem_parent {
            MEM_FS.lock().add_file(&path_norm, Vec::new());
            clear_whiteout(&path_norm);
            let times = MEM_FS
                .lock()
                .get_file(&path_norm)
                .map(|file| file.times)
                .unwrap_or_else(super::FileTimes::now);
            return Ok(fd::FileDescriptor::MemFile {
                name: path_norm,
                content: fd::MemFileContent::new(),
                times,
                offset: 0,
                readable: read_ok,
                writable: write_ok,
                append,
                linked: true,
            });
        }
        return Err(missing_path_errno(&path_norm));
    }

    Err(missing_path_errno(&path_norm))
}

pub fn list_dir(path: &str) -> Result<Vec<fd::DirEntryRecord>, SysErrNo> {
    let norm = normalize_path(path);

    let mem_dir = MEM_FS.lock().is_dir(&norm);
    let mem_entries: Vec<fd::DirEntryRecord> = if mem_dir {
        MEM_FS.lock().list_dir(&norm)?
    } else {
        Vec::new()
    };

    let ext_dir = ext4_vol::is_ext4_mounted() && ext4_vol::ext4_dir_path_exists(&norm);

    if !mem_dir && !ext_dir {
        let m = MEM_FS.lock();
        if m.get_file(&norm).is_some() {
            return Err(SysErrNo::ENOTDIR);
        }
        drop(m);
        if ext4_vol::is_ext4_mounted() && ext4_vol::ext4_file_path_exists(&norm) {
            return Err(SysErrNo::ENOTDIR);
        }
        return Err(SysErrNo::ENOENT);
    }

    let mut map: BTreeMap<String, fd::DirEntryRecord> = BTreeMap::new();
    for e in mem_entries {
        map.insert(e.name.clone(), e);
    }
    if ext_dir {
        for (name, is_dir) in ext4_vol::ext4_list_dir(&norm)? {
            let child = if norm == "/" {
                alloc::format!("/{}", name)
            } else {
                alloc::format!("{}/{}", norm, name)
            };
            if is_removed(&child) {
                continue;
            }
            map.entry(name.clone())
                .or_insert(fd::DirEntryRecord { name, is_dir });
        }
    }

    Ok(map.into_values().collect())
}

pub fn list_files() -> Vec<String> {
    let mem_paths = MEM_FS.lock().list_file_names();
    ext4_vol::merged_list_all_file_paths(mem_paths)
        .into_iter()
        .filter(|path| !is_removed(path))
        .collect()
}
