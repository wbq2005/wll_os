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
    filesystem_magic, is_removed, link_path, list_dir, list_files, metadata, metadata_for_fd,
    metadata_for_lookup, mount_fs, open_path, read_executable_file, read_file, read_interpreter,
    read_link, refresh_block_device_nodes, remove_dir, remove_file, rename_path, set_mode_fd,
    set_mode_path, set_owner_fd, set_owner_path, set_times_fd, set_times_path, statfs_for_fd,
    statfs_for_path, sync_all, sync_fd, truncate_fd, truncate_path, umount_fs, VfsMetadata,
    VfsNodeKind, VfsStatFs,
};

use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::{String, ToString};
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
}

impl MemNodeMetadata {
    fn new(mode: u32) -> Self {
        Self {
            mode: mode & 0o7777,
            uid: 0,
            gid: 0,
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
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MemSpecialKind {
    Fifo,
    Socket,
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
pub struct MemFile {
    pub name: String,
    pub content: FileContent,
    pub times: FileTimes,
    link_key: String,
}

impl MemFile {
    pub fn new(name: &str, content: Vec<u8>) -> Self {
        Self {
            name: String::from(name),
            content: FileContent::from_slice(&content),
            times: FileTimes::now(),
            link_key: String::from(name),
        }
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
        Self {
            name: String::from(name),
            content,
            times,
            link_key,
        }
    }

    pub fn size(&self) -> usize {
        self.content.len()
    }
}

/// 内存文件系统
pub struct MemFileSystem {
    files: Vec<MemFile>,
    dirs: Vec<String>,
    symlinks: BTreeMap<String, String>,
    specials: BTreeMap<String, MemSpecialKind>,
    metadata: BTreeMap<String, MemNodeMetadata>,
}

impl MemFileSystem {
    pub fn new() -> Self {
        let mut metadata = BTreeMap::new();
        metadata.insert(String::from("/"), MemNodeMetadata::new(0o755));
        Self {
            files: Vec::new(),
            dirs: vec!["/".to_string()],
            symlinks: BTreeMap::new(),
            specials: BTreeMap::new(),
            metadata,
        }
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
        self.specials.remove(&name);

        // 如果文件已存在，先删除
        self.files.retain(|f| f.name != name);
        let len = content.len();
        self.files.push(MemFile::new(&name, content));
        self.metadata
            .insert(name.clone(), MemNodeMetadata::new_for_current(mode));
        log::info!("[fs] Added file '{}' ({} bytes)", name, len);
    }

    pub fn add_file_with_mode(&mut self, name: &str, content: Vec<u8>, mode: u32) {
        self.add_file(name, content);
        let _ = self.set_mode(name, mode);
    }

    pub fn write_file_content(
        &mut self,
        name: &str,
        content: FileContent,
        times: FileTimes,
    ) -> bool {
        let name = normalize_path(name);
        if let Some(link_key) = self
            .files
            .iter()
            .find(|file| file.name == name)
            .map(|file| file.link_key.clone())
        {
            for file in self
                .files
                .iter_mut()
                .filter(|file| file.link_key == link_key)
            {
                file.content = content.clone();
                file.times = times;
            }
            true
        } else {
            false
        }
    }

    /// 添加目录
    pub fn add_dir(&mut self, name: &str) {
        let name = normalize_path(name);
        self.ensure_parent_dirs(&name);
        self.symlinks.remove(&name);
        self.specials.remove(&name);
        if !self.dirs.iter().any(|dir| dir == &name) {
            self.dirs.push(name.clone());
            log::info!("[fs] Added directory '{}'", name);
        }
        self.metadata
            .entry(name)
            .or_insert_with(|| MemNodeMetadata::new_for_current(0o755));
    }

    pub fn add_dir_with_mode(&mut self, name: &str, mode: u32) {
        self.add_dir(name);
        let _ = self.set_mode(name, mode);
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
        self.metadata
            .insert(name, MemNodeMetadata::new_for_current(0o777));
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
        self.metadata
            .insert(name, MemNodeMetadata::new_for_current(mode));
        Ok(())
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

    pub fn metadata(&self, name: &str) -> Option<MemNodeMetadata> {
        let name = normalize_path(name);
        self.metadata.get(&name).copied()
    }

    pub fn file_link_count(&self, name: &str) -> u32 {
        let name = normalize_path(name);
        let Some(link_key) = self
            .files
            .iter()
            .find(|file| file.name == name)
            .map(|file| file.link_key.as_str())
        else {
            return 1;
        };
        self.files
            .iter()
            .filter(|file| file.link_key == link_key)
            .count()
            .max(1)
            .min(u32::MAX as usize) as u32
    }

    pub fn file_link_key(&self, name: &str) -> Option<String> {
        let name = normalize_path(name);
        self.files
            .iter()
            .find(|file| file.name == name)
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
        self.files.iter().map(|f| f.content.len()).sum::<usize>()
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

    pub fn remove_dir(&mut self, name: &str) -> Result<(), SysErrNo> {
        let name = normalize_path(name);
        if name == "/" {
            return Err(SysErrNo::EINVAL);
        }
        if !self.is_dir(&name) {
            return Err(SysErrNo::ENOENT);
        }
        if self
            .files
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
        {
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
            if self.exists(&new) {
                return Err(SysErrNo::EEXIST);
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
            let metadata = self
                .metadata
                .get(&old)
                .copied()
                .unwrap_or_else(|| MemNodeMetadata::new(0o666));
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
            let metadata = self
                .metadata
                .get(&old)
                .copied()
                .unwrap_or_else(|| MemNodeMetadata::new(0o777));
            self.metadata.remove(&old);
            self.symlinks.insert(new.clone(), target);
            self.metadata.insert(new, metadata);
            return Ok(());
        }

        let content = self
            .files
            .iter()
            .find(|file| file.name == old)
            .map(|file| (file.content.clone(), file.times, file.link_key.clone()))
            .ok_or(SysErrNo::ENOENT)?;
        let (content, times, link_key) = content;
        if self.is_dir(&new) {
            return Err(SysErrNo::EISDIR);
        }
        let metadata = self
            .metadata
            .get(&old)
            .copied()
            .unwrap_or_else(|| MemNodeMetadata::new(0o666));
        self.files
            .retain(|file| file.name != old && file.name != new);
        self.symlinks.remove(&new);
        self.specials.remove(&new);
        self.files
            .push(MemFile::with_link_key(&new, content, times, link_key));
        self.metadata.remove(&old);
        self.metadata.insert(new, metadata);
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

        if let Some(file) = self.files.iter().find(|file| file.name == old) {
            let metadata = self
                .metadata
                .get(&old)
                .copied()
                .unwrap_or_else(|| MemNodeMetadata::new(0o666));
            self.files.push(MemFile::with_link_key(
                &new,
                file.content.clone(),
                file.times,
                file.link_key.clone(),
            ));
            self.metadata.insert(new, metadata);
            return Ok(());
        }

        if let Some(target) = self.symlinks.get(&old).cloned() {
            let metadata = self
                .metadata
                .get(&old)
                .copied()
                .unwrap_or_else(|| MemNodeMetadata::new(0o777));
            self.symlinks.insert(new.clone(), target);
            self.metadata.insert(new, metadata);
            return Ok(());
        }

        if let Some(kind) = self.specials.get(&old).copied() {
            let metadata = self
                .metadata
                .get(&old)
                .copied()
                .unwrap_or_else(|| MemNodeMetadata::new(0o666));
            self.specials.insert(new.clone(), kind);
            self.metadata.insert(new, metadata);
            return Ok(());
        }

        Err(SysErrNo::ENOENT)
    }

    pub fn truncate_file(&mut self, name: &str, new_len: usize) -> Result<(), SysErrNo> {
        let name = normalize_path(name);
        let link_key = self
            .files
            .iter()
            .find(|f| f.name == name)
            .map(|f| f.link_key.clone())
            .ok_or(SysErrNo::ENOENT)?;
        let now = FileTimes::now();
        for file in self.files.iter_mut().filter(|f| f.link_key == link_key) {
            file.content.resize(new_len);
            file.times.mtime_sec = now.mtime_sec;
            file.times.mtime_nsec = now.mtime_nsec;
            file.times.ctime_sec = now.ctime_sec;
            file.times.ctime_nsec = now.ctime_nsec;
        }
        Ok(())
    }

    pub fn set_file_times(
        &mut self,
        name: &str,
        atime: Option<(isize, isize)>,
        mtime: Option<(isize, isize)>,
    ) -> Result<(), SysErrNo> {
        let name = normalize_path(name);
        let link_key = self
            .files
            .iter()
            .find(|f| f.name == name)
            .map(|f| f.link_key.clone())
            .ok_or(SysErrNo::ENOENT)?;
        for file in self.files.iter_mut().filter(|f| f.link_key == link_key) {
            file.times.set_access_modify(atime, mtime);
        }
        Ok(())
    }

    pub fn set_mode(&mut self, name: &str, mode: u32) -> Result<(), SysErrNo> {
        let name = normalize_path(name);
        if !self.exists(&name) {
            return Err(SysErrNo::ENOENT);
        }
        let entry = self
            .metadata
            .entry(name.clone())
            .or_insert_with(|| MemNodeMetadata::new(0o666));
        entry.mode = mode & 0o7777;
        if let Some(file) = self.files.iter_mut().find(|f| f.name == name) {
            let now = FileTimes::now();
            file.times.ctime_sec = now.ctime_sec;
            file.times.ctime_nsec = now.ctime_nsec;
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
        let entry = self
            .metadata
            .entry(name.clone())
            .or_insert_with(|| MemNodeMetadata::new(0o666));
        if let Some(uid) = uid {
            entry.uid = uid;
        }
        if let Some(gid) = gid {
            entry.gid = gid;
        }
        if let Some(file) = self.files.iter_mut().find(|f| f.name == name) {
            let now = FileTimes::now();
            file.times.ctime_sec = now.ctime_sec;
            file.times.ctime_nsec = now.ctime_nsec;
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
            self.metadata
                .entry(current.clone())
                .or_insert_with(|| MemNodeMetadata::new(0o755));
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
    let meminfo = b"MemTotal:       131072 kB\nMemFree:         65536 kB\nMemAvailable:    65536 kB\nBuffers:             0 kB\nCached:              0 kB\nSwapTotal:           0 kB\nSwapFree:            0 kB\n";
    let cpuinfo = b"processor\t: 0\nhart\t\t: 0\nisa\t\t: rv64imac\n";
    let proc_self_status = b"Name:\twll_OS\nUmask:\t0022\nState:\tR (running)\nTgid:\t1\nNgid:\t0\nPid:\t1\nPPid:\t0\nTracerPid:\t0\nUid:\t0\t0\t0\t0\nGid:\t0\t0\t0\t0\nThreads:\t1\nMems_allowed:\t1\nMems_allowed_list:\t0\nCpus_allowed:\t1\nCpus_allowed_list:\t0\n";
    let passwd =
        b"root:x:0:0:root:/root:/bin/sh\nnobody:x:65534:65534:nobody:/nonexistent:/sbin/nologin\n";
    let group = b"root:x:0:\nnogroup:x:65534:\n";
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
    let mut fs = MEM_FS.lock();
    for root in ["", "/musl", "/glibc"] {
        fs.add_dir_with_mode(&alloc::format!("{}/tmp", root), 0o1777);
        fs.add_dir_with_mode(&alloc::format!("{}/var/tmp", root), 0o1777);
        fs.add_dir(&alloc::format!("{}/etc", root));
        fs.add_dir(&alloc::format!("{}/dev/shm", root));
        fs.add_dir(&alloc::format!("{}/proc", root));
        fs.add_dir(&alloc::format!("{}/proc/self", root));
        fs.add_dir(&alloc::format!("{}/sys", root));
        fs.add_dir(&alloc::format!("{}/sys/kernel", root));
        fs.add_dir(&alloc::format!("{}/sys/devices", root));
        fs.add_dir(&alloc::format!("{}/sys/devices/system", root));
        fs.add_dir(&alloc::format!("{}/sys/devices/system/cpu", root));
        fs.add_dir(&alloc::format!("{}/sys/devices/system/node", root));
        fs.add_dir(&alloc::format!("{}/sys/devices/system/node/node0", root));
        fs.add_file(&alloc::format!("{}/proc/mounts", root), mounts.to_vec());
        fs.add_file(
            &alloc::format!("{}/proc/self/status", root),
            proc_self_status.to_vec(),
        );
        fs.add_file(&alloc::format!("{}/proc/cpuinfo", root), cpuinfo.to_vec());
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
    for root in ["", "/musl", "/glibc"] {
        let busybox_path = alloc::format!("{}/busybox", root);
        if read_executable_file(&busybox_path).is_none() {
            continue;
        }

        let shell_path = alloc::format!("{}/bin/sh", root);
        if file_exists(&shell_path) {
            continue;
        }

        let mut fs = MEM_FS.lock();
        fs.add_dir(&alloc::format!("{}/bin", root));
        let _ = fs.add_symlink(&shell_path, "../busybox");
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
