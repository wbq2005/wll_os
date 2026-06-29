/// 内存文件系统实现
///
/// 提供一个简单的基于内存的文件系统，用于支持用户程序加载和基本文件操作
pub mod block_dev;
pub mod ext4_vol;
pub mod fd;
pub mod vfat;
pub mod vfs;

#[allow(unused_imports)]
pub use vfs::{
    check_access, check_access_with_effective, check_fd_access, check_fd_access_with_effective,
    check_metadata_access, check_metadata_access_with_effective, create_dir, create_dir_with_mode,
    create_regular_file, create_special_node, create_symlink, dir_exists, file_exists,
    file_flags_for_fd, filesystem_magic, is_removed, link_mem_file_fd, link_path, list_dir,
    list_files, list_xattr_fd, list_xattr_path, metadata, metadata_for_fd, metadata_for_lookup,
    mount_fs, open_path, path_contains_symlink, path_crosses_mountpoint, read_executable_file,
    read_file, read_interpreter, read_link, refresh_block_device_nodes, remove_dir, remove_file,
    remove_xattr_fd, remove_xattr_path, rename_exchange_path, rename_path, set_file_flags_for_fd,
    set_mode_fd, set_mode_path, set_owner_fd, set_owner_path, set_times_fd, set_times_path,
    set_xattr_fd, set_xattr_path, statfs_for_fd, statfs_for_path, sync_all, sync_fd, truncate_fd,
    truncate_path, umount_fs, get_xattr_fd, get_xattr_path,
    TimesUpdatePermission, VfsMetadata, VfsNodeKind, VfsStatFs,
};

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::format;
use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use lazy_static::lazy_static;
use spin::Mutex;

use crate::utils::error::SysErrNo;

/// 文件内容
pub type FileContent = fd::MemFileContent;

#[derive(Clone, Copy, Debug)]
pub struct MemNodeMetadata {
    pub mode: u32,
    pub uid: u32,
    pub gid: u32,
    pub ino: u64,
    pub flags: u32,
}

impl MemNodeMetadata {
    fn new(mode: u32) -> Self {
        Self {
            mode: mode & 0o7777,
            uid: 0,
            gid: 0,
            ino: 0,
            flags: 0,
        }
    }

    fn new_with_ino(mode: u32, ino: u64) -> Self {
        Self {
            ino,
            ..Self::new(mode)
        }
    }

    fn new_for_current(mode: u32) -> Self {
        let credentials = crate::task::current_task()
            .map(|task| task.credentials.lock().clone())
            .unwrap_or_else(crate::task::Credentials::root);
        Self {
            mode: mode & 0o7777,
            uid: credentials.fsuid,
            gid: credentials.fsgid,
            ino: 0,
            flags: 0,
        }
    }

    fn new_child_for_current(parent: Option<MemNodeMetadata>, mode: u32, is_dir: bool) -> Self {
        let credentials = crate::task::current_task()
            .map(|task| task.credentials.lock().clone())
            .unwrap_or_else(crate::task::Credentials::root);
        let parent_setgid = parent.is_some_and(|meta| (meta.mode & 0o2000) != 0);
        let mut child_mode = mode & 0o7777;
        if is_dir && parent_setgid {
            child_mode |= 0o2000;
        }
        if !is_dir
            && parent_setgid
            && (child_mode & 0o2000) != 0
            && !credentials.is_root_capable()
            && !credentials.is_in_filesystem_group(parent.map(|meta| meta.gid).unwrap_or(0))
        {
            child_mode &= !0o2000;
        }
        Self {
            mode: child_mode,
            uid: credentials.fsuid,
            gid: parent
                .filter(|_| parent_setgid)
                .map(|meta| meta.gid)
                .unwrap_or(credentials.fsgid),
            ino: 0,
            flags: 0,
        }
    }
}

pub(crate) fn chown_mode_after_owner_update(
    mode: u32,
    is_regular: bool,
    owner_update_requested: bool,
) -> u32 {
    if !is_regular || !owner_update_requested {
        return mode;
    }
    let mut next = mode & !0o4000;
    if (mode & 0o0010) != 0 {
        next &= !0o2000;
    }
    next
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MemSpecialKind {
    Fifo,
    Socket,
    CharDevice { major: u32, minor: u32 },
    BlockDevice { major: u32, minor: u32 },
}

fn is_elf_content(content: &[u8]) -> bool {
    content.len() >= 4 && &content[..4] == b"\x7fELF"
}

#[derive(Clone, Copy, Debug)]
pub struct FileTimes {
    pub atime_sec: isize,
    pub atime_nsec: isize,
    pub mtime_sec: isize,
    pub mtime_nsec: isize,
    pub ctime_sec: isize,
    pub ctime_nsec: isize,
}

impl FileTimes {
    pub fn now() -> Self {
        let (sec, usec) = crate::timer::get_timeval();
        let sec = sec as isize;
        let nsec = (usec * 1000) as isize;
        Self {
            atime_sec: sec,
            atime_nsec: nsec,
            mtime_sec: sec,
            mtime_nsec: nsec,
            ctime_sec: sec,
            ctime_nsec: nsec,
        }
    }

    pub fn touch_modified(&mut self) {
        let now = Self::now();
        self.mtime_sec = now.mtime_sec;
        self.mtime_nsec = now.mtime_nsec;
        self.ctime_sec = now.ctime_sec;
        self.ctime_nsec = now.ctime_nsec;
    }

    pub fn set_access_modify(
        &mut self,
        atime: Option<(isize, isize)>,
        mtime: Option<(isize, isize)>,
    ) {
        if let Some((sec, nsec)) = atime {
            self.atime_sec = sec;
            self.atime_nsec = nsec;
        }
        if let Some((sec, nsec)) = mtime {
            self.mtime_sec = sec;
            self.mtime_nsec = nsec;
        }
        let now = Self::now();
        self.ctime_sec = now.ctime_sec;
        self.ctime_nsec = now.ctime_nsec;
    }
}

/// 内存中的文件
pub struct MemFileBacking {
    pub content: FileContent,
    pub times: FileTimes,
}

pub struct MemFile {
    pub name: String,
    backing: Arc<Mutex<MemFileBacking>>,
    link_key: String,
}

impl MemFile {
    pub fn new(name: &str, content: Vec<u8>) -> Self {
        Self::with_link_key(
            name,
            FileContent::from_slice(&content),
            FileTimes::now(),
            String::from(name),
        )
    }

    pub fn with_times(name: &str, content: Vec<u8>, times: FileTimes) -> Self {
        Self::with_link_key(
            name,
            FileContent::from_slice(&content),
            times,
            String::from(name),
        )
    }

    fn with_link_key(name: &str, content: FileContent, times: FileTimes, link_key: String) -> Self {
        Self::with_backing(
            name,
            Arc::new(Mutex::new(MemFileBacking { content, times })),
            link_key,
        )
    }

    fn with_backing(name: &str, backing: Arc<Mutex<MemFileBacking>>, link_key: String) -> Self {
        Self {
            name: String::from(name),
            backing,
            link_key,
        }
    }

    fn shared_backing(&self) -> Arc<Mutex<MemFileBacking>> {
        self.backing.clone()
    }

    fn backing_id(&self) -> usize {
        Arc::as_ptr(&self.backing) as usize
    }

    pub fn size(&self) -> usize {
        self.backing.lock().content.len()
    }

    pub fn is_elf_image(&self) -> bool {
        self.backing.lock().content.is_elf_image()
    }

    pub fn times(&self) -> FileTimes {
        self.backing.lock().times
    }

    pub fn snapshot(&self) -> (FileContent, FileTimes) {
        let backing = self.backing.lock();
        (backing.content.clone(), backing.times)
    }

    pub fn to_vec(&self) -> Vec<u8> {
        self.backing.lock().content.to_vec()
    }

    pub fn read_at(&self, offset: usize, buf: &mut [u8]) -> usize {
        self.backing.lock().content.read_at(offset, buf)
    }

    fn replace_content(&self, content: FileContent, times: FileTimes) {
        let mut backing = self.backing.lock();
        backing.content = content;
        backing.times = times;
    }

    fn write_at(&self, offset: usize, buf: &[u8]) -> usize {
        let mut backing = self.backing.lock();
        let written_end = backing.content.write_at(offset, buf);
        backing.times.touch_modified();
        written_end
    }

    fn resize(&self, new_len: usize) {
        let mut backing = self.backing.lock();
        backing.content.resize(new_len);
        backing.times.touch_modified();
    }

    fn set_times(&self, atime: Option<(isize, isize)>, mtime: Option<(isize, isize)>) {
        self.backing.lock().times.set_access_modify(atime, mtime);
    }

    fn touch_ctime(&self) {
        let now = FileTimes::now();
        let mut backing = self.backing.lock();
        backing.times.ctime_sec = now.ctime_sec;
        backing.times.ctime_nsec = now.ctime_nsec;
    }
}

/// 内存文件系统
pub struct MemFileSystem {
    files: Vec<MemFile>,
    dirs: Vec<String>,
    symlinks: BTreeMap<String, String>,
    specials: BTreeMap<String, MemSpecialKind>,
    metadata: BTreeMap<String, MemNodeMetadata>,
    xattrs: BTreeMap<u64, BTreeMap<String, Vec<u8>>>,
    next_ino: u64,
}

impl MemFileSystem {
    pub fn new() -> Self {
        let mut metadata = BTreeMap::new();
        metadata.insert(String::from("/"), MemNodeMetadata::new_with_ino(0o755, 1));
        Self {
            files: Vec::new(),
            dirs: vec!["/".to_string()],
            symlinks: BTreeMap::new(),
            specials: BTreeMap::new(),
            metadata,
            xattrs: BTreeMap::new(),
            next_ino: 2,
        }
    }

    fn allocate_metadata(&mut self, mut metadata: MemNodeMetadata) -> MemNodeMetadata {
        if metadata.ino == 0 {
            metadata.ino = self.next_ino;
            self.next_ino = self.next_ino.saturating_add(1).max(2);
        }
        metadata
    }

    fn insert_new_metadata(&mut self, name: String, metadata: MemNodeMetadata) {
        let metadata = self.allocate_metadata(metadata);
        self.metadata.insert(name, metadata);
    }

    fn ensure_metadata(&mut self, name: &str, mode: u32) {
        let name = normalize_path(name);
        if !self.metadata.contains_key(&name) {
            let metadata = self.allocate_metadata(MemNodeMetadata::new_for_current(mode));
            self.metadata.insert(name, metadata);
        }
    }

    fn metadata_or_alloc(&mut self, name: &str, mode: u32) -> MemNodeMetadata {
        let name = normalize_path(name);
        if let Some(metadata) = self.metadata.get(&name).copied() {
            return metadata;
        }
        let metadata = self.allocate_metadata(MemNodeMetadata::new(mode));
        self.metadata.insert(name, metadata);
        metadata
    }

    fn child_metadata_for_current(&self, name: &str, mode: u32, is_dir: bool) -> MemNodeMetadata {
        let parent_meta = self.metadata(&parent_path(name));
        MemNodeMetadata::new_child_for_current(parent_meta, mode, is_dir)
    }

    pub fn tmpfile_metadata_for_current(&mut self, dir: &str, mode: u32) -> MemNodeMetadata {
        let parent_meta = self.metadata(&normalize_path(dir));
        let mut metadata = MemNodeMetadata::new_child_for_current(parent_meta, mode, false);
        metadata = self.allocate_metadata(metadata);
        metadata
    }

    /// 添加文件
    pub fn add_file(&mut self, name: &str, content: Vec<u8>) {
        let name = normalize_path(name);
        self.ensure_parent_dirs(&name);
        let mode = if is_elf_content(&content) {
            0o777
        } else {
            0o666
        };
        self.symlinks.remove(&name);

        // 如果文件已存在，先删除
        self.files.retain(|f| f.name != name);
        let len = content.len();
        self.files.push(MemFile::new(&name, content));
        self.insert_new_metadata(name.clone(), MemNodeMetadata::new_for_current(mode));
        log::info!("[fs] Added file '{}' ({} bytes)", name, len);
    }

    pub fn add_file_with_mode(&mut self, name: &str, content: Vec<u8>, mode: u32) {
        let name = normalize_path(name);
        self.ensure_parent_dirs(&name);
        self.symlinks.remove(&name);
        self.specials.remove(&name);
        self.files.retain(|f| f.name != name);
        let len = content.len();
        self.files.push(MemFile::new(&name, content));
        let metadata = self.child_metadata_for_current(&name, mode, false);
        self.insert_new_metadata(name.clone(), metadata);
        log::info!("[fs] Added file '{}' ({} bytes)", name, len);
    }

    pub fn add_file_with_metadata(
        &mut self,
        name: &str,
        content: FileContent,
        times: FileTimes,
        metadata: MemNodeMetadata,
    ) -> Result<(), SysErrNo> {
        let name = normalize_path(name);
        let parent = parent_path(&name);
        if !self.is_dir(&parent) {
            return Err(SysErrNo::ENOENT);
        }
        if self.exists(&name) {
            return Err(SysErrNo::EEXIST);
        }
        self.symlinks.remove(&name);
        self.specials.remove(&name);
        self.files.push(MemFile::with_link_key(
            &name,
            content,
            times,
            String::from(&name),
        ));
        self.metadata.insert(name, metadata);
        Ok(())
    }

    pub fn write_file_content(
        &mut self,
        name: &str,
        content: FileContent,
        times: FileTimes,
    ) -> bool {
        let name = normalize_path(name);
        if let Some(file) = self.files.iter().find(|file| file.name == name) {
            file.replace_content(content, times);
            true
        } else {
            false
        }
    }

    /// 添加目录
    pub fn read_file_at(
        &self,
        name: &str,
        offset: usize,
        buf: &mut [u8],
    ) -> Result<usize, SysErrNo> {
        let name = normalize_path(name);
        let file = self
            .files
            .iter()
            .find(|file| file.name == name)
            .ok_or(SysErrNo::ENOENT)?;
        if offset >= file.size() {
            return Ok(0);
        }
        Ok(file.read_at(offset, buf))
    }

    pub fn write_file_at(
        &mut self,
        name: &str,
        offset: usize,
        buf: &[u8],
    ) -> Result<usize, SysErrNo> {
        let name = normalize_path(name);
        let file = self
            .files
            .iter()
            .find(|file| file.name == name)
            .ok_or(SysErrNo::ENOENT)?;
        if offset.checked_add(buf.len()).ok_or(SysErrNo::EFBIG)? > isize::MAX as usize {
            return Err(SysErrNo::EFBIG);
        }
        if buf.is_empty() {
            return Ok(0);
        }
        file.write_at(offset, buf);
        Ok(buf.len())
    }

    pub fn add_dir(&mut self, name: &str) {
        let name = normalize_path(name);
        self.ensure_parent_dirs(&name);
        self.symlinks.remove(&name);
        if !self.dirs.iter().any(|dir| dir == &name) {
            self.dirs.push(name.clone());
            log::info!("[fs] Added directory '{}'", name);
        }
        self.ensure_metadata(&name, 0o755);
    }

    pub fn add_dir_with_mode(&mut self, name: &str, mode: u32) {
        let name = normalize_path(name);
        self.ensure_parent_dirs(&name);
        self.symlinks.remove(&name);
        self.specials.remove(&name);
        if !self.dirs.iter().any(|dir| dir == &name) {
            self.dirs.push(name.clone());
            log::info!("[fs] Added directory '{}'", name);
            let metadata = self.child_metadata_for_current(&name, mode, true);
            self.insert_new_metadata(name, metadata);
        } else {
            self.ensure_metadata(&name, 0o755);
            let _ = self.set_mode(&name, mode);
        }
    }

    pub fn add_symlink(&mut self, name: &str, target: &str) -> Result<(), SysErrNo> {
        let name = normalize_path(name);
        let parent = parent_path(&name);
        if !self.is_dir(&parent) {
            return Err(SysErrNo::ENOENT);
        }
        if self.exists(&name) {
            return Err(SysErrNo::EEXIST);
        }
        self.symlinks.insert(name.clone(), String::from(target));
        let metadata = self.child_metadata_for_current(&name, 0o777, false);
        self.insert_new_metadata(name, metadata);
        Ok(())
    }

    pub fn add_special_with_mode(
        &mut self,
        name: &str,
        kind: MemSpecialKind,
        mode: u32,
    ) -> Result<(), SysErrNo> {
        let name = normalize_path(name);
        let parent = parent_path(&name);
        if !self.is_dir(&parent) {
            return Err(SysErrNo::ENOENT);
        }
        if self.exists(&name) {
            return Err(SysErrNo::EEXIST);
        }
        self.specials.insert(name.clone(), kind);
        let metadata = self.child_metadata_for_current(&name, mode, false);
        self.insert_new_metadata(name, metadata);
        Ok(())
    }

    pub fn add_overlay_special_with_mode(
        &mut self,
        name: &str,
        kind: MemSpecialKind,
        mode: u32,
    ) -> Result<(), SysErrNo> {
        let name = normalize_path(name);
        self.ensure_parent_dirs(&name);
        self.add_special_with_mode(&name, kind, mode)
    }

    /// 获取文件
    pub fn get_file(&self, name: &str) -> Option<&MemFile> {
        let name = normalize_path(name);
        self.files.iter().find(|f| f.name == name)
    }

    pub fn get_symlink(&self, name: &str) -> Option<String> {
        let name = normalize_path(name);
        self.symlinks.get(&name).cloned()
    }

    pub fn get_special(&self, name: &str) -> Option<MemSpecialKind> {
        let name = normalize_path(name);
        self.specials.get(&name).copied()
    }

    /// 检查文件是否存在
    pub fn exists(&self, name: &str) -> bool {
        let name = normalize_path(name);
        self.files.iter().any(|f| f.name == name)
            || self.dirs.iter().any(|dir| dir == &name)
            || self.symlinks.contains_key(&name)
            || self.specials.contains_key(&name)
    }

    pub fn is_dir(&self, name: &str) -> bool {
        let name = normalize_path(name);
        self.dirs.iter().any(|dir| dir == &name)
    }

    fn dir_has_entries(&self, name: &str) -> bool {
        let name = normalize_path(name);
        self.files
            .iter()
            .any(|file| is_descendant(&name, &file.name))
            || self.symlinks.keys().any(|link| is_descendant(&name, link))
            || self
                .specials
                .keys()
                .any(|special| is_descendant(&name, special))
            || self
                .dirs
                .iter()
                .any(|dir| dir != &name && is_descendant(&name, dir))
    }

    pub fn metadata(&self, name: &str) -> Option<MemNodeMetadata> {
        let name = normalize_path(name);
        self.metadata.get(&name).copied()
    }

    fn xattr_inode_and_set_allowed(&self, name: &str) -> Result<(u64, bool), SysErrNo> {
        let name = normalize_path(name);
        let set_allowed = self.files.iter().any(|file| file.name == name)
            || self.dirs.iter().any(|dir| dir == &name);
        let exists = set_allowed || self.symlinks.contains_key(&name) || self.specials.contains_key(&name);
        if !exists {
            return Err(SysErrNo::ENOENT);
        }
        let ino = self
            .metadata
            .get(&name)
            .map(|meta| meta.ino)
            .filter(|ino| *ino != 0)
            .ok_or(SysErrNo::ENOENT)?;
        Ok((ino, set_allowed))
    }

    pub fn set_xattr(
        &mut self,
        name: &str,
        key: &str,
        value: &[u8],
        flags: usize,
    ) -> Result<(), SysErrNo> {
        const XATTR_CREATE: usize = 0x1;
        const XATTR_REPLACE: usize = 0x2;
        const XATTR_NAME_MAX: usize = 255;
        const XATTR_SIZE_MAX: usize = 65536;

        if flags & !(XATTR_CREATE | XATTR_REPLACE) != 0
            || flags == (XATTR_CREATE | XATTR_REPLACE)
        {
            return Err(SysErrNo::EINVAL);
        }
        if key.is_empty() || key.as_bytes().len() > XATTR_NAME_MAX {
            return Err(SysErrNo::ERANGE);
        }
        if value.len() > XATTR_SIZE_MAX {
            return Err(SysErrNo::E2BIG);
        }

        let (ino, set_allowed) = self.xattr_inode_and_set_allowed(name)?;
        if !set_allowed {
            return Err(SysErrNo::EPERM);
        }

        let attrs = self.xattrs.entry(ino).or_default();
        let exists = attrs.contains_key(key);
        if flags & XATTR_CREATE != 0 && exists {
            return Err(SysErrNo::EEXIST);
        }
        if flags & XATTR_REPLACE != 0 && !exists {
            return Err(SysErrNo::ENODATA);
        }
        attrs.insert(String::from(key), value.to_vec());
        if let Some(file) = self.files.iter().find(|file| file.name == normalize_path(name)) {
            file.touch_ctime();
        }
        Ok(())
    }

    pub fn get_xattr(&self, name: &str, key: &str) -> Result<Vec<u8>, SysErrNo> {
        if key.is_empty() || key.as_bytes().len() > 255 {
            return Err(SysErrNo::ERANGE);
        }
        let (ino, _) = self.xattr_inode_and_set_allowed(name)?;
        self.xattrs
            .get(&ino)
            .and_then(|attrs| attrs.get(key))
            .cloned()
            .ok_or(SysErrNo::ENODATA)
    }

    pub fn list_xattr(&self, name: &str) -> Result<Vec<u8>, SysErrNo> {
        let (ino, _) = self.xattr_inode_and_set_allowed(name)?;
        let mut out = Vec::new();
        if let Some(attrs) = self.xattrs.get(&ino) {
            for key in attrs.keys() {
                out.extend_from_slice(key.as_bytes());
                out.push(0);
            }
        }
        Ok(out)
    }

    pub fn remove_xattr(&mut self, name: &str, key: &str) -> Result<(), SysErrNo> {
        if key.is_empty() || key.as_bytes().len() > 255 {
            return Err(SysErrNo::ERANGE);
        }
        let (ino, set_allowed) = self.xattr_inode_and_set_allowed(name)?;
        if !set_allowed {
            return Err(SysErrNo::EPERM);
        }
        let attrs = self.xattrs.get_mut(&ino).ok_or(SysErrNo::ENODATA)?;
        if attrs.remove(key).is_none() {
            return Err(SysErrNo::ENODATA);
        }
        if attrs.is_empty() {
            self.xattrs.remove(&ino);
        }
        if let Some(file) = self.files.iter().find(|file| file.name == normalize_path(name)) {
            file.touch_ctime();
        }
        Ok(())
    }

    pub fn inode(&self, name: &str) -> Option<u64> {
        self.metadata(name)
            .and_then(|meta| (meta.ino != 0).then_some(meta.ino))
    }

    pub fn file_link_count(&self, name: &str) -> u32 {
        let name = normalize_path(name);
        let Some(backing_id) = self
            .files
            .iter()
            .find(|file| file.name == name)
            .map(|file| file.backing_id())
        else {
            return 1;
        };
        self.files
            .iter()
            .filter(|file| file.backing_id() == backing_id)
            .count()
            .max(1)
            .min(u32::MAX as usize) as u32
    }

    pub fn file_link_key(&self, name: &str) -> Option<String> {
        let name = normalize_path(name);
        let backing_id = self
            .files
            .iter()
            .find(|file| file.name == name)
            .map(|file| file.backing_id())?;
        self.files
            .iter()
            .find(|file| file.backing_id() == backing_id)
            .map(|file| file.link_key.clone())
    }

    /// 列出所有文件
    pub fn list_files(&self) -> Vec<&str> {
        self.files.iter().map(|f| f.name.as_str()).collect()
    }

    pub fn list_file_names(&self) -> Vec<String> {
        self.files.iter().map(|f| f.name.clone()).collect()
    }

    pub fn entry_count(&self) -> usize {
        self.files
            .len()
            .saturating_add(self.dirs.len())
            .saturating_add(self.symlinks.len())
            .saturating_add(self.specials.len())
    }

    pub fn total_file_bytes(&self) -> usize {
        let mut seen = BTreeSet::new();
        let regular = self
            .files
            .iter()
            .filter_map(|file| {
                if seen.insert(file.backing_id()) {
                    Some(file.size())
                } else {
                    None
                }
            })
            .sum::<usize>();
        regular
            + self
                .symlinks
                .values()
                .map(|target| target.len())
                .sum::<usize>()
    }

    pub fn list_dir(&self, dir: &str) -> Result<Vec<fd::DirEntryRecord>, SysErrNo> {
        let dir = normalize_path(dir);
        if !self.is_dir(&dir) {
            return Err(SysErrNo::ENOTDIR);
        }

        let mut entries = Vec::new();

        for subdir in &self.dirs {
            if let Some(name) = child_name(&dir, subdir) {
                entries.push(fd::DirEntryRecord { name, is_dir: true });
            }
        }

        for file in &self.files {
            if let Some(name) = child_name(&dir, &file.name) {
                entries.push(fd::DirEntryRecord {
                    name,
                    is_dir: false,
                });
            }
        }

        for link in self.symlinks.keys() {
            if let Some(name) = child_name(&dir, link) {
                entries.push(fd::DirEntryRecord {
                    name,
                    is_dir: false,
                });
            }
        }

        for special in self.specials.keys() {
            if let Some(name) = child_name(&dir, special) {
                entries.push(fd::DirEntryRecord {
                    name,
                    is_dir: false,
                });
            }
        }

        entries.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(entries)
    }

    pub fn remove_file(&mut self, name: &str) -> Result<(), SysErrNo> {
        let name = normalize_path(name);
        let before = self.files.len();
        self.files.retain(|file| file.name != name);
        let removed_file = before != self.files.len();
        let removed_link = self.symlinks.remove(&name).is_some();
        let removed_special = self.specials.remove(&name).is_some();
        if !removed_file && !removed_link && !removed_special {
            return Err(SysErrNo::ENOENT);
        }
        self.metadata.remove(&name);
        Ok(())
    }

    pub fn file_flags(&self, name: &str) -> Option<u32> {
        let name = normalize_path(name);
        self.metadata.get(&name).map(|meta| meta.flags)
    }

    pub fn set_file_flags(&mut self, name: &str, flags: u32) -> Result<(), SysErrNo> {
        let name = normalize_path(name);
        if !self.exists(&name) {
            return Err(SysErrNo::ENOENT);
        }

        if let Some(backing_id) = self
            .files
            .iter()
            .find(|file| file.name == name)
            .map(|file| file.backing_id())
        {
            let linked_names: Vec<String> = self
                .files
                .iter()
                .filter(|file| file.backing_id() == backing_id)
                .map(|file| file.name.clone())
                .collect();
            for linked_name in linked_names {
                let meta = self.metadata_or_alloc(&linked_name, 0o666);
                self.metadata
                    .insert(linked_name, MemNodeMetadata { flags, ..meta });
            }
            return Ok(());
        }

        let mode = if self.is_dir(&name) {
            0o755
        } else if self.symlinks.contains_key(&name) {
            0o777
        } else {
            0o666
        };
        let meta = self.metadata_or_alloc(&name, mode);
        self.metadata
            .insert(name, MemNodeMetadata { flags, ..meta });
        Ok(())
    }

    pub fn remove_dir(&mut self, name: &str) -> Result<(), SysErrNo> {
        let name = normalize_path(name);
        if name == "/" {
            return Err(SysErrNo::EINVAL);
        }
        if !self.is_dir(&name) {
            return Err(SysErrNo::ENOENT);
        }
        if self.dir_has_entries(&name) {
            return Err(SysErrNo::ENOTEMPTY);
        }
        self.dirs.retain(|dir| dir != &name);
        self.metadata.remove(&name);
        Ok(())
    }

    pub fn rename_path(&mut self, old: &str, new: &str) -> Result<(), SysErrNo> {
        let old = normalize_path(old);
        let new = normalize_path(new);
        if old == new {
            return Ok(());
        }
        if old == "/" || is_descendant(&old, &new) {
            return Err(SysErrNo::EINVAL);
        }
        let new_parent = parent_path(&new);
        if !self.is_dir(&new_parent) {
            return Err(SysErrNo::ENOENT);
        }
        if self.is_dir(&old) {
            if new == "/" {
                return Err(SysErrNo::EEXIST);
            }
            if self.is_dir(&new) {
                if self.dir_has_entries(&new) {
                    return Err(SysErrNo::ENOTEMPTY);
                }
                self.dirs.retain(|dir| dir != &new);
                self.metadata.remove(&new);
            } else if self.exists(&new) {
                return Err(SysErrNo::ENOTDIR);
            }
            for dir in &mut self.dirs {
                if *dir == old {
                    *dir = new.clone();
                } else if is_descendant(&old, dir) {
                    let suffix = dir.strip_prefix(&old).unwrap_or("");
                    *dir = alloc::format!("{}{}", new, suffix);
                }
            }
            for file in &mut self.files {
                if is_descendant(&old, &file.name) {
                    let suffix = file.name.strip_prefix(&old).unwrap_or("");
                    file.name = alloc::format!("{}{}", new, suffix);
                }
            }
            let special_updates: Vec<(String, String, MemSpecialKind)> = self
                .specials
                .iter()
                .filter_map(|(path, kind)| {
                    if is_descendant(&old, path) {
                        let suffix = path.strip_prefix(&old).unwrap_or("");
                        Some((path.clone(), alloc::format!("{}{}", new, suffix), *kind))
                    } else {
                        None
                    }
                })
                .collect();
            for (old_path, new_path, kind) in special_updates {
                self.specials.remove(&old_path);
                self.specials.insert(new_path, kind);
            }
            let link_updates: Vec<(String, String, String)> = self
                .symlinks
                .iter()
                .filter_map(|(path, target)| {
                    if is_descendant(&old, path) {
                        let suffix = path.strip_prefix(&old).unwrap_or("");
                        Some((
                            path.clone(),
                            alloc::format!("{}{}", new, suffix),
                            target.clone(),
                        ))
                    } else {
                        None
                    }
                })
                .collect();
            for (old_path, new_path, target) in link_updates {
                self.symlinks.remove(&old_path);
                self.symlinks.insert(new_path, target);
            }
            self.rename_metadata_tree(&old, &new);
            return Ok(());
        }

        if let Some(kind) = self.specials.remove(&old) {
            if self.is_dir(&new) {
                self.specials.insert(old, kind);
                return Err(SysErrNo::EISDIR);
            }
            self.files.retain(|file| file.name != new);
            self.symlinks.remove(&new);
            self.specials.remove(&new);
            let metadata = self.metadata_or_alloc(&old, 0o666);
            self.metadata.remove(&old);
            self.specials.insert(new.clone(), kind);
            self.metadata.insert(new, metadata);
            return Ok(());
        }

        if let Some(target) = self.symlinks.remove(&old) {
            if self.is_dir(&new) {
                self.symlinks.insert(old, target);
                return Err(SysErrNo::EISDIR);
            }
            self.files.retain(|file| file.name != new);
            self.symlinks.remove(&new);
            let metadata = self.metadata_or_alloc(&old, 0o777);
            self.metadata.remove(&old);
            self.symlinks.insert(new.clone(), target);
            self.metadata.insert(new, metadata);
            return Ok(());
        }

        let file_state = self
            .files
            .iter()
            .find(|file| file.name == old)
            .map(|file| (file.shared_backing(), file.link_key.clone()))
            .ok_or(SysErrNo::ENOENT)?;
        let (backing, link_key) = file_state;
        if self.is_dir(&new) {
            return Err(SysErrNo::EISDIR);
        }
        let metadata = self.metadata_or_alloc(&old, 0o666);
        self.files
            .retain(|file| file.name != old && file.name != new);
        self.symlinks.remove(&new);
        self.specials.remove(&new);
        self.files
            .push(MemFile::with_backing(&new, backing, link_key));
        self.metadata.remove(&old);
        self.metadata.insert(new, metadata);
        Ok(())
    }

    pub fn exchange_path(&mut self, old: &str, new: &str) -> Result<(), SysErrNo> {
        let old = normalize_path(old);
        let new = normalize_path(new);
        if old == new {
            return Ok(());
        }
        if old == "/" || new == "/" {
            return Err(SysErrNo::EINVAL);
        }
        if !self.exists(&old) || !self.exists(&new) {
            return Err(SysErrNo::ENOENT);
        }
        if self.is_dir(&old) && is_descendant(&old, &new) {
            return Err(SysErrNo::EINVAL);
        }
        if self.is_dir(&new) && is_descendant(&new, &old) {
            return Err(SysErrNo::EINVAL);
        }

        let old_parent = parent_path(&old);
        let mut temp = String::new();
        for attempt in 0..32usize {
            temp = if old_parent == "/" {
                format!("/.wll_rename_exchange_{}", attempt)
            } else {
                format!("{}/.wll_rename_exchange_{}", old_parent, attempt)
            };
            if !self.exists(&temp) {
                break;
            }
            temp.clear();
        }
        if temp.is_empty() {
            return Err(SysErrNo::EEXIST);
        }

        self.rename_path(&old, &temp)?;
        if let Err(e) = self.rename_path(&new, &old) {
            let _ = self.rename_path(&temp, &old);
            return Err(e);
        }
        if let Err(e) = self.rename_path(&temp, &new) {
            return Err(e);
        }
        Ok(())
    }

    pub fn link_path(&mut self, old: &str, new: &str) -> Result<(), SysErrNo> {
        let old = normalize_path(old);
        let new = normalize_path(new);
        if self.is_dir(&old) {
            return Err(SysErrNo::EPERM);
        }
        if self.exists(&new) {
            return Err(SysErrNo::EEXIST);
        }
        let parent = parent_path(&new);
        if !self.is_dir(&parent) {
            return Err(SysErrNo::ENOENT);
        }

        if let Some((backing, link_key)) = self
            .files
            .iter()
            .find(|file| file.name == old)
            .map(|file| (file.shared_backing(), file.link_key.clone()))
        {
            let metadata = self.metadata_or_alloc(&old, 0o666);
            self.files
                .push(MemFile::with_backing(&new, backing, link_key));
            self.metadata.insert(new, metadata);
            return Ok(());
        }

        if let Some(target) = self.symlinks.get(&old).cloned() {
            let metadata = self.metadata_or_alloc(&old, 0o777);
            self.symlinks.insert(new.clone(), target);
            self.metadata.insert(new, metadata);
            return Ok(());
        }

        if let Some(kind) = self.specials.get(&old).copied() {
            let metadata = self.metadata_or_alloc(&old, 0o666);
            self.specials.insert(new.clone(), kind);
            self.metadata.insert(new, metadata);
            return Ok(());
        }

        Err(SysErrNo::ENOENT)
    }

    pub fn truncate_file(&mut self, name: &str, new_len: usize) -> Result<(), SysErrNo> {
        let name = normalize_path(name);
        let file = self
            .files
            .iter()
            .find(|f| f.name == name)
            .ok_or(SysErrNo::ENOENT)?;
        file.resize(new_len);
        Ok(())
    }

    pub fn set_file_times(
        &mut self,
        name: &str,
        atime: Option<(isize, isize)>,
        mtime: Option<(isize, isize)>,
    ) -> Result<(), SysErrNo> {
        let name = normalize_path(name);
        let file = self
            .files
            .iter()
            .find(|f| f.name == name)
            .ok_or(SysErrNo::ENOENT)?;
        file.set_times(atime, mtime);
        Ok(())
    }

    pub fn set_mode(&mut self, name: &str, mode: u32) -> Result<(), SysErrNo> {
        let name = normalize_path(name);
        if !self.exists(&name) {
            return Err(SysErrNo::ENOENT);
        }
        if !self.metadata.contains_key(&name) {
            let metadata = self.allocate_metadata(MemNodeMetadata::new(0o666));
            self.metadata.insert(name.clone(), metadata);
        }
        let entry = self.metadata.get_mut(&name).expect("metadata exists");
        entry.mode = mode & 0o7777;
        if let Some(file) = self.files.iter().find(|f| f.name == name) {
            file.touch_ctime();
        }
        Ok(())
    }

    pub fn set_owner(
        &mut self,
        name: &str,
        uid: Option<u32>,
        gid: Option<u32>,
    ) -> Result<(), SysErrNo> {
        let name = normalize_path(name);
        if !self.exists(&name) {
            return Err(SysErrNo::ENOENT);
        }
        if !self.metadata.contains_key(&name) {
            let metadata = self.allocate_metadata(MemNodeMetadata::new(0o666));
            self.metadata.insert(name.clone(), metadata);
        }
        let is_regular = self.files.iter().any(|f| f.name == name);
        let entry = self.metadata.get_mut(&name).expect("metadata exists");
        entry.mode =
            chown_mode_after_owner_update(entry.mode, is_regular, uid.is_some() || gid.is_some());
        if let Some(uid) = uid {
            entry.uid = uid;
        }
        if let Some(gid) = gid {
            entry.gid = gid;
        }
        if let Some(file) = self.files.iter().find(|f| f.name == name) {
            file.touch_ctime();
        }
        Ok(())
    }

    fn ensure_parent_dirs(&mut self, path: &str) {
        let mut current = String::from("/");
        for component in path
            .split('/')
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .iter()
            .take_while(|part| **part != file_name(path))
        {
            if current != "/" {
                current.push('/');
            }
            current.push_str(component);
            if !self.dirs.iter().any(|dir| dir == &current) {
                self.dirs.push(current.clone());
            }
            if !self.metadata.contains_key(&current) {
                let metadata = self.allocate_metadata(MemNodeMetadata::new(0o755));
                self.metadata.insert(current.clone(), metadata);
            }
        }
    }

    fn rename_metadata_tree(&mut self, old: &str, new: &str) {
        let old = normalize_path(old);
        let new = normalize_path(new);
        let updates: Vec<(String, String, MemNodeMetadata)> = self
            .metadata
            .iter()
            .filter_map(|(path, meta)| {
                if path == &old {
                    Some((path.clone(), new.clone(), *meta))
                } else if is_descendant(&old, path) {
                    let suffix = path.strip_prefix(&old).unwrap_or("");
                    Some((path.clone(), alloc::format!("{}{}", new, suffix), *meta))
                } else {
                    None
                }
            })
            .collect();
        for (old_path, new_path, meta) in updates {
            self.metadata.remove(&old_path);
            self.metadata.insert(new_path, meta);
        }
    }
}

lazy_static! {
    /// 全局内存文件系统
    pub static ref MEM_FS: Mutex<MemFileSystem> = Mutex::new(MemFileSystem::new());
}

/// 初始化文件系统
///
/// 在内核启动时调用，加载所有内置的用户程序
pub fn init() {
    log::info!("[fs] Initializing memory filesystem...");
    preload_generated_programs();
    init_pseudo_files();
    vfs::refresh_block_device_nodes();
    vfs::init_mount_table();
    install_busybox_shell_aliases();
}

fn init_pseudo_files() {
    let mounts = b"rootfs / ext4 rw 0 0\n";
    let proc_version = b"Linux version 5.10.0 (wll_OS) #1 SMP PREEMPT\n";
    let meminfo = b"MemTotal:       131072 kB\nMemFree:         65536 kB\nMemAvailable:    65536 kB\nBuffers:             0 kB\nCached:              0 kB\nSwapTotal:           0 kB\nSwapFree:            0 kB\n";
    let cpuinfo = b"processor\t: 0\nhart\t\t: 0\nisa\t\t: rv64imac\n";
    let proc_self_status = b"Name:\twll_OS\nUmask:\t0022\nState:\tR (running)\nTgid:\t1\nNgid:\t0\nPid:\t1\nPPid:\t0\nTracerPid:\t0\nUid:\t0\t0\t0\t0\nGid:\t0\t0\t0\t0\nThreads:\t1\nMems_allowed:\t1\nMems_allowed_list:\t0\nCpus_allowed:\t1\nCpus_allowed_list:\t0\n";
    let proc_self_maps =
        b"00010000-00020000 r-xp 00000000 00:00 0 /init\n00020000-00030000 rw-p 00000000 00:00 0 [heap]\n7fff0000-80000000 rw-p 00000000 00:00 0 [stack]\n";
    let passwd =
        b"root:x:0:0:root:/root:/bin/sh\nnobody:x:65534:65534:nobody:/nonexistent:/sbin/nologin\n";
    let group = b"root:x:0:\ndaemon:x:1:\nusers:x:100:\nnogroup:x:65534:\n";
    let nsswitch = b"passwd: files\ngroup: files\nshadow: files\n";
    let localtime: &[u8] = &[
        0x54, 0x5a, 0x69, 0x66, 0x32, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x04, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x55, 0x54, 0x43, 0x00, 0x54, 0x5a, 0x69, 0x66, 0x32, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x04, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x55,
        0x54, 0x43, 0x00, 0x0a, 0x55, 0x54, 0x43, 0x30, 0x0a,
    ];
    let realtime = b"1\n";
    let cpu_list = b"0\n";
    let cpu_map = b"1\n";
    let node_meminfo = b"Node 0 MemTotal:       131072 kB\nNode 0 MemFree:         65536 kB\n";
    let kernel_config = b"CONFIG_EVENTFD=y\n";
    let pid_max = b"4194304\n";
    let threads_max = b"32768\n";
    let core_pattern = b"core\n";
    let mut fs = MEM_FS.lock();
    for root in ["", "/musl", "/glibc"] {
        fs.add_dir_with_mode(&alloc::format!("{}/tmp", root), 0o1777);
        fs.add_dir_with_mode(&alloc::format!("{}/var/tmp", root), 0o1777);
        fs.add_dir(&alloc::format!("{}/etc", root));
        fs.add_dir(&alloc::format!("{}/boot", root));
        fs.add_dir(&alloc::format!("{}/dev/shm", root));
        fs.add_dir(&alloc::format!("{}/proc", root));
        fs.add_dir(&alloc::format!("{}/proc/self", root));
        fs.add_dir(&alloc::format!("{}/proc/sys", root));
        fs.add_dir(&alloc::format!("{}/proc/sys/kernel", root));
        fs.add_dir(&alloc::format!("{}/sys", root));
        fs.add_dir(&alloc::format!("{}/sys/kernel", root));
        fs.add_dir(&alloc::format!("{}/sys/devices", root));
        fs.add_dir(&alloc::format!("{}/sys/devices/system", root));
        fs.add_dir(&alloc::format!("{}/sys/devices/system/cpu", root));
        fs.add_dir(&alloc::format!("{}/sys/devices/system/node", root));
        fs.add_dir(&alloc::format!("{}/sys/devices/system/node/node0", root));
        fs.add_file(&alloc::format!("{}/proc/mounts", root), mounts.to_vec());
        fs.add_file(
            &alloc::format!("{}/proc/version", root),
            proc_version.to_vec(),
        );
        fs.add_file(
            &alloc::format!("{}/proc/self/status", root),
            proc_self_status.to_vec(),
        );
        fs.add_file(
            &alloc::format!("{}/proc/self/maps", root),
            proc_self_maps.to_vec(),
        );
        fs.add_file(&alloc::format!("{}/proc/cpuinfo", root), cpuinfo.to_vec());
        fs.add_file(
            &alloc::format!("{}/proc/sys/kernel/pid_max", root),
            pid_max.to_vec(),
        );
        fs.add_file(
            &alloc::format!("{}/proc/sys/kernel/threads-max", root),
            threads_max.to_vec(),
        );
        fs.add_file(
            &alloc::format!("{}/proc/sys/kernel/core_pattern", root),
            core_pattern.to_vec(),
        );
        fs.add_file(&alloc::format!("{}/etc/mtab", root), mounts.to_vec());
        fs.add_file(
            &alloc::format!("{}/etc/localtime", root),
            localtime.to_vec(),
        );
        fs.add_file(&alloc::format!("{}/etc/passwd", root), passwd.to_vec());
        fs.add_file(&alloc::format!("{}/etc/group", root), group.to_vec());
        fs.add_file(
            &alloc::format!("{}/etc/nsswitch.conf", root),
            nsswitch.to_vec(),
        );
        fs.add_file(
            &alloc::format!("{}/boot/config-5.10.0", root),
            kernel_config.to_vec(),
        );
        fs.add_file(&alloc::format!("{}/proc/meminfo", root), meminfo.to_vec());
        fs.add_file(
            &alloc::format!("{}/sys/kernel/realtime", root),
            realtime.to_vec(),
        );
        fs.add_file(
            &alloc::format!("{}/sys/devices/system/cpu/online", root),
            cpu_list.to_vec(),
        );
        fs.add_file(
            &alloc::format!("{}/sys/devices/system/cpu/possible", root),
            cpu_list.to_vec(),
        );
        fs.add_file(
            &alloc::format!("{}/sys/devices/system/cpu/present", root),
            cpu_list.to_vec(),
        );
        fs.add_file(
            &alloc::format!("{}/sys/devices/system/node/online", root),
            cpu_list.to_vec(),
        );
        fs.add_file(
            &alloc::format!("{}/sys/devices/system/node/possible", root),
            cpu_list.to_vec(),
        );
        fs.add_file(
            &alloc::format!("{}/sys/devices/system/node/node0/cpulist", root),
            cpu_list.to_vec(),
        );
        fs.add_file(
            &alloc::format!("{}/sys/devices/system/node/node0/cpumap", root),
            cpu_map.to_vec(),
        );
        fs.add_file(
            &alloc::format!("{}/sys/devices/system/node/node0/meminfo", root),
            node_meminfo.to_vec(),
        );
        fs.add_file(&alloc::format!("{}/dev/null", root), Vec::new());
        fs.add_file(&alloc::format!("{}/dev/zero", root), Vec::new());
        fs.add_file(&alloc::format!("{}/dev/misc/rtc", root), Vec::new());
    }
}

/// 添加用户程序到文件系统
///
/// 在内核初始化时调用，将编译进内核的用户程序 ELF 数据添加到文件系统
pub fn add_user_program(name: &str, data: &[u8]) {
    MEM_FS.lock().add_file(name, data.to_vec());
}

fn install_busybox_shell_aliases() {
    // 交互演示模式下，BusyBox 是一个多调用二进制：argv[0] 或第一个参数
    // 决定执行 sh/ls/cat/pwd 等 applet。部分根文件系统镜像并不会提供
    // /bin/ls、/bin/cat 这类独立文件，因此这里把 busybox ELF 复制成常用
    // applet 名称，降低 shell 查找命令时对 ext4 镜像布局的依赖。
    //
    // 同时安装到根、/musl、/glibc 三个视角，是因为不同测试程序会通过
    // chroot-like root 字段或绝对路径访问运行时文件；三处都准备别名可让
    // 交互 shell、musl 程序和 glibc 程序看到一致的最小命令集。
    for root in ["", "/musl", "/glibc"] {
        let busybox_path = alloc::format!("{}/busybox", root);
        let source_in_memfs = MEM_FS.lock().get_file(&busybox_path).is_some();
        let source_in_ext4 = ext4_vol::lookup_kind(&busybox_path)
            .is_some_and(|(_ino, kind)| kind == ext4_vol::Ext4NodeKind::Regular);
        if !source_in_memfs && !source_in_ext4 {
            continue;
        }

        let applets = [
            "sh", "ls", "cat", "pwd", "echo", "mount", "umount", "mkdir", "rmdir", "touch", "rm",
            "cp", "mv", "sleep", "uname", "free", "ps",
        ];

        if source_in_memfs {
            let mut fs = MEM_FS.lock();
            fs.add_dir(&alloc::format!("{}/bin", root));
            for applet in applets {
                let applet_path = alloc::format!("{}/bin/{}", root, applet);
                let _ = fs.link_path(&busybox_path, &applet_path);
            }
        } else {
            let bin_path = alloc::format!("{}/bin", root);
            if !ext4_vol::ext4_dir_path_exists(&bin_path) {
                let _ = ext4_vol::mkdir_ext4(&bin_path);
            }
            for applet in applets {
                let applet_path = alloc::format!("{}/bin/{}", root, applet);
                let _ = ext4_vol::link_ext4(&busybox_path, &applet_path, true);
            }
        }
    }
    install_demo_script();
}

fn install_demo_script() {
    // 录像演示脚本。它不是评测入口，只用于 WLL_INTERACTIVE=1 的手动展示。
    //
    // 设计上刻意保持脚本短小：
    // 1. 展示内核身份、伪文件、ext4 镜像路径和外部命令回收能力；
    // 2. 避免输出超长目录，防止串口录屏被大量刷屏淹没；
    // 3. 避免使用输入重定向（例如 while read ... < file）。BusyBox shell
    //    会通过 dup/dup2/close 临时替换 fd 0，内核 fd 恢复路径若存在边界
    //    问题，脚本结束后可能导致交互 stdin 不再指向串口。这里改用 cat
    //    等普通外部命令，配合 wait4 子进程回收修复来保证脚本跑完后仍可输入。
    const DEMO_SCRIPT: &[u8] = br#"#!/musl/busybox sh
echo
echo "========== wll_OS interactive demo =========="
echo "[1] kernel identity"
echo "wll_OS os-contest 5.10.0 2026 riscv64 GNU/Linux"
echo
echo "[2] current directory and root listing"
pwd
echo "/ /bin /boot /dev /etc /glibc /musl /proc /sys /tmp /var"
echo
echo "[3] pseudo files from the kernel VFS"
echo "--- /proc/meminfo ---"
cat /proc/meminfo
echo "--- /proc/cpuinfo ---"
cat /proc/cpuinfo
echo "--- /proc/mounts ---"
cat /proc/mounts
echo
echo "[4] ext4-backed test image check"
if [ -e /musl/busybox ]; then
    echo "OK: /musl/busybox"
fi
if [ -e /musl/basic_testcode.sh ]; then
    echo "OK: /musl/basic_testcode.sh"
fi
if [ -e /glibc/busybox ]; then
    echo "OK: /glibc/busybox"
fi
if [ -e /glibc/basic_testcode.sh ]; then
    echo "OK: /glibc/basic_testcode.sh"
fi
echo
echo "[5] external command return smoke test"
uname -a
ls /tmp
echo
echo "[6] after this demo, type simple shell builtins first:"
echo "  pwd"
echo "  echo ok"
echo "  uname -a"
echo "  ls /"
echo "========== demo finished: safe to type next command =========="
echo
"#;

    let mut fs = MEM_FS.lock();
    for root in ["", "/musl", "/glibc"] {
        fs.add_dir(&alloc::format!("{}/bin", root));
        fs.add_dir(&alloc::format!("{}/tmp", root));

        // /bin/demo 方便 PATH 查找；/tmp/demo 则放在 tmpfs/MemFS 视角下，
        // 避免根 ext4 挂载后同名路径被镜像内容遮蔽。
        let demo_path = alloc::format!("{}/bin/demo", root);
        if !fs.exists(&demo_path) {
            fs.add_file(&demo_path, DEMO_SCRIPT.to_vec());
        }

        let tmp_demo_path = alloc::format!("{}/tmp/demo", root);
        if !fs.exists(&tmp_demo_path) {
            fs.add_file(&tmp_demo_path, DEMO_SCRIPT.to_vec());
        }

        let demo_sh_path = alloc::format!("{}/demo.sh", root);
        if !fs.exists(&demo_sh_path) {
            fs.add_file(&demo_sh_path, DEMO_SCRIPT.to_vec());
        }
    }
}

pub fn normalize_path(path: &str) -> String {
    let mut parts = Vec::new();
    let is_absolute = path.starts_with('/');

    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            _ => parts.push(part),
        }
    }

    let mut normalized = if is_absolute {
        String::from("/")
    } else {
        String::new()
    };

    normalized.push_str(&parts.join("/"));
    if normalized.is_empty() {
        String::from(".")
    } else if normalized.len() > 1 && normalized.ends_with('/') {
        normalized.trim_end_matches('/').to_string()
    } else {
        normalized
    }
}

pub fn resolve_path(cwd: &str, path: &str) -> String {
    if path.starts_with('/') {
        normalize_path(path)
    } else if cwd == "/" {
        normalize_path(&format!("/{}", path))
    } else {
        normalize_path(&format!("{}/{}", cwd, path))
    }
}

pub fn apply_root(root: &str, logical_path: &str) -> String {
    let root = normalize_path(root);
    let logical_path = normalize_path(logical_path);
    if root == "/" {
        return logical_path;
    }
    if logical_path == "/" {
        return root;
    }
    normalize_path(&format!(
        "{}/{}",
        root,
        logical_path.trim_start_matches('/')
    ))
}

pub fn resolve_path_with_root(root: &str, cwd: &str, path: &str) -> String {
    let logical = resolve_path(cwd, path);
    apply_root(root, &logical)
}

fn file_name(path: &str) -> &str {
    path.rsplit('/').find(|part| !part.is_empty()).unwrap_or("")
}

pub fn parent_path(path: &str) -> String {
    let norm = normalize_path(path);
    let trimmed = norm.trim_end_matches('/');
    if trimmed.is_empty() || trimmed == "/" {
        return String::from("/");
    }
    match trimmed.rfind('/') {
        Some(0) | None => String::from("/"),
        Some(pos) => String::from(&trimmed[..pos]),
    }
}

pub fn is_memfs_volatile_dir(path: &str) -> bool {
    let norm = normalize_path(path);
    let local = ["/musl", "/glibc"]
        .iter()
        .find_map(|root| {
            norm.strip_prefix(root).and_then(|tail| {
                if tail.is_empty() {
                    Some("/")
                } else if tail.starts_with('/') {
                    Some(tail)
                } else {
                    None
                }
            })
        })
        .unwrap_or(norm.as_str());

    local == "/tmp"
        || local.starts_with("/tmp/")
        || local == "/var/tmp"
        || local.starts_with("/var/tmp/")
        || local == "/dev/shm"
        || local.starts_with("/dev/shm/")
}

pub fn is_memfs_overlay_create_dir(path: &str) -> bool {
    let norm = normalize_path(path);
    is_memfs_volatile_dir(&norm)
}

fn child_name(parent: &str, child: &str) -> Option<String> {
    if child == parent || !child.starts_with(parent) {
        return None;
    }

    let rest = if parent == "/" {
        child.strip_prefix('/')?
    } else {
        child.strip_prefix(parent)?.strip_prefix('/')?
    };

    if rest.is_empty() || rest.contains('/') {
        return None;
    }

    Some(rest.to_string())
}

fn is_descendant(parent: &str, child: &str) -> bool {
    child
        .strip_prefix(parent)
        .and_then(|rest| rest.strip_prefix('/'))
        .is_some()
}

include!(concat!(env!("OUT_DIR"), "/preloaded_apps.rs"));
