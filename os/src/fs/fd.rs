use alloc::collections::VecDeque;
/// 文件描述符管理
///
/// 管理进程打开的文件，实现 POSIX 风格的文件描述符表
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use lazy_static::lazy_static;
use spin::Mutex;

use crate::fs;
use crate::fs::ext4_vol;
use crate::fs::MEM_FS;
use crate::utils::error::SysErrNo;

/// 最大文件描述符数量
pub const MAX_FD_NUM: usize = 1024;

/// 标准文件描述符
pub const STDIN_FD: usize = 0;
pub const STDOUT_FD: usize = 1;
pub const STDERR_FD: usize = 2;

/// 文件描述符偏移量
pub type FileOffset = usize;

const CONSOLE_LINE_CAP: usize = 512;

lazy_static! {
    static ref CONSOLE_LINE_BUFFERS: Mutex<Vec<(usize, Vec<u8>)>> = Mutex::new(Vec::new());
}

fn write_console_buffered(buf: &[u8]) {
    let writer = crate::task::current_task()
        .map(|task| task.pid.0)
        .unwrap_or(usize::MAX);
    let mut buffers = CONSOLE_LINE_BUFFERS.lock();
    let idx = buffers
        .iter()
        .position(|(pid, _)| *pid == writer)
        .unwrap_or_else(|| {
            buffers.push((writer, Vec::new()));
            buffers.len() - 1
        });
    let line = &mut buffers[idx].1;

    for &byte in buf {
        line.push(byte);
        if byte == b'\n' || line.len() >= CONSOLE_LINE_CAP {
            for &out in line.iter() {
                crate::console::putchar(out);
            }
            line.clear();
        }
    }
}

/// Flush pending stdout/stderr bytes for a task.
pub fn flush_console_buffer_for_pid(pid: usize) {
    let mut buffers = CONSOLE_LINE_BUFFERS.lock();
    if let Some(idx) = buffers.iter().position(|(owner, _)| *owner == pid) {
        let (_, line) = buffers.remove(idx);
        for byte in line {
            crate::console::putchar(byte);
        }
    }
}

/// 打开标志（与 Linux asm-generic/fcntl.h 常用 ABI 对齐）
pub mod open_flags {
    pub const O_ACCMODE: u32 = 0o00000003;
    pub const O_RDONLY: u32 = 0o00000000;
    pub const O_WRONLY: u32 = 0o00000001;
    pub const O_RDWR: u32 = 0o00000002;
    pub const O_CREAT: u32 = 0o00000100;
    pub const O_EXCL: u32 = 0o00000200;
    pub const O_TRUNC: u32 = 0o00001000;
    pub const O_APPEND: u32 = 0o00002000;
    pub const O_DIRECTORY: u32 = 0o00200000;
}

/// pipe2 标志（Linux ABI）
pub mod pipe_flags {
    pub const O_NONBLOCK: usize = 0o4000;
    pub const O_CLOEXEC: usize = 0o2000000;
}

/// 文件描述符类型
#[derive(Debug)]
pub enum FileDescriptor {
    /// 标准输入
    Stdin,
    /// 标准输出
    Stdout,
    /// 标准错误
    Stderr,
    /// 内存文件（预载只读 `writable=false`；`O_CREAT` 可走可写）
    MemFile {
        name: String,
        content: Vec<u8>,
        offset: FileOffset,
        writable: bool,
        append: bool,
    },
    /// 内存目录
    MemDir {
        path: String,
        entries: Vec<DirEntryRecord>,
        offset: usize,
    },
    /// ext4 普通文件（运行时块设备挂载）
    Ext4Regular {
        ino: u32,
        offset: usize,
        readable: bool,
        writable: bool,
        append: bool,
    },
    /// ext4 目录（运行时块设备挂载）
    Ext4Dir { ino: u32, offset: usize },
    /// 管道读端
    PipeRead {
        state: Arc<Mutex<PipeState>>,
        nonblock: bool,
    },
    /// 管道写端
    PipeWrite {
        state: Arc<Mutex<PipeState>>,
        nonblock: bool,
    },
}

#[derive(Debug)]
pub struct PipeState {
    buf: VecDeque<u8>,
    readers: usize,
    writers: usize,
}

#[derive(Debug, Clone)]
pub struct DirEntryRecord {
    pub name: String,
    pub is_dir: bool,
}

impl FileDescriptor {
    pub fn pipe_read_nonblocking(&self) -> bool {
        matches!(self, FileDescriptor::PipeRead { nonblock: true, .. })
    }

    /// 检查文件是否可读
    pub fn readable(&self) -> bool {
        match self {
            FileDescriptor::Stdin => true,
            FileDescriptor::Stdout => false,
            FileDescriptor::Stderr => false,
            FileDescriptor::MemFile { .. } => true,
            FileDescriptor::MemDir { .. } => false,
            FileDescriptor::Ext4Regular { readable, .. } => *readable,
            FileDescriptor::Ext4Dir { .. } => false,
            FileDescriptor::PipeRead { .. } => true,
            FileDescriptor::PipeWrite { .. } => false,
        }
    }

    /// 检查文件是否可写
    pub fn writable(&self) -> bool {
        match self {
            FileDescriptor::Stdin => false,
            FileDescriptor::Stdout => true,
            FileDescriptor::Stderr => true,
            FileDescriptor::MemFile { writable, .. } => *writable,
            FileDescriptor::MemDir { .. } => false,
            FileDescriptor::Ext4Regular { writable, .. } => *writable,
            FileDescriptor::Ext4Dir { .. } => false,
            FileDescriptor::PipeRead { .. } => false,
            FileDescriptor::PipeWrite { .. } => true,
        }
    }

    /// 读取数据
    pub fn read(&mut self, buf: &mut [u8]) -> Result<usize, SysErrNo> {
        match self {
            FileDescriptor::Stdin => {
                if let Some(c) = crate::console::getchar() {
                    if !buf.is_empty() {
                        buf[0] = c;
                        Ok(1)
                    } else {
                        Ok(0)
                    }
                } else {
                    // No input available. In the evaluation environment there is no
                    // interactive terminal, so return EOF (0) immediately instead
                    // of EAGAIN. EAGAIN would cause blocking reads to spin forever.
                    Ok(0)
                }
            }
            FileDescriptor::MemFile {
                content, offset, ..
            } => {
                if *offset >= content.len() {
                    return Ok(0);
                }
                let remaining = content.len() - *offset;
                let to_read = buf.len().min(remaining);
                buf[..to_read].copy_from_slice(&content[*offset..*offset + to_read]);
                *offset += to_read;
                Ok(to_read)
            }
            FileDescriptor::MemDir { .. } => Err(SysErrNo::EISDIR),
            FileDescriptor::Ext4Regular {
                ino,
                offset,
                readable,
                ..
            } => {
                if !*readable {
                    return Err(SysErrNo::EBADF);
                }
                let n = ext4_vol::ext4_read_at(*ino, *offset, buf)?;
                *offset += n;
                Ok(n)
            }
            FileDescriptor::Ext4Dir { .. } => Err(SysErrNo::EISDIR),
            FileDescriptor::PipeRead { state, .. } => {
                if buf.is_empty() {
                    return Ok(0);
                }
                let mut pipe = state.lock();
                if !pipe.buf.is_empty() {
                    let mut n = 0usize;
                    while n < buf.len() {
                        if let Some(b) = pipe.buf.pop_front() {
                            buf[n] = b;
                            n += 1;
                        } else {
                            break;
                        }
                    }
                    return Ok(n);
                }
                if pipe.writers == 0 {
                    return Ok(0);
                }
                Err(SysErrNo::EAGAIN)
            }
            FileDescriptor::PipeWrite { .. } => Err(SysErrNo::EBADF),
            _ => Err(SysErrNo::EBADF),
        }
    }

    /// 写入数据
    /// Read from a regular file at a fixed offset without changing the fd offset.
    pub fn read_at(&mut self, offset: usize, buf: &mut [u8]) -> Result<usize, SysErrNo> {
        match self {
            FileDescriptor::MemFile { content, .. } => {
                if offset >= content.len() {
                    return Ok(0);
                }
                let to_read = buf.len().min(content.len() - offset);
                buf[..to_read].copy_from_slice(&content[offset..offset + to_read]);
                Ok(to_read)
            }
            FileDescriptor::Ext4Regular { ino, readable, .. } => {
                if !*readable {
                    return Err(SysErrNo::EBADF);
                }
                ext4_vol::ext4_read_at(*ino, offset, buf)
            }
            FileDescriptor::MemDir { .. } | FileDescriptor::Ext4Dir { .. } => Err(SysErrNo::EISDIR),
            FileDescriptor::PipeRead { .. } | FileDescriptor::PipeWrite { .. } => {
                Err(SysErrNo::ESPIPE)
            }
            _ => Err(SysErrNo::EBADF),
        }
    }

    /// Write data to this descriptor.
    pub fn write(&mut self, buf: &[u8]) -> Result<usize, SysErrNo> {
        match self {
            FileDescriptor::Stdout | FileDescriptor::Stderr => {
                write_console_buffered(buf);
                Ok(buf.len())
            }
            FileDescriptor::MemDir { .. } => Err(SysErrNo::EISDIR),
            FileDescriptor::MemFile {
                name,
                content,
                offset,
                writable,
                append,
            } => {
                if !*writable {
                    return Err(SysErrNo::EBADF);
                }
                if *append {
                    *offset = content.len();
                }
                let start = *offset;
                let end = start.saturating_add(buf.len());
                if end > content.len() {
                    content.resize(end, 0);
                }
                content[start..end].copy_from_slice(buf);
                *offset = end;
                MEM_FS.lock().add_file(name, content.clone());

                Ok(buf.len())
            }
            FileDescriptor::Ext4Regular {
                ino,
                offset,
                writable,
                append,
                ..
            } => {
                if !*writable {
                    return Err(SysErrNo::EBADF);
                }
                if *append {
                    *offset = ext4_vol::regular_file_size(*ino)?;
                }
                let n = ext4_vol::ext4_write_at(*ino, *offset, buf)?;
                *offset += n;
                Ok(n)
            }
            FileDescriptor::Ext4Dir { .. } => Err(SysErrNo::EISDIR),
            FileDescriptor::PipeWrite { state, .. } => {
                let mut pipe = state.lock();
                if pipe.readers == 0 {
                    return Err(SysErrNo::EPIPE);
                }
                for &b in buf {
                    pipe.buf.push_back(b);
                }
                Ok(buf.len())
            }
            FileDescriptor::PipeRead { .. } => Err(SysErrNo::EBADF),
            _ => Err(SysErrNo::EBADF),
        }
    }

    /// lseek with signed offset
    pub fn seek_signed(&mut self, offset: isize, whence: usize) -> Result<FileOffset, SysErrNo> {
        match self {
            FileDescriptor::MemFile {
                content,
                offset: current_offset,
                writable,
                ..
            } => {
                let new_offset: isize = match whence {
                    0 => offset,                            // SEEK_SET
                    1 => *current_offset as isize + offset, // SEEK_CUR
                    2 => content.len() as isize + offset,   // SEEK_END
                    _ => return Err(SysErrNo::EINVAL),
                };
                if new_offset < 0 {
                    return Err(SysErrNo::EINVAL);
                }
                let new_off = new_offset as usize;
                if new_off > content.len() && *writable {
                    content.resize(new_off, 0);
                }
                *current_offset = new_off;
                Ok(new_off)
            }
            FileDescriptor::Ext4Regular {
                ino,
                offset: current_offset,
                ..
            } => {
                let sz = ext4_vol::regular_file_size(*ino)? as isize;
                let new_offset: isize = match whence {
                    0 => offset,
                    1 => *current_offset as isize + offset,
                    2 => sz + offset,
                    _ => return Err(SysErrNo::EINVAL),
                };
                if new_offset < 0 {
                    return Err(SysErrNo::EINVAL);
                }
                *current_offset = new_offset as usize;
                Ok(*current_offset)
            }
            FileDescriptor::PipeRead { .. } | FileDescriptor::PipeWrite { .. } => {
                Err(SysErrNo::ESPIPE)
            }
            FileDescriptor::MemDir {
                entries,
                offset: current_offset,
                ..
            } => {
                let end = entries.len() as isize;
                let new_offset: isize = match whence {
                    0 => offset,
                    1 => *current_offset as isize + offset,
                    2 => end + offset,
                    _ => return Err(SysErrNo::EINVAL),
                };
                if new_offset < 0 || new_offset > end {
                    return Err(SysErrNo::EINVAL);
                }
                *current_offset = new_offset as usize;
                Ok(*current_offset)
            }
            FileDescriptor::Ext4Dir { .. } => Err(SysErrNo::ESPIPE),
            _ => Err(SysErrNo::ESPIPE),
        }
    }

    /// 设置文件偏移
    pub fn seek(&mut self, offset: FileOffset, whence: usize) -> Result<FileOffset, SysErrNo> {
        match self {
            FileDescriptor::MemFile {
                content,
                offset: current_offset,
                writable,
                ..
            } => {
                let new_offset = match whence {
                    0 => offset,
                    1 => (*current_offset as isize + offset as isize) as usize,
                    2 => (content.len() as isize + offset as isize) as usize,
                    _ => return Err(SysErrNo::EINVAL),
                };
                if new_offset > content.len() && !*writable {
                    return Err(SysErrNo::EINVAL);
                }
                if new_offset > content.len() && *writable {
                    content.resize(new_offset, 0);
                }
                *current_offset = new_offset;
                Ok(new_offset)
            }
            FileDescriptor::MemDir {
                entries,
                offset: current_offset,
                ..
            } => {
                let end = entries.len();
                let new_offset = match whence {
                    0 => offset,
                    1 => *current_offset + offset,
                    2 => end + offset,
                    _ => return Err(SysErrNo::EINVAL),
                };
                if new_offset > end {
                    return Err(SysErrNo::EINVAL);
                }
                *current_offset = new_offset;
                Ok(new_offset)
            }
            FileDescriptor::Ext4Dir { .. } => Err(SysErrNo::ESPIPE),
            FileDescriptor::Ext4Regular {
                ino,
                offset: current_offset,
                writable,
                ..
            } => {
                let sz = ext4_vol::regular_file_size(*ino)?;
                let new_offset = match whence {
                    0 => offset,
                    1 => (*current_offset as isize + offset as isize) as usize,
                    2 => (sz as isize + offset as isize) as usize,
                    _ => return Err(SysErrNo::EINVAL),
                };
                if new_offset > sz && !*writable {
                    return Err(SysErrNo::EINVAL);
                }
                *current_offset = new_offset;
                Ok(new_offset)
            }
            FileDescriptor::PipeRead { .. } | FileDescriptor::PipeWrite { .. } => {
                Err(SysErrNo::ESPIPE)
            }
            _ => Err(SysErrNo::ESPIPE),
        }
    }

    /// 获取文件大小
    pub fn size(&self) -> usize {
        match self {
            FileDescriptor::MemFile { content, .. } => content.len(),
            FileDescriptor::MemDir { entries, .. } => entries.len(),
            FileDescriptor::Ext4Regular { ino, .. } => {
                ext4_vol::regular_file_size(*ino).unwrap_or(0)
            }
            FileDescriptor::Ext4Dir { ino, .. } => ext4_vol::ext4_list_dir_by_ino(*ino)
                .map(|v| v.len())
                .unwrap_or(0),
            FileDescriptor::PipeRead { state, .. } | FileDescriptor::PipeWrite { state, .. } => {
                state.lock().buf.len()
            }
            _ => 0,
        }
    }

    pub fn read_dirents64(&mut self, buf: &mut [u8]) -> Result<usize, SysErrNo> {
        match self {
            FileDescriptor::MemDir {
                entries, offset, ..
            } => {
                let mut written = 0usize;

                while *offset < entries.len() {
                    let entry = &entries[*offset];
                    let name_bytes = entry.name.as_bytes();
                    let reclen = (19 + name_bytes.len() + 1 + 7) & !7;
                    if written + reclen > buf.len() {
                        break;
                    }

                    let base = written;
                    let ino = (*offset + 1) as u64;
                    let off = (*offset + 1) as i64;
                    let d_type = if entry.is_dir { 4u8 } else { 8u8 };

                    buf[base..base + 8].copy_from_slice(&ino.to_le_bytes());
                    buf[base + 8..base + 16].copy_from_slice(&off.to_le_bytes());
                    buf[base + 16..base + 18].copy_from_slice(&(reclen as u16).to_le_bytes());
                    buf[base + 18] = d_type;
                    let name_start = base + 19;
                    buf[name_start..name_start + name_bytes.len()].copy_from_slice(name_bytes);
                    buf[name_start + name_bytes.len()] = 0;
                    for byte in &mut buf[base + 19 + name_bytes.len() + 1..base + reclen] {
                        *byte = 0;
                    }

                    written += reclen;
                    *offset += 1;
                }

                Ok(written)
            }
            FileDescriptor::Ext4Dir { ino, offset } => {
                let entries = ext4_vol::ext4_list_dir_by_ino(*ino)?;
                let mut written = 0usize;

                while *offset < entries.len() {
                    let (child_ino, name, is_dir) = &entries[*offset];
                    let name_bytes = name.as_bytes();
                    let reclen = (19 + name_bytes.len() + 1 + 7) & !7;
                    if written + reclen > buf.len() {
                        break;
                    }

                    let base = written;
                    let off = (*offset + 1) as i64;
                    let d_type = if *is_dir { 4u8 } else { 8u8 };

                    buf[base..base + 8].copy_from_slice(&(*child_ino as u64).to_le_bytes());
                    buf[base + 8..base + 16].copy_from_slice(&off.to_le_bytes());
                    buf[base + 16..base + 18].copy_from_slice(&(reclen as u16).to_le_bytes());
                    buf[base + 18] = d_type;
                    let name_start = base + 19;
                    buf[name_start..name_start + name_bytes.len()].copy_from_slice(name_bytes);
                    buf[name_start + name_bytes.len()] = 0;
                    for byte in &mut buf[base + 19 + name_bytes.len() + 1..base + reclen] {
                        *byte = 0;
                    }

                    written += reclen;
                    *offset += 1;
                }

                Ok(written)
            }
            _ => Err(SysErrNo::ENOTDIR),
        }
    }
}

impl Clone for FileDescriptor {
    fn clone(&self) -> Self {
        match self {
            FileDescriptor::Stdin => FileDescriptor::Stdin,
            FileDescriptor::Stdout => FileDescriptor::Stdout,
            FileDescriptor::Stderr => FileDescriptor::Stderr,
            FileDescriptor::MemFile {
                name,
                content,
                offset,
                writable,
                append,
            } => FileDescriptor::MemFile {
                name: name.clone(),
                content: content.clone(),
                offset: *offset,
                writable: *writable,
                append: *append,
            },
            FileDescriptor::MemDir {
                path,
                entries,
                offset,
            } => FileDescriptor::MemDir {
                path: path.clone(),
                entries: entries.clone(),
                offset: *offset,
            },
            FileDescriptor::Ext4Regular {
                ino,
                offset,
                readable,
                writable,
                append,
            } => FileDescriptor::Ext4Regular {
                ino: *ino,
                offset: *offset,
                readable: *readable,
                writable: *writable,
                append: *append,
            },
            FileDescriptor::Ext4Dir { ino, offset } => FileDescriptor::Ext4Dir {
                ino: *ino,
                offset: *offset,
            },
            FileDescriptor::PipeRead { state, nonblock } => {
                state.lock().readers += 1;
                FileDescriptor::PipeRead {
                    state: state.clone(),
                    nonblock: *nonblock,
                }
            }
            FileDescriptor::PipeWrite { state, nonblock } => {
                state.lock().writers += 1;
                FileDescriptor::PipeWrite {
                    state: state.clone(),
                    nonblock: *nonblock,
                }
            }
        }
    }
}

impl Drop for FileDescriptor {
    fn drop(&mut self) {
        match self {
            FileDescriptor::PipeRead { state, .. } => {
                let mut pipe = state.lock();
                if pipe.readers > 0 {
                    pipe.readers -= 1;
                }
            }
            FileDescriptor::PipeWrite { state, .. } => {
                let mut pipe = state.lock();
                if pipe.writers > 0 {
                    pipe.writers -= 1;
                }
            }
            _ => {}
        }
    }
}

/// 文件描述符表
#[derive(Clone)]
pub struct FileDescriptorTable {
    fds: Vec<Option<FileDescriptor>>,
}

impl FileDescriptorTable {
    pub fn new() -> Self {
        let mut fds = Vec::with_capacity(MAX_FD_NUM);
        fds.push(Some(FileDescriptor::Stdin));
        fds.push(Some(FileDescriptor::Stdout));
        fds.push(Some(FileDescriptor::Stderr));

        for _ in 3..MAX_FD_NUM {
            fds.push(None);
        }

        Self { fds }
    }

    pub fn alloc(&mut self, fd: FileDescriptor) -> Option<usize> {
        for (i, slot) in self.fds.iter_mut().enumerate() {
            if slot.is_none() {
                *slot = Some(fd);
                return Some(i);
            }
        }
        None
    }

    pub fn alloc_from(&mut self, start: usize, fd: FileDescriptor) -> Option<usize> {
        for (i, slot) in self.fds.iter_mut().enumerate().skip(start) {
            if slot.is_none() {
                *slot = Some(fd);
                return Some(i);
            }
        }
        None
    }

    pub fn alloc_at(&mut self, index: usize, fd: FileDescriptor) -> Result<(), SysErrNo> {
        if index >= MAX_FD_NUM {
            return Err(SysErrNo::EBADF);
        }
        self.fds[index] = Some(fd);
        Ok(())
    }

    pub fn get(&self, fd: usize) -> Option<&FileDescriptor> {
        if fd >= MAX_FD_NUM {
            return None;
        }
        self.fds[fd].as_ref()
    }

    pub fn get_mut(&mut self, fd: usize) -> Option<&mut FileDescriptor> {
        if fd >= MAX_FD_NUM {
            return None;
        }
        self.fds[fd].as_mut()
    }

    pub fn free(&mut self, fd: usize) -> Result<(), SysErrNo> {
        if fd >= MAX_FD_NUM {
            return Err(SysErrNo::EBADF);
        }
        if self.fds[fd].is_none() {
            return Err(SysErrNo::EBADF);
        }
        self.fds[fd] = None;
        Ok(())
    }

    pub fn dup(&mut self, old_fd: usize) -> Result<usize, SysErrNo> {
        if old_fd >= MAX_FD_NUM || self.fds[old_fd].is_none() {
            return Err(SysErrNo::EBADF);
        }
        let fd = self.fds[old_fd].clone().unwrap();
        match self.alloc(fd) {
            Some(new_fd) => Ok(new_fd),
            None => Err(SysErrNo::EMFILE),
        }
    }

    pub fn dup2(&mut self, old_fd: usize, new_fd: usize) -> Result<usize, SysErrNo> {
        if old_fd >= MAX_FD_NUM || self.fds[old_fd].is_none() {
            return Err(SysErrNo::EBADF);
        }
        if new_fd >= MAX_FD_NUM {
            return Err(SysErrNo::EBADF);
        }
        if old_fd == new_fd {
            return Ok(new_fd);
        }
        if self.fds[new_fd].is_some() {
            let _ = self.free(new_fd);
        }
        let fd = self.fds[old_fd].clone().unwrap();
        self.fds[new_fd] = Some(fd);
        Ok(new_fd)
    }

    pub fn len(&self) -> usize {
        self.fds.iter().filter(|slot| slot.is_some()).count()
    }
}

impl Default for FileDescriptorTable {
    fn default() -> Self {
        Self::new()
    }
}

fn open_dir_descriptor(path_norm: &str) -> Result<FileDescriptor, SysErrNo> {
    if MEM_FS.lock().is_dir(path_norm) {
        let entries = fs::list_dir(path_norm)?;
        return Ok(FileDescriptor::MemDir {
            path: path_norm.into(),
            entries,
            offset: 0,
        });
    }

    let Some((ino, is_dir)) = ext4_vol::lookup_path(path_norm) else {
        return Err(SysErrNo::ENOENT);
    };
    if !is_dir {
        return Err(SysErrNo::ENOTDIR);
    }
    Ok(FileDescriptor::Ext4Dir { ino, offset: 0 })
}

/// 打开路径：`flags`/`mode` 语义对齐 Linux `openat` 子集。
pub fn open_file(path: &str, flags: u32, _mode: u32) -> Result<FileDescriptor, SysErrNo> {
    use open_flags::*;

    let path_norm = fs::normalize_path(path);
    let accmode = flags & O_ACCMODE;
    let read_ok = accmode == O_RDONLY || accmode == O_RDWR;
    let write_ok = accmode == O_WRONLY || accmode == O_RDWR;
    let want_dir = (flags & O_DIRECTORY) != 0;
    let want_create = (flags & O_CREAT) != 0;
    let want_excl = (flags & O_EXCL) != 0;
    let want_trunc = (flags & O_TRUNC) != 0;
    let append = (flags & O_APPEND) != 0;

    let removed = fs::is_removed(&path_norm);
    if removed {
        if !(want_create && write_ok) {
            return Err(SysErrNo::ENOENT);
        }
    }

    if fs::dir_exists(&path_norm) {
        if write_ok || want_trunc || want_create {
            return Err(SysErrNo::EISDIR);
        }
        return open_dir_descriptor(&path_norm);
    }

    if want_dir {
        if !fs::dir_exists(&path_norm) {
            return Err(SysErrNo::ENOENT);
        }
        // MemFS 目录优先走 MemDir（预载已全部加载到内存）。
        // ext4 独有目录走 Ext4Dir（实时读取目录项，支持 getdents64）。
        if MEM_FS.lock().is_dir(&path_norm) {
            let entries = fs::list_dir(&path_norm)?;
            return Ok(FileDescriptor::MemDir {
                path: path_norm,
                entries,
                offset: 0,
            });
        } else {
            // ext4 目录：通过 lookup_path 获取 inode 并返回 Ext4Dir
            let Some((ino, _)) = ext4_vol::lookup_path(&path_norm) else {
                return Err(SysErrNo::ENOENT);
            };
            return Ok(FileDescriptor::Ext4Dir { ino, offset: 0 });
        }
    }

    if fs::dir_exists(&path_norm) {
        return Err(SysErrNo::EISDIR);
    }

    let mem = fs::MEM_FS.lock();
    let mem_has_file = mem.get_file(&path_norm).is_some();
    drop(mem);

    if mem_has_file {
        if want_excl && want_create {
            return Err(SysErrNo::EEXIST);
        }
        let mut content = fs::read_file(&path_norm).unwrap_or_default();
        if want_trunc && write_ok {
            content.clear();
            fs::MEM_FS.lock().truncate_file(&path_norm, 0)?;
        }
        let base_off = if append && write_ok { content.len() } else { 0 };
        return Ok(FileDescriptor::MemFile {
            name: path_norm,
            content,
            offset: base_off,
            writable: write_ok,
            append,
        });
    }

    if !removed && ext4_vol::ext4_regular_file_exists(&path_norm) {
        if want_excl && want_create {
            return Err(SysErrNo::EEXIST);
        }
        let Some((ino, is_dir)) = ext4_vol::lookup_path(&path_norm) else {
            return Err(SysErrNo::ENOENT);
        };
        if is_dir {
            return Err(SysErrNo::EISDIR);
        }
        if write_ok || want_trunc {
            let mut content = if want_trunc {
                Vec::new()
            } else {
                ext4_vol::slurp_regular_file(&path_norm).unwrap_or_default()
            };
            let base_off = if append && write_ok { content.len() } else { 0 };
            fs::MEM_FS.lock().add_file(&path_norm, content.clone());
            return Ok(FileDescriptor::MemFile {
                name: path_norm,
                content,
                offset: base_off,
                writable: write_ok,
                append,
            });
        }
        if want_trunc && write_ok {
            ext4_vol::truncate_regular_ext4(&path_norm, 0)?;
        }
        let base_off = if append && write_ok {
            ext4_vol::regular_file_size(ino)?
        } else {
            0
        };
        return Ok(FileDescriptor::Ext4Regular {
            ino,
            offset: base_off,
            readable: read_ok,
            writable: write_ok,
            append,
        });
    }

    if want_create && write_ok {
        fs::MEM_FS.lock().add_file(&path_norm, Vec::new());
        return Ok(FileDescriptor::MemFile {
            name: path_norm,
            content: Vec::new(),
            offset: 0,
            writable: true,
            append,
        });
    }

    Err(SysErrNo::ENOENT)
}

pub fn create_pipe(nonblock: bool) -> (FileDescriptor, FileDescriptor) {
    let state = Arc::new(Mutex::new(PipeState {
        buf: VecDeque::new(),
        readers: 1,
        writers: 1,
    }));
    (
        FileDescriptor::PipeRead {
            state: state.clone(),
            nonblock,
        },
        FileDescriptor::PipeWrite { state, nonblock },
    )
}
