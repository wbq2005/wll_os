use crate::utils::error::SysErrNo;
use super::SyscallRet;
use crate::console::putchar;
use crate::task::{current_task, suspend_current_and_run_next};
use alloc::string::String;

use crate::fs::fd::{self, FileDescriptor};

/// 标准文件描述符
const FD_STDIN: usize = 0;
const FD_STDOUT: usize = 1;
const FD_STDERR: usize = 2;
const AT_FDCWD: isize = -100;

const F_DUPFD: usize = 0;
const F_GETFD: usize = 1;
const F_SETFD: usize = 2;
const F_GETFL: usize = 3;
const F_SETFL: usize = 4;
const F_DUPFD_CLOEXEC: usize = 1030;

const S_IFIFO: u32 = 0o010000;
const S_IFDIR: u32 = 0o040000;
const S_IFREG: u32 = 0o100000;

#[repr(C)]
#[derive(Clone, Copy)]
struct KStat {
    st_dev: u64,
    st_ino: u64,
    st_mode: u32,
    st_nlink: u32,
    st_uid: u32,
    st_gid: u32,
    st_rdev: u64,
    __pad: usize,
    st_size: isize,
    st_blksize: u32,
    __pad2: i32,
    st_blocks: u64,
    st_atime_sec: isize,
    st_atime_nsec: isize,
    st_mtime_sec: isize,
    st_mtime_nsec: isize,
    st_ctime_sec: isize,
    st_ctime_nsec: isize,
    __unused: [u32; 2],
}

#[repr(C)]
#[derive(Clone, Copy)]
struct IoVec {
    iov_base: *mut u8,
    iov_len: usize,
}

fn read_user_cstr(ptr: *const u8) -> Result<String, SysErrNo> {
    if ptr.is_null() {
        return Err(SysErrNo::EFAULT);
    }

    let task = current_task().ok_or(SysErrNo::ESRCH)?;
    let memory_set = task.memory_set.lock();
    let mut bytes = alloc::vec::Vec::new();
    let mut addr = ptr as usize;
    const MAX_PATH_LEN: usize = 4096;

    for _ in 0..MAX_PATH_LEN {
        let pa = memory_set
            .translate(polyhal::VirtAddr::new(addr))
            .ok_or(SysErrNo::EFAULT)?;
        let byte = unsafe { *(pa.raw() as *const u8) };
        if byte == 0 {
            return String::from_utf8(bytes).map_err(|_| SysErrNo::EINVAL);
        }
        bytes.push(byte);
        addr += 1;
    }

    Err(SysErrNo::EFAULT)
}

fn copy_to_user(dst: *mut u8, src: &[u8]) -> Result<(), SysErrNo> {
    if dst.is_null() {
        return Err(SysErrNo::EFAULT);
    }
    let task = current_task().ok_or(SysErrNo::ESRCH)?;
    let memory_set = task.memory_set.lock();
    let mut addr = dst as usize;
    for &byte in src {
        let pa = memory_set
            .translate(polyhal::VirtAddr::new(addr))
            .ok_or(SysErrNo::EFAULT)?;
        unsafe { *(pa.raw() as *mut u8) = byte; }
        addr += 1;
    }
    Ok(())
}

fn copy_from_user(src: *const u8, dst: &mut [u8]) -> Result<(), SysErrNo> {
    if src.is_null() {
        return Err(SysErrNo::EFAULT);
    }
    let task = current_task().ok_or(SysErrNo::ESRCH)?;
    let memory_set = task.memory_set.lock();
    let mut addr = src as usize;
    for byte in dst {
        let pa = memory_set
            .translate(polyhal::VirtAddr::new(addr))
            .ok_or(SysErrNo::EFAULT)?;
        *byte = unsafe { *(pa.raw() as *const u8) };
        addr += 1;
    }
    Ok(())
}

fn copy_object_from_user<T: Copy>(src: *const T) -> Result<T, SysErrNo> {
    let mut obj = core::mem::MaybeUninit::<T>::uninit();
    let bytes = unsafe {
        core::slice::from_raw_parts_mut(
            obj.as_mut_ptr() as *mut u8,
            core::mem::size_of::<T>(),
        )
    };
    copy_from_user(src as *const u8, bytes)?;
    Ok(unsafe { obj.assume_init() })
}

fn resolve_path(dirfd: isize, pathname: *const u8) -> Result<String, SysErrNo> {
    let path = read_user_cstr(pathname)?;
    if path.is_empty() {
        return Err(SysErrNo::ENOENT);
    }
    if path.starts_with('/') {
        return Ok(crate::fs::normalize_path(&path));
    }

    let base = resolve_base_dir(dirfd)?;
    Ok(crate::fs::resolve_path(&base, &path))
}

fn current_root() -> Result<String, SysErrNo> {
    let task = current_task().ok_or(SysErrNo::ESRCH)?;
    let root = task.inner.lock().root.clone();
    Ok(root)
}

fn resolve_host_path(dirfd: isize, pathname: *const u8) -> Result<(String, String), SysErrNo> {
    let logical = resolve_path(dirfd, pathname)?;
    let root = current_root()?;
    let host = crate::fs::apply_root(&root, &logical);
    Ok((logical, host))
}

fn resolve_base_dir(dirfd: isize) -> Result<String, SysErrNo> {
    let task = current_task().ok_or(SysErrNo::ESRCH)?;
    let inner = task.inner.lock();
    if dirfd == AT_FDCWD {
        return Ok(inner.cwd.clone());
    }
    let dirfd = usize::try_from(dirfd).map_err(|_| SysErrNo::EBADF)?;
    let result = match inner.fd_table.lock().get(dirfd) {
        Some(FileDescriptor::MemDir { path, .. }) => Ok(path.clone()),
        Some(FileDescriptor::Ext4Dir { .. }) => {
            // ext4 directories don't store a path string; use cwd as fallback
            Ok(inner.cwd.clone())
        }
        Some(_) => Err(SysErrNo::ENOTDIR),
        None => Err(SysErrNo::EBADF),
    };
    result
}

fn pseudo_inode(path: &str) -> u64 {
    let mut hash = 1469598103934665603u64;
    for &b in path.as_bytes() {
        hash ^= b as u64;
        hash = hash.wrapping_mul(1099511628211);
    }
    hash
}

fn regular_blocks(size: usize) -> u64 {
    size.div_ceil(512) as u64
}

fn current_times() -> (isize, isize) {
    let (sec, usec) = crate::timer::get_timeval();
    (sec as isize, (usec * 1000) as isize)
}

fn make_kstat(ino: u64, mode: u32, size: usize) -> KStat {
    let (sec, nsec) = current_times();
    KStat {
        st_dev: 0,
        st_ino: ino,
        st_mode: mode,
        st_nlink: 1,
        st_uid: 0,
        st_gid: 0,
        st_rdev: 0,
        __pad: 0,
        st_size: size as isize,
        st_blksize: 4096,
        __pad2: 0,
        st_blocks: regular_blocks(size),
        st_atime_sec: sec,
        st_atime_nsec: nsec,
        st_mtime_sec: sec,
        st_mtime_nsec: nsec,
        st_ctime_sec: sec,
        st_ctime_nsec: nsec,
        __unused: [0; 2],
    }
}

fn stat_for_fd(file_desc: &FileDescriptor) -> KStat {
    match file_desc {
        FileDescriptor::Stdin => make_kstat(0, S_IFIFO | 0o444, 0),
        FileDescriptor::Stdout | FileDescriptor::Stderr => make_kstat(0, S_IFIFO | 0o222, 0),
        FileDescriptor::MemFile { name, content, writable, .. } => {
            let mode = S_IFREG | if *writable { 0o666 } else { 0o444 };
            make_kstat(pseudo_inode(name), mode, content.len())
        }
        FileDescriptor::MemDir { path, entries, .. } => {
            make_kstat(pseudo_inode(path), S_IFDIR | 0o755, entries.len())
        }
        FileDescriptor::Ext4Regular {
            ino,
            readable,
            writable,
            ..
        } => {
            let perms = match (*readable, *writable) {
                (true, true) => 0o666,
                (false, true) => 0o222,
                _ => 0o444,
            };
            make_kstat(*ino as u64, S_IFREG | perms, crate::fs::ext4_vol::regular_file_size(*ino).unwrap_or(0))
        }
        FileDescriptor::Ext4Dir { ino, .. } => {
            make_kstat(
                *ino as u64,
                S_IFDIR | 0o755,
                crate::fs::ext4_vol::ext4_list_dir_by_ino(*ino)
                    .map(|v| v.len())
                    .unwrap_or(0),
            )
        }
        FileDescriptor::PipeRead { .. } => make_kstat(0, S_IFIFO | 0o444, 0),
        FileDescriptor::PipeWrite { .. } => make_kstat(0, S_IFIFO | 0o222, 0),
    }
}

fn stat_for_path(path: &str) -> Result<KStat, SysErrNo> {
    let norm = crate::fs::normalize_path(path);
    if crate::fs::dir_exists(&norm) {
        let entries = crate::fs::list_dir(&norm)?;
        return Ok(make_kstat(pseudo_inode(&norm), S_IFDIR | 0o755, entries.len()));
    }
    if crate::fs::file_exists(&norm) {
        let size = crate::fs::read_file(&norm).map(|v| v.len()).unwrap_or(0);
        return Ok(make_kstat(pseudo_inode(&norm), S_IFREG | 0o444, size));
    }
    if let Some((ino, is_dir)) = crate::fs::ext4_vol::lookup_path(&norm) {
        if is_dir {
            let entries = crate::fs::ext4_vol::ext4_list_dir(&norm)?;
            Ok(make_kstat(ino as u64, S_IFDIR | 0o755, entries.len()))
        } else {
            let size = crate::fs::ext4_vol::regular_file_size(ino)?;
            Ok(make_kstat(ino as u64, S_IFREG | 0o666, size))
        }
    } else {
        Err(SysErrNo::ENOENT)
    }
}

fn copy_kstat_out(statbuf: *mut u8, st: &KStat) -> Result<(), SysErrNo> {
    let bytes = unsafe {
        core::slice::from_raw_parts(
            st as *const KStat as *const u8,
            core::mem::size_of::<KStat>(),
        )
    };
    copy_to_user(statbuf, bytes)
}

fn fd_status_flags(file_desc: &FileDescriptor) -> usize {
    match file_desc {
        FileDescriptor::Stdin => fd::open_flags::O_RDONLY as usize,
        FileDescriptor::Stdout | FileDescriptor::Stderr => fd::open_flags::O_WRONLY as usize,
        FileDescriptor::MemFile { writable, append, .. } => {
            let mut flags = if *writable {
                fd::open_flags::O_RDWR as usize
            } else {
                fd::open_flags::O_RDONLY as usize
            };
            if *append {
                flags |= fd::open_flags::O_APPEND as usize;
            }
            flags
        }
        FileDescriptor::MemDir { .. } => {
            fd::open_flags::O_RDONLY as usize | fd::open_flags::O_DIRECTORY as usize
        }
        FileDescriptor::Ext4Regular {
            readable,
            writable,
            append,
            ..
        } => {
            let mut flags = match (*readable, *writable) {
                (true, true) => fd::open_flags::O_RDWR as usize,
                (false, true) => fd::open_flags::O_WRONLY as usize,
                _ => fd::open_flags::O_RDONLY as usize,
            };
            if *append {
                flags |= fd::open_flags::O_APPEND as usize;
            }
            flags
        }
        FileDescriptor::Ext4Dir { .. } => {
            fd::open_flags::O_RDONLY as usize | fd::open_flags::O_DIRECTORY as usize
        }
        FileDescriptor::PipeRead { nonblock, .. } => {
            let mut flags = fd::open_flags::O_RDONLY as usize;
            if *nonblock {
                flags |= fd::pipe_flags::O_NONBLOCK;
            }
            flags
        }
        FileDescriptor::PipeWrite { nonblock, .. } => {
            let mut flags = fd::open_flags::O_WRONLY as usize;
            if *nonblock {
                flags |= fd::pipe_flags::O_NONBLOCK;
            }
            flags
        }
    }
}

/// openat 系统调用
///
/// 打开或创建一个文件
/// - dirfd: 目录文件描述符
/// - pathname: 文件路径
/// - flags: 打开标志
/// - mode: 文件模式
pub fn sys_openat(dirfd: isize, pathname: *const u8, flags: u32, mode: u32) -> SyscallRet {
    let (logical_path, host_path) = resolve_host_path(dirfd, pathname)?;

    log::info!(
        "[syscall] openat(dirfd={}, pathname='{}' -> '{}', flags={}, mode={})",
        dirfd,
        logical_path,
        host_path,
        flags,
        mode
    );

    // 获取当前任务的文件描述符表
    if let Some(task) = current_task() {
        let mut inner = task.inner.lock();

        match crate::fs::fd::open_file(&host_path, flags, mode) {
            Ok(fd_desc) => {
                let mut fds = inner.fd_table.lock();
                match fds.alloc(fd_desc) {
                    Some(new_fd) => {
                        log::info!(
                            "[syscall] openat: allocated fd={} for '{}', fd_table.len={}",
                            new_fd,
                            host_path,
                            fds.len()
                        );
                        Ok(new_fd)
                    }
                    None => Err(SysErrNo::EMFILE),
                }
            }
            Err(e) => Err(e),
        }
    } else {
        Err(SysErrNo::ESRCH)
    }
}

pub fn sys_getcwd(buf: *mut u8, size: usize) -> SyscallRet {
    if buf.is_null() {
        return Err(SysErrNo::EFAULT);
    }
    if size == 0 {
        return Err(SysErrNo::EINVAL);
    }

    if let Some(task) = current_task() {
        let cwd = task.inner.lock().cwd.clone();
        let bytes = cwd.as_bytes();
        if bytes.len() + 1 > size {
            return Err(SysErrNo::ERANGE);
        }
        copy_to_user(buf, bytes)?;
        copy_to_user(unsafe { buf.add(bytes.len()) }, &[0])?;
        Ok(buf as usize)
    } else {
        Err(SysErrNo::ESRCH)
    }
}

pub fn sys_chdir(pathname: *const u8) -> SyscallRet {
    let (logical_path, host_path) = resolve_host_path(AT_FDCWD, pathname)?;
    if !crate::fs::dir_exists(&host_path) {
        return Err(SysErrNo::ENOENT);
    }
    if let Some(task) = current_task() {
        task.inner.lock().cwd = logical_path;
        Ok(0)
    } else {
        Err(SysErrNo::ESRCH)
    }
}

pub fn sys_mkdirat(dirfd: isize, pathname: *const u8, _mode: u32) -> SyscallRet {
    let (_logical_path, host_path) = resolve_host_path(dirfd, pathname)?;
    crate::fs::create_dir(&host_path)?;
    Ok(0)
}

pub fn sys_unlinkat(dirfd: isize, pathname: *const u8, _flags: usize) -> SyscallRet {
    let (_logical_path, host_path) = resolve_host_path(dirfd, pathname)?;
    crate::fs::remove_file(&host_path)?;
    Ok(0)
}

pub fn sys_getdents64(fd: usize, dirp: *mut u8, count: usize) -> SyscallRet {
    if dirp.is_null() {
        return Err(SysErrNo::EFAULT);
    }
    if let Some(task) = current_task() {
        let mut inner = task.inner.lock();
        let mut fds = inner.fd_table.lock();
        match fds.get_mut(fd) {
            Some(file_desc) => {
                let mut kbuf = alloc::vec![0u8; count];
                let n = file_desc.read_dirents64(&mut kbuf)?;
                copy_to_user(dirp, &kbuf[..n])?;
                Ok(n)
            }
            None => Err(SysErrNo::EBADF),
        }
    } else {
        Err(SysErrNo::ESRCH)
    }
}

pub fn sys_pipe2(pipefd: *mut i32, flags: usize) -> SyscallRet {
    use crate::fs::fd::pipe_flags;

    if pipefd.is_null() {
        return Err(SysErrNo::EFAULT);
    }
    let allowed = pipe_flags::O_NONBLOCK | pipe_flags::O_CLOEXEC;
    if flags & !allowed != 0 {
        return Err(SysErrNo::EINVAL);
    }
    let nonblock = (flags & pipe_flags::O_NONBLOCK) != 0;
    if let Some(task) = current_task() {
        let mut inner = task.inner.lock();
        let (read_fd, write_fd) = {
            let mut fds = inner.fd_table.lock();
            let (read_end, write_end) = crate::fs::fd::create_pipe(nonblock);
            let read_fd = fds.alloc(read_end).ok_or(SysErrNo::EMFILE)?;
            let write_fd = match fds.alloc(write_end) {
                Some(fd) => fd,
                None => {
                    let _ = fds.free(read_fd);
                    return Err(SysErrNo::EMFILE);
                }
            };
            (read_fd, write_fd)
        };
        let pipefd_vals = [read_fd as i32, write_fd as i32];
        let bytes = unsafe {
            core::slice::from_raw_parts(
                pipefd_vals.as_ptr() as *const u8,
                core::mem::size_of_val(&pipefd_vals),
            )
        };
        copy_to_user(pipefd as *mut u8, bytes)?;
        Ok(0)
    } else {
        Err(SysErrNo::ESRCH)
    }
}

pub fn sys_mount(
    source: *const u8,
    target: *const u8,
    fstype: *const u8,
    flags: usize,
    data: usize,
) -> SyscallRet {
    let _ = flags;
    let _ = data;
    let fst = read_user_cstr(fstype)?;
    let tgt = read_user_cstr(target)?;
    let _src = if source.is_null() {
        String::new()
    } else {
        read_user_cstr(source)?
    };

    let norm = crate::fs::normalize_path(&tgt);
    if norm != "/" {
        log::info!("[syscall] mount: unsupported target '{}'", tgt);
        return Ok(0);
    }

    match fst.as_str() {
        "ext4" => {
            if crate::fs::ext4_vol::is_ext4_mounted() {
                Ok(0)
            } else {
                Err(SysErrNo::ENODEV)
            }
        }
        _ => {
            log::debug!("[syscall] mount fstype '{}' — ignored (MemFS / no-op)", fst);
            Ok(0)
        }
    }
}

pub fn sys_umount2(target: *const u8, flags: usize) -> SyscallRet {
    let _ = flags;
    let _tgt = read_user_cstr(target)?;
    Ok(0)
}

/// close 系统调用
///
/// 关闭文件描述符
/// - fd: 文件描述符
pub fn sys_close(fd: usize) -> SyscallRet {
    log::debug!("[syscall] close(fd={})", fd);

    // 获取当前任务的文件描述符表
    if let Some(task) = current_task() {
        let mut inner = task.inner.lock();
        inner.fd_table.lock().free(fd)?;
        Ok(0)
    } else {
        Err(SysErrNo::ESRCH)
    }
}

/// read 系统调用
///
/// 从文件描述符读取数据
/// - fd: 文件描述符 (0=stdin, 1=stdout, 2=stderr)
/// - buf: 用户空间缓冲区指针
/// - count: 要读取的最大字节数
///
/// 返回值: 成功返回读取的字节数，失败返回错误码
pub fn sys_read(fd: usize, buf: *mut u8, count: usize) -> SyscallRet {
    log::debug!("[syscall] read(fd={}, buf={:p}, count={})", fd, buf, count);

    // 安全检查：确保缓冲区不为 null
    if buf.is_null() {
        return Err(SysErrNo::EFAULT);
    }

    // 安全检查：确保 count 不会导致溢出
    if count > isize::MAX as usize {
        return Err(SysErrNo::EINVAL);
    }

    let task = current_task().ok_or(SysErrNo::ESRCH)?;

    loop {
        let res = {
            let mut inner = task.inner.lock();
            let mut fds = inner.fd_table.lock();
            match fds.get_mut(fd) {
                Some(file_desc) => {
                    let mut kbuf = alloc::vec![0u8; count];
                    match file_desc.read(&mut kbuf) {
                        Ok(n) => {
                            copy_to_user(buf, &kbuf[..n])?;
                            Ok(n)
                        }
                        Err(e) => Err(e),
                    }
                }
                None => Err(SysErrNo::EBADF),
            }
        };

        match res {
            Ok(n) => return Ok(n),
            Err(SysErrNo::EAGAIN) => {
                let nb_pipe = {
                    let inner = task.inner.lock();
                    let fds = inner.fd_table.lock();
                    fds.get(fd).map(|f| f.pipe_read_nonblocking()).unwrap_or(false)
                };
                if nb_pipe {
                    return Err(SysErrNo::EAGAIN);
                }
                suspend_current_and_run_next();
            }
            Err(e) => return Err(e),
        }
    }
}

/// write 系统调用
///
/// 将数据写入文件描述符
/// - fd: 文件描述符 (0=stdin, 1=stdout, 2=stderr)
/// - buf: 用户空间缓冲区指针
/// - count: 要写入的字节数
///
/// 返回值: 成功返回写入的字节数，失败返回错误码
pub fn sys_write(fd: usize, buf: *const u8, count: usize) -> SyscallRet {
    log::debug!("[syscall] write(fd={}, buf={:p}, count={})", fd, buf, count);

    // 安全检查：确保缓冲区不为 null
    if buf.is_null() {
        return Err(SysErrNo::EFAULT);
    }

    // 安全检查：确保 count 不会导致溢出
    if count > isize::MAX as usize {
        return Err(SysErrNo::EINVAL);
    }

    // 获取当前任务
    if let Some(task) = current_task() {
        let mut kbuf = alloc::vec![0u8; count];
        copy_from_user(buf, &mut kbuf)?;
        let mut inner = task.inner.lock();
        let mut fds = inner.fd_table.lock();

        match fds.get_mut(fd) {
            Some(file_desc) => file_desc.write(&kbuf),
            None => Err(SysErrNo::EBADF),
        }
    } else {
        Err(SysErrNo::ESRCH)
    }
}

/// lseek 系统调用
///
/// 设置文件读写偏移
/// - fd: 文件描述符
/// - offset: 偏移量
/// - whence: 起始位置 (0=SEEK_SET, 1=SEEK_CUR, 2=SEEK_END)
pub fn sys_lseek(fd: usize, offset: isize, whence: usize) -> SyscallRet {
    log::debug!("[syscall] lseek(fd={}, offset={}, whence={})", fd, offset, whence);

    let task = current_task().ok_or(SysErrNo::ESRCH)?;
    let inner = task.inner.lock();
    let mut fds = inner.fd_table.lock();

    match fds.get_mut(fd) {
        Some(file_desc) => file_desc.seek_signed(offset, whence),
        None => Err(SysErrNo::EBADF),
    }
}

/// dup 系统调用
///
/// 复制文件描述符
/// - old_fd: 旧文件描述符
pub fn sys_pread64(fd: usize, buf: *mut u8, count: usize, offset: usize) -> SyscallRet {
    if buf.is_null() {
        return Err(SysErrNo::EFAULT);
    }
    if count > isize::MAX as usize {
        return Err(SysErrNo::EINVAL);
    }

    let task = current_task().ok_or(SysErrNo::ESRCH)?;
    let mut inner = task.inner.lock();
    let mut fds = inner.fd_table.lock();
    match fds.get_mut(fd) {
        Some(file_desc) => {
            let mut kbuf = alloc::vec![0u8; count];
            let n = file_desc.read_at(offset, &mut kbuf)?;
            copy_to_user(buf, &kbuf[..n])?;
            Ok(n)
        }
        None => Err(SysErrNo::EBADF),
    }
}

pub fn sys_dup(old_fd: usize) -> SyscallRet {
    log::debug!("[syscall] dup(old_fd={})", old_fd);

    let task = current_task().ok_or(SysErrNo::ESRCH)?;
    let inner = task.inner.lock();
    let mut fds = inner.fd_table.lock();
    fds.dup(old_fd)
}

/// dup3 系统调用 (dup2 的现代版本)
///
/// 复制文件描述符到指定位置
/// - old_fd: 旧文件描述符
/// - new_fd: 新文件描述符
/// - flags: 标志
pub fn sys_dup3(old_fd: usize, new_fd: usize, _flags: usize) -> SyscallRet {
    log::debug!("[syscall] dup3(old_fd={}, new_fd={}, flags={})", old_fd, new_fd, _flags);

    let task = current_task().ok_or(SysErrNo::ESRCH)?;
    let inner = task.inner.lock();
    let mut fds = inner.fd_table.lock();
    fds.dup2(old_fd, new_fd)
}

pub fn sys_fcntl(fd: usize, cmd: usize, arg: usize) -> SyscallRet {
    let task = current_task().ok_or(SysErrNo::ESRCH)?;
    let inner = task.inner.lock();
    let mut fds = inner.fd_table.lock();
    match cmd {
        F_DUPFD | F_DUPFD_CLOEXEC => {
            let file_desc = fds.get(fd).cloned().ok_or(SysErrNo::EBADF)?;
            fds.alloc_from(arg, file_desc).ok_or(SysErrNo::EMFILE)
        }
        F_GETFD => {
            let _ = fds.get(fd).ok_or(SysErrNo::EBADF)?;
            Ok(0)
        }
        F_SETFD => {
            let _ = arg;
            let _ = fds.get(fd).ok_or(SysErrNo::EBADF)?;
            Ok(0)
        }
        F_GETFL => {
            let file_desc = fds.get(fd).ok_or(SysErrNo::EBADF)?;
            Ok(fd_status_flags(file_desc))
        }
        F_SETFL => {
            let _ = arg;
            let _ = fds.get(fd).ok_or(SysErrNo::EBADF)?;
            Ok(0)
        }
        _ => Err(SysErrNo::ENOSYS),
    }
}

pub fn sys_ioctl(fd: usize, _request: usize, _argp: usize) -> SyscallRet {
    let task = current_task().ok_or(SysErrNo::ESRCH)?;
    let inner = task.inner.lock();
    let fds = inner.fd_table.lock();
    let _ = fds.get(fd).ok_or(SysErrNo::EBADF)?;
    Ok(0)
}

pub fn sys_readv(fd: usize, iov: *const u8, iovcnt: usize) -> SyscallRet {
    if iov.is_null() {
        return Err(SysErrNo::EFAULT);
    }
    let mut total = 0usize;
    for i in 0..iovcnt {
        let iovec = copy_object_from_user(unsafe { (iov as *const IoVec).add(i) })?;
        if iovec.iov_len == 0 {
            continue;
        }
        let n = sys_read(fd, iovec.iov_base, iovec.iov_len)?;
        total += n;
        if n < iovec.iov_len {
            break;
        }
    }
    Ok(total)
}

pub fn sys_writev(fd: usize, iov: *const u8, iovcnt: usize) -> SyscallRet {
    if iov.is_null() {
        return Err(SysErrNo::EFAULT);
    }
    let mut total = 0usize;
    for i in 0..iovcnt {
        let iovec = copy_object_from_user(unsafe { (iov as *const IoVec).add(i) })?;
        if iovec.iov_len == 0 {
            continue;
        }
        total += sys_write(fd, iovec.iov_base as *const u8, iovec.iov_len)?;
    }
    Ok(total)
}

/// fstat 系统调用
///
/// 获取文件状态
/// - fd: 文件描述符
/// - statbuf: 状态缓冲区
pub fn sys_fstat(fd: usize, statbuf: *mut u8) -> SyscallRet {
    log::debug!("[syscall] fstat(fd={})", fd);

    if let Some(task) = current_task() {
        let inner = task.inner.lock();
        let fds = inner.fd_table.lock();
        match fds.get(fd) {
            Some(file_desc) => {
                log::debug!("[syscall] fstat: fd={} size={}", fd, file_desc.size());
                let st = stat_for_fd(file_desc);
                copy_kstat_out(statbuf, &st)?;
                Ok(0)
            }
            None => Err(SysErrNo::EBADF),
        }
    } else {
        Err(SysErrNo::ESRCH)
    }
}

pub fn sys_newfstatat(
    dirfd: isize,
    pathname: *const u8,
    statbuf: *mut u8,
    _flags: usize,
) -> SyscallRet {
    let (_logical_path, host_path) = resolve_host_path(dirfd, pathname)?;
    let st = stat_for_path(&host_path)?;
    copy_kstat_out(statbuf, &st)?;
    Ok(0)
}

/// write 系统调用的安全版本
///
/// 用于从内核态调用，buf 是内核空间指针
pub fn sys_write_kernel(fd: usize, buf: &[u8]) -> SyscallRet {
    match fd {
        FD_STDOUT | FD_STDERR => {
            for &byte in buf {
                putchar(byte);
            }
            Ok(buf.len())
        }
        _ => Err(SysErrNo::EBADF),
    }
}
