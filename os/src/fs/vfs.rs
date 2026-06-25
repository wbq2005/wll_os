//! 最薄 VFS：统一 MemFS + ext4 后端的查找与变更入口；`mount`/`umount2` 语义见 `syscall::fs`。

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use lazy_static::lazy_static;
use spin::Mutex;

use crate::utils::error::SysErrNo;

use super::fd;
use super::{block_dev, ext4_vol, vfat};
use super::{normalize_path, MemNodeMetadata, MemSpecialKind, MEM_FS};

const S_IFDIR: u32 = 0o040000;
const S_IFMT: u32 = 0o170000;
const S_IFIFO: u32 = 0o010000;
const S_IFCHR: u32 = 0o020000;
const S_IFBLK: u32 = 0o060000;
const S_IFREG: u32 = 0o100000;
const S_IFLNK: u32 = 0o120000;
const S_IFSOCK: u32 = 0o140000;
const DEV_NULL_MAJOR: u32 = 1;
const DEV_NULL_MINOR: u32 = 3;
const DEV_ZERO_MAJOR: u32 = 1;
const DEV_ZERO_MINOR: u32 = 5;

lazy_static! {
    static ref WHITEOUTS: Mutex<BTreeSet<String>> = Mutex::new(BTreeSet::new());
    static ref MOUNT_TABLE: Mutex<Vec<MountEntry>> = Mutex::new(Vec::new());
}

#[derive(Clone)]
struct MountEntry {
    source: String,
    logical_target: String,
    host_target: String,
    fstype: String,
    backend: MountBackend,
}

#[derive(Clone)]
enum MountBackend {
    Root,
    Ext4Root,
    Tmpfs,
    Vfat(Arc<vfat::VfatVolume>),
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

#[derive(Clone, Copy)]
enum CredentialIdentity {
    Real,
    Effective,
    Filesystem,
}

impl CredentialIdentity {
    fn from_effective_flag(effective: bool) -> Self {
        if effective {
            Self::Effective
        } else {
            Self::Real
        }
    }

    fn uid(self, credentials: &crate::task::Credentials) -> u32 {
        match self {
            Self::Real => credentials.real_uid,
            Self::Effective => credentials.effective_uid,
            Self::Filesystem => credentials.fsuid,
        }
    }

    fn is_in_group(self, credentials: &crate::task::Credentials, gid: u32) -> bool {
        match self {
            Self::Real => credentials.is_in_group(gid, false),
            Self::Effective => credentials.is_in_group(gid, true),
            Self::Filesystem => credentials.is_in_filesystem_group(gid),
        }
    }
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

fn path_is_under(path: &str, parent: &str) -> bool {
    let path = normalize_path(path);
    let parent = normalize_path(parent);
    path == parent
        || (parent != "/"
            && path
                .strip_prefix(parent.as_str())
                .is_some_and(|tail| tail.starts_with('/')))
}

fn mounted_ext4_backend_path(path: &str) -> Option<String> {
    let norm = normalize_path(path);
    let mounts = MOUNT_TABLE.lock();
    let entry = mounts
        .iter()
        .filter(|entry| {
            matches!(entry.backend, MountBackend::Ext4Root)
                && entry.host_target != "/"
                && path_is_under(&norm, &entry.host_target)
        })
        .max_by_key(|entry| entry.host_target.len())?;
    let suffix = if norm == entry.host_target {
        ""
    } else {
        norm.strip_prefix(entry.host_target.as_str())
            .and_then(|tail| tail.strip_prefix('/'))
            .unwrap_or("")
    };
    if suffix.is_empty() {
        Some(String::from("/"))
    } else {
        Some(normalize_path(&alloc::format!("/{}", suffix)))
    }
}

fn is_tmpfs_path(path: &str) -> bool {
    let norm = normalize_path(path);
    let mounts = MOUNT_TABLE.lock();
    mounts.iter().any(|entry| {
        matches!(entry.backend, MountBackend::Tmpfs) && path_is_under(&norm, &entry.host_target)
    })
}

fn mounted_vfat_backend(path: &str) -> Option<(Arc<vfat::VfatVolume>, String)> {
    let norm = normalize_path(path);
    let mounts = MOUNT_TABLE.lock();
    let entry = mounts
        .iter()
        .filter(|entry| {
            matches!(entry.backend, MountBackend::Vfat(_))
                && path_is_under(&norm, &entry.host_target)
        })
        .max_by_key(|entry| entry.host_target.len())?;
    let MountBackend::Vfat(volume) = &entry.backend else {
        return None;
    };
    let suffix = if norm == entry.host_target {
        ""
    } else {
        norm.strip_prefix(entry.host_target.as_str())
            .and_then(|tail| tail.strip_prefix('/'))
            .unwrap_or("")
    };
    let backend_path = if suffix.is_empty() {
        String::from("/")
    } else {
        normalize_path(&alloc::format!("/{}", suffix))
    };
    Some((volume.clone(), backend_path))
}

fn vfat_metadata_for_path(path: &str) -> Option<Result<vfat::VfatMetadata, SysErrNo>> {
    mounted_vfat_backend(path).map(|(volume, backend_path)| volume.metadata(&backend_path))
}

fn parent_vfat_metadata_for_path(path: &str) -> Option<Result<vfat::VfatMetadata, SysErrNo>> {
    vfat_metadata_for_path(&parent_path(path))
}

fn ext4_lookup_path(path: &str) -> String {
    mounted_ext4_backend_path(path).unwrap_or_else(|| normalize_path(path))
}

fn symlink_target_path(link_path: &str, target: &str) -> String {
    if target.starts_with('/') {
        normalize_path(target)
    } else {
        normalize_path(&alloc::format!("{}/{}", parent_path(link_path), target))
    }
}

fn resolve_final_symlink(path: &str, nofollow: bool) -> Result<String, SysErrNo> {
    let mut current = normalize_path(path);
    for _ in 0..40 {
        let ext_current = ext4_lookup_path(&current);
        if mounted_ext4_backend_path(&current).is_none() {
            if let Some(target) = MEM_FS.lock().get_symlink(&current) {
                if nofollow {
                    return Err(SysErrNo::ELOOP);
                }
                current = symlink_target_path(&current, &target);
                continue;
            }
        }
        match ext4_vol::lookup_kind(&ext_current) {
            Some((_ino, ext4_vol::Ext4NodeKind::Symlink)) if nofollow => {
                return Err(SysErrNo::ELOOP);
            }
            Some((_ino, ext4_vol::Ext4NodeKind::Symlink)) => {
                current = ext4_vol::resolve_symlinks(&ext_current)?;
                continue;
            }
            _ => return Ok(current),
        }
    }
    Err(SysErrNo::ELOOP)
}

fn lookup_symlink_target(path: &str) -> Result<Option<String>, SysErrNo> {
    let norm = normalize_path(path);
    if is_removed(&norm) {
        return Err(SysErrNo::ENOENT);
    }
    if mounted_ext4_backend_path(&norm).is_none() {
        if let Some(target) = MEM_FS.lock().get_symlink(&norm) {
            return Ok(Some(target));
        }
    }
    match ext4_vol::lookup_kind(&ext4_lookup_path(&norm)) {
        Some((_ino, ext4_vol::Ext4NodeKind::Symlink)) => {
            ext4_vol::readlink_ext4(&ext4_lookup_path(&norm)).map(Some)
        }
        _ => Ok(None),
    }
}

fn resolve_parent_symlinks_for_lookup(path: &str) -> Result<String, SysErrNo> {
    let norm = normalize_path(path);
    let tail = norm.trim_matches('/');
    if tail.is_empty() {
        return Ok(norm);
    }

    let parts: Vec<&str> = tail.split('/').filter(|part| !part.is_empty()).collect();
    if parts.len() <= 1 {
        return Ok(norm);
    }

    let final_name = parts[parts.len() - 1];
    let mut current = String::from("/");
    let mut index = 0usize;
    let mut followed = 0usize;
    while index + 1 < parts.len() {
        if current != "/" {
            current.push('/');
        }
        current.push_str(parts[index]);

        loop {
            match lookup_symlink_target(&current)? {
                Some(target) => {
                    followed += 1;
                    if followed > 40 {
                        return Err(SysErrNo::ELOOP);
                    }
                    current = symlink_target_path(&current, &target);
                }
                None => break,
            }
        }

        let meta = metadata(&current, false)?;
        if meta.kind != VfsNodeKind::Directory {
            return Err(SysErrNo::ENOTDIR);
        }
        index += 1;
    }

    if current == "/" {
        Ok(normalize_path(&alloc::format!("/{}", final_name)))
    } else {
        Ok(normalize_path(&alloc::format!(
            "{}/{}", current, final_name
        )))
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
        S_IFLNK => VfsNodeKind::Symlink,
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

fn local_device_path(path: &str) -> &str {
    for root in ["/glibc", "/musl"] {
        if let Some(tail) = path.strip_prefix(root) {
            if tail.is_empty() {
                return "/";
            }
            if tail.starts_with('/') {
                return tail;
            }
        }
    }
    path
}

fn device_numbers(path: &str) -> Option<(u32, u32)> {
    block_dev::device_numbers_for_path(local_device_path(path))
}

fn block_device_minor(path: &str) -> Option<u32> {
    device_numbers(path)
        .filter(|(major, _)| *major == block_dev::DEV_VIRTIO_BLK_MAJOR)
        .map(|(_, minor)| minor)
}

fn procfs_link_source(path: &str) -> bool {
    let norm = normalize_path(path);
    let local = local_device_path(&norm);
    local == "/proc" || local.starts_with("/proc/")
}

fn metadata_for_char_device(path: &str, major: u32, minor: u32) -> VfsMetadata {
    let mut meta = synthetic_metadata(path, VfsNodeKind::Other, S_IFCHR | 0o666, 0, 1);
    meta.rdev_major = major;
    meta.rdev_minor = minor;
    meta
}

fn metadata_for_block_device(path: &str, minor: u32) -> VfsMetadata {
    let mut meta = synthetic_metadata(path, VfsNodeKind::Other, S_IFBLK | 0o600, 0, 1);
    meta.rdev_major = block_dev::DEV_VIRTIO_BLK_MAJOR;
    meta.rdev_minor = minor;
    meta
}

fn metadata_for_block_device_number(path: &str, major: u32, minor: u32) -> VfsMetadata {
    let mut meta = synthetic_metadata(path, VfsNodeKind::Other, S_IFBLK | 0o600, 0, 1);
    meta.rdev_major = major;
    meta.rdev_minor = minor;
    meta
}

fn metadata_from_vfat(path: &str, meta: vfat::VfatMetadata) -> VfsMetadata {
    synthetic_metadata(
        path,
        if meta.is_dir {
            VfsNodeKind::Directory
        } else {
            VfsNodeKind::Regular
        },
        if meta.is_dir {
            S_IFDIR | 0o755
        } else if meta.readonly {
            S_IFREG | 0o444
        } else {
            S_IFREG | 0o644
        },
        meta.size as u64,
        1,
    )
}

fn default_mem_metadata(mode: u32) -> MemNodeMetadata {
    MemNodeMetadata {
        mode,
        uid: 0,
        gid: 0,
    }
}

fn metadata_for_mem_file(
    name: &str,
    len: usize,
    _is_elf: bool,
    node: MemNodeMetadata,
    nlink: u32,
    ino_key: &str,
) -> VfsMetadata {
    if is_dev_null_path(name) {
        return metadata_for_char_device(name, DEV_NULL_MAJOR, DEV_NULL_MINOR);
    }
    if is_dev_zero_path(name) {
        return metadata_for_char_device(name, DEV_ZERO_MAJOR, DEV_ZERO_MINOR);
    }
    if let Some((major, minor)) = device_numbers(name) {
        if major == block_dev::DEV_LOOP_CONTROL_MAJOR {
            return metadata_for_char_device(name, major, minor);
        }
        return metadata_for_block_device_number(name, major, minor);
    }
    let mut meta = synthetic_metadata(
        ino_key,
        VfsNodeKind::Regular,
        S_IFREG | (node.mode & 0o7777),
        len as u64,
        nlink,
    );
    meta.uid = node.uid;
    meta.gid = node.gid;
    meta
}

fn metadata_for_mem_symlink(name: &str, target: &str, node: MemNodeMetadata) -> VfsMetadata {
    let mut meta = synthetic_metadata(
        name,
        VfsNodeKind::Symlink,
        S_IFLNK | (node.mode & 0o7777),
        target.len() as u64,
        1,
    );
    meta.uid = node.uid;
    meta.gid = node.gid;
    meta
}

fn metadata_for_mem_special(
    name: &str,
    kind: MemSpecialKind,
    node: MemNodeMetadata,
) -> VfsMetadata {
    let file_type = match kind {
        MemSpecialKind::Fifo => S_IFIFO,
        MemSpecialKind::Socket => S_IFSOCK,
    };
    let mut meta = synthetic_metadata(
        name,
        VfsNodeKind::Other,
        file_type | (node.mode & 0o7777),
        0,
        1,
    );
    meta.uid = node.uid;
    meta.gid = node.gid;
    meta
}

fn metadata_from_mem_file(
    file: &super::MemFile,
    node: MemNodeMetadata,
    nlink: u32,
    ino_key: &str,
) -> VfsMetadata {
    let mut meta = metadata_for_mem_file(
        &file.name,
        file.content.len(),
        file.content.is_elf_image(),
        node,
        nlink,
        ino_key,
    );
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
    let node = MEM_FS.lock().metadata(name).unwrap_or_else(|| {
        default_mem_metadata(if content.is_elf_image() { 0o777 } else { 0o666 })
    });
    let mem = MEM_FS.lock();
    let nlink = mem.file_link_count(name);
    let link_key = mem
        .file_link_key(name)
        .unwrap_or_else(|| String::from(name));
    drop(mem);
    let mut meta = metadata_for_mem_file(
        name,
        content.len(),
        content.is_elf_image(),
        node,
        nlink,
        &link_key,
    );
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
    if let Some((volume, backend_path)) = mounted_vfat_backend(&norm) {
        return volume
            .metadata(&backend_path)
            .map(|meta| metadata_from_vfat(&norm, meta));
    }
    let tmpfs_path = is_tmpfs_path(&norm);
    let ext_norm = ext4_lookup_path(&norm);
    let mounted_ext4 = mounted_ext4_backend_path(&norm).is_some();

    if tmpfs_path || !mounted_ext4 {
        let mem = MEM_FS.lock();
        if mem.is_dir(&norm) {
            let entries = mem.list_dir(&norm)?;
            let (sec, nsec) = current_times();
            let node = mem
                .metadata(&norm)
                .unwrap_or_else(|| default_mem_metadata(0o755));
            return Ok(VfsMetadata {
                ino: pseudo_inode(&norm),
                kind: VfsNodeKind::Directory,
                mode: S_IFDIR | (node.mode & 0o7777),
                nlink: 1,
                uid: node.uid,
                gid: node.gid,
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
            let node = mem.metadata(&norm).unwrap_or_else(|| {
                default_mem_metadata(if file.content.is_elf_image() {
                    0o777
                } else {
                    0o666
                })
            });
            let nlink = mem.file_link_count(&norm);
            let link_key = mem.file_link_key(&norm).unwrap_or_else(|| norm.clone());
            return Ok(metadata_from_mem_file(file, node, nlink, &link_key));
        }
        if let Some(target) = mem.get_symlink(&norm) {
            if follow_symlink {
                drop(mem);
                let resolved = resolve_final_symlink(&norm, false)?;
                return metadata(&resolved, true);
            }
            let node = mem
                .metadata(&norm)
                .unwrap_or_else(|| default_mem_metadata(0o777));
            return Ok(metadata_for_mem_symlink(&norm, &target, node));
        }
        if let Some(kind) = mem.get_special(&norm) {
            let node = mem
                .metadata(&norm)
                .unwrap_or_else(|| default_mem_metadata(0o666));
            return Ok(metadata_for_mem_special(&norm, kind, node));
        }
    }

    if tmpfs_path {
        return Err(missing_path_errno(&norm));
    }

    let ext_path = match ext4_vol::lookup_kind(&ext_norm) {
        Some((_ino, ext4_vol::Ext4NodeKind::Symlink)) if follow_symlink => {
            ext4_vol::resolve_symlinks(&ext_norm)?
        }
        Some((_ino, kind)) => {
            let meta = ext4_vol::metadata(&ext_norm)?;
            let mut out = metadata_from_ext4(meta);
            out.kind = kind_from_ext4(kind);
            return Ok(out);
        }
        None => return Err(missing_path_errno(&norm)),
    };

    ext4_vol::metadata(&ext_path).map(metadata_from_ext4)
}

pub fn metadata_for_lookup(path: &str, follow_symlink: bool) -> Result<VfsMetadata, SysErrNo> {
    let norm = resolve_parent_symlinks_for_lookup(path)?;
    check_search_access(&norm, CredentialIdentity::Filesystem)?;
    metadata(&norm, follow_symlink)
}

fn mount_record_line(entry: &MountEntry) -> String {
    alloc::format!(
        "{} {} {} rw 0 0\n",
        entry.source,
        entry.logical_target,
        entry.fstype
    )
}

fn mount_table_text() -> Vec<u8> {
    let mounts = MOUNT_TABLE.lock();
    let mut text = String::new();
    for entry in mounts.iter() {
        text.push_str(&mount_record_line(entry));
    }
    text.into_bytes()
}

fn refresh_mount_pseudo_files() {
    let data = mount_table_text();
    let now = super::FileTimes::now();
    let mut fs = MEM_FS.lock();
    for root in ["", "/musl", "/glibc"] {
        let proc_mounts = alloc::format!("{}/proc/mounts", root);
        if !fs.write_file_content(&proc_mounts, fd::MemFileContent::from_slice(&data), now) {
            fs.add_file(&proc_mounts, data.clone());
        }
        let etc_mtab = alloc::format!("{}/etc/mtab", root);
        if !fs.write_file_content(&etc_mtab, fd::MemFileContent::from_slice(&data), now) {
            fs.add_file(&etc_mtab, data.clone());
        }
    }
}

pub fn refresh_block_device_nodes() {
    let devices = block_dev::list_device_paths();
    if devices.is_empty() {
        return;
    }
    let mut fs = MEM_FS.lock();
    for root in ["", "/musl", "/glibc"] {
        fs.add_dir(&alloc::format!("{}/dev", root));
        fs.add_dir(&alloc::format!("{}/dev/loop", root));
        fs.add_dir(&alloc::format!("{}/dev/block", root));
    }
    for dev in devices {
        for root in ["", "/musl", "/glibc"] {
            let path = alloc::format!("{}{}", root, dev);
            fs.add_file(&path, Vec::new());
        }
    }
}

fn validate_mount_source(source: &str) -> Result<(), SysErrNo> {
    let meta = metadata(source, true)?;
    let local = local_device_path(source);
    if meta.kind != VfsNodeKind::Other
        || (meta.mode & S_IFMT) != S_IFBLK
        || device_numbers(source) != Some((meta.rdev_major, meta.rdev_minor))
    {
        return Err(SysErrNo::ENOTBLK);
    }
    if block_dev::range_for_path(local).is_none() {
        return Err(SysErrNo::ENODEV);
    }
    if block_dev::is_root_source(local) {
        return Err(SysErrNo::EBUSY);
    }
    Ok(())
}

fn resolve_mount_backend(source: &str, fstype: &str) -> Result<(String, MountBackend), SysErrNo> {
    match fstype {
        "tmpfs" => {
            let _ = source;
            Ok((String::from("tmpfs"), MountBackend::Tmpfs))
        }
        "vfat" | "fat" | "msdos" => {
            let range =
                block_dev::range_for_path(local_device_path(source)).ok_or(SysErrNo::ENODEV)?;
            let volume = vfat::VfatVolume::open(range)?;
            Ok((String::from("vfat"), MountBackend::Vfat(Arc::new(volume))))
        }
        // Non-root ext4 mounts need an independent ext4 volume object. Until
        // that exists, accepting ext4 here would alias the root filesystem.
        "ext4" => Err(SysErrNo::ENODEV),
        _ => Err(SysErrNo::ENODEV),
    }
}

pub fn init_mount_table() {
    let mut mounts = MOUNT_TABLE.lock();
    mounts.clear();
    let fstype = if ext4_vol::is_ext4_mounted() {
        "ext4"
    } else {
        "tmpfs"
    };
    mounts.push(MountEntry {
        source: String::from("rootfs"),
        logical_target: String::from("/"),
        host_target: String::from("/"),
        fstype: String::from(fstype),
        backend: MountBackend::Root,
    });
    drop(mounts);
    refresh_mount_pseudo_files();
}

pub fn mount_fs(
    source: &str,
    logical_target: &str,
    host_target: &str,
    fstype: &str,
    flags: usize,
) -> Result<(), SysErrNo> {
    let source = normalize_path(source);
    let logical_target = normalize_path(logical_target);
    let host_target = normalize_path(host_target);
    if flags != 0 {
        return Err(SysErrNo::EINVAL);
    }
    let (backend_fstype, backend) = resolve_mount_backend(&source, fstype)?;
    let tmpfs_backend = matches!(&backend, MountBackend::Tmpfs);
    if !tmpfs_backend && !ext4_vol::is_ext4_mounted() {
        return Err(SysErrNo::ENODEV);
    }
    if !tmpfs_backend {
        validate_mount_source(&source)?;
    }
    let target_meta = metadata(&host_target, true)?;
    if target_meta.kind != VfsNodeKind::Directory {
        return Err(SysErrNo::ENOTDIR);
    }
    if !tmpfs_backend {
        ext4_vol::metadata("/")?;
    }
    if tmpfs_backend {
        MEM_FS.lock().add_dir_with_mode(&host_target, 0o1777);
    }

    let mut mounts = MOUNT_TABLE.lock();
    if mounts.iter().any(|entry| entry.host_target == host_target) {
        return Err(SysErrNo::EBUSY);
    }
    mounts.push(MountEntry {
        source,
        logical_target,
        host_target,
        fstype: backend_fstype,
        backend,
    });
    drop(mounts);
    refresh_mount_pseudo_files();
    Ok(())
}

pub fn umount_fs(logical_target: &str, host_target: &str, flags: usize) -> Result<(), SysErrNo> {
    const MNT_FORCE: usize = 1;
    const MNT_DETACH: usize = 2;
    const MNT_EXPIRE: usize = 4;
    const UMOUNT_NOFOLLOW: usize = 8;
    if flags & !(MNT_FORCE | MNT_DETACH | MNT_EXPIRE | UMOUNT_NOFOLLOW) != 0 {
        return Err(SysErrNo::EINVAL);
    }

    let logical_target = normalize_path(logical_target);
    let host_target = normalize_path(host_target);
    let mut mounts = MOUNT_TABLE.lock();
    let Some(index) = mounts.iter().rposition(|entry| {
        entry.logical_target == logical_target && entry.host_target == host_target
    }) else {
        return Err(SysErrNo::EINVAL);
    };
    if mounts[index].logical_target == "/" {
        return Err(SysErrNo::EBUSY);
    }
    mounts.remove(index);
    drop(mounts);
    refresh_mount_pseudo_files();
    Ok(())
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
                    let node = mem.metadata(name).unwrap_or_else(|| {
                        default_mem_metadata(if file.content.is_elf_image() {
                            0o777
                        } else {
                            0o666
                        })
                    });
                    let nlink = mem.file_link_count(name);
                    let link_key = mem.file_link_key(name).unwrap_or_else(|| name.clone());
                    Ok(metadata_from_mem_file(file, node, nlink, &link_key))
                } else {
                    drop(mem);
                    Ok(metadata_from_mem_fd(name, content, *times))
                }
            } else {
                Ok(metadata_from_mem_fd(name, content, *times))
            }
        }
        fd::FileDescriptor::MemDir { path, entries, .. } => {
            let node = MEM_FS
                .lock()
                .metadata(path)
                .unwrap_or_else(|| default_mem_metadata(0o755));
            let mut meta = synthetic_metadata(
                path,
                VfsNodeKind::Directory,
                S_IFDIR | (node.mode & 0o7777),
                entries.len() as u64,
                1,
            );
            meta.uid = node.uid;
            meta.gid = node.gid;
            Ok(meta)
        }
        fd::FileDescriptor::Ext4Regular { ino, .. } | fd::FileDescriptor::Ext4Dir { ino, .. } => {
            ext4_vol::metadata_by_ino(*ino).map(metadata_from_ext4)
        }
        fd::FileDescriptor::Path { host_path, .. } => metadata(host_path, false),
        fd::FileDescriptor::LoopControl => Ok(metadata_for_char_device(
            "/dev/loop-control",
            block_dev::DEV_LOOP_CONTROL_MAJOR,
            block_dev::DEV_LOOP_CONTROL_MINOR,
        )),
        fd::FileDescriptor::LoopDevice { index, .. } => Ok(metadata_for_block_device_number(
            &block_dev::loop_device_path(*index),
            block_dev::DEV_LOOP_MAJOR,
            *index as u32,
        )),
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
    check_metadata_access_with_identity(meta, access_mode, CredentialIdentity::Real)
}

pub fn check_metadata_access_with_effective(
    meta: &VfsMetadata,
    access_mode: usize,
    effective: bool,
) -> Result<(), SysErrNo> {
    check_metadata_access_with_identity(
        meta,
        access_mode,
        CredentialIdentity::from_effective_flag(effective),
    )
}

fn check_metadata_access_with_identity(
    meta: &VfsMetadata,
    access_mode: usize,
    identity: CredentialIdentity,
) -> Result<(), SysErrNo> {
    const R_OK: usize = 4;
    const W_OK: usize = 2;
    const X_OK: usize = 1;
    let credentials = crate::task::current_task()
        .map(|task| task.credentials.lock().clone())
        .unwrap_or_else(crate::task::Credentials::root);
    let uid = identity.uid(&credentials);

    if uid == 0 {
        if (access_mode & X_OK) != 0
            && meta.kind == VfsNodeKind::Regular
            && (meta.mode & 0o111) == 0
        {
            return Err(SysErrNo::EACCES);
        }
        return Ok(());
    }

    let class_shift = if uid == meta.uid {
        6
    } else if identity.is_in_group(&credentials, meta.gid) {
        3
    } else {
        0
    };
    let perm = (meta.mode >> class_shift) & 0o7;
    let mut required = 0;
    if (access_mode & R_OK) != 0 {
        required |= 0o4;
    }
    if (access_mode & W_OK) != 0 {
        required |= 0o2;
    }
    if (access_mode & X_OK) != 0 {
        required |= 0o1;
    }
    if (perm & required) != required {
        return Err(SysErrNo::EACCES);
    }
    Ok(())
}

pub fn check_access(path: &str, follow_symlink: bool, access_mode: usize) -> Result<(), SysErrNo> {
    check_access_with_identity(path, follow_symlink, access_mode, CredentialIdentity::Real)
}

pub fn check_access_with_effective(
    path: &str,
    follow_symlink: bool,
    access_mode: usize,
    effective: bool,
) -> Result<(), SysErrNo> {
    check_access_with_identity(
        path,
        follow_symlink,
        access_mode,
        CredentialIdentity::from_effective_flag(effective),
    )
}

fn check_access_with_filesystem(
    path: &str,
    follow_symlink: bool,
    access_mode: usize,
) -> Result<(), SysErrNo> {
    check_access_with_identity(
        path,
        follow_symlink,
        access_mode,
        CredentialIdentity::Filesystem,
    )
}

fn check_access_with_identity(
    path: &str,
    follow_symlink: bool,
    access_mode: usize,
    identity: CredentialIdentity,
) -> Result<(), SysErrNo> {
    check_search_access(path, identity)?;
    let meta = metadata(path, follow_symlink)?;
    check_metadata_access_with_identity(&meta, access_mode, identity)
}

pub fn check_fd_access(file: &fd::FileDescriptor, access_mode: usize) -> Result<(), SysErrNo> {
    let meta = metadata_for_fd(file)?;
    check_metadata_access_with_identity(&meta, access_mode, CredentialIdentity::Real)
}

pub fn check_fd_access_with_effective(
    file: &fd::FileDescriptor,
    access_mode: usize,
    effective: bool,
) -> Result<(), SysErrNo> {
    let meta = metadata_for_fd(file)?;
    check_metadata_access_with_identity(
        &meta,
        access_mode,
        CredentialIdentity::from_effective_flag(effective),
    )
}

fn check_noatime_permission(meta: &VfsMetadata) -> Result<(), SysErrNo> {
    let credentials = crate::task::current_task()
        .map(|task| task.credentials.lock().clone())
        .unwrap_or_else(crate::task::Credentials::root);
    if credentials.fsuid == 0 || credentials.fsuid == meta.uid {
        Ok(())
    } else {
        Err(SysErrNo::EPERM)
    }
}

fn check_search_access(path: &str, identity: CredentialIdentity) -> Result<(), SysErrNo> {
    const X_OK: usize = 1;
    let norm = normalize_path(path);
    let components: Vec<&str> = norm.split('/').filter(|part| !part.is_empty()).collect();
    if components.len() <= 1 {
        return Ok(());
    }
    let mut current = String::from("/");
    for component in components.iter().take(components.len() - 1) {
        if current != "/" {
            current.push('/');
        }
        current.push_str(component);
        let meta = metadata(&current, true)?;
        check_metadata_access_with_identity(&meta, X_OK, identity)?;
    }
    Ok(())
}

fn check_create_access(parent: &str) -> Result<(), SysErrNo> {
    const W_OK: usize = 2;
    const X_OK: usize = 1;
    let meta = metadata(parent, true)?;
    if meta.kind != VfsNodeKind::Directory {
        return Err(SysErrNo::ENOTDIR);
    }
    check_search_access(parent, CredentialIdentity::Filesystem)?;
    check_metadata_access_with_identity(&meta, W_OK | X_OK, CredentialIdentity::Filesystem)
}

pub fn read_file(name: &str) -> Option<Vec<u8>> {
    let original = normalize_path(name);
    if is_removed(&original) {
        return None;
    }
    let norm = resolve_final_symlink(&original, false).ok()?;
    if is_removed(&norm) {
        return None;
    }
    if let Some((volume, backend_path)) = mounted_vfat_backend(&norm) {
        return volume.read_file(&backend_path).ok();
    }
    if is_tmpfs_path(&norm) || mounted_ext4_backend_path(&norm).is_none() {
        let m = MEM_FS.lock();
        if let Some(f) = m.get_file(&norm) {
            return Some(f.content.to_vec());
        }
        drop(m);
    }
    if is_tmpfs_path(&norm) {
        return None;
    }
    ext4_vol::slurp_regular_file(&ext4_lookup_path(&norm))
}

fn is_elf_image(data: &[u8]) -> bool {
    data.len() >= 4 && &data[..4] == b"\x7fELF"
}

pub fn read_executable_file(name: &str) -> Option<Vec<u8>> {
    let original = normalize_path(name);
    if is_removed(&original) {
        return None;
    }
    let norm = resolve_final_symlink(&original, false).ok()?;
    if is_removed(&norm) {
        return None;
    }

    if let Some((volume, backend_path)) = mounted_vfat_backend(&norm) {
        return volume
            .read_file(&backend_path)
            .ok()
            .filter(|data| is_elf_image(data));
    }

    let tmpfs_path = is_tmpfs_path(&norm);
    let mem_data = if tmpfs_path || mounted_ext4_backend_path(&norm).is_none() {
        MEM_FS.lock().get_file(&norm).map(|f| f.content.to_vec())
    } else {
        None
    };
    if mem_data.as_ref().is_some_and(|data| is_elf_image(data)) {
        return mem_data;
    }

    if tmpfs_path {
        return mem_data;
    }

    let ext4_data = ext4_vol::slurp_regular_file(&ext4_lookup_path(&norm));
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
    if let Some((volume, backend_path)) = mounted_vfat_backend(&norm) {
        return volume
            .metadata(&backend_path)
            .is_ok_and(|meta| !meta.is_dir);
    }
    if is_tmpfs_path(&norm) || mounted_ext4_backend_path(&norm).is_none() {
        let mem = MEM_FS.lock();
        if mem.get_file(&norm).is_some()
            || mem.get_symlink(&norm).is_some()
            || mem.get_special(&norm).is_some()
        {
            return true;
        }
    }
    if is_tmpfs_path(&norm) {
        return false;
    }
    matches!(
        ext4_vol::lookup_kind(&ext4_lookup_path(&norm)),
        Some((
            _,
            ext4_vol::Ext4NodeKind::Regular
                | ext4_vol::Ext4NodeKind::Symlink
                | ext4_vol::Ext4NodeKind::Other
        ))
    )
}

pub fn dir_exists(name: &str) -> bool {
    let norm = normalize_path(name);
    if is_removed(&norm) {
        return false;
    }
    if let Some((volume, backend_path)) = mounted_vfat_backend(&norm) {
        return volume.metadata(&backend_path).is_ok_and(|meta| meta.is_dir);
    }
    if is_tmpfs_path(&norm) {
        return MEM_FS.lock().is_dir(&norm);
    }
    mounted_ext4_backend_path(&norm).is_some()
        || MEM_FS.lock().is_dir(&norm)
        || ext4_vol::ext4_dir_path_exists(&ext4_lookup_path(&norm))
}

fn path_uses_ext4(path: &str) -> bool {
    let norm = normalize_path(path);
    if !ext4_vol::is_ext4_mounted() || is_tmpfs_path(&norm) {
        return false;
    }
    if mounted_ext4_backend_path(&norm).is_some() || ext4_vol::lookup_kind(&norm).is_some() {
        return true;
    }
    MOUNT_TABLE
        .lock()
        .iter()
        .any(|entry| entry.fstype == "ext4" && path_is_under(&norm, &entry.host_target))
}

pub fn filesystem_magic() -> usize {
    if path_uses_ext4("/") {
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

fn vfat_statfs() -> VfsStatFs {
    VfsStatFs {
        f_type: 0x4d44,
        f_bsize: 512,
        f_blocks: 0,
        f_bfree: 0,
        f_bavail: 0,
        f_files: 0,
        f_ffree: 0,
        f_namelen: 255,
        f_frsize: 512,
        f_flags: 1,
    }
}

fn current_statfs(use_ext4: bool) -> VfsStatFs {
    if use_ext4 {
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
    }
    mem_statfs()
}

#[cfg(any())]
fn old_current_statfs_unused() -> VfsStatFs {
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
    if is_tmpfs_path(path) {
        return Ok(mem_statfs());
    }
    if mounted_vfat_backend(path).is_some() {
        return Ok(vfat_statfs());
    }
    Ok(current_statfs(path_uses_ext4(path)))
}

pub fn statfs_for_fd(file: &fd::FileDescriptor) -> Result<VfsStatFs, SysErrNo> {
    metadata_for_fd(file)?;
    let use_ext4 = matches!(
        file,
        fd::FileDescriptor::Ext4Regular { .. } | fd::FileDescriptor::Ext4Dir { .. }
    ) || match file {
        fd::FileDescriptor::MemFile { name, .. } => path_uses_ext4(name),
        fd::FileDescriptor::MemDir { path, .. } => path_uses_ext4(path),
        fd::FileDescriptor::Path { host_path, .. } => path_uses_ext4(host_path),
        fd::FileDescriptor::LoopControl | fd::FileDescriptor::LoopDevice { .. } => false,
        _ => false,
    };
    let use_vfat = match file {
        fd::FileDescriptor::MemFile { name, .. } => mounted_vfat_backend(name).is_some(),
        fd::FileDescriptor::MemDir { path, .. } => mounted_vfat_backend(path).is_some(),
        fd::FileDescriptor::Path { host_path, .. } => mounted_vfat_backend(host_path).is_some(),
        fd::FileDescriptor::LoopControl | fd::FileDescriptor::LoopDevice { .. } => false,
        _ => false,
    };
    if use_vfat {
        return Ok(vfat_statfs());
    }
    Ok(current_statfs(use_ext4))
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
    if let Some(meta) = vfat_metadata_for_path(&norm) {
        let meta = meta?;
        return if meta.is_dir {
            Err(SysErrNo::EISDIR)
        } else {
            Err(SysErrNo::EROFS)
        };
    }
    let tmpfs_path = is_tmpfs_path(&norm);
    {
        let mut m = MEM_FS.lock();
        if m.is_dir(&norm) {
            return Err(SysErrNo::EISDIR);
        }
        if m.get_file(&norm).is_some()
            || m.get_symlink(&norm).is_some()
            || m.get_special(&norm).is_some()
        {
            m.remove_file(&norm)?;
            mark_whiteout(&norm);
            return Ok(());
        }
    }

    if tmpfs_path {
        return Err(missing_path_errno(&norm));
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
    if let Some(meta) = vfat_metadata_for_path(&norm) {
        let meta = meta?;
        return if meta.is_dir {
            Err(SysErrNo::EROFS)
        } else {
            Err(SysErrNo::ENOTDIR)
        };
    }
    let tmpfs_path = is_tmpfs_path(&norm);
    {
        let mut m = MEM_FS.lock();
        if m.get_file(&norm).is_some()
            || m.get_symlink(&norm).is_some()
            || m.get_special(&norm).is_some()
        {
            return Err(SysErrNo::ENOTDIR);
        }
        if m.is_dir(&norm) {
            m.remove_dir(&norm)?;
            mark_whiteout(&norm);
            return Ok(());
        }
    }

    if tmpfs_path {
        return Err(missing_path_errno(&norm));
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
    if let Some(meta) = vfat_metadata_for_path(&old) {
        meta?;
        return Err(SysErrNo::EROFS);
    }
    if let Some(parent_meta) = parent_vfat_metadata_for_path(&new) {
        let parent_meta = parent_meta?;
        return if parent_meta.is_dir {
            Err(SysErrNo::EROFS)
        } else {
            Err(SysErrNo::ENOTDIR)
        };
    }
    if no_replace && (file_exists(&new) || dir_exists(&new)) {
        return Err(SysErrNo::EEXIST);
    }
    let old_tmpfs = is_tmpfs_path(&old);
    let new_tmpfs = is_tmpfs_path(&new);
    if old_tmpfs != new_tmpfs {
        return Err(SysErrNo::EXDEV);
    }

    let mut m = MEM_FS.lock();
    if m.exists(&old) {
        if !no_replace
            && !new_tmpfs
            && (ext4_vol::ext4_file_path_exists(&new) || ext4_vol::ext4_dir_path_exists(&new))
        {
            return Err(SysErrNo::EEXIST);
        }
        let new_parent = parent_path(&new);
        if !m.is_dir(&new_parent) {
            drop(m);
            if !new_tmpfs && ext4_vol::ext4_dir_path_exists(&new_parent) {
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

    if old_tmpfs {
        return Err(missing_path_errno(&old));
    }

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
    if let Some(meta) = vfat_metadata_for_path(&old) {
        meta?;
        return Err(SysErrNo::EXDEV);
    }
    if let Some(parent_meta) = parent_vfat_metadata_for_path(&new) {
        let parent_meta = parent_meta?;
        return if parent_meta.is_dir {
            Err(SysErrNo::EROFS)
        } else {
            Err(SysErrNo::ENOTDIR)
        };
    }
    if file_exists(&new) || dir_exists(&new) {
        return Err(SysErrNo::EEXIST);
    }
    let old_tmpfs = is_tmpfs_path(&old);
    let new_tmpfs = is_tmpfs_path(&new);
    if old_tmpfs != new_tmpfs {
        return Err(SysErrNo::EXDEV);
    }
    if MEM_FS.lock().exists(&old) {
        if procfs_link_source(&old) {
            return Err(SysErrNo::EXDEV);
        }
        let new_parent = parent_path(&new);
        let mem_new_parent = MEM_FS.lock().is_dir(&new_parent);
        let ext_new_parent = !new_tmpfs && ext4_vol::ext4_dir_path_exists(&new_parent);
        if !mem_new_parent {
            if ext_new_parent {
                return Err(SysErrNo::EXDEV);
            }
            if path_exists_non_dir(&new_parent) {
                return Err(SysErrNo::ENOTDIR);
            }
            return Err(SysErrNo::ENOENT);
        }
        if ext_new_parent && !super::is_memfs_volatile_dir(&new_parent) {
            return Err(SysErrNo::EXDEV);
        }
        MEM_FS.lock().link_path(&old, &new)?;
        clear_whiteout(&new);
        return Ok(());
    }
    if old_tmpfs {
        return Err(SysErrNo::ENOENT);
    }
    if ext4_vol::lookup_kind(&old).is_none() && path_exists_non_dir(&parent_path(&old)) {
        return Err(SysErrNo::ENOTDIR);
    }
    if ext4_vol::lookup_kind(&old).is_none() {
        return Err(SysErrNo::ENOENT);
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
    if let Some(parent_meta) = vfat_metadata_for_path(&parent) {
        let parent_meta = parent_meta?;
        return if parent_meta.is_dir {
            Err(SysErrNo::EROFS)
        } else {
            Err(SysErrNo::ENOTDIR)
        };
    }
    let parent_tmpfs = is_tmpfs_path(&parent);
    let mem_parent = MEM_FS.lock().is_dir(&parent);
    let ext_parent = !parent_tmpfs && ext4_vol::ext4_dir_path_exists(&parent);
    if mem_parent || ext_parent {
        check_create_access(&parent)?;
    }
    if mem_parent && (super::is_memfs_volatile_dir(&parent) || !ext_parent) {
        MEM_FS.lock().add_symlink(&norm, target)?;
        clear_whiteout(&norm);
        return Ok(());
    }
    if ext_parent {
        clear_whiteout(&norm);
        return ext4_vol::create_symlink_ext4(target, &norm);
    }
    if mem_parent {
        MEM_FS.lock().add_symlink(&norm, target)?;
        clear_whiteout(&norm);
        return Ok(());
    }
    if path_exists_non_dir(&parent) {
        return Err(SysErrNo::ENOTDIR);
    }
    Err(SysErrNo::ENOENT)
}

pub fn read_link(path: &str) -> Result<String, SysErrNo> {
    let norm = resolve_parent_symlinks_for_lookup(path)?;
    if is_removed(&norm) {
        return Err(SysErrNo::ENOENT);
    }
    check_search_access(&norm, CredentialIdentity::Filesystem)?;
    if let Some(meta) = vfat_metadata_for_path(&norm) {
        meta?;
        return Err(SysErrNo::EINVAL);
    }
    if MEM_FS.lock().exists(&norm) {
        if let Some(target) = MEM_FS.lock().get_symlink(&norm) {
            return Ok(target);
        }
        return Err(SysErrNo::EINVAL);
    }
    if is_tmpfs_path(&norm) {
        return Err(missing_path_errno(&norm));
    }
    match ext4_vol::lookup_kind(&norm) {
        Some((_ino, ext4_vol::Ext4NodeKind::Symlink)) => ext4_vol::readlink_ext4(&norm),
        Some(_) => Err(SysErrNo::EINVAL),
        None => Err(missing_path_errno(&norm)),
    }
}

pub fn truncate_path(path: &str, size: u64) -> Result<(), SysErrNo> {
    let norm = normalize_path(path);
    if let Some(meta) = vfat_metadata_for_path(&norm) {
        let meta = meta?;
        return if meta.is_dir {
            Err(SysErrNo::EISDIR)
        } else {
            Err(SysErrNo::EROFS)
        };
    }
    {
        let mut mem = MEM_FS.lock();
        if mem.is_dir(&norm) {
            return Err(SysErrNo::EISDIR);
        }
        if mem.get_file(&norm).is_some() {
            return mem.truncate_file(&norm, size as usize);
        }
        if mem.get_special(&norm).is_some() {
            return Err(SysErrNo::EINVAL);
        }
    }
    if is_tmpfs_path(&norm) {
        return Err(missing_path_errno(&norm));
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

pub fn set_mode_path(path: &str, follow_symlink: bool, mode: u32) -> Result<(), SysErrNo> {
    let norm = normalize_path(path);
    if let Some(meta) = vfat_metadata_for_path(&norm) {
        meta?;
        return Err(SysErrNo::EROFS);
    }
    {
        let mut mem = MEM_FS.lock();
        if mem.is_dir(&norm) || mem.get_file(&norm).is_some() || mem.get_special(&norm).is_some() {
            return mem.set_mode(&norm, mode);
        }
    }
    if is_tmpfs_path(&norm) {
        return Err(missing_path_errno(&norm));
    }
    let ext_path = match ext4_vol::lookup_kind(&norm) {
        Some((_ino, ext4_vol::Ext4NodeKind::Symlink)) if follow_symlink => {
            ext4_vol::resolve_symlinks(&norm)?
        }
        Some(_) => norm.clone(),
        None => return Err(missing_path_errno(&norm)),
    };
    ext4_vol::set_mode_path(&ext_path, mode)
}

pub fn set_mode_fd(file: &mut fd::FileDescriptor, mode: u32) -> Result<(), SysErrNo> {
    match file {
        fd::FileDescriptor::MemFile { name, .. } => MEM_FS.lock().set_mode(name, mode),
        fd::FileDescriptor::MemDir { path, .. } => MEM_FS.lock().set_mode(path, mode),
        fd::FileDescriptor::Ext4Regular { ino, .. } | fd::FileDescriptor::Ext4Dir { ino, .. } => {
            ext4_vol::set_mode_ino(*ino, mode)
        }
        _ => Err(SysErrNo::EINVAL),
    }
}

pub fn set_owner_path(
    path: &str,
    follow_symlink: bool,
    uid: Option<u32>,
    gid: Option<u32>,
) -> Result<(), SysErrNo> {
    let norm = normalize_path(path);
    if let Some(meta) = vfat_metadata_for_path(&norm) {
        meta?;
        return Err(SysErrNo::EROFS);
    }
    {
        let mut mem = MEM_FS.lock();
        if mem.is_dir(&norm) || mem.get_file(&norm).is_some() || mem.get_special(&norm).is_some() {
            return mem.set_owner(&norm, uid, gid);
        }
    }
    if is_tmpfs_path(&norm) {
        return Err(missing_path_errno(&norm));
    }
    let ext_path = match ext4_vol::lookup_kind(&norm) {
        Some((_ino, ext4_vol::Ext4NodeKind::Symlink)) if follow_symlink => {
            ext4_vol::resolve_symlinks(&norm)?
        }
        Some(_) => norm.clone(),
        None => return Err(missing_path_errno(&norm)),
    };
    ext4_vol::set_owner_path(&ext_path, uid, gid)
}

pub fn set_owner_fd(
    file: &mut fd::FileDescriptor,
    uid: Option<u32>,
    gid: Option<u32>,
) -> Result<(), SysErrNo> {
    match file {
        fd::FileDescriptor::MemFile { name, .. } => MEM_FS.lock().set_owner(name, uid, gid),
        fd::FileDescriptor::MemDir { path, .. } => MEM_FS.lock().set_owner(path, uid, gid),
        fd::FileDescriptor::Ext4Regular { ino, .. } | fd::FileDescriptor::Ext4Dir { ino, .. } => {
            ext4_vol::set_owner_ino(*ino, uid, gid)
        }
        _ => Err(SysErrNo::EINVAL),
    }
}

pub fn set_times_path(
    path: &str,
    follow_symlink: bool,
    atime: Option<(isize, isize)>,
    mtime: Option<(isize, isize)>,
) -> Result<(), SysErrNo> {
    let norm = normalize_path(path);
    if let Some(meta) = vfat_metadata_for_path(&norm) {
        meta?;
        return Err(SysErrNo::EROFS);
    }
    {
        let mut mem = MEM_FS.lock();
        if mem.is_dir(&norm) {
            return Ok(());
        }
        if mem.get_file(&norm).is_some() {
            return mem.set_file_times(&norm, atime, mtime);
        }
        if mem.get_special(&norm).is_some() {
            return Ok(());
        }
    }
    if is_tmpfs_path(&norm) {
        return Err(missing_path_errno(&norm));
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
    if let Some(parent_meta) = vfat_metadata_for_path(&parent) {
        let parent_meta = parent_meta?;
        return if parent_meta.is_dir {
            Err(SysErrNo::EROFS)
        } else {
            Err(SysErrNo::ENOTDIR)
        };
    }
    let parent_tmpfs = is_tmpfs_path(&parent);
    let mem_parent = MEM_FS.lock().is_dir(&parent);
    let ext_parent = !parent_tmpfs && ext4_vol::ext4_dir_path_exists(&parent);
    if mem_parent && (super::is_memfs_volatile_dir(&parent) || !ext_parent) {
        MEM_FS.lock().add_dir_with_mode(&norm, mode);
        return Ok(());
    }
    if ext_parent {
        ext4_vol::mkdir_ext4_with_mode(&norm, mode)?;
        return Ok(());
    }
    if mem_parent {
        MEM_FS.lock().add_dir_with_mode(&norm, mode);
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
    if let Some(parent_meta) = vfat_metadata_for_path(&parent) {
        let parent_meta = parent_meta?;
        return if parent_meta.is_dir {
            Err(SysErrNo::EROFS)
        } else {
            Err(SysErrNo::ENOTDIR)
        };
    }
    let parent_tmpfs = is_tmpfs_path(&parent);
    let mem_parent = MEM_FS.lock().is_dir(&parent);
    let ext_parent = !parent_tmpfs && ext4_vol::ext4_dir_path_exists(&parent);
    if mem_parent && (super::is_memfs_volatile_dir(&parent) || !ext_parent) {
        MEM_FS.lock().add_file_with_mode(&norm, Vec::new(), mode);
        return Ok(pseudo_inode(&norm) as u32);
    }
    if ext_parent {
        return ext4_vol::create_regular_ext4_with_mode(&norm, mode);
    }
    if mem_parent {
        MEM_FS.lock().add_file_with_mode(&norm, Vec::new(), mode);
        return Ok(pseudo_inode(&norm) as u32);
    }
    Err(SysErrNo::ENOENT)
}

pub fn create_special_node(path: &str, kind: MemSpecialKind, mode: u32) -> Result<u32, SysErrNo> {
    let norm = normalize_path(path);
    if file_exists(&norm) || dir_exists(&norm) {
        return Err(SysErrNo::EEXIST);
    }
    clear_whiteout(&norm);
    let parent = parent_path(&norm);
    if let Some(parent_meta) = vfat_metadata_for_path(&parent) {
        let parent_meta = parent_meta?;
        return if parent_meta.is_dir {
            Err(SysErrNo::EROFS)
        } else {
            Err(SysErrNo::ENOTDIR)
        };
    }
    let parent_tmpfs = is_tmpfs_path(&parent);
    let mem_parent = MEM_FS.lock().is_dir(&parent);
    let ext_parent = !parent_tmpfs && ext4_vol::ext4_dir_path_exists(&parent);
    if mem_parent || ext_parent {
        check_create_access(&parent)?;
    }
    if mem_parent && (super::is_memfs_volatile_dir(&parent) || !ext_parent) {
        MEM_FS.lock().add_special_with_mode(&norm, kind, mode)?;
        return Ok(pseudo_inode(&norm) as u32);
    }
    if ext_parent {
        return Err(SysErrNo::EOPNOTSUPP);
    }
    if mem_parent {
        MEM_FS.lock().add_special_with_mode(&norm, kind, mode)?;
        return Ok(pseudo_inode(&norm) as u32);
    }
    if path_exists_non_dir(&parent) {
        return Err(SysErrNo::ENOTDIR);
    }
    Err(SysErrNo::ENOENT)
}

fn open_dir_descriptor(
    host_path: &str,
    logical_path: &str,
) -> Result<fd::FileDescriptor, SysErrNo> {
    if let Some((volume, backend_path)) = mounted_vfat_backend(host_path) {
        let entries = volume.list_dir(&backend_path)?;
        return Ok(fd::FileDescriptor::MemDir {
            path: logical_path.into(),
            entries,
            offset: 0,
        });
    }
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

fn open_path_descriptor(
    host_path: &str,
    logical_path: &str,
    flags: u32,
    follow_symlink: bool,
) -> Result<fd::FileDescriptor, SysErrNo> {
    let meta = metadata(host_path, follow_symlink)?;
    Ok(fd::FileDescriptor::Path {
        logical_path: normalize_path(logical_path),
        host_path: normalize_path(host_path),
        kind: meta.kind,
        flags,
    })
}

fn path_exists_non_dir(path: &str) -> bool {
    let norm = normalize_path(path);
    if is_removed(&norm) {
        return false;
    }
    if let Some(meta) = vfat_metadata_for_path(&norm) {
        return meta.is_ok_and(|meta| !meta.is_dir);
    }
    let tmpfs_path = is_tmpfs_path(&norm);
    let ext_norm = ext4_lookup_path(&norm);
    let mounted = mounted_ext4_backend_path(&norm).is_some();
    {
        let mem = MEM_FS.lock();
        if !mounted && mem.get_file(&norm).is_some() {
            return true;
        }
        if !mounted && mem.get_symlink(&norm).is_some() {
            return true;
        }
        if !mounted && mem.get_special(&norm).is_some() {
            return true;
        }
        if !mounted && mem.is_dir(&norm) {
            return false;
        }
    }
    if tmpfs_path {
        return false;
    }
    matches!(
        ext4_vol::lookup_kind(&ext_norm),
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

fn open_loop_device_descriptor(
    local_path: &str,
    read_ok: bool,
    write_ok: bool,
) -> Option<Result<fd::FileDescriptor, SysErrNo>> {
    if block_dev::loop_control_path(local_path) {
        let _ = (read_ok, write_ok);
        return Some(Ok(fd::FileDescriptor::LoopControl));
    }
    block_dev::loop_index_for_path(local_path).map(|index| {
        Ok(fd::FileDescriptor::LoopDevice {
            index,
            offset: 0,
            readable: read_ok,
            writable: write_ok,
        })
    })
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
    let noatime = (flags & O_NOATIME) != 0;
    let path_only = (flags & O_PATH) != 0;
    let append = (flags & O_APPEND) != 0;
    let mut open_access = 0usize;
    if read_ok {
        open_access |= 4;
    }
    if write_ok {
        open_access |= 2;
    }

    if want_dir && want_create {
        return Err(SysErrNo::EINVAL);
    }

    let removed = is_removed(&path_norm);
    if removed && !want_create {
        return Err(SysErrNo::ENOENT);
    }

    if path_only {
        if want_create || want_trunc {
            return Err(SysErrNo::EINVAL);
        }
        return open_path_descriptor(&path_norm, &logical_norm, flags, !nofollow);
    }

    let open_norm = if !removed {
        resolve_final_symlink(&path_norm, nofollow)?
    } else {
        path_norm.clone()
    };
    if let Some(opened) =
        open_loop_device_descriptor(local_device_path(&open_norm), read_ok, write_ok)
    {
        if want_dir {
            return Err(SysErrNo::ENOTDIR);
        }
        if want_create || want_trunc {
            return Err(SysErrNo::EINVAL);
        }
        if want_create && want_excl {
            return Err(SysErrNo::EEXIST);
        }
        return opened;
    }
    if let Some((volume, backend_path)) = mounted_vfat_backend(&open_norm) {
        if want_create || want_trunc || write_ok {
            return Err(SysErrNo::EROFS);
        }
        let meta = volume.metadata(&backend_path)?;
        if want_dir && !meta.is_dir {
            return Err(SysErrNo::ENOTDIR);
        }
        if meta.is_dir {
            let entries = volume.list_dir(&backend_path)?;
            return Ok(fd::FileDescriptor::MemDir {
                path: logical_norm,
                entries,
                offset: 0,
            });
        }
        let data = volume.read_file(&backend_path)?;
        return Ok(fd::FileDescriptor::MemFile {
            name: open_norm,
            content: fd::MemFileContent::from_slice(&data),
            times: super::FileTimes::now(),
            offset: 0,
            readable: read_ok,
            writable: false,
            append: false,
            linked: false,
        });
    }
    let tmpfs_path = is_tmpfs_path(&open_norm);
    let mounted_ext4 = !tmpfs_path && mounted_ext4_backend_path(&open_norm).is_some();
    let mem_has_file = !mounted_ext4 && MEM_FS.lock().get_file(&open_norm).is_some();
    let mem_has_special = !mounted_ext4 && MEM_FS.lock().get_special(&open_norm).is_some();
    let ext_path_norm = ext4_lookup_path(&open_norm);

    if mem_has_file {
        if want_dir {
            return Err(SysErrNo::ENOTDIR);
        }
        if want_excl && want_create {
            return Err(SysErrNo::EEXIST);
        }
        check_access_with_filesystem(&open_norm, true, open_access)?;
        if noatime {
            let meta = metadata(&open_norm, true)?;
            check_noatime_permission(&meta)?;
        }
        let source = MEM_FS
            .lock()
            .get_file(&open_norm)
            .map(|file| (file.content.clone(), file.times));
        let (mut content, mut times) =
            source.unwrap_or_else(|| (fd::MemFileContent::new(), super::FileTimes::now()));
        if want_trunc && write_ok {
            content.clear();
            MEM_FS.lock().truncate_file(&open_norm, 0)?;
            if let Some(file) = MEM_FS.lock().get_file(&open_norm) {
                times = file.times;
            }
        }
        let base_off = if append && write_ok { content.len() } else { 0 };
        return Ok(fd::FileDescriptor::MemFile {
            name: open_norm,
            content,
            times,
            offset: base_off,
            readable: read_ok,
            writable: write_ok,
            append,
            linked: true,
        });
    }

    if mem_has_special {
        if want_dir {
            return Err(SysErrNo::ENOTDIR);
        }
        if want_create && want_excl {
            return Err(SysErrNo::EEXIST);
        }
        return Err(SysErrNo::ENXIO);
    }

    if (!mounted_ext4 && MEM_FS.lock().is_dir(&open_norm))
        || (!tmpfs_path && ext4_vol::ext4_dir_path_exists(&ext_path_norm))
    {
        if write_ok || want_trunc || want_create {
            return Err(SysErrNo::EISDIR);
        }
        let open_path = if !mounted_ext4 && MEM_FS.lock().is_dir(&open_norm) {
            open_norm.clone()
        } else {
            ext_path_norm.clone()
        };
        check_access_with_filesystem(&open_path, true, open_access)?;
        return open_dir_descriptor(&open_path, &logical_norm);
    }

    if want_dir {
        if (!tmpfs_path && ext4_vol::ext4_regular_file_exists(&ext_path_norm))
            || path_exists_non_dir(&open_norm)
        {
            return Err(SysErrNo::ENOTDIR);
        }
        return Err(missing_path_errno(&open_norm));
    }

    if !tmpfs_path && !removed && ext4_vol::ext4_regular_file_exists(&ext_path_norm) {
        if want_excl && want_create {
            return Err(SysErrNo::EEXIST);
        }
        let Some((ino, is_dir)) = ext4_vol::lookup_path(&ext_path_norm) else {
            return Err(SysErrNo::ENOENT);
        };
        if is_dir {
            return Err(SysErrNo::EISDIR);
        }
        check_access_with_filesystem(&ext_path_norm, true, open_access)?;
        if noatime {
            let meta = metadata(&ext_path_norm, true)?;
            check_noatime_permission(&meta)?;
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
        let parent = parent_path(&open_norm);
        let parent_tmpfs = is_tmpfs_path(&parent);
        let parent_mounted_ext4 = !parent_tmpfs && mounted_ext4_backend_path(&parent).is_some();
        let mem_parent = !parent_mounted_ext4 && MEM_FS.lock().is_dir(&parent);
        let ext_parent =
            !parent_tmpfs && ext4_vol::ext4_dir_path_exists(&ext4_lookup_path(&parent));
        if mem_parent && (super::is_memfs_volatile_dir(&parent) || !ext_parent) {
            MEM_FS
                .lock()
                .add_file_with_mode(&open_norm, Vec::new(), mode);
            clear_whiteout(&open_norm);
            let times = MEM_FS
                .lock()
                .get_file(&open_norm)
                .map(|file| file.times)
                .unwrap_or_else(super::FileTimes::now);
            return Ok(fd::FileDescriptor::MemFile {
                name: open_norm,
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
            let create_path = ext4_lookup_path(&open_norm);
            let ino = ext4_vol::create_regular_ext4_with_mode(&create_path, mode)?;
            clear_whiteout(&open_norm);
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
            MEM_FS
                .lock()
                .add_file_with_mode(&open_norm, Vec::new(), mode);
            clear_whiteout(&open_norm);
            let times = MEM_FS
                .lock()
                .get_file(&open_norm)
                .map(|file| file.times)
                .unwrap_or_else(super::FileTimes::now);
            return Ok(fd::FileDescriptor::MemFile {
                name: open_norm,
                content: fd::MemFileContent::new(),
                times,
                offset: 0,
                readable: read_ok,
                writable: write_ok,
                append,
                linked: true,
            });
        }
        return Err(missing_path_errno(&open_norm));
    }

    Err(missing_path_errno(&open_norm))
}

pub fn list_dir(path: &str) -> Result<Vec<fd::DirEntryRecord>, SysErrNo> {
    let norm = normalize_path(path);
    if let Some((volume, backend_path)) = mounted_vfat_backend(&norm) {
        return volume.list_dir(&backend_path);
    }
    let tmpfs_path = is_tmpfs_path(&norm);
    let ext_norm = ext4_lookup_path(&norm);
    let mounted_ext4 = !tmpfs_path && mounted_ext4_backend_path(&norm).is_some();

    let mem_dir = !mounted_ext4 && MEM_FS.lock().is_dir(&norm);
    let mem_entries: Vec<fd::DirEntryRecord> = if mem_dir {
        MEM_FS.lock().list_dir(&norm)?
    } else {
        Vec::new()
    };

    let ext_dir =
        !tmpfs_path && ext4_vol::is_ext4_mounted() && ext4_vol::ext4_dir_path_exists(&ext_norm);

    if !mem_dir && !ext_dir {
        let m = MEM_FS.lock();
        if m.get_file(&norm).is_some() {
            return Err(SysErrNo::ENOTDIR);
        }
        drop(m);
        if ext4_vol::is_ext4_mounted() && ext4_vol::ext4_file_path_exists(&ext_norm) {
            return Err(SysErrNo::ENOTDIR);
        }
        return Err(SysErrNo::ENOENT);
    }

    let mut map: BTreeMap<String, fd::DirEntryRecord> = BTreeMap::new();
    for e in mem_entries {
        map.insert(e.name.clone(), e);
    }
    if ext_dir {
        for (name, is_dir) in ext4_vol::ext4_list_dir(&ext_norm)? {
            let child = if ext_norm == "/" {
                alloc::format!("/{}", name)
            } else {
                alloc::format!("{}/{}", ext_norm, name)
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
