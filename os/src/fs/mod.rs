/// 内存文件系统实现
///
/// 提供一个简单的基于内存的文件系统，用于支持用户程序加载和基本文件操作
pub mod ext4_vol;
pub mod fd;
pub mod vfs;

#[allow(unused_imports)]
pub use vfs::{
    check_access, check_fd_access, check_metadata_access, create_dir, create_dir_with_mode,
    create_regular_file, create_symlink, dir_exists, file_exists, filesystem_magic, is_removed,
    link_path, list_dir, list_files, metadata, metadata_for_fd, open_path, read_executable_file,
    read_file, read_interpreter, read_link, remove_dir, remove_file, rename_path, set_times_fd,
    set_times_path, statfs_for_fd, statfs_for_path, sync_all, sync_fd, truncate_fd, truncate_path,
    VfsMetadata, VfsNodeKind, VfsStatFs,
};

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;
use lazy_static::lazy_static;
use spin::Mutex;

use crate::utils::error::SysErrNo;

/// 文件内容
pub type FileContent = Vec<u8>;

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
}

impl MemFile {
    pub fn new(name: &str, content: Vec<u8>) -> Self {
        Self {
            name: String::from(name),
            content,
            times: FileTimes::now(),
        }
    }

    pub fn with_times(name: &str, content: Vec<u8>, times: FileTimes) -> Self {
        Self {
            name: String::from(name),
            content,
            times,
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
}

impl MemFileSystem {
    pub fn new() -> Self {
        Self {
            files: Vec::new(),
            dirs: vec!["/".to_string()],
        }
    }

    /// 添加文件
    pub fn add_file(&mut self, name: &str, content: Vec<u8>) {
        let name = normalize_path(name);
        self.ensure_parent_dirs(&name);

        // 如果文件已存在，先删除
        self.files.retain(|f| f.name != name);
        let len = content.len();
        self.files.push(MemFile::new(&name, content));
        log::info!("[fs] Added file '{}' ({} bytes)", name, len);
    }

    pub fn write_file_content(&mut self, name: &str, content: Vec<u8>, times: FileTimes) -> bool {
        let name = normalize_path(name);
        if let Some(file) = self.files.iter_mut().find(|f| f.name == name) {
            file.content = content;
            file.times = times;
            true
        } else {
            false
        }
    }

    /// 添加目录
    pub fn add_dir(&mut self, name: &str) {
        let name = normalize_path(name);
        self.ensure_parent_dirs(&name);
        if !self.dirs.iter().any(|dir| dir == &name) {
            self.dirs.push(name.clone());
            log::info!("[fs] Added directory '{}'", name);
        }
    }

    /// 获取文件
    pub fn get_file(&self, name: &str) -> Option<&MemFile> {
        let name = normalize_path(name);
        self.files.iter().find(|f| f.name == name)
    }

    /// 检查文件是否存在
    pub fn exists(&self, name: &str) -> bool {
        let name = normalize_path(name);
        self.files.iter().any(|f| f.name == name) || self.dirs.iter().any(|dir| dir == &name)
    }

    pub fn is_dir(&self, name: &str) -> bool {
        let name = normalize_path(name);
        self.dirs.iter().any(|dir| dir == &name)
    }

    /// 列出所有文件
    pub fn list_files(&self) -> Vec<&str> {
        self.files.iter().map(|f| f.name.as_str()).collect()
    }

    pub fn list_file_names(&self) -> Vec<String> {
        self.files.iter().map(|f| f.name.clone()).collect()
    }

    pub fn entry_count(&self) -> usize {
        self.files.len().saturating_add(self.dirs.len())
    }

    pub fn total_file_bytes(&self) -> usize {
        self.files.iter().map(|f| f.content.len()).sum()
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

        entries.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(entries)
    }

    pub fn remove_file(&mut self, name: &str) -> Result<(), SysErrNo> {
        let name = normalize_path(name);
        let before = self.files.len();
        self.files.retain(|file| file.name != name);
        if before == self.files.len() {
            return Err(SysErrNo::ENOENT);
        }
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
            || self
                .dirs
                .iter()
                .any(|dir| dir != &name && is_descendant(&name, dir))
        {
            return Err(SysErrNo::ENOTEMPTY);
        }
        self.dirs.retain(|dir| dir != &name);
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
            return Ok(());
        }

        let content = self
            .files
            .iter()
            .find(|file| file.name == old)
            .map(|file| file.content.clone())
            .ok_or(SysErrNo::ENOENT)?;
        let times = self
            .files
            .iter()
            .find(|file| file.name == old)
            .map(|file| file.times)
            .ok_or(SysErrNo::ENOENT)?;
        if self.is_dir(&new) {
            return Err(SysErrNo::EISDIR);
        }
        self.files
            .retain(|file| file.name != old && file.name != new);
        self.files.push(MemFile::with_times(&new, content, times));
        Ok(())
    }

    pub fn truncate_file(&mut self, name: &str, new_len: usize) -> Result<(), SysErrNo> {
        let name = normalize_path(name);
        let file = self
            .files
            .iter_mut()
            .find(|f| f.name == name)
            .ok_or(SysErrNo::ENOENT)?;
        file.content.resize(new_len, 0);
        let now = FileTimes::now();
        file.times.mtime_sec = now.mtime_sec;
        file.times.mtime_nsec = now.mtime_nsec;
        file.times.ctime_sec = now.ctime_sec;
        file.times.ctime_nsec = now.ctime_nsec;
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
            .iter_mut()
            .find(|f| f.name == name)
            .ok_or(SysErrNo::ENOENT)?;
        file.times.set_access_modify(atime, mtime);
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
}

fn init_pseudo_files() {
    let mounts = b"rootfs / ext4 rw 0 0\n";
    let meminfo = b"MemTotal:       131072 kB\nMemFree:         65536 kB\nMemAvailable:    65536 kB\nBuffers:             0 kB\nCached:              0 kB\nSwapTotal:           0 kB\nSwapFree:            0 kB\n";
    let mut fs = MEM_FS.lock();
    for root in ["", "/musl", "/glibc"] {
        fs.add_dir(&alloc::format!("{}/tmp", root));
        fs.add_dir(&alloc::format!("{}/var/tmp", root));
        fs.add_dir(&alloc::format!("{}/dev/shm", root));
        fs.add_file(&alloc::format!("{}/proc/mounts", root), mounts.to_vec());
        fs.add_file(&alloc::format!("{}/etc/mtab", root), mounts.to_vec());
        fs.add_file(&alloc::format!("{}/proc/meminfo", root), meminfo.to_vec());
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
