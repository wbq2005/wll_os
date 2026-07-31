use super::SyscallRet;
use crate::console::putchar;
use crate::task::wait_queue::{
    sleep_on_io_if, sleep_on_io_key_if, sleep_on_io_keys_if, WaitKey, WaitOutcome,
};
use crate::task::{current_task, SharedFdTable};
use crate::utils::error::SysErrNo;
use alloc::string::String;

use crate::fs::fd::{self, FileDescriptor};
use alloc::vec::Vec;
use lazy_static::lazy_static;
use spin::Mutex;

/// 标准文件描述符
const FD_STDOUT: usize = 1;
const FD_STDERR: usize = 2;
const AT_FDCWD: isize = -100;
const AT_EMPTY_PATH: usize = 0x1000;
const AT_NO_AUTOMOUNT: usize = 0x800;
const AT_STATX_SYNC_TYPE: usize = 0x6000;
const STATX_RESERVED: usize = 0x8000_0000;
const S_IFMT: u32 = 0o170000;
const S_IFIFO: u32 = 0o010000;
const S_IFCHR: u32 = 0o020000;
const S_IFBLK: u32 = 0o060000;
const S_IFREG: u32 = 0o100000;
const S_IFSOCK: u32 = 0o140000;

const F_DUPFD: usize = 0;
const F_GETFD: usize = 1;
const F_SETFD: usize = 2;
const F_GETFL: usize = 3;
const F_SETFL: usize = 4;
const F_GETLK: usize = 5;
const F_SETLK: usize = 6;
const F_SETLKW: usize = 7;
const F_SETLEASE: usize = 1024;
const F_GETLEASE: usize = 1025;
const F_OFD_GETLK: usize = 36;
const F_OFD_SETLK: usize = 37;
const F_OFD_SETLKW: usize = 38;
const F_DUPFD_CLOEXEC: usize = 1030;
const F_RDLCK: i16 = 0;
const F_WRLCK: i16 = 1;
const F_UNLCK: i16 = 2;
const LOCK_SH: usize = 1;
const LOCK_EX: usize = 2;
const LOCK_NB: usize = 4;
const LOCK_UN: usize = 8;
const FIONREAD: usize = 0x541B;
const FIONBIO: usize = 0x5421;
const FS_IOC_GETFLAGS: usize = 0x8008_6601;
const FS_IOC_SETFLAGS: usize = 0x4008_6602;
const LOOP_SET_FD: usize = 0x4C00;
const LOOP_CLR_FD: usize = 0x4C01;
const LOOP_SET_STATUS: usize = 0x4C02;
const LOOP_GET_STATUS: usize = 0x4C03;
const LOOP_SET_STATUS64: usize = 0x4C04;
const LOOP_GET_STATUS64: usize = 0x4C05;
const LOOP_CTL_GET_FREE: usize = 0x4C82;
const BLKSSZGET: usize = 0x1268;
const BLKGETSIZE64: usize = 0x80081272;

const FALLOC_FL_KEEP_SIZE: usize = 0x01;

const EFD_SEMAPHORE: usize = 0x01;
const EFD_NONBLOCK: usize = fd::pipe_flags::O_NONBLOCK;
const EFD_CLOEXEC: usize = fd::pipe_flags::O_CLOEXEC;

const EPOLL_CLOEXEC: usize = fd::open_flags::O_CLOEXEC as usize;
const EPOLL_CTL_ADD: usize = 1;
const EPOLL_CTL_DEL: usize = 2;
const EPOLL_CTL_MOD: usize = 3;
const EPOLLIN: u32 = 0x0001;
const EPOLLOUT: u32 = 0x0004;
const EPOLLERR: u32 = 0x0008;
const EPOLLHUP: u32 = 0x0010;
const EPOLLRDNORM: u32 = 0x0040;
const EPOLLWRNORM: u32 = 0x0100;
const EPOLL_READ_EVENTS: u32 = EPOLLIN | EPOLLRDNORM;
const EPOLL_WRITE_EVENTS: u32 = EPOLLOUT | EPOLLWRNORM;

const SPLICE_F_MOVE: usize = 0x01;
const SPLICE_F_NONBLOCK: usize = 0x02;
const SPLICE_F_MORE: usize = 0x04;
const SPLICE_F_GIFT: usize = 0x08;
const SPLICE_F_ALL: usize = SPLICE_F_MOVE | SPLICE_F_NONBLOCK | SPLICE_F_MORE | SPLICE_F_GIFT;
const SPLICE_CHUNK: usize = 64 * 1024;
const COPY_FILE_RANGE_CHUNK: usize = 64 * 1024;

const AT_SYMLINK_NOFOLLOW: usize = 0x100;
const AT_EACCESS: usize = 0x200;
const UTIME_NOW: isize = 0x3fffffff;
const UTIME_OMIT: isize = 0x3ffffffe;
const IOV_MAX: usize = 1024;
const NAME_MAX: usize = 255;
const RESOLVE_NO_XDEV: u64 = 0x01;
const RESOLVE_NO_MAGICLINKS: u64 = 0x02;
const RESOLVE_NO_SYMLINKS: u64 = 0x04;
const RESOLVE_BENEATH: u64 = 0x08;
const RESOLVE_IN_ROOT: u64 = 0x10;
const RESOLVE_KNOWN_MASK: u64 = RESOLVE_NO_XDEV
    | RESOLVE_NO_MAGICLINKS
    | RESOLVE_NO_SYMLINKS
    | RESOLVE_BENEATH
    | RESOLVE_IN_ROOT;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct OpenHow {
    flags: u64,
    mode: u64,
    resolve: u64,
}

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
struct StatxTimestamp {
    tv_sec: i64,
    tv_nsec: u32,
    __reserved: i32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct Statx {
    stx_mask: u32,
    stx_blksize: u32,
    stx_attributes: u64,
    stx_nlink: u32,
    stx_uid: u32,
    stx_gid: u32,
    stx_mode: u16,
    __spare0: u16,
    stx_ino: u64,
    stx_size: u64,
    stx_blocks: u64,
    stx_attributes_mask: u64,
    stx_atime: StatxTimestamp,
    stx_btime: StatxTimestamp,
    stx_ctime: StatxTimestamp,
    stx_mtime: StatxTimestamp,
    stx_rdev_major: u32,
    stx_rdev_minor: u32,
    stx_dev_major: u32,
    stx_dev_minor: u32,
    stx_mnt_id: u64,
    stx_dio_mem_align: u32,
    stx_dio_offset_align: u32,
    __spare3: [u64; 12],
}

#[repr(C)]
#[derive(Clone, Copy)]
struct IoVec {
    iov_base: *mut u8,
    iov_len: usize,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) struct EpollEvent {
    events: u32,
    data: u64,
}

impl EpollEvent {
    fn new(events: u32, data: u64) -> Self {
        Self { events, data }
    }

    fn events(&self) -> u32 {
        self.events
    }

    fn data(&self) -> u64 {
        self.data
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) struct PollFd {
    fd: i32,
    events: i16,
    revents: i16,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) struct TimeSpec {
    tv_sec: isize,
    tv_nsec: isize,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct StatFs {
    f_type: usize,
    f_bsize: usize,
    f_blocks: usize,
    f_bfree: usize,
    f_bavail: usize,
    f_files: usize,
    f_ffree: usize,
    f_fsid: [i32; 2],
    f_namelen: usize,
    f_frsize: usize,
    f_flags: usize,
    f_spare: [usize; 4],
}

const POLLIN: i16 = 0x0001;
const POLLOUT: i16 = 0x0004;
const POLLERR: i16 = 0x0008;
const POLLHUP: i16 = 0x0010;
const POLLNVAL: i16 = 0x0020;
const POLLRDNORM: i16 = 0x0040;
const POLLWRNORM: i16 = 0x0100;
const POLL_READ_EVENTS: i16 = POLLIN | POLLRDNORM;
const POLL_WRITE_EVENTS: i16 = POLLOUT | POLLWRNORM;
const SMALL_IO_STACK_BUF: usize = 4096;
const VECTORED_STACK_BUF: usize = 16 * 1024;
// Bound the temporary buffer while still coalescing small user iovecs.
const VECTORED_IO_CHUNK: usize = 64 * 1024;

fn read_user_cstr(ptr: *const u8) -> Result<String, SysErrNo> {
    super::user::read_cstr(ptr as usize)
}

fn read_user_path(ptr: *const u8) -> Result<String, SysErrNo> {
    super::user::read_path_cstr(ptr as usize)
}

fn copy_to_user(dst: *mut u8, src: &[u8]) -> Result<(), SysErrNo> {
    super::user::copy_to_user(dst as usize, src)
}

fn copy_from_user(src: *const u8, dst: &mut [u8]) -> Result<(), SysErrNo> {
    super::user::copy_from_user(src as usize, dst)
}

fn copy_object_from_user<T: Copy>(src: *const T) -> Result<T, SysErrNo> {
    super::user::copy_object_from_user(src as usize)
}

fn copy_object_to_user<T>(dst: *mut T, obj: &T) -> Result<(), SysErrNo> {
    super::user::copy_object_to_user(dst as usize, obj)
}

fn with_user_read_buf<T>(
    src: *const u8,
    count: usize,
    f: impl FnOnce(&[u8]) -> Result<T, SysErrNo>,
) -> Result<T, SysErrNo> {
    if count <= SMALL_IO_STACK_BUF {
        let mut stack_buf = [0u8; SMALL_IO_STACK_BUF];
        copy_from_user(src, &mut stack_buf[..count])?;
        return f(&stack_buf[..count]);
    }

    let mut kbuf = alloc::vec![0u8; count];
    copy_from_user(src, &mut kbuf)?;
    f(&kbuf)
}

fn checked_io_count(count: usize) -> Result<(), SysErrNo> {
    if count > isize::MAX as usize {
        Err(SysErrNo::EINVAL)
    } else {
        Ok(())
    }
}

fn checked_fixed_offset(offset: usize, done: usize) -> Result<usize, SysErrNo> {
    offset.checked_add(done).ok_or(SysErrNo::EFBIG)
}

// Keep the descriptor slot stable without cloning FileDescriptor: cloning pipe
// descriptors changes reader/writer refcounts. Use this only for fixed-offset
// read_at/write_at style operations; offset-advancing and blocking paths need
// their own lock/sleep boundaries.
fn with_fixed_io_fd_mut<T>(
    fd: usize,
    f: impl FnOnce(&mut FileDescriptor) -> Result<T, SysErrNo>,
) -> Result<T, SysErrNo> {
    let task = current_task().ok_or(SysErrNo::ESRCH)?;
    let fd_table = task.inner.lock().fd_table.clone();
    let mut fds = fd_table.lock();
    let file_desc = fds.get_mut(fd).ok_or(SysErrNo::EBADF)?;
    f(file_desc)
}

fn load_user_iovecs_batch(
    iov: *const u8,
    start: usize,
    iovcnt: usize,
    max_total_len: usize,
) -> Result<(Vec<IoVec>, usize, usize), SysErrNo> {
    if start >= iovcnt {
        return Ok((Vec::new(), start, 0));
    }

    let mut iovecs = Vec::new();
    let mut total_len = 0usize;
    let mut index = start;
    while index < iovcnt {
        let iovec = copy_object_from_user(unsafe { (iov as *const IoVec).add(index) })?;
        index += 1;
        if iovec.iov_len == 0 {
            continue;
        }
        total_len = total_len
            .checked_add(iovec.iov_len)
            .filter(|sum| *sum <= max_total_len)
            .ok_or(SysErrNo::EINVAL)?;
        iovecs.push(iovec);
        if total_len >= VECTORED_IO_CHUNK {
            break;
        }
    }

    Ok((iovecs, index, total_len))
}

fn user_iov_addr(iovec: &IoVec, offset: usize) -> Result<usize, SysErrNo> {
    (iovec.iov_base as usize)
        .checked_add(offset)
        .ok_or(SysErrNo::EFAULT)
}

#[derive(Clone, Copy)]
struct IovCursor {
    index: usize,
    offset: usize,
}

impl IovCursor {
    fn new() -> Self {
        Self {
            index: 0,
            offset: 0,
        }
    }

    fn remaining_span(&mut self, iovecs: &[IoVec]) -> Option<usize> {
        while self.index < iovecs.len() {
            if self.offset < iovecs[self.index].iov_len {
                return Some(iovecs[self.index].iov_len - self.offset);
            }
            self.index += 1;
            self.offset = 0;
        }
        None
    }

    fn user_addr(&self, iovecs: &[IoVec]) -> Result<usize, SysErrNo> {
        user_iov_addr(&iovecs[self.index], self.offset)
    }

    fn advance(&mut self, n: usize) {
        self.offset += n;
    }

    fn remaining_bytes_capped(&self, iovecs: &[IoVec], cap: usize) -> usize {
        let mut scan = *self;
        let mut total = 0usize;
        while total < cap {
            let Some(remaining) = scan.remaining_span(iovecs) else {
                break;
            };
            let n = remaining.min(cap - total);
            total += n;
            scan.advance(n);
        }
        total
    }
}

fn read_fixed_at_into_kernel(
    file_desc: &mut FileDescriptor,
    offset: usize,
    buf: &mut [u8],
) -> SyscallRet {
    super::with_kernel_page_table(|| file_desc.read_at(offset, buf))
}

fn write_fixed_at_from_kernel(
    file_desc: &mut FileDescriptor,
    offset: usize,
    buf: &[u8],
) -> SyscallRet {
    super::with_kernel_page_table(|| file_desc.write_at(offset, buf))
}

fn read_fd_into_kernel(file_desc: &mut FileDescriptor, buf: &mut [u8]) -> SyscallRet {
    if file_desc.is_pipe_read() {
        file_desc.read(buf)
    } else {
        super::with_kernel_page_table(|| file_desc.read(buf))
    }
}

fn write_fd_from_kernel(file_desc: &mut FileDescriptor, buf: &[u8]) -> SyscallRet {
    if file_desc.is_pipe_write() {
        file_desc.write(buf)
    } else {
        super::with_kernel_page_table(|| file_desc.write(buf))
    }
}

fn is_dev_null_name(name: &str) -> bool {
    matches!(name, "/dev/null" | "/glibc/dev/null" | "/musl/dev/null")
}

fn is_dev_zero_name(name: &str) -> bool {
    matches!(name, "/dev/zero" | "/glibc/dev/zero" | "/musl/dev/zero")
}

fn direct_read_fd_to_user(
    file_desc: &mut FileDescriptor,
    dst: *mut u8,
    count: usize,
) -> Result<Option<usize>, SysErrNo> {
    match file_desc {
        FileDescriptor::MemFile {
            name,
            readable,
            offset,
            ..
        } if is_dev_null_name(name) || is_dev_zero_name(name) => {
            if !*readable {
                return Err(SysErrNo::EBADF);
            }
            if is_dev_null_name(name) {
                return Ok(Some(0));
            }
            super::user::clear_user(dst as usize, count)?;
            *offset = offset.saturating_add(count);
            return Ok(Some(count));
        }
        _ => return Ok(None),
    }
}

fn direct_read_at_fd_to_user(
    file_desc: &mut FileDescriptor,
    dst: *mut u8,
    count: usize,
    _offset: usize,
) -> Result<Option<usize>, SysErrNo> {
    match file_desc {
        FileDescriptor::MemFile { name, readable, .. }
            if is_dev_null_name(name) || is_dev_zero_name(name) =>
        {
            if !*readable {
                return Err(SysErrNo::EBADF);
            }
            if is_dev_null_name(name) {
                return Ok(Some(0));
            }
            super::user::clear_user(dst as usize, count)?;
            return Ok(Some(count));
        }
        _ => return Ok(None),
    }
}

fn direct_write_fd_from_user(
    file_desc: &mut FileDescriptor,
    src: *const u8,
    count: usize,
) -> Result<Option<usize>, SysErrNo> {
    match file_desc {
        FileDescriptor::MemFile { name, writable, .. }
            if is_dev_null_name(name) || is_dev_zero_name(name) =>
        {
            if !*writable {
                return Err(SysErrNo::EBADF);
            }
            super::user::check_user_readable(src as usize, count)?;
            return Ok(Some(count));
        }
        _ => return Ok(None),
    }
}

fn direct_write_at_fd_from_user(
    file_desc: &mut FileDescriptor,
    src: *const u8,
    count: usize,
    _offset: usize,
) -> Result<Option<usize>, SysErrNo> {
    match file_desc {
        FileDescriptor::MemFile { name, writable, .. }
            if is_dev_null_name(name) || is_dev_zero_name(name) =>
        {
            if !*writable {
                return Err(SysErrNo::EBADF);
            }
            super::user::check_user_readable(src as usize, count)?;
            return Ok(Some(count));
        }
        _ => return Ok(None),
    }
}

fn wait_read_after_eagain(fd_table: &SharedFdTable, fd: usize) -> Result<bool, SysErrNo> {
    let (wait_key, deadline) = {
        let fds = fd_table.lock();
        let file_desc = fds.get(fd).ok_or(SysErrNo::EBADF)?;
        if file_desc.is_pipe_read() {
            if file_desc.pipe_read_nonblocking() {
                return Err(SysErrNo::EAGAIN);
            }
            if !file_desc.pipe_read_would_block() {
                return Ok(false);
            }
            (
                file_desc.pipe_read_wait_key().ok_or(SysErrNo::EBADF)?,
                None,
            )
        } else if let Some(wait_key) = file_desc.eventfd_read_wait_key() {
            if file_desc.eventfd_read_nonblocking() {
                return Err(SysErrNo::EAGAIN);
            }
            if !file_desc.eventfd_read_would_block() {
                return Ok(false);
            }
            (wait_key, None)
        } else if let Some(wait_key) = file_desc.socket_read_wait_key() {
            if file_desc.socket_read_nonblocking() {
                return Err(SysErrNo::EAGAIN);
            }
            if !file_desc.socket_read_would_block() {
                return Ok(false);
            }
            let deadline = file_desc
                .socket_recv_timeout_us()
                .map(|timeout| crate::timer::get_time_us().saturating_add(timeout));
            (wait_key, deadline)
        } else {
            return Err(SysErrNo::EAGAIN);
        }
    };
    let outcome = sleep_on_io_key_if(wait_key, deadline, || {
        let fds = fd_table.lock();
        let file_desc = fds.get(fd).ok_or(SysErrNo::EBADF)?;
        if file_desc.is_pipe_read() {
            if file_desc.pipe_read_wait_key() != Some(wait_key) {
                return Ok(false);
            }
            Ok(file_desc.pipe_read_would_block())
        } else if file_desc.eventfd_read_wait_key() == Some(wait_key) {
            Ok(file_desc.eventfd_read_would_block())
        } else if file_desc.socket_read_wait_key() == Some(wait_key) {
            Ok(file_desc.socket_read_would_block())
        } else {
            Ok(false)
        }
    })?;
    if outcome == WaitOutcome::TimedOut {
        return Err(SysErrNo::EAGAIN);
    }
    Ok(true)
}

fn wait_write_after_eagain(fd_table: &SharedFdTable, fd: usize) -> Result<bool, SysErrNo> {
    let wait_key = {
        let fds = fd_table.lock();
        let file_desc = fds.get(fd).ok_or(SysErrNo::EBADF)?;
        if file_desc.is_pipe_write() {
            if file_desc.pipe_write_nonblocking() {
                return Err(SysErrNo::EAGAIN);
            }
            if !file_desc.pipe_write_would_block() {
                return Ok(false);
            }
            file_desc.pipe_write_wait_key().ok_or(SysErrNo::EBADF)?
        } else if let Some(wait_key) = file_desc.eventfd_write_wait_key() {
            if file_desc.eventfd_write_nonblocking() {
                return Err(SysErrNo::EAGAIN);
            }
            if !file_desc.eventfd_write_would_block() {
                return Ok(false);
            }
            wait_key
        } else {
            return Err(SysErrNo::EAGAIN);
        }
    };
    let _ = sleep_on_io_key_if(wait_key, None, || {
        let fds = fd_table.lock();
        let file_desc = fds.get(fd).ok_or(SysErrNo::EBADF)?;
        if file_desc.is_pipe_write() {
            if file_desc.pipe_write_wait_key() != Some(wait_key) {
                return Ok(false);
            }
            Ok(file_desc.pipe_write_would_block())
        } else if file_desc.eventfd_write_wait_key() == Some(wait_key) {
            Ok(file_desc.eventfd_write_would_block())
        } else {
            Ok(false)
        }
    })?;
    Ok(true)
}

fn vectored_read_at_to_user(fd: usize, iovecs: &[IoVec], offset: usize) -> SyscallRet {
    let mut total = 0usize;
    let mut cursor = IovCursor::new();
    let mut kbuf = Vec::new();

    while cursor.index < iovecs.len() {
        let mut want = 0usize;
        let mut scan = cursor;
        while want < VECTORED_IO_CHUNK {
            let Some(remaining) = scan.remaining_span(iovecs) else {
                break;
            };
            let n = remaining.min(VECTORED_IO_CHUNK - want);
            want += n;
            scan.advance(n);
        }
        if want == 0 {
            break;
        }

        let fixed_offset = checked_fixed_offset(offset, total)?;
        kbuf.resize(want, 0);
        let n = with_fixed_io_fd_mut(fd, |file_desc| {
            read_fixed_at_into_kernel(file_desc, fixed_offset, &mut kbuf[..want])
        })?;
        if n == 0 {
            break;
        }

        let mut copied = 0usize;
        while copied < n {
            let Some(remaining) = cursor.remaining_span(iovecs) else {
                break;
            };
            let to_copy = remaining.min(n - copied);
            let dst = match cursor.user_addr(iovecs) {
                Ok(addr) => addr as *mut u8,
                Err(err) => {
                    return if total + copied != 0 {
                        Ok(total + copied)
                    } else {
                        Err(err)
                    };
                }
            };
            match copy_to_user(dst, &kbuf[copied..copied + to_copy]) {
                Ok(()) => {
                    copied += to_copy;
                    cursor.advance(to_copy);
                }
                Err(err) => {
                    return if total + copied != 0 {
                        Ok(total + copied)
                    } else {
                        Err(err)
                    };
                }
            }
        }

        total += copied;
        if n < want || copied < n {
            break;
        }
    }

    Ok(total)
}

fn vectored_write_at_from_user(
    file_desc: &mut FileDescriptor,
    iovecs: &[IoVec],
    offset: usize,
) -> SyscallRet {
    let mut total = 0usize;
    let mut cursor = IovCursor::new();
    let mut heap_buf = Vec::new();

    while cursor.index < iovecs.len() {
        let remaining = cursor.remaining_bytes_capped(iovecs, VECTORED_STACK_BUF + 1);
        if remaining == 0 {
            break;
        }
        let fixed_offset = checked_fixed_offset(offset, total)?;
        let mut stack_buf;
        let (copied, pending_user_error, write_buf) = if remaining <= VECTORED_STACK_BUF {
            stack_buf = [0u8; VECTORED_STACK_BUF];
            let (copied, err) =
                fill_vectored_write_buf(iovecs, &mut cursor, &mut stack_buf[..remaining]);
            (copied, err, &stack_buf[..copied])
        } else {
            heap_buf.resize(VECTORED_IO_CHUNK, 0);
            let (copied, err) = fill_vectored_write_buf(iovecs, &mut cursor, &mut heap_buf);
            (copied, err, &heap_buf[..copied])
        };

        if copied == 0 {
            return match pending_user_error {
                Some(err) if total == 0 => Err(err),
                _ => Ok(total),
            };
        }

        match write_fixed_at_from_kernel(file_desc, fixed_offset, write_buf) {
            Ok(n) => {
                total += n;
                if n < copied {
                    return Ok(total);
                }
            }
            Err(err) => {
                return if total != 0 { Ok(total) } else { Err(err) };
            }
        }

        if pending_user_error.is_some() {
            return Ok(total);
        }
    }

    Ok(total)
}

fn vectored_read_to_user(fd: usize, iovecs: &[IoVec]) -> SyscallRet {
    let task = current_task().ok_or(SysErrNo::ESRCH)?;
    let fd_table = task.inner.lock().fd_table.clone();
    let mut total = 0usize;
    let mut cursor = IovCursor::new();
    let mut kbuf = Vec::new();

    while cursor.index < iovecs.len() {
        let want = cursor.remaining_bytes_capped(iovecs, VECTORED_IO_CHUNK);
        if want == 0 {
            break;
        }

        kbuf.resize(want, 0);
        let res = {
            let mut fds = fd_table.lock();
            match fds.get_mut(fd) {
                Some(file_desc) => read_fd_into_kernel(file_desc, &mut kbuf[..want]),
                None => Err(SysErrNo::EBADF),
            }
        };

        match res {
            Ok(n) => {
                if n == 0 {
                    break;
                }

                let mut copied = 0usize;
                while copied < n {
                    let Some(remaining) = cursor.remaining_span(iovecs) else {
                        break;
                    };
                    let to_copy = remaining.min(n - copied);
                    let dst = match cursor.user_addr(iovecs) {
                        Ok(addr) => addr as *mut u8,
                        Err(err) => {
                            return if total + copied != 0 {
                                Ok(total + copied)
                            } else {
                                Err(err)
                            };
                        }
                    };
                    match copy_to_user(dst, &kbuf[copied..copied + to_copy]) {
                        Ok(()) => {
                            copied += to_copy;
                            cursor.advance(to_copy);
                        }
                        Err(err) => {
                            return if total + copied != 0 {
                                Ok(total + copied)
                            } else {
                                Err(err)
                            };
                        }
                    }
                }

                total += copied;
                if n < want || copied < n {
                    break;
                }
            }
            Err(SysErrNo::EAGAIN) => {
                if total != 0 {
                    return Ok(total);
                }
                if !wait_read_after_eagain(&fd_table, fd)? {
                    continue;
                }
            }
            Err(err) => {
                return if total != 0 { Ok(total) } else { Err(err) };
            }
        }
    }

    Ok(total)
}

fn vectored_write_from_user(fd: usize, iovecs: &[IoVec]) -> SyscallRet {
    let task = current_task().ok_or(SysErrNo::ESRCH)?;
    let fd_table = task.inner.lock().fd_table.clone();
    let mut total = 0usize;
    let mut cursor = IovCursor::new();
    let mut heap_buf = Vec::new();

    while cursor.index < iovecs.len() {
        let remaining = cursor.remaining_bytes_capped(iovecs, VECTORED_STACK_BUF + 1);
        if remaining == 0 {
            break;
        }

        let mut stack_buf;
        let (copied, pending_user_error, write_buf) = if remaining <= VECTORED_STACK_BUF {
            stack_buf = [0u8; VECTORED_STACK_BUF];
            let (copied, err) =
                fill_vectored_write_buf(iovecs, &mut cursor, &mut stack_buf[..remaining]);
            (copied, err, &stack_buf[..copied])
        } else {
            heap_buf.resize(VECTORED_IO_CHUNK, 0);
            let (copied, err) = fill_vectored_write_buf(iovecs, &mut cursor, &mut heap_buf);
            (copied, err, &heap_buf[..copied])
        };

        if copied == 0 {
            return match pending_user_error {
                Some(err) if total == 0 => Err(err),
                _ => Ok(total),
            };
        }

        loop {
            let res = {
                let mut fds = fd_table.lock();
                match fds.get_mut(fd) {
                    Some(file_desc) => write_fd_from_kernel(file_desc, write_buf),
                    None => Err(SysErrNo::EBADF),
                }
            };

            match res {
                Ok(n) => {
                    total += n;
                    if n < copied {
                        return Ok(total);
                    }
                    break;
                }
                Err(SysErrNo::EAGAIN) => {
                    if total != 0 {
                        return Ok(total);
                    }
                    match wait_write_after_eagain(&fd_table, fd) {
                        Ok(false) => continue,
                        Ok(_) => {}
                        Err(SysErrNo::EINTR) if total != 0 => return Ok(total),
                        Err(err) => return Err(err),
                    }
                }
                Err(SysErrNo::EPIPE) if total == 0 => {
                    crate::syscall::signal::send_sigpipe_to_current();
                    return Err(SysErrNo::EPIPE);
                }
                Err(err) => {
                    return if total != 0 { Ok(total) } else { Err(err) };
                }
            }
        }

        if pending_user_error.is_some() {
            return Ok(total);
        }
    }

    Ok(total)
}

fn fill_vectored_write_buf(
    iovecs: &[IoVec],
    cursor: &mut IovCursor,
    kbuf: &mut [u8],
) -> (usize, Option<SysErrNo>) {
    let mut copied = 0usize;
    while copied < kbuf.len() {
        let Some(remaining) = cursor.remaining_span(iovecs) else {
            break;
        };
        let to_copy = remaining.min(kbuf.len() - copied);
        let src = match cursor.user_addr(iovecs) {
            Ok(addr) => addr as *const u8,
            Err(err) => return (copied, Some(err)),
        };
        match copy_from_user(src, &mut kbuf[copied..copied + to_copy]) {
            Ok(()) => {
                copied += to_copy;
                cursor.advance(to_copy);
            }
            Err(err) => return (copied, Some(err)),
        }
    }
    (copied, None)
}

fn duration_us_from_timespec(ts: TimeSpec) -> Result<usize, SysErrNo> {
    if ts.tv_sec < 0 || ts.tv_nsec < 0 || ts.tv_nsec >= 1_000_000_000 {
        return Err(SysErrNo::EINVAL);
    }
    let tv_sec = ts.tv_sec as usize;
    let tv_nsec = ts.tv_nsec as usize;
    Ok(tv_sec
        .saturating_mul(1_000_000)
        .saturating_add(tv_nsec.div_ceil(1000)))
}

fn deadline_from_timespec_ptr(ptr: usize) -> Result<Option<usize>, SysErrNo> {
    if ptr == 0 {
        return Ok(None);
    }
    let duration_us =
        duration_us_from_timespec(super::user::copy_object_from_user::<TimeSpec>(ptr)?)?;
    Ok(Some(crate::timer::deadline_after_us(duration_us)))
}

fn current_time_pair() -> (isize, isize) {
    let (sec, usec) = crate::timer::get_timeval();
    (sec as isize, (usec * 1000) as isize)
}

fn parse_utimens_times(
    times: *const TimeSpec,
) -> Result<(Option<(isize, isize)>, Option<(isize, isize)>), SysErrNo> {
    if times.is_null() {
        let now = current_time_pair();
        return Ok((Some(now), Some(now)));
    }

    let atime = copy_object_from_user(times)?;
    let mtime = copy_object_from_user(unsafe { times.add(1) })?;

    fn convert(ts: TimeSpec) -> Result<Option<(isize, isize)>, SysErrNo> {
        match ts.tv_nsec {
            UTIME_OMIT => Ok(None),
            UTIME_NOW => Ok(Some(current_time_pair())),
            nsec if (0..1_000_000_000).contains(&nsec) && ts.tv_sec >= 0 => {
                Ok(Some((ts.tv_sec, nsec)))
            }
            _ => Err(SysErrNo::EINVAL),
        }
    }

    Ok((convert(atime)?, convert(mtime)?))
}

fn parse_utimens_times_with_permission(
    times: *const TimeSpec,
) -> Result<
    (
        Option<(isize, isize)>,
        Option<(isize, isize)>,
        crate::fs::TimesUpdatePermission,
    ),
    SysErrNo,
> {
    if times.is_null() {
        let now = current_time_pair();
        return Ok((
            Some(now),
            Some(now),
            crate::fs::TimesUpdatePermission::Current,
        ));
    }

    let atime = copy_object_from_user(times)?;
    let mtime = copy_object_from_user(unsafe { times.add(1) })?;

    fn convert(ts: TimeSpec) -> Result<(Option<(isize, isize)>, bool), SysErrNo> {
        match ts.tv_nsec {
            UTIME_OMIT => Ok((None, false)),
            UTIME_NOW => Ok((Some(current_time_pair()), false)),
            nsec if (0..1_000_000_000).contains(&nsec) && ts.tv_sec >= 0 => {
                Ok((Some((ts.tv_sec, nsec)), true))
            }
            _ => Err(SysErrNo::EINVAL),
        }
    }

    let (atime, atime_explicit) = convert(atime)?;
    let (mtime, mtime_explicit) = convert(mtime)?;
    let permission = if atime.is_none() && mtime.is_none() {
        crate::fs::TimesUpdatePermission::None
    } else if atime_explicit || mtime_explicit {
        crate::fs::TimesUpdatePermission::Explicit
    } else {
        crate::fs::TimesUpdatePermission::Current
    };
    Ok((atime, mtime, permission))
}

fn resolve_path_str(dirfd: isize, path: &str) -> Result<String, SysErrNo> {
    if path.is_empty() {
        return Err(SysErrNo::ENOENT);
    }
    check_path_component_lengths(path)?;
    if path.starts_with('/') {
        return Ok(crate::fs::normalize_path(path));
    }

    let base = resolve_base_dir(dirfd)?;
    Ok(crate::fs::resolve_path(&base, path))
}

fn check_path_component_lengths(path: &str) -> Result<(), SysErrNo> {
    for part in path.split('/') {
        if matches!(part, "" | "." | "..") {
            continue;
        }
        if part.as_bytes().len() > NAME_MAX {
            return Err(SysErrNo::ENAMETOOLONG);
        }
    }
    Ok(())
}

fn resolve_path(dirfd: isize, pathname: *const u8) -> Result<String, SysErrNo> {
    let path = read_user_path(pathname)?;
    resolve_path_str(dirfd, &path)
}

fn current_root() -> Result<String, SysErrNo> {
    let task = current_task().ok_or(SysErrNo::ESRCH)?;
    let root = task.fs.lock().root.clone();
    Ok(root)
}

fn current_cwd() -> Result<String, SysErrNo> {
    let task = current_task().ok_or(SysErrNo::ESRCH)?;
    let cwd = task.fs.lock().cwd.clone();
    Ok(cwd)
}

fn resolve_host_path_str(dirfd: isize, path: &str) -> Result<(String, String), SysErrNo> {
    let logical = resolve_path_str(dirfd, path)?;
    let root = current_root()?;
    let host = crate::fs::apply_root(&root, &logical);
    Ok((logical, host))
}

fn resolve_host_path(dirfd: isize, pathname: *const u8) -> Result<(String, String), SysErrNo> {
    let logical = resolve_path(dirfd, pathname)?;
    let root = current_root()?;
    let host = crate::fs::apply_root(&root, &logical);
    Ok((logical, host))
}

fn path_has_parent_component(path: &str) -> bool {
    path.split('/').any(|component| component == "..")
}

fn path_is_under_or_same(path: &str, base: &str) -> bool {
    let path = crate::fs::normalize_path(path);
    let base = crate::fs::normalize_path(base);
    path == base
        || (base != "/"
            && path
                .strip_prefix(base.as_str())
                .is_some_and(|tail| tail.starts_with('/')))
}

fn resolve_path_in_root_str(dirfd: isize, path: &str) -> Result<String, SysErrNo> {
    if path.is_empty() {
        return Err(SysErrNo::ENOENT);
    }
    check_path_component_lengths(path)?;
    let base = resolve_base_dir(dirfd)?;
    let mut components: Vec<String> = base
        .trim_matches('/')
        .split('/')
        .filter(|component| !component.is_empty())
        .map(String::from)
        .collect();
    let root_len = components.len();
    for component in path.split('/') {
        match component {
            "" | "." => {}
            ".." => {
                if components.len() > root_len {
                    components.pop();
                }
            }
            name => components.push(String::from(name)),
        }
    }
    if components.is_empty() {
        Ok(String::from("/"))
    } else {
        Ok(crate::fs::normalize_path(&alloc::format!(
            "/{}",
            components.join("/")
        )))
    }
}

fn openat2_resolve_path_str(
    dirfd: isize,
    path: &str,
    resolve: u64,
) -> Result<(String, String), SysErrNo> {
    if resolve & RESOLVE_BENEATH != 0 && (path.starts_with('/') || path_has_parent_component(path))
    {
        return Err(SysErrNo::EXDEV);
    }

    let logical = if resolve & RESOLVE_IN_ROOT != 0 {
        resolve_path_in_root_str(dirfd, path)?
    } else {
        resolve_path_str(dirfd, path)?
    };

    if resolve & RESOLVE_BENEATH != 0 {
        let base = resolve_base_dir(dirfd)?;
        if !path_is_under_or_same(&logical, &base) {
            return Err(SysErrNo::EXDEV);
        }
    }

    let root = current_root()?;
    let host = crate::fs::apply_root(&root, &logical);

    if (resolve & (RESOLVE_NO_MAGICLINKS | RESOLVE_NO_SYMLINKS)) != 0
        && is_proc_magic_link_path(&logical)
    {
        return Err(SysErrNo::ELOOP);
    }

    if resolve & RESOLVE_NO_XDEV != 0 && crate::fs::path_crosses_mountpoint(&host) {
        return Err(SysErrNo::EXDEV);
    }

    if resolve & RESOLVE_NO_SYMLINKS != 0
        && super::with_kernel_page_table(|| crate::fs::path_contains_symlink(&host))?
    {
        return Err(SysErrNo::ELOOP);
    }

    Ok((logical, host))
}

fn copy_open_how_from_user(how: *const OpenHow, size: usize) -> Result<OpenHow, SysErrNo> {
    let expected = core::mem::size_of::<OpenHow>();
    if size < expected {
        return Err(SysErrNo::EINVAL);
    }
    let value = copy_object_from_user(how)?;
    if size > expected {
        let extra_len = size - expected;
        if extra_len > 4096 {
            return Err(SysErrNo::E2BIG);
        }
        let extra_addr = (how as usize)
            .checked_add(expected)
            .ok_or(SysErrNo::EFAULT)?;
        let mut extra = Vec::new();
        extra.resize(extra_len, 0);
        super::user::copy_from_user(extra_addr, &mut extra)?;
        if extra.iter().any(|byte| *byte != 0) {
            return Err(SysErrNo::E2BIG);
        }
    }
    Ok(value)
}

fn validate_openat2_how(how: OpenHow) -> Result<(u32, u32, u64), SysErrNo> {
    const O_NOCTTY: u64 = 0o00000400;
    const O_NONBLOCK: u64 = 0o00004000;
    const O_DSYNC: u64 = 0o00010000;
    const O_ASYNC: u64 = 0o00020000;
    const O_DIRECT: u64 = 0o00040000;
    const O_LARGEFILE: u64 = 0o00100000;
    const O_SYNC: u64 = 0o04010000;
    let known_flags = fd::open_flags::O_ACCMODE as u64
        | fd::open_flags::O_CREAT as u64
        | fd::open_flags::O_EXCL as u64
        | O_NOCTTY
        | fd::open_flags::O_TRUNC as u64
        | fd::open_flags::O_APPEND as u64
        | O_NONBLOCK
        | O_DSYNC
        | O_ASYNC
        | O_DIRECT
        | O_LARGEFILE
        | fd::open_flags::O_DIRECTORY as u64
        | fd::open_flags::O_NOFOLLOW as u64
        | fd::open_flags::O_NOATIME as u64
        | fd::open_flags::O_CLOEXEC as u64
        | O_SYNC
        | fd::open_flags::O_PATH as u64
        | fd::open_flags::O_TMPFILE as u64;
    if how.flags & !known_flags != 0 {
        return Err(SysErrNo::EINVAL);
    }

    let tmpfile_bit = fd::open_flags::O_TMPFILE as u64 & !(fd::open_flags::O_DIRECTORY as u64);
    if (how.flags & tmpfile_bit) != 0
        && (how.flags & fd::open_flags::O_TMPFILE as u64) != fd::open_flags::O_TMPFILE as u64
    {
        return Err(SysErrNo::EINVAL);
    }

    if how.mode & !0o7777 != 0 {
        return Err(SysErrNo::EINVAL);
    }
    if how.mode != 0
        && (how.flags & (fd::open_flags::O_CREAT as u64 | fd::open_flags::O_TMPFILE as u64)) == 0
    {
        return Err(SysErrNo::EINVAL);
    }
    if how.resolve & !RESOLVE_KNOWN_MASK != 0 {
        return Err(SysErrNo::EINVAL);
    }
    if (how.resolve & RESOLVE_BENEATH) != 0 && (how.resolve & RESOLVE_IN_ROOT) != 0 {
        return Err(SysErrNo::EINVAL);
    }

    Ok((how.flags as u32, how.mode as u32, how.resolve))
}

fn parse_decimal_fd(text: &str) -> Result<usize, SysErrNo> {
    if text.is_empty() || !text.as_bytes().iter().all(|b| b.is_ascii_digit()) {
        return Err(SysErrNo::ENOENT);
    }

    let mut value = 0usize;
    for &byte in text.as_bytes() {
        value = value
            .checked_mul(10)
            .and_then(|n| n.checked_add((byte - b'0') as usize))
            .ok_or(SysErrNo::ENOENT)?;
    }
    Ok(value)
}

fn proc_self_fd_number(logical_path: &str) -> Result<Option<usize>, SysErrNo> {
    for prefix in ["/proc/self/fd/", "/proc/thread-self/fd/"] {
        if let Some(tail) = logical_path.strip_prefix(prefix) {
            if tail.contains('/') {
                return Ok(None);
            }
            return parse_decimal_fd(tail).map(Some);
        }
    }
    Ok(None)
}

fn proc_self_fd_target(logical_path: &str) -> Result<Option<String>, SysErrNo> {
    let Some(fd) = proc_self_fd_number(logical_path)? else {
        return Ok(None);
    };
    let task = current_task().ok_or(SysErrNo::ESRCH)?;
    let inner = task.inner.lock();
    let fds = inner.fd_table.lock();
    let file_desc = fds.get(fd).ok_or(SysErrNo::EBADF)?;
    let target = match file_desc {
        FileDescriptor::MemFile { name, linked, .. } => {
            if *linked {
                name.clone()
            } else {
                alloc::format!("{} (deleted)", name)
            }
        }
        FileDescriptor::MemDir { path, .. } => path.clone(),
        FileDescriptor::Ext4Regular { ino, .. } => alloc::format!("anon_inode:[ext4:{}]", ino),
        FileDescriptor::Ext4Dir { path, .. } => path.clone(),
        FileDescriptor::Path { logical_path, .. } => logical_path.clone(),
        FileDescriptor::Stdin => String::from("/dev/stdin"),
        FileDescriptor::Stdout => String::from("/dev/stdout"),
        FileDescriptor::Stderr => String::from("/dev/stderr"),
        FileDescriptor::LoopControl => String::from("/dev/loop-control"),
        FileDescriptor::LoopDevice { index, .. } => alloc::format!("/dev/loop{}", index),
        FileDescriptor::PipeRead { .. } | FileDescriptor::PipeWrite { .. } => {
            String::from("pipe:[0]")
        }
        FileDescriptor::Socket { .. } => String::from("socket:[0]"),
        FileDescriptor::EventFd { .. } => String::from("anon_inode:[eventfd]"),
        FileDescriptor::Epoll { .. } => String::from("anon_inode:[eventpoll]"),
    };
    Ok(Some(target))
}

fn local_pseudo_path(path: &str) -> &str {
    for root in ["/glibc", "/musl"] {
        if let Some(tail) = path.strip_prefix(root) {
            return if tail.is_empty() {
                "/"
            } else if tail.starts_with('/') {
                tail
            } else {
                path
            };
        }
    }
    path
}

fn is_proc_magic_link_path(path: &str) -> bool {
    matches!(
        local_pseudo_path(path),
        "/proc/self/exe" | "/proc/thread-self/exe"
    )
}

fn proc_magic_link_target(logical_path: &str) -> Result<Option<String>, SysErrNo> {
    if !is_proc_magic_link_path(logical_path) {
        return Ok(None);
    }
    let task = current_task().ok_or(SysErrNo::ESRCH)?;
    let exec_path = task.inner.lock().exec_path.clone();
    if exec_path.is_empty() {
        return Err(SysErrNo::ENOENT);
    }
    Ok(Some(exec_path))
}

fn resolve_base_dir(dirfd: isize) -> Result<String, SysErrNo> {
    let task = current_task().ok_or(SysErrNo::ESRCH)?;
    if dirfd == AT_FDCWD {
        return Ok(task.fs.lock().cwd.clone());
    }
    let inner = task.inner.lock();
    let dirfd = usize::try_from(dirfd).map_err(|_| SysErrNo::EBADF)?;
    let result = match inner.fd_table.lock().get(dirfd) {
        Some(FileDescriptor::MemDir { path, .. }) => Ok(path.clone()),
        Some(FileDescriptor::Ext4Dir { path, .. }) => Ok(path.clone()),
        Some(FileDescriptor::Path {
            logical_path,
            kind: crate::fs::VfsNodeKind::Directory,
            ..
        }) => Ok(logical_path.clone()),
        Some(_) => Err(SysErrNo::ENOTDIR),
        None => Err(SysErrNo::EBADF),
    };
    result
}

fn regular_blocks(size: usize) -> u64 {
    size.div_ceil(512) as u64
}

fn encode_dev(major: u32, minor: u32) -> u64 {
    ((minor & 0xff) | ((major & 0xfff) << 8) | ((minor & !0xff) << 12)) as u64
}

fn decode_dev(dev: usize) -> Result<(u32, u32), SysErrNo> {
    let dev = dev as u64;
    let major = ((dev >> 8) & 0xfff) | ((dev >> 32) & !0xfff);
    let minor = (dev & 0xff) | ((dev >> 12) & !0xff);
    if major > u32::MAX as u64 || minor > u32::MAX as u64 {
        return Err(SysErrNo::EINVAL);
    }
    Ok((major as u32, minor as u32))
}

fn kstat_from_vfs(meta: crate::fs::VfsMetadata) -> KStat {
    let st_dev = if let Some((major, minor)) = crate::fs::block_dev::root_device_numbers() {
        encode_dev(major, minor)
    } else {
        // Linux reserves device 0 for nodes not associated with a device.
        // Give the unified in-memory root a stable anonymous device identity so
        // mount walkers can match a pathname to its root mount.
        encode_dev(0, 1)
    };
    KStat {
        st_dev,
        st_ino: meta.ino,
        st_mode: meta.mode,
        st_nlink: meta.nlink,
        st_uid: meta.uid,
        st_gid: meta.gid,
        st_rdev: encode_dev(meta.rdev_major, meta.rdev_minor),
        __pad: 0,
        st_size: meta.size as isize,
        st_blksize: 4096,
        __pad2: 0,
        st_blocks: meta.blocks.max(regular_blocks(meta.size as usize)),
        st_atime_sec: meta.atime_sec,
        st_atime_nsec: meta.atime_nsec,
        st_mtime_sec: meta.mtime_sec,
        st_mtime_nsec: meta.mtime_nsec,
        st_ctime_sec: meta.ctime_sec,
        st_ctime_nsec: meta.ctime_nsec,
        __unused: [0; 2],
    }
}

fn stat_for_fd(file_desc: &FileDescriptor) -> Result<KStat, SysErrNo> {
    crate::fs::metadata_for_fd(file_desc).map(kstat_from_vfs)
}

fn stat_for_path(path: &str, follow_symlink: bool) -> Result<KStat, SysErrNo> {
    crate::fs::metadata_for_lookup(path, follow_symlink).map(kstat_from_vfs)
}

fn stat_empty_path(dirfd: isize) -> Result<KStat, SysErrNo> {
    if dirfd == AT_FDCWD {
        let task = current_task().ok_or(SysErrNo::ESRCH)?;
        let (root, cwd) = {
            let fs = task.fs.lock();
            (fs.root.clone(), fs.cwd.clone())
        };
        let host_path = crate::fs::apply_root(&root, &cwd);
        return super::with_kernel_page_table(|| stat_for_path(&host_path, true));
    }

    let fd = usize::try_from(dirfd).map_err(|_| SysErrNo::EBADF)?;
    let task = current_task().ok_or(SysErrNo::ESRCH)?;
    let inner = task.inner.lock();
    let fds = inner.fd_table.lock();
    let file_desc = fds.get(fd).ok_or(SysErrNo::EBADF)?;
    super::with_kernel_page_table(|| stat_for_fd(file_desc))
}

fn check_fstatat_flags(flags: usize) -> Result<(), SysErrNo> {
    if flags & !(AT_EMPTY_PATH | AT_SYMLINK_NOFOLLOW | AT_NO_AUTOMOUNT) != 0 {
        Err(SysErrNo::EINVAL)
    } else {
        Ok(())
    }
}

fn check_statx_flags(flags: usize) -> Result<(), SysErrNo> {
    if flags & !(AT_EMPTY_PATH | AT_SYMLINK_NOFOLLOW | AT_NO_AUTOMOUNT | AT_STATX_SYNC_TYPE) != 0 {
        Err(SysErrNo::EINVAL)
    } else {
        Ok(())
    }
}

fn check_statx_mask(mask: usize) -> Result<(), SysErrNo> {
    if mask & STATX_RESERVED != 0 {
        Err(SysErrNo::EINVAL)
    } else {
        Ok(())
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

fn make_statx_timestamp(sec: isize, nsec: isize) -> StatxTimestamp {
    StatxTimestamp {
        tv_sec: sec as i64,
        tv_nsec: nsec.max(0) as u32,
        __reserved: 0,
    }
}

fn make_statx(st: &KStat, mask: usize) -> Statx {
    const STATX_BASIC_STATS: u32 = 0x0000_07ff;

    Statx {
        stx_mask: STATX_BASIC_STATS | mask as u32,
        stx_blksize: st.st_blksize,
        stx_attributes: 0,
        stx_nlink: st.st_nlink,
        stx_uid: st.st_uid,
        stx_gid: st.st_gid,
        stx_mode: (st.st_mode & 0xffff) as u16,
        __spare0: 0,
        stx_ino: st.st_ino,
        stx_size: st.st_size.max(0) as u64,
        stx_blocks: st.st_blocks,
        stx_attributes_mask: 0,
        stx_atime: make_statx_timestamp(st.st_atime_sec, st.st_atime_nsec),
        stx_btime: make_statx_timestamp(st.st_ctime_sec, st.st_ctime_nsec),
        stx_ctime: make_statx_timestamp(st.st_ctime_sec, st.st_ctime_nsec),
        stx_mtime: make_statx_timestamp(st.st_mtime_sec, st.st_mtime_nsec),
        stx_rdev_major: ((st.st_rdev >> 8) & 0xfff) as u32,
        stx_rdev_minor: ((st.st_rdev & 0xff) | ((st.st_rdev >> 12) & !0xff)) as u32,
        stx_dev_major: 0,
        stx_dev_minor: 0,
        stx_mnt_id: 0,
        stx_dio_mem_align: 0,
        stx_dio_offset_align: 0,
        __spare3: [0; 12],
    }
}

fn copy_statx_out(statxbuf: *mut u8, st: &Statx) -> Result<(), SysErrNo> {
    let bytes = unsafe {
        core::slice::from_raw_parts(
            st as *const Statx as *const u8,
            core::mem::size_of::<Statx>(),
        )
    };
    copy_to_user(statxbuf, bytes)
}

fn copy_statfs_out(buf: *mut u8, st: &StatFs) -> Result<(), SysErrNo> {
    let bytes = unsafe {
        core::slice::from_raw_parts(
            st as *const StatFs as *const u8,
            core::mem::size_of::<StatFs>(),
        )
    };
    copy_to_user(buf, bytes)
}

#[derive(Clone, PartialEq, Eq)]
enum FileLockKey {
    MemInode(u64),
    MemPath(String),
    Ext4(u32),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum FileLockOwner {
    Posix(usize),
    Ofd(usize),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum FileLockKind {
    Read,
    Write,
}

#[derive(Clone, Copy)]
struct FileLockRange {
    start: u64,
    end: Option<u64>,
}

#[derive(Clone)]
struct FileRecordLock {
    file: FileLockKey,
    owner: FileLockOwner,
    pid: usize,
    kind: FileLockKind,
    range: FileLockRange,
}

#[derive(Clone)]
struct WholeFileLock {
    file: FileLockKey,
    owner: usize,
    kind: FileLockKind,
}

#[derive(Clone, Copy)]
struct FileLockWaiter {
    waiter: FileLockOwner,
    waiting_for: FileLockOwner,
}

#[derive(Clone, Copy)]
struct UserFlock64 {
    l_type: i16,
    l_whence: i16,
    l_start: i64,
    l_len: i64,
    l_pid: i32,
}

lazy_static! {
    static ref FILE_RECORD_LOCKS: Mutex<Vec<FileRecordLock>> = Mutex::new(Vec::new());
    static ref FILE_LOCK_WAITERS: Mutex<Vec<FileLockWaiter>> = Mutex::new(Vec::new());
    static ref WHOLE_FILE_LOCKS: Mutex<Vec<WholeFileLock>> = Mutex::new(Vec::new());
}

fn file_lock_hash_bytes(mut hash: usize, bytes: &[u8]) -> usize {
    for &byte in bytes {
        hash ^= byte as usize;
        hash = hash.wrapping_mul(0x0100_0000_01b3usize);
    }
    hash
}

fn file_lock_wait_key(file: &FileLockKey) -> WaitKey {
    let hash = match file {
        FileLockKey::MemInode(ino) => {
            file_lock_hash_bytes(0xcbf2_9ce4_8422_2325usize, &ino.to_ne_bytes())
        }
        FileLockKey::MemPath(name) => {
            file_lock_hash_bytes(0x9ce4_8422_2325_cbf2usize, name.as_bytes())
        }
        FileLockKey::Ext4(ino) => {
            file_lock_hash_bytes(0x8422_2325_cbf2_9ce4usize, &ino.to_ne_bytes())
        }
    };
    WaitKey::new(hash, 0x464c_4f43)
}

fn fd_lock_key(file_desc: &FileDescriptor) -> Option<FileLockKey> {
    match file_desc {
        FileDescriptor::MemFile { name, node, .. } => {
            let ino = node
                .and_then(|metadata| (metadata.ino != 0).then_some(metadata.ino))
                .or_else(|| crate::fs::MEM_FS.lock().inode(name));
            Some(match ino {
                Some(ino) => FileLockKey::MemInode(ino),
                None => FileLockKey::MemPath(name.clone()),
            })
        }
        FileDescriptor::Ext4Regular { ino, .. } => Some(FileLockKey::Ext4(*ino)),
        _ => None,
    }
}

fn fd_lock_offset(file_desc: &FileDescriptor) -> usize {
    match file_desc {
        FileDescriptor::MemFile { offset, .. } | FileDescriptor::Ext4Regular { offset, .. } => {
            *offset
        }
        _ => 0,
    }
}

fn flock_i16(bytes: &[u8], offset: usize) -> i16 {
    i16::from_ne_bytes([bytes[offset], bytes[offset + 1]])
}

fn flock_i32(bytes: &[u8], offset: usize) -> i32 {
    i32::from_ne_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
    ])
}

fn flock_i64(bytes: &[u8], offset: usize) -> i64 {
    i64::from_ne_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
        bytes[offset + 4],
        bytes[offset + 5],
        bytes[offset + 6],
        bytes[offset + 7],
    ])
}

fn copy_flock_from_user(arg: usize) -> Result<UserFlock64, SysErrNo> {
    if arg == 0 {
        return Err(SysErrNo::EFAULT);
    }
    let mut bytes = [0u8; 32];
    copy_from_user(arg as *const u8, &mut bytes)?;
    Ok(UserFlock64 {
        l_type: flock_i16(&bytes, 0),
        l_whence: flock_i16(&bytes, 2),
        l_start: flock_i64(&bytes, 8),
        l_len: flock_i64(&bytes, 16),
        l_pid: flock_i32(&bytes, 24),
    })
}

fn copy_flock_to_user(arg: usize, flock: &UserFlock64) -> Result<(), SysErrNo> {
    if arg == 0 {
        return Err(SysErrNo::EFAULT);
    }
    let mut bytes = [0u8; 32];
    bytes[0..2].copy_from_slice(&flock.l_type.to_ne_bytes());
    bytes[2..4].copy_from_slice(&flock.l_whence.to_ne_bytes());
    bytes[8..16].copy_from_slice(&flock.l_start.to_ne_bytes());
    bytes[16..24].copy_from_slice(&flock.l_len.to_ne_bytes());
    bytes[24..28].copy_from_slice(&flock.l_pid.to_ne_bytes());
    copy_to_user(arg as *mut u8, &bytes)
}

fn flock_kind(lock_type: i16) -> Result<Option<FileLockKind>, SysErrNo> {
    match lock_type {
        F_RDLCK => Ok(Some(FileLockKind::Read)),
        F_WRLCK => Ok(Some(FileLockKind::Write)),
        F_UNLCK => Ok(None),
        _ => Err(SysErrNo::EINVAL),
    }
}

fn signed_lock_base(
    whence: i16,
    current_offset: usize,
    file_size: usize,
) -> Result<i128, SysErrNo> {
    match whence {
        0 => Ok(0),
        1 => Ok(current_offset as i128),
        2 => Ok(file_size as i128),
        _ => Err(SysErrNo::EINVAL),
    }
}

fn flock_range(
    flock: &UserFlock64,
    current_offset: usize,
    file_size: usize,
) -> Result<FileLockRange, SysErrNo> {
    let base = signed_lock_base(flock.l_whence, current_offset, file_size)?;
    let start = base
        .checked_add(flock.l_start as i128)
        .ok_or(SysErrNo::EINVAL)?;
    let len = flock.l_len as i128;
    let (start, end) = if len > 0 {
        let end = start.checked_add(len).ok_or(SysErrNo::EINVAL)?;
        (start, Some(end))
    } else if len < 0 {
        let new_start = start.checked_add(len).ok_or(SysErrNo::EINVAL)?;
        (new_start, Some(start))
    } else {
        (start, None)
    };
    if start < 0 || end.map(|value| value < 0).unwrap_or(false) {
        return Err(SysErrNo::EINVAL);
    }
    Ok(FileLockRange {
        start: start as u64,
        end: end.map(|value| value as u64),
    })
}

fn lock_ranges_overlap(left: FileLockRange, right: FileLockRange) -> bool {
    let left_before_right = left.end.map(|end| end <= right.start).unwrap_or(false);
    let right_before_left = right.end.map(|end| end <= left.start).unwrap_or(false);
    !left_before_right && !right_before_left
}

fn lock_ranges_touch_or_overlap(left: FileLockRange, right: FileLockRange) -> bool {
    let left_before_right = left.end.map(|end| end < right.start).unwrap_or(false);
    let right_before_left = right.end.map(|end| end < left.start).unwrap_or(false);
    !left_before_right && !right_before_left
}

fn merge_lock_ranges(left: FileLockRange, right: FileLockRange) -> FileLockRange {
    let start = left.start.min(right.start);
    let end = match (left.end, right.end) {
        (Some(left_end), Some(right_end)) => Some(left_end.max(right_end)),
        _ => None,
    };
    FileLockRange { start, end }
}

fn lock_overlap_start(left: FileLockRange, right: FileLockRange) -> Option<u64> {
    lock_ranges_overlap(left, right).then_some(left.start.max(right.start))
}

fn lock_kinds_conflict(left: FileLockKind, right: FileLockKind) -> bool {
    !(left == FileLockKind::Read && right == FileLockKind::Read)
}

fn lock_range_nonempty(range: FileLockRange) -> bool {
    range.end.map(|end| range.start < end).unwrap_or(true)
}

fn subtract_lock_range(old: FileLockRange, cut: FileLockRange) -> Vec<FileLockRange> {
    let mut out = Vec::new();
    if old.start < cut.start {
        let left = FileLockRange {
            start: old.start,
            end: Some(cut.start),
        };
        if lock_range_nonempty(left) {
            out.push(left);
        }
    }
    if let Some(cut_end) = cut.end {
        let has_right = old.end.map(|end| end > cut_end).unwrap_or(true);
        if has_right {
            let right = FileLockRange {
                start: cut_end.max(old.start),
                end: old.end,
            };
            if lock_range_nonempty(right) {
                out.push(right);
            }
        }
    }
    out
}

fn find_lock_conflict(
    locks: &[FileRecordLock],
    file: &FileLockKey,
    owner: FileLockOwner,
    kind: FileLockKind,
    range: FileLockRange,
) -> Option<FileRecordLock> {
    let mut best: Option<(u64, FileRecordLock)> = None;
    for lock in locks.iter() {
        if &lock.file != file || lock.owner == owner || !lock_kinds_conflict(lock.kind, kind) {
            continue;
        }
        let Some(overlap_start) = lock_overlap_start(lock.range, range) else {
            continue;
        };
        let replace = best
            .as_ref()
            .map(|(best_start, best_lock)| {
                overlap_start < *best_start
                    || (overlap_start == *best_start && lock.range.start < best_lock.range.start)
            })
            .unwrap_or(true);
        if replace {
            best = Some((overlap_start, lock.clone()));
        }
    }
    best.map(|(_, lock)| lock)
}

fn remove_lock_waiter(owner: FileLockOwner) {
    FILE_LOCK_WAITERS
        .lock()
        .retain(|waiter| waiter.waiter != owner);
}

fn set_lock_waiter(waiter: FileLockOwner, waiting_for: FileLockOwner) {
    let mut waiters = FILE_LOCK_WAITERS.lock();
    if let Some(entry) = waiters.iter_mut().find(|entry| entry.waiter == waiter) {
        entry.waiting_for = waiting_for;
    } else {
        waiters.push(FileLockWaiter {
            waiter,
            waiting_for,
        });
    }
}

fn file_lock_would_deadlock(waiter: FileLockOwner, waiting_for: FileLockOwner) -> bool {
    if waiter == waiting_for {
        return true;
    }
    let waiters = FILE_LOCK_WAITERS.lock();
    let mut owner = waiting_for;
    let mut depth = 0usize;
    while depth <= waiters.len() {
        if owner == waiter {
            return true;
        }
        let Some(next) = waiters
            .iter()
            .find(|entry| entry.waiter == owner)
            .map(|entry| entry.waiting_for)
        else {
            return false;
        };
        owner = next;
        depth += 1;
    }
    false
}

fn apply_file_unlock(
    locks: &mut Vec<FileRecordLock>,
    file: &FileLockKey,
    owner: FileLockOwner,
    range: FileLockRange,
) -> bool {
    let mut changed = false;
    let mut index = 0usize;
    while index < locks.len() {
        if &locks[index].file == file
            && locks[index].owner == owner
            && lock_ranges_overlap(locks[index].range, range)
        {
            let lock = locks.remove(index);
            for remainder in subtract_lock_range(lock.range, range) {
                locks.push(FileRecordLock {
                    range: remainder,
                    ..lock.clone()
                });
            }
            changed = true;
        } else {
            index += 1;
        }
    }
    changed
}

fn push_merged_file_lock(locks: &mut Vec<FileRecordLock>, mut new_lock: FileRecordLock) {
    let mut index = 0usize;
    while index < locks.len() {
        if locks[index].file == new_lock.file
            && locks[index].owner == new_lock.owner
            && locks[index].kind == new_lock.kind
            && lock_ranges_touch_or_overlap(locks[index].range, new_lock.range)
        {
            let old = locks.remove(index);
            new_lock.range = merge_lock_ranges(new_lock.range, old.range);
        } else {
            index += 1;
        }
    }
    locks.push(new_lock);
}

fn apply_file_lock(
    file: &FileLockKey,
    owner: FileLockOwner,
    pid: usize,
    kind: Option<FileLockKind>,
    range: FileLockRange,
) -> Result<Option<FileLockOwner>, SysErrNo> {
    let mut locks = FILE_RECORD_LOCKS.lock();
    if let Some(kind) = kind {
        if let Some(conflict) = find_lock_conflict(&locks, file, owner, kind, range) {
            return Ok(Some(conflict.owner));
        }
        apply_file_unlock(&mut locks, file, owner, range);
        push_merged_file_lock(
            &mut locks,
            FileRecordLock {
                file: file.clone(),
                owner,
                pid,
                kind,
                range,
            },
        );
        Ok(None)
    } else {
        apply_file_unlock(&mut locks, file, owner, range);
        Ok(None)
    }
}

fn file_lock_conflict_pid(conflict: &FileRecordLock) -> i32 {
    match conflict.owner {
        FileLockOwner::Posix(_) => conflict.pid as i32,
        FileLockOwner::Ofd(_) => -1,
    }
}

fn flock_from_record(lock: &FileRecordLock) -> UserFlock64 {
    UserFlock64 {
        l_type: match lock.kind {
            FileLockKind::Read => F_RDLCK,
            FileLockKind::Write => F_WRLCK,
        },
        l_whence: 0,
        l_start: lock.range.start as i64,
        l_len: lock
            .range
            .end
            .map(|end| end.saturating_sub(lock.range.start) as i64)
            .unwrap_or(0),
        l_pid: file_lock_conflict_pid(lock),
    }
}

fn wake_file_lock_waiters(file: &FileLockKey) {
    crate::task::wait_queue::wake_io_keyed_waiters(file_lock_wait_key(file));
}

fn release_posix_locks_for_file(pid: usize, file: &FileLockKey) {
    let mut changed = false;
    FILE_RECORD_LOCKS.lock().retain(|lock| {
        let remove = lock.file == *file && lock.owner == FileLockOwner::Posix(pid);
        changed |= remove;
        !remove
    });
    remove_lock_waiter(FileLockOwner::Posix(pid));
    if changed {
        wake_file_lock_waiters(file);
    }
}

pub fn release_file_locks_for_pid(pid: usize) {
    let owner = FileLockOwner::Posix(pid);
    let mut files = Vec::new();
    FILE_RECORD_LOCKS.lock().retain(|lock| {
        if lock.owner == owner {
            files.push(lock.file.clone());
            false
        } else {
            true
        }
    });
    remove_lock_waiter(owner);
    for file in files {
        wake_file_lock_waiters(&file);
    }
}

pub fn release_file_locks_for_ofd(id: usize) {
    let owner = FileLockOwner::Ofd(id);
    let mut files = Vec::new();
    FILE_RECORD_LOCKS.lock().retain(|lock| {
        if lock.owner == owner {
            if !files.iter().any(|file| file == &lock.file) {
                files.push(lock.file.clone());
            }
            false
        } else {
            true
        }
    });
    WHOLE_FILE_LOCKS.lock().retain(|lock| {
        if lock.owner == id {
            if !files.iter().any(|file| file == &lock.file) {
                files.push(lock.file.clone());
            }
            false
        } else {
            true
        }
    });
    remove_lock_waiter(owner);
    for file in files {
        wake_file_lock_waiters(&file);
    }
}

pub fn release_posix_locks_for_closed_files(pid: usize, files: &[FileDescriptor]) {
    let mut keys = Vec::new();
    for file in files {
        if let Some(key) = fd_lock_key(file) {
            if !keys.iter().any(|seen| seen == &key) {
                keys.push(key);
            }
        }
    }
    for key in keys {
        release_posix_locks_for_file(pid, &key);
    }
}

fn make_statfs(info: crate::fs::VfsStatFs) -> StatFs {
    StatFs {
        f_type: info.f_type,
        f_bsize: info.f_bsize,
        f_blocks: info.f_blocks,
        f_bfree: info.f_bfree,
        f_bavail: info.f_bavail,
        f_files: info.f_files,
        f_ffree: info.f_ffree,
        f_fsid: [0, 0],
        f_namelen: info.f_namelen,
        f_frsize: info.f_frsize,
        f_flags: info.f_flags,
        f_spare: [0; 4],
    }
}

fn fd_status_flags(file_desc: &FileDescriptor) -> usize {
    match file_desc {
        FileDescriptor::Stdin => fd::open_flags::O_RDONLY as usize,
        FileDescriptor::Stdout | FileDescriptor::Stderr => fd::open_flags::O_WRONLY as usize,
        FileDescriptor::MemFile {
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
        FileDescriptor::Path { flags, .. } => (flags & !fd::open_flags::O_CLOEXEC) as usize,
        FileDescriptor::LoopControl => fd::open_flags::O_RDWR as usize,
        FileDescriptor::LoopDevice {
            readable, writable, ..
        } => match (*readable, *writable) {
            (true, true) => fd::open_flags::O_RDWR as usize,
            (false, true) => fd::open_flags::O_WRONLY as usize,
            _ => fd::open_flags::O_RDONLY as usize,
        },
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
        FileDescriptor::Socket { state } => {
            let socket = state.lock();
            let mut flags = fd::open_flags::O_RDWR as usize;
            if socket.nonblock {
                flags |= fd::pipe_flags::O_NONBLOCK;
            }
            flags
        }
        FileDescriptor::EventFd { state } => {
            let eventfd = state.lock();
            let mut flags = fd::open_flags::O_RDWR as usize;
            if eventfd.nonblock {
                flags |= fd::pipe_flags::O_NONBLOCK;
            }
            flags
        }
        FileDescriptor::Epoll { .. } => fd::open_flags::O_RDWR as usize,
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
    #[cfg(feature = "buildstorm-diagnostics")]
    let started_at = crate::timer::get_time_us();
    let resolved = resolve_host_path(dirfd, pathname);
    #[cfg(feature = "buildstorm-diagnostics")]
    crate::buildstorm_diagnostics::note_phase(
        3,
        crate::timer::get_time_us().saturating_sub(started_at),
    );
    let (logical_path, host_path) = resolved?;
    #[cfg(feature = "buildstorm-diagnostics")]
    let started_at = crate::timer::get_time_us();
    let result = open_resolved_path(logical_path, host_path, flags, mode);
    #[cfg(feature = "buildstorm-diagnostics")]
    crate::buildstorm_diagnostics::note_phase(
        4,
        crate::timer::get_time_us().saturating_sub(started_at),
    );
    result
}

fn open_resolved_path(
    logical_path: String,
    host_path: String,
    flags: u32,
    mode: u32,
) -> SyscallRet {
    let fd_flags = if (flags & fd::open_flags::O_CLOEXEC) != 0 {
        fd::FD_CLOEXEC
    } else {
        0
    };
    let open_flags = flags & !fd::open_flags::O_CLOEXEC;

    let (logical_path, host_path) = if let Some(target) = proc_magic_link_target(&logical_path)? {
        if (open_flags & fd::open_flags::O_NOFOLLOW) != 0 {
            return Err(SysErrNo::ELOOP);
        }
        let root = current_root()?;
        let host = crate::fs::apply_root(&root, &target);
        (target, host)
    } else {
        (logical_path, host_path)
    };

    log::info!(
        "[syscall] open(path='{}' -> '{}', flags={}, mode={})",
        logical_path,
        host_path,
        flags,
        mode
    );

    // 获取当前任务的文件描述符表
    if let Some(task) = current_task() {
        let create_mode =
            if (open_flags & (fd::open_flags::O_CREAT | fd::open_flags::O_TMPFILE)) != 0 {
                mode & !task.fs.lock().umask
            } else {
                mode
            };
        let inner = task.inner.lock();
        let nofile_limit = inner.rlimit_nofile;

        let opened = super::with_kernel_page_table(|| {
            crate::fs::open_path(&host_path, &logical_path, open_flags, create_mode)
        });
        match opened {
            Ok(fd_desc) => {
                let mut fds = inner.fd_table.lock();
                match fds.alloc_with_flags_below(fd_desc, fd_flags, nofile_limit) {
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

pub fn sys_openat2(
    dirfd: isize,
    pathname: *const u8,
    how: *const OpenHow,
    size: usize,
) -> SyscallRet {
    let how = copy_open_how_from_user(how, size)?;
    let (flags, mode, resolve) = validate_openat2_how(how)?;
    let path = read_user_path(pathname)?;
    let (logical_path, host_path) = openat2_resolve_path_str(dirfd, &path, resolve)?;
    open_resolved_path(logical_path, host_path, flags, mode)
}

pub fn sys_getcwd(buf: *mut u8, size: usize) -> SyscallRet {
    if buf.is_null() {
        return Err(SysErrNo::EFAULT);
    }
    if size == 0 {
        return Err(SysErrNo::EINVAL);
    }

    if let Some(task) = current_task() {
        let cwd = task.fs.lock().cwd.clone();
        let bytes = cwd.as_bytes();
        if bytes.len() + 1 > size {
            return Err(SysErrNo::ERANGE);
        }
        copy_to_user(buf, bytes)?;
        copy_to_user(unsafe { buf.add(bytes.len()) }, &[0])?;
        Ok(bytes.len() + 1)
    } else {
        Err(SysErrNo::ESRCH)
    }
}

pub fn sys_chdir(pathname: *const u8) -> SyscallRet {
    let (logical_path, host_path) = resolve_host_path(AT_FDCWD, pathname)?;
    if !super::with_kernel_page_table(|| crate::fs::dir_exists(&host_path)) {
        return Err(SysErrNo::ENOENT);
    }
    if let Some(task) = current_task() {
        task.fs.lock().cwd = logical_path.clone();
        task.inner.lock().cwd = logical_path;
        Ok(0)
    } else {
        Err(SysErrNo::ESRCH)
    }
}

pub fn sys_mkdirat(dirfd: isize, pathname: *const u8, mode: u32) -> SyscallRet {
    let (_logical_path, host_path) = resolve_host_path(dirfd, pathname)?;
    let task = current_task().ok_or(SysErrNo::ESRCH)?;
    let mode = mode & !task.fs.lock().umask;
    super::with_kernel_page_table(|| crate::fs::create_dir_with_mode(&host_path, mode))?;
    Ok(0)
}

pub fn sys_mknodat(dirfd: isize, pathname: *const u8, mode: u32, dev: usize) -> SyscallRet {
    let (_logical_path, host_path) = resolve_host_path(dirfd, pathname)?;
    let task = current_task().ok_or(SysErrNo::ESRCH)?;
    let root_capable = task.credentials.lock().is_root_capable();
    let permissions = mode & 0o7777 & !task.fs.lock().umask;
    match mode & S_IFMT {
        0 | S_IFREG => {
            super::with_kernel_page_table(|| {
                crate::fs::create_regular_file(&host_path, permissions)
            })?;
        }
        S_IFIFO => {
            super::with_kernel_page_table(|| {
                crate::fs::create_special_node(
                    &host_path,
                    crate::fs::MemSpecialKind::Fifo,
                    permissions,
                )
            })?;
        }
        S_IFSOCK => {
            super::with_kernel_page_table(|| {
                crate::fs::create_special_node(
                    &host_path,
                    crate::fs::MemSpecialKind::Socket,
                    permissions,
                )
            })?;
        }
        S_IFCHR | S_IFBLK => {
            if !root_capable {
                return Err(SysErrNo::EPERM);
            }
            let (major, minor) = decode_dev(dev)?;
            let kind = if mode & S_IFMT == S_IFCHR {
                crate::fs::MemSpecialKind::CharDevice { major, minor }
            } else {
                crate::fs::MemSpecialKind::BlockDevice { major, minor }
            };
            super::with_kernel_page_table(|| {
                crate::fs::create_special_node(&host_path, kind, permissions)
            })?;
        }
        _ => return Err(SysErrNo::EINVAL),
    }
    Ok(0)
}

pub fn sys_fchmodat(dirfd: isize, pathname: *const u8, mode: u32) -> SyscallRet {
    let path = read_user_path(pathname)?;
    if path.is_empty() {
        return Err(SysErrNo::ENOENT);
    }
    let (logical_path, host_path) = resolve_host_path_str(dirfd, &path)?;
    if let Some(fd) = proc_self_fd_number(&logical_path)? {
        let task = current_task().ok_or(SysErrNo::ESRCH)?;
        let inner = task.inner.lock();
        let mut fds = inner.fd_table.lock();
        let file_desc = fds.get_mut(fd).ok_or(SysErrNo::EBADF)?;
        super::with_kernel_page_table(|| crate::fs::set_mode_fd(file_desc, mode))?;
    } else {
        super::with_kernel_page_table(|| crate::fs::set_mode_path(&host_path, true, mode))?;
    }
    Ok(0)
}

pub fn sys_fchmod(fd: usize, mode: u32) -> SyscallRet {
    let task = current_task().ok_or(SysErrNo::ESRCH)?;
    let inner = task.inner.lock();
    let mut fds = inner.fd_table.lock();
    let file_desc = fds.get_mut(fd).ok_or(SysErrNo::EBADF)?;
    super::with_kernel_page_table(|| crate::fs::set_mode_fd(file_desc, mode))?;
    Ok(0)
}

fn chown_id(raw: usize) -> Result<Option<u32>, SysErrNo> {
    if raw == usize::MAX || raw == u32::MAX as usize {
        Ok(None)
    } else if raw > u32::MAX as usize {
        Err(SysErrNo::EINVAL)
    } else {
        Ok(Some(raw as u32))
    }
}

pub fn sys_fchownat(
    dirfd: isize,
    pathname: *const u8,
    uid: usize,
    gid: usize,
    flags: usize,
) -> SyscallRet {
    if flags & !AT_SYMLINK_NOFOLLOW != 0 {
        return Err(SysErrNo::EINVAL);
    }
    let uid = chown_id(uid)?;
    let gid = chown_id(gid)?;
    let path = read_user_path(pathname)?;
    if path.is_empty() {
        return Err(SysErrNo::ENOENT);
    }

    let (logical_path, host_path) = resolve_host_path_str(dirfd, &path)?;
    if let Some(fd) = proc_self_fd_number(&logical_path)? {
        let task = current_task().ok_or(SysErrNo::ESRCH)?;
        let inner = task.inner.lock();
        let mut fds = inner.fd_table.lock();
        let file_desc = fds.get_mut(fd).ok_or(SysErrNo::EBADF)?;
        super::with_kernel_page_table(|| crate::fs::set_owner_fd(file_desc, uid, gid))?;
    } else {
        let follow = flags & AT_SYMLINK_NOFOLLOW == 0;
        super::with_kernel_page_table(|| crate::fs::set_owner_path(&host_path, follow, uid, gid))?;
    }
    Ok(0)
}

pub fn sys_fchown(fd: usize, uid: usize, gid: usize) -> SyscallRet {
    let task = current_task().ok_or(SysErrNo::ESRCH)?;
    let inner = task.inner.lock();
    let mut fds = inner.fd_table.lock();
    let file_desc = fds.get_mut(fd).ok_or(SysErrNo::EBADF)?;
    let uid = chown_id(uid)?;
    let gid = chown_id(gid)?;
    super::with_kernel_page_table(|| crate::fs::set_owner_fd(file_desc, uid, gid))?;
    Ok(0)
}

fn read_xattr_value(value: *const u8, size: usize) -> Result<Vec<u8>, SysErrNo> {
    const XATTR_SIZE_MAX: usize = 65536;
    if size > XATTR_SIZE_MAX {
        return Err(SysErrNo::E2BIG);
    }
    let mut data = Vec::new();
    data.resize(size, 0);
    if size != 0 {
        copy_from_user(value, &mut data)?;
    }
    Ok(data)
}

fn validate_xattr_name_and_flags(
    key: &str,
    flags: usize,
    check_flags: bool,
) -> Result<(), SysErrNo> {
    const XATTR_CREATE: usize = 0x1;
    const XATTR_REPLACE: usize = 0x2;
    const XATTR_NAME_MAX: usize = 255;
    if check_flags
        && (flags & !(XATTR_CREATE | XATTR_REPLACE) != 0 || flags == (XATTR_CREATE | XATTR_REPLACE))
    {
        return Err(SysErrNo::EINVAL);
    }
    if key.is_empty() || key.as_bytes().len() > XATTR_NAME_MAX {
        return Err(SysErrNo::ERANGE);
    }
    Ok(())
}

fn copy_xattr_output(value: *mut u8, size: usize, data: &[u8]) -> SyscallRet {
    if size == 0 {
        return Ok(data.len());
    }
    if size < data.len() {
        return Err(SysErrNo::ERANGE);
    }
    copy_to_user(value, data)?;
    Ok(data.len())
}

pub fn sys_setxattr(
    pathname: *const u8,
    name: *const u8,
    value: *const u8,
    size: usize,
    flags: usize,
    follow_symlink: bool,
) -> SyscallRet {
    let path = read_user_path(pathname)?;
    let key = read_user_cstr(name)?;
    validate_xattr_name_and_flags(&key, flags, true)?;
    let data = read_xattr_value(value, size)?;
    let (_logical_path, host_path) = resolve_host_path_str(AT_FDCWD, &path)?;
    super::with_kernel_page_table(|| {
        crate::fs::set_xattr_path(&host_path, follow_symlink, &key, &data, flags)
    })?;
    Ok(0)
}

pub fn sys_fsetxattr(
    fd: usize,
    name: *const u8,
    value: *const u8,
    size: usize,
    flags: usize,
) -> SyscallRet {
    let key = read_user_cstr(name)?;
    validate_xattr_name_and_flags(&key, flags, true)?;
    let data = read_xattr_value(value, size)?;
    let task = current_task().ok_or(SysErrNo::ESRCH)?;
    let inner = task.inner.lock();
    let fds = inner.fd_table.lock();
    let file_desc = fds.get(fd).ok_or(SysErrNo::EBADF)?;
    super::with_kernel_page_table(|| crate::fs::set_xattr_fd(file_desc, &key, &data, flags))?;
    Ok(0)
}

pub fn sys_getxattr(
    pathname: *const u8,
    name: *const u8,
    value: *mut u8,
    size: usize,
    follow_symlink: bool,
) -> SyscallRet {
    let path = read_user_path(pathname)?;
    let key = read_user_cstr(name)?;
    validate_xattr_name_and_flags(&key, 0, false)?;
    let (_logical_path, host_path) = resolve_host_path_str(AT_FDCWD, &path)?;
    let data = super::with_kernel_page_table(|| {
        crate::fs::get_xattr_path(&host_path, follow_symlink, &key)
    })?;
    copy_xattr_output(value, size, &data)
}

pub fn sys_fgetxattr(fd: usize, name: *const u8, value: *mut u8, size: usize) -> SyscallRet {
    let key = read_user_cstr(name)?;
    validate_xattr_name_and_flags(&key, 0, false)?;
    let task = current_task().ok_or(SysErrNo::ESRCH)?;
    let inner = task.inner.lock();
    let fds = inner.fd_table.lock();
    let file_desc = fds.get(fd).ok_or(SysErrNo::EBADF)?;
    let data = super::with_kernel_page_table(|| crate::fs::get_xattr_fd(file_desc, &key))?;
    copy_xattr_output(value, size, &data)
}

pub fn sys_listxattr(
    pathname: *const u8,
    list: *mut u8,
    size: usize,
    follow_symlink: bool,
) -> SyscallRet {
    let path = read_user_path(pathname)?;
    let (_logical_path, host_path) = resolve_host_path_str(AT_FDCWD, &path)?;
    let data =
        super::with_kernel_page_table(|| crate::fs::list_xattr_path(&host_path, follow_symlink))?;
    copy_xattr_output(list, size, &data)
}

pub fn sys_flistxattr(fd: usize, list: *mut u8, size: usize) -> SyscallRet {
    let task = current_task().ok_or(SysErrNo::ESRCH)?;
    let inner = task.inner.lock();
    let fds = inner.fd_table.lock();
    let file_desc = fds.get(fd).ok_or(SysErrNo::EBADF)?;
    let data = super::with_kernel_page_table(|| crate::fs::list_xattr_fd(file_desc))?;
    copy_xattr_output(list, size, &data)
}

pub fn sys_removexattr(pathname: *const u8, name: *const u8, follow_symlink: bool) -> SyscallRet {
    let path = read_user_path(pathname)?;
    let key = read_user_cstr(name)?;
    validate_xattr_name_and_flags(&key, 0, false)?;
    let (_logical_path, host_path) = resolve_host_path_str(AT_FDCWD, &path)?;
    super::with_kernel_page_table(|| {
        crate::fs::remove_xattr_path(&host_path, follow_symlink, &key)
    })?;
    Ok(0)
}

pub fn sys_fremovexattr(fd: usize, name: *const u8) -> SyscallRet {
    let key = read_user_cstr(name)?;
    validate_xattr_name_and_flags(&key, 0, false)?;
    let task = current_task().ok_or(SysErrNo::ESRCH)?;
    let inner = task.inner.lock();
    let fds = inner.fd_table.lock();
    let file_desc = fds.get(fd).ok_or(SysErrNo::EBADF)?;
    super::with_kernel_page_table(|| crate::fs::remove_xattr_fd(file_desc, &key))?;
    Ok(0)
}

pub fn sys_unlinkat(dirfd: isize, pathname: *const u8, flags: usize) -> SyscallRet {
    const AT_REMOVEDIR: usize = 0x200;
    let raw_path = read_user_path(pathname)?;
    if flags & !AT_REMOVEDIR != 0 {
        return Err(SysErrNo::EINVAL);
    }
    if flags & AT_REMOVEDIR != 0 {
        let trimmed = raw_path.trim_end_matches('/');
        if trimmed == "." || trimmed.ends_with("/.") {
            return Err(SysErrNo::EINVAL);
        }
    }
    let (_logical_path, host_path) = resolve_host_path_str(dirfd, &raw_path)?;
    if flags & AT_REMOVEDIR != 0 {
        super::with_kernel_page_table(|| crate::fs::remove_dir(&host_path))?;
    } else {
        super::with_kernel_page_table(|| crate::fs::remove_file(&host_path))?;
    }
    Ok(0)
}

pub fn sys_unlink(pathname: *const u8) -> SyscallRet {
    sys_unlinkat(AT_FDCWD, pathname, 0)
}

pub fn sys_rmdir(pathname: *const u8) -> SyscallRet {
    const AT_REMOVEDIR: usize = 0x200;
    sys_unlinkat(AT_FDCWD, pathname, AT_REMOVEDIR)
}

pub fn sys_renameat2(
    olddirfd: isize,
    oldpath: *const u8,
    newdirfd: isize,
    newpath: *const u8,
    flags: usize,
) -> SyscallRet {
    const RENAME_NOREPLACE: usize = 1;
    const RENAME_EXCHANGE: usize = 2;
    const RENAME_WHITEOUT: usize = 4;
    if flags & !(RENAME_NOREPLACE | RENAME_EXCHANGE | RENAME_WHITEOUT) != 0 {
        return Err(SysErrNo::EINVAL);
    }
    if (flags & RENAME_WHITEOUT) != 0
        || (flags & RENAME_NOREPLACE) != 0 && (flags & RENAME_EXCHANGE) != 0
    {
        return Err(SysErrNo::EINVAL);
    }
    let (_old_logical, old_host) = resolve_host_path(olddirfd, oldpath)?;
    let (_new_logical, new_host) = resolve_host_path(newdirfd, newpath)?;
    if (flags & RENAME_EXCHANGE) != 0 {
        super::with_kernel_page_table(|| crate::fs::rename_exchange_path(&old_host, &new_host))?;
    } else {
        super::with_kernel_page_table(|| {
            crate::fs::rename_path(&old_host, &new_host, flags & RENAME_NOREPLACE != 0)
        })?;
    }
    Ok(0)
}

pub fn sys_renameat(
    olddirfd: isize,
    oldpath: *const u8,
    newdirfd: isize,
    newpath: *const u8,
) -> SyscallRet {
    sys_renameat2(olddirfd, oldpath, newdirfd, newpath, 0)
}

pub fn sys_rename(oldpath: *const u8, newpath: *const u8) -> SyscallRet {
    sys_renameat2(AT_FDCWD, oldpath, AT_FDCWD, newpath, 0)
}

pub fn sys_linkat(
    olddirfd: isize,
    oldpath: *const u8,
    newdirfd: isize,
    newpath: *const u8,
    flags: usize,
) -> SyscallRet {
    const AT_SYMLINK_FOLLOW: usize = 0x400;
    if flags & !AT_SYMLINK_FOLLOW != 0 {
        return Err(SysErrNo::EINVAL);
    }
    let (old_logical, old_host) = resolve_host_path(olddirfd, oldpath)?;
    let (_new_logical, new_host) = resolve_host_path(newdirfd, newpath)?;
    if (flags & AT_SYMLINK_FOLLOW) != 0 {
        if let Some(fd) = proc_self_fd_number(&old_logical)? {
            let task = current_task().ok_or(SysErrNo::ESRCH)?;
            let inner = task.inner.lock();
            let mut fds = inner.fd_table.lock();
            let file_desc = fds.get_mut(fd).ok_or(SysErrNo::EBADF)?;
            super::with_kernel_page_table(|| crate::fs::link_mem_file_fd(file_desc, &new_host))?;
            return Ok(0);
        }
    }
    super::with_kernel_page_table(|| {
        crate::fs::link_path(&old_host, &new_host, flags & AT_SYMLINK_FOLLOW != 0)
    })?;
    Ok(0)
}

pub fn sys_link(oldpath: *const u8, newpath: *const u8) -> SyscallRet {
    sys_linkat(AT_FDCWD, oldpath, AT_FDCWD, newpath, 0)
}

pub fn sys_symlinkat(target: *const u8, newdirfd: isize, linkpath: *const u8) -> SyscallRet {
    let target = read_user_path(target)?;
    if target.is_empty() {
        return Err(SysErrNo::ENOENT);
    }
    let (_logical_path, host_path) = resolve_host_path(newdirfd, linkpath)?;
    super::with_kernel_page_table(|| crate::fs::create_symlink(&target, &host_path))?;
    Ok(0)
}

pub fn sys_symlink(target: *const u8, linkpath: *const u8) -> SyscallRet {
    sys_symlinkat(target, AT_FDCWD, linkpath)
}

pub fn sys_faccessat(dirfd: isize, pathname: *const u8, mode: usize, flags: usize) -> SyscallRet {
    const R_OK: usize = 4;
    const W_OK: usize = 2;
    const X_OK: usize = 1;
    if mode & !(R_OK | W_OK | X_OK) != 0 {
        return Err(SysErrNo::EINVAL);
    }
    if flags & !(AT_SYMLINK_NOFOLLOW | AT_EACCESS | AT_EMPTY_PATH) != 0 {
        return Err(SysErrNo::EINVAL);
    }
    let use_effective = (flags & AT_EACCESS) != 0;
    let path = read_user_path(pathname)?;
    if path.is_empty() {
        if flags & AT_EMPTY_PATH == 0 {
            return Err(SysErrNo::ENOENT);
        }
        if dirfd == AT_FDCWD {
            let task = current_task().ok_or(SysErrNo::ESRCH)?;
            let (root, cwd) = {
                let fs = task.fs.lock();
                (fs.root.clone(), fs.cwd.clone())
            };
            let host_path = crate::fs::apply_root(&root, &cwd);
            super::with_kernel_page_table(|| {
                crate::fs::check_access_with_effective(&host_path, true, mode, use_effective)
            })?;
        } else {
            let fd = usize::try_from(dirfd).map_err(|_| SysErrNo::EBADF)?;
            let task = current_task().ok_or(SysErrNo::ESRCH)?;
            let inner = task.inner.lock();
            let fds = inner.fd_table.lock();
            let file_desc = fds.get(fd).ok_or(SysErrNo::EBADF)?;
            super::with_kernel_page_table(|| {
                crate::fs::check_fd_access_with_effective(file_desc, mode, use_effective)
            })?;
        }
        return Ok(0);
    }
    let (_logical_path, host_path) = resolve_host_path(dirfd, pathname)?;
    let follow = (flags & AT_SYMLINK_NOFOLLOW) == 0;
    super::with_kernel_page_table(|| {
        crate::fs::check_access_with_effective(&host_path, follow, mode, use_effective)
    })?;
    Ok(0)
}

pub fn sys_access(pathname: *const u8, mode: usize) -> SyscallRet {
    sys_faccessat(AT_FDCWD, pathname, mode, 0)
}

fn readlink_target_at(dirfd: isize, path: &str) -> Result<String, SysErrNo> {
    if path.is_empty() {
        if dirfd == AT_FDCWD {
            return Err(SysErrNo::ENOENT);
        }
        let fd = usize::try_from(dirfd).map_err(|_| SysErrNo::EBADF)?;
        let task = current_task().ok_or(SysErrNo::ESRCH)?;
        let inner = task.inner.lock();
        let fds = inner.fd_table.lock();
        let file_desc = fds.get(fd).ok_or(SysErrNo::EBADF)?;
        return match file_desc {
            FileDescriptor::Path {
                host_path, flags, ..
            } if (flags & fd::open_flags::O_NOFOLLOW) != 0 => {
                super::with_kernel_page_table(|| crate::fs::read_link(host_path))
            }
            _ => Err(SysErrNo::ENOENT),
        };
    }

    // glibc asks /proc/self/exe during startup to name the executable used for
    // diagnostics and pointer-guard setup. Model this as a procfs symlink
    // backed by task metadata rather than a BusyBox-specific string.
    let logical = resolve_path_str(dirfd, path)?;
    if let Some(target) = proc_self_fd_target(&logical)? {
        return Ok(target);
    }
    if let Some(target) = super::process::proc_self_exe_target(&logical)? {
        return Ok(target);
    }

    let root = current_root()?;
    let host_path = crate::fs::apply_root(&root, &logical);
    super::with_kernel_page_table(|| crate::fs::read_link(&host_path))
}

pub fn sys_readlinkat(
    dirfd: isize,
    pathname: *const u8,
    buf: *mut u8,
    bufsiz: usize,
) -> SyscallRet {
    if buf.is_null() {
        return Err(SysErrNo::EFAULT);
    }
    if bufsiz == 0 {
        return Err(SysErrNo::EINVAL);
    }

    #[cfg(feature = "buildstorm-diagnostics")]
    let started_at = crate::timer::get_time_us();
    let path = read_user_path(pathname);
    #[cfg(feature = "buildstorm-diagnostics")]
    crate::buildstorm_diagnostics::note_phase(
        0,
        crate::timer::get_time_us().saturating_sub(started_at),
    );
    let path = path?;
    #[cfg(feature = "buildstorm-diagnostics")]
    let started_at = crate::timer::get_time_us();
    let target = readlink_target_at(dirfd, &path);
    #[cfg(feature = "buildstorm-diagnostics")]
    crate::buildstorm_diagnostics::note_phase(
        1,
        crate::timer::get_time_us().saturating_sub(started_at),
    );
    let target = target?;
    let bytes = target.as_bytes();
    let n = bytes.len().min(bufsiz);
    #[cfg(feature = "buildstorm-diagnostics")]
    let started_at = crate::timer::get_time_us();
    let copied = copy_to_user(buf, &bytes[..n]);
    #[cfg(feature = "buildstorm-diagnostics")]
    crate::buildstorm_diagnostics::note_phase(
        2,
        crate::timer::get_time_us().saturating_sub(started_at),
    );
    copied?;
    Ok(n)
}

pub fn sys_getdents64(fd: usize, dirp: *mut u8, count: usize) -> SyscallRet {
    if dirp.is_null() {
        return Err(SysErrNo::EFAULT);
    }
    if let Some(task) = current_task() {
        let inner = task.inner.lock();
        let mut fds = inner.fd_table.lock();
        match fds.get_mut(fd) {
            Some(file_desc) => {
                let mut kbuf = alloc::vec![0u8; count];
                let n = super::with_kernel_page_table(|| file_desc.read_dirents64(&mut kbuf))?;
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
    let fd_flags = if (flags & pipe_flags::O_CLOEXEC) != 0 {
        fd::FD_CLOEXEC
    } else {
        0
    };
    if let Some(task) = current_task() {
        let inner = task.inner.lock();
        let nofile_limit = inner.rlimit_nofile;
        let (read_fd, write_fd) = {
            let mut fds = inner.fd_table.lock();
            let (read_end, write_end) = crate::fs::fd::create_pipe(nonblock);
            let read_fd = fds
                .alloc_with_flags_below(read_end, fd_flags, nofile_limit)
                .ok_or(SysErrNo::EMFILE)?;
            let write_fd = match fds.alloc_with_flags_below(write_end, fd_flags, nofile_limit) {
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

pub fn sys_eventfd2(initval: usize, flags: usize) -> SyscallRet {
    let allowed = EFD_SEMAPHORE | EFD_NONBLOCK | EFD_CLOEXEC;
    if flags & !allowed != 0 {
        return Err(SysErrNo::EINVAL);
    }
    let fd_flags = if flags & EFD_CLOEXEC != 0 {
        fd::FD_CLOEXEC
    } else {
        0
    };
    let eventfd = crate::fs::fd::create_eventfd(
        initval as u64,
        flags & EFD_SEMAPHORE != 0,
        flags & EFD_NONBLOCK != 0,
    );

    let task = current_task().ok_or(SysErrNo::ESRCH)?;
    let inner = task.inner.lock();
    let nofile_limit = inner.rlimit_nofile;
    let mut fds = inner.fd_table.lock();
    fds.alloc_with_flags_below(eventfd, fd_flags, nofile_limit)
        .ok_or(SysErrNo::EMFILE)
}

pub fn sys_mount(
    source: *const u8,
    target: *const u8,
    fstype: *const u8,
    flags: usize,
    data: usize,
) -> SyscallRet {
    let _ = data;
    if target.is_null() || fstype.is_null() {
        return Err(SysErrNo::EFAULT);
    }
    let fst = read_user_cstr(fstype)?;
    let target_path = read_user_path(target)?;
    let source_path = if source.is_null() {
        String::new()
    } else {
        read_user_path(source)?
    };
    if target_path.is_empty() {
        return Err(SysErrNo::ENOENT);
    }
    if source_path.is_empty() && fst != "tmpfs" {
        return Err(SysErrNo::ENODEV);
    }

    let target_logical = crate::fs::resolve_path(&current_cwd()?, &target_path);
    let root = current_root()?;
    let target_host = crate::fs::apply_root(&root, &target_logical);
    let source_logical = crate::fs::resolve_path("/", &source_path);

    super::with_kernel_page_table(|| {
        crate::fs::mount_fs(&source_logical, &target_logical, &target_host, &fst, flags)
    })?;
    Ok(0)
}

pub fn sys_umount2(target: *const u8, flags: usize) -> SyscallRet {
    if target.is_null() {
        return Err(SysErrNo::EFAULT);
    }
    let target_path = read_user_path(target)?;
    if target_path.is_empty() {
        return Err(SysErrNo::ENOENT);
    }
    let target_logical = crate::fs::resolve_path(&current_cwd()?, &target_path);
    let root = current_root()?;
    let target_host = crate::fs::apply_root(&root, &target_logical);
    super::with_kernel_page_table(|| crate::fs::umount_fs(&target_logical, &target_host, flags))?;
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
        #[cfg(feature = "buildstorm-diagnostics")]
        let started_at = crate::timer::get_time_us();
        let old = {
            let inner = task.inner.lock();
            let fd_table = inner.fd_table.clone();
            drop(inner);
            let mut fds = fd_table.lock();
            fds.remove(fd)?
        };
        #[cfg(feature = "buildstorm-diagnostics")]
        crate::buildstorm_diagnostics::note_phase(
            5,
            crate::timer::get_time_us().saturating_sub(started_at),
        );
        if let Some(file_key) = fd_lock_key(&old) {
            release_posix_locks_for_file(task.thread_group.tgid(), &file_key);
        }
        #[cfg(feature = "buildstorm-diagnostics")]
        let started_at = crate::timer::get_time_us();
        super::with_kernel_page_table(|| drop(old));
        #[cfg(feature = "buildstorm-diagnostics")]
        crate::buildstorm_diagnostics::note_phase(
            6,
            crate::timer::get_time_us().saturating_sub(started_at),
        );
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

    if count == 0 {
        let task = current_task().ok_or(SysErrNo::ESRCH)?;
        let fd_table = task.inner.lock().fd_table.clone();
        let mut fds = fd_table.lock();
        let mut empty: [u8; 0] = [];
        return match fds.get_mut(fd) {
            Some(file_desc) => read_fd_into_kernel(file_desc, &mut empty),
            None => Err(SysErrNo::EBADF),
        };
    }

    // 安全检查：确保缓冲区不为 null
    if buf.is_null() {
        return Err(SysErrNo::EFAULT);
    }

    // 安全检查：确保 count 不会导致溢出
    if count > isize::MAX as usize {
        return Err(SysErrNo::EINVAL);
    }

    let task = current_task().ok_or(SysErrNo::ESRCH)?;
    let fd_table = task.inner.lock().fd_table.clone();

    if count <= SMALL_IO_STACK_BUF {
        let mut kbuf = [0u8; SMALL_IO_STACK_BUF];
        loop {
            let res = {
                let mut fds = fd_table.lock();
                match fds.get_mut(fd) {
                    Some(file_desc) => match direct_read_fd_to_user(file_desc, buf, count) {
                        Ok(Some(n)) => return Ok(n),
                        Ok(None) => read_fd_into_kernel(file_desc, &mut kbuf[..count]),
                        Err(err) => Err(err),
                    },
                    None => Err(SysErrNo::EBADF),
                }
            };

            match res {
                Ok(n) => {
                    copy_to_user(buf, &kbuf[..n])?;
                    return Ok(n);
                }
                Err(SysErrNo::EAGAIN) => {
                    if !wait_read_after_eagain(&fd_table, fd)? {
                        continue;
                    }
                }
                Err(e) => return Err(e),
            }
        }
    }

    let mut kbuf = alloc::vec![0u8; count];
    loop {
        let res = {
            let mut fds = fd_table.lock();
            match fds.get_mut(fd) {
                Some(file_desc) => read_fd_into_kernel(file_desc, &mut kbuf),
                None => Err(SysErrNo::EBADF),
            }
        };

        match res {
            Ok(n) => {
                copy_to_user(buf, &kbuf[..n])?;
                return Ok(n);
            }
            Err(SysErrNo::EAGAIN) => {
                if !wait_read_after_eagain(&fd_table, fd)? {
                    continue;
                }
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

    if count == 0 {
        let task = current_task().ok_or(SysErrNo::ESRCH)?;
        let fd_table = task.inner.lock().fd_table.clone();
        let mut fds = fd_table.lock();
        return match fds.get_mut(fd) {
            Some(file_desc) => write_fd_from_kernel(file_desc, &[]),
            None => Err(SysErrNo::EBADF),
        };
    }
    if buf.is_null() {
        return Err(SysErrNo::EFAULT);
    }
    if count > isize::MAX as usize {
        return Err(SysErrNo::EINVAL);
    }

    let task = current_task().ok_or(SysErrNo::ESRCH)?;
    let fd_table = task.inner.lock().fd_table.clone();
    if count <= SMALL_IO_STACK_BUF {
        let mut fds = fd_table.lock();
        if let Some(file_desc) = fds.get_mut(fd) {
            if let Some(n) = direct_write_fd_from_user(file_desc, buf, count)? {
                return Ok(n);
            }
        } else {
            return Err(SysErrNo::EBADF);
        }
    }
    with_user_read_buf(buf, count, |kbuf| {
        let mut written = 0usize;

        loop {
            let res = {
                let mut fds = fd_table.lock();
                match fds.get_mut(fd) {
                    Some(file_desc) => write_fd_from_kernel(file_desc, &kbuf[written..]),
                    None => Err(SysErrNo::EBADF),
                }
            };

            match res {
                Ok(n) => {
                    written += n;
                    if written == count || n == 0 {
                        return Ok(written);
                    }
                }
                Err(SysErrNo::EAGAIN) => {
                    if written != 0 {
                        return Ok(written);
                    }
                    match wait_write_after_eagain(&fd_table, fd) {
                        Ok(false) => continue,
                        Ok(_) => {}
                        Err(SysErrNo::EINTR) if written != 0 => return Ok(written),
                        Err(err) => return Err(err),
                    }
                }
                Err(SysErrNo::EPIPE) if written == 0 => {
                    crate::syscall::signal::send_sigpipe_to_current();
                    return Err(SysErrNo::EPIPE);
                }
                Err(err) => {
                    return if written != 0 { Ok(written) } else { Err(err) };
                }
            }
        }
    })
}

/// lseek 系统调用
///
/// 设置文件读写偏移
/// - fd: 文件描述符
/// - offset: 偏移量
/// - whence: 起始位置 (0=SEEK_SET, 1=SEEK_CUR, 2=SEEK_END)
pub fn sys_lseek(fd: usize, offset: isize, whence: usize) -> SyscallRet {
    log::debug!(
        "[syscall] lseek(fd={}, offset={}, whence={})",
        fd,
        offset,
        whence
    );

    let task = current_task().ok_or(SysErrNo::ESRCH)?;
    let inner = task.inner.lock();
    let mut fds = inner.fd_table.lock();

    match fds.get_mut(fd) {
        Some(file_desc) => super::with_kernel_page_table(|| file_desc.seek_signed(offset, whence)),
        None => Err(SysErrNo::EBADF),
    }
}

/// dup 系统调用
///
/// 复制文件描述符
/// - old_fd: 旧文件描述符
pub fn sys_pread64(fd: usize, buf: *mut u8, count: usize, offset: usize) -> SyscallRet {
    if count == 0 {
        let mut empty: [u8; 0] = [];
        return with_fixed_io_fd_mut(fd, |file_desc| {
            super::with_kernel_page_table(|| file_desc.read_at(offset, &mut empty))
        });
    }
    if buf.is_null() {
        return Err(SysErrNo::EFAULT);
    }
    checked_io_count(count)?;

    if count <= SMALL_IO_STACK_BUF {
        let mut kbuf = [0u8; SMALL_IO_STACK_BUF];
        let mut direct = false;
        let n = with_fixed_io_fd_mut(fd, |file_desc| {
            match direct_read_at_fd_to_user(file_desc, buf, count, offset)? {
                Some(n) => {
                    direct = true;
                    Ok(n)
                }
                None => read_fixed_at_into_kernel(file_desc, offset, &mut kbuf[..count]),
            }
        })?;
        if !direct && n != 0 {
            copy_to_user(buf, &kbuf[..n])?;
        }
        return Ok(n);
    }

    let mut kbuf = alloc::vec![0u8; count];
    let n = with_fixed_io_fd_mut(fd, |file_desc| {
        read_fixed_at_into_kernel(file_desc, offset, &mut kbuf)
    })?;
    copy_to_user(buf, &kbuf[..n])?;
    Ok(n)
}

pub fn sys_pwrite64(fd: usize, buf: *const u8, count: usize, offset: usize) -> SyscallRet {
    if count == 0 {
        return with_fixed_io_fd_mut(fd, |file_desc| {
            super::with_kernel_page_table(|| file_desc.write_at(offset, &[]))
        });
    }
    if buf.is_null() {
        return Err(SysErrNo::EFAULT);
    }
    checked_io_count(count)?;

    if count <= SMALL_IO_STACK_BUF {
        if let Some(n) = with_fixed_io_fd_mut(fd, |file_desc| {
            direct_write_at_fd_from_user(file_desc, buf, count, offset)
        })? {
            return Ok(n);
        }
    }

    with_user_read_buf(buf, count, |kbuf| {
        with_fixed_io_fd_mut(fd, |file_desc| {
            write_fixed_at_from_kernel(file_desc, offset, kbuf)
        })
    })
}

pub fn sys_dup(old_fd: usize) -> SyscallRet {
    log::debug!("[syscall] dup(old_fd={})", old_fd);

    let task = current_task().ok_or(SysErrNo::ESRCH)?;
    let inner = task.inner.lock();
    let nofile_limit = inner.rlimit_nofile;
    let mut fds = inner.fd_table.lock();
    fds.dup_below(old_fd, nofile_limit)
}

/// dup3 系统调用 (dup2 的现代版本)
///
/// 复制文件描述符到指定位置
/// - old_fd: 旧文件描述符
/// - new_fd: 新文件描述符
/// - flags: 标志
pub fn sys_dup3(old_fd: usize, new_fd: usize, flags: usize) -> SyscallRet {
    log::debug!(
        "[syscall] dup3(old_fd={}, new_fd={}, flags={})",
        old_fd,
        new_fd,
        flags
    );
    let allowed = fd::open_flags::O_CLOEXEC as usize;
    if old_fd == new_fd || (flags & !allowed) != 0 {
        return Err(SysErrNo::EINVAL);
    }
    let fd_flags = if (flags & allowed) != 0 {
        fd::FD_CLOEXEC
    } else {
        0
    };

    let task = current_task().ok_or(SysErrNo::ESRCH)?;
    let pid = task.thread_group.tgid();
    let inner = task.inner.lock();
    let nofile_limit = inner.rlimit_nofile;
    let mut fds = inner.fd_table.lock();
    let replaced_key = fds.get(new_fd).and_then(fd_lock_key);
    let new_fd = fds.dup2_below(old_fd, new_fd, nofile_limit)?;
    fds.set_fd_flags(new_fd, fd_flags)?;
    drop(fds);
    drop(inner);
    if let Some(file_key) = replaced_key {
        release_posix_locks_for_file(pid, &file_key);
    }
    Ok(new_fd)
}

fn whole_file_lock_conflicts(
    locks: &[WholeFileLock],
    file: &FileLockKey,
    owner: usize,
    kind: FileLockKind,
) -> bool {
    locks.iter().any(|lock| {
        lock.file == *file && lock.owner != owner && lock_kinds_conflict(lock.kind, kind)
    })
}

pub fn sys_flock(fd: usize, operation: usize) -> SyscallRet {
    let nonblock = (operation & LOCK_NB) != 0;
    if (operation & !(LOCK_SH | LOCK_EX | LOCK_NB | LOCK_UN)) != 0 {
        return Err(SysErrNo::EINVAL);
    }
    let kind = match operation & !LOCK_NB {
        LOCK_SH => Some(FileLockKind::Read),
        LOCK_EX => Some(FileLockKind::Write),
        LOCK_UN => None,
        _ => return Err(SysErrNo::EINVAL),
    };

    let task = current_task().ok_or(SysErrNo::ESRCH)?;
    let (file_key, owner, wait_key) = {
        let fd_table = task.inner.lock().fd_table.clone();
        let fds = fd_table.lock();
        let file_desc = fds.get(fd).ok_or(SysErrNo::EBADF)?;
        let file_key = fd_lock_key(file_desc).ok_or(SysErrNo::EINVAL)?;
        let owner = file_desc.ofd_owner_id().ok_or(SysErrNo::EINVAL)?;
        let wait_key = file_lock_wait_key(&file_key);
        (file_key, owner, wait_key)
    };

    if kind.is_none() {
        let mut changed = false;
        WHOLE_FILE_LOCKS.lock().retain(|lock| {
            let remove = lock.file == file_key && lock.owner == owner;
            changed |= remove;
            !remove
        });
        if changed {
            wake_file_lock_waiters(&file_key);
        }
        return Ok(0);
    }

    let kind = kind.unwrap();
    let mut released_previous = false;
    loop {
        {
            let mut locks = WHOLE_FILE_LOCKS.lock();
            if !released_previous {
                locks.retain(|lock| !(lock.file == file_key && lock.owner == owner));
                released_previous = true;
            }
            if !whole_file_lock_conflicts(&locks, &file_key, owner, kind) {
                locks.push(WholeFileLock {
                    file: file_key.clone(),
                    owner,
                    kind,
                });
                return Ok(0);
            }
        }

        if nonblock {
            return Err(SysErrNo::EAGAIN);
        }
        sleep_on_io_key_if(wait_key, None, || {
            let locks = WHOLE_FILE_LOCKS.lock();
            Ok(whole_file_lock_conflicts(&locks, &file_key, owner, kind))
        })?;
    }
}

pub fn sys_fcntl(fd: usize, cmd: usize, arg: usize) -> SyscallRet {
    if matches!(
        cmd,
        F_GETLK | F_SETLK | F_SETLKW | F_OFD_GETLK | F_OFD_SETLK | F_OFD_SETLKW
    ) {
        return sys_fcntl_file_lock(fd, cmd, arg);
    }

    let task = current_task().ok_or(SysErrNo::ESRCH)?;
    let inner = task.inner.lock();
    let nofile_limit = inner.rlimit_nofile;
    let mut fds = inner.fd_table.lock();
    match cmd {
        F_DUPFD | F_DUPFD_CLOEXEC => {
            if arg >= nofile_limit.min(fd::MAX_FD_NUM) {
                return Err(SysErrNo::EINVAL);
            }
            let file_desc = fds.get(fd).cloned().ok_or(SysErrNo::EBADF)?;
            let flags = if cmd == F_DUPFD_CLOEXEC {
                fd::FD_CLOEXEC
            } else {
                0
            };
            fds.alloc_from_with_flags_below(arg, file_desc, flags, nofile_limit)
                .ok_or(SysErrNo::EMFILE)
        }
        F_GETFD => fds.fd_flags(fd),
        F_SETFD => {
            fds.set_fd_flags(fd, arg)?;
            Ok(0)
        }
        F_GETFL => {
            let file_desc = fds.get(fd).ok_or(SysErrNo::EBADF)?;
            Ok(fd_status_flags(file_desc))
        }
        F_SETFL => {
            let file_desc = fds.get_mut(fd).ok_or(SysErrNo::EBADF)?;
            file_desc.set_status_flags(arg);
            Ok(0)
        }
        F_SETLEASE => {
            let file_desc = fds.get(fd).ok_or(SysErrNo::EBADF)?;
            let lease = match arg {
                value if value == F_RDLCK as usize => {
                    if !file_desc.readable() {
                        return Err(SysErrNo::EBADF);
                    }
                    F_RDLCK
                }
                value if value == F_WRLCK as usize => {
                    if !file_desc.writable() {
                        return Err(SysErrNo::EBADF);
                    }
                    F_WRLCK
                }
                value if value == F_UNLCK as usize => F_UNLCK,
                _ => return Err(SysErrNo::EINVAL),
            };
            file_desc.set_lease(lease)?;
            Ok(0)
        }
        F_GETLEASE => {
            let file_desc = fds.get(fd).ok_or(SysErrNo::EBADF)?;
            file_desc
                .lease()
                .map(|lease| lease as usize)
                .ok_or(SysErrNo::EINVAL)
        }
        // Linux reports EINVAL for fcntl commands outside the supported
        // command set.  ENOSYS is reserved for a missing syscall entry, not
        // for an unrecognized command argument to an implemented syscall.
        _ => Err(SysErrNo::EINVAL),
    }
}

fn sys_fcntl_file_lock(fd: usize, cmd: usize, arg: usize) -> SyscallRet {
    let task = current_task().ok_or(SysErrNo::ESRCH)?;
    let flock = copy_flock_from_user(arg)?;
    let kind = flock_kind(flock.l_type)?;
    let is_ofd = matches!(cmd, F_OFD_GETLK | F_OFD_SETLK | F_OFD_SETLKW);
    let blocking = matches!(cmd, F_SETLKW | F_OFD_SETLKW);
    let getlk = matches!(cmd, F_GETLK | F_OFD_GETLK);
    if is_ofd && flock.l_pid != 0 {
        return Err(SysErrNo::EINVAL);
    }
    if getlk && kind.is_none() {
        return Err(SysErrNo::EINVAL);
    }

    let (file_key, range, owner, pid, wait_key) = {
        let inner = task.inner.lock();
        let fd_table = inner.fd_table.clone();
        drop(inner);
        let fds = fd_table.lock();
        let file_desc = fds.get(fd).ok_or(SysErrNo::EBADF)?;
        let file_key = fd_lock_key(file_desc).ok_or(SysErrNo::EINVAL)?;
        let range = flock_range(&flock, fd_lock_offset(file_desc), file_desc.size())?;
        if !getlk {
            match kind {
                Some(FileLockKind::Read) if !file_desc.readable() => return Err(SysErrNo::EBADF),
                Some(FileLockKind::Write) if !file_desc.writable() => return Err(SysErrNo::EBADF),
                _ => {}
            }
        }
        let owner = if is_ofd {
            FileLockOwner::Ofd(file_desc.ofd_owner_id().ok_or(SysErrNo::EINVAL)?)
        } else {
            FileLockOwner::Posix(task.thread_group.tgid())
        };
        let wait_key = file_lock_wait_key(&file_key);
        let pid = if is_ofd { 0 } else { task.thread_group.tgid() };
        (file_key, range, owner, pid, wait_key)
    };

    if getlk {
        let mut out = flock;
        if let Some(kind) = kind {
            let locks = FILE_RECORD_LOCKS.lock();
            if let Some(conflict) = find_lock_conflict(&locks, &file_key, owner, kind, range) {
                out = flock_from_record(&conflict);
            } else {
                out.l_type = F_UNLCK;
            }
        } else {
            out.l_type = F_UNLCK;
        }
        copy_flock_to_user(arg, &out)?;
        return Ok(0);
    }

    if kind.is_none() {
        let conflict = apply_file_lock(&file_key, owner, pid, None, range)?;
        debug_assert!(conflict.is_none());
        remove_lock_waiter(owner);
        wake_file_lock_waiters(&file_key);
        return Ok(0);
    }

    let kind = kind.unwrap();
    loop {
        match apply_file_lock(&file_key, owner, pid, Some(kind), range)? {
            None => {
                remove_lock_waiter(owner);
                return Ok(0);
            }
            Some(_conflict_owner) if !blocking => {
                remove_lock_waiter(owner);
                return Err(SysErrNo::EAGAIN);
            }
            Some(conflict_owner) => {
                if !is_ofd {
                    if file_lock_would_deadlock(owner, conflict_owner) {
                        remove_lock_waiter(owner);
                        return Err(SysErrNo::EDEADLK);
                    }
                    set_lock_waiter(owner, conflict_owner);
                }
                let sleep_result = sleep_on_io_key_if(wait_key, None, || {
                    let locks = FILE_RECORD_LOCKS.lock();
                    Ok(find_lock_conflict(&locks, &file_key, owner, kind, range).is_some())
                });
                match sleep_result {
                    Ok(_) => {}
                    Err(SysErrNo::ERESTARTSYS) => return Err(SysErrNo::ERESTARTSYS),
                    Err(err) => {
                        remove_lock_waiter(owner);
                        return Err(err);
                    }
                }
            }
        }
    }
}

fn sys_sync_fd(fd: usize, data_only: bool) -> SyscallRet {
    let task = current_task().ok_or(SysErrNo::ESRCH)?;
    let file_desc = {
        let inner = task.inner.lock();
        let fds = inner.fd_table.lock();
        fds.get(fd).ok_or(SysErrNo::EBADF)?.clone()
    };
    super::mm::write_back_shared_mappings_for_file(&file_desc)?;
    super::with_kernel_page_table(|| crate::fs::sync_fd(&file_desc, data_only))?;
    Ok(0)
}

pub fn sys_fsync(fd: usize) -> SyscallRet {
    sys_sync_fd(fd, false)
}

pub fn sys_fdatasync(fd: usize) -> SyscallRet {
    sys_sync_fd(fd, true)
}

pub fn sys_sync() -> SyscallRet {
    super::mm::write_back_all_shared_file_mappings()?;
    super::with_kernel_page_table(crate::fs::sync_all)?;
    Ok(0)
}

pub fn sys_statfs(pathname: *const u8, buf: *mut u8) -> SyscallRet {
    if buf.is_null() {
        return Err(SysErrNo::EFAULT);
    }
    let (_logical_path, host_path) = resolve_host_path(AT_FDCWD, pathname)?;
    let info = super::with_kernel_page_table(|| crate::fs::statfs_for_path(&host_path))?;
    copy_statfs_out(buf, &make_statfs(info))?;
    Ok(0)
}

pub fn sys_fstatfs(fd: usize, buf: *mut u8) -> SyscallRet {
    if buf.is_null() {
        return Err(SysErrNo::EFAULT);
    }
    let task = current_task().ok_or(SysErrNo::ESRCH)?;
    let inner = task.inner.lock();
    let fds = inner.fd_table.lock();
    let file_desc = fds.get(fd).ok_or(SysErrNo::EBADF)?;
    let info = super::with_kernel_page_table(|| crate::fs::statfs_for_fd(file_desc))?;
    copy_statfs_out(buf, &make_statfs(info))?;
    Ok(0)
}

fn loop_backing_from_fd(
    file_desc: &FileDescriptor,
) -> Result<crate::fs::block_dev::LoopBacking, SysErrNo> {
    match file_desc {
        FileDescriptor::MemFile {
            name,
            writable,
            linked,
            ..
        } => {
            if !*writable {
                return Err(SysErrNo::EBADF);
            }
            if !*linked || crate::fs::MEM_FS.lock().get_file(name).is_none() {
                return Err(SysErrNo::ENOENT);
            }
            Ok(crate::fs::block_dev::LoopBacking::MemFile { path: name.clone() })
        }
        FileDescriptor::Ext4Regular { ino, writable, .. } => {
            if !*writable {
                return Err(SysErrNo::EBADF);
            }
            Ok(crate::fs::block_dev::LoopBacking::Ext4Regular { ino: *ino })
        }
        _ => Err(SysErrNo::EINVAL),
    }
}

fn loop_set_fd(index: usize, backing_fd: usize, fd_table: &fd::FileDescriptorTable) -> SyscallRet {
    let backing = {
        let backing_desc = fd_table.get(backing_fd).ok_or(SysErrNo::EBADF)?;
        loop_backing_from_fd(backing_desc)?
    };
    crate::fs::block_dev::attach_loop(index, backing)?;
    Ok(0)
}

fn loop_status_ioctl(index: usize, argp: usize) -> SyscallRet {
    if !crate::fs::block_dev::loop_is_attached(index)? {
        return Err(SysErrNo::ENXIO);
    }
    if argp != 0 {
        let zero = [0u8; 232];
        let len = zero.len().min(232);
        super::user::copy_to_user(argp, &zero[..len])?;
    }
    Ok(0)
}

fn loop_device_ioctl(
    index: usize,
    request: usize,
    argp: usize,
    fd_table: &fd::FileDescriptorTable,
) -> SyscallRet {
    match request {
        LOOP_SET_FD => loop_set_fd(index, argp, fd_table),
        LOOP_CLR_FD => {
            crate::fs::block_dev::detach_loop(index)?;
            Ok(0)
        }
        LOOP_GET_STATUS | LOOP_GET_STATUS64 => loop_status_ioctl(index, argp),
        LOOP_SET_STATUS | LOOP_SET_STATUS64 => {
            if !crate::fs::block_dev::loop_is_attached(index)? {
                return Err(SysErrNo::ENXIO);
            }
            Ok(0)
        }
        BLKGETSIZE64 => {
            if argp == 0 {
                return Err(SysErrNo::EFAULT);
            }
            let size = crate::fs::block_dev::loop_size(index)? as u64;
            super::user::copy_object_to_user(argp, &size)?;
            Ok(0)
        }
        BLKSSZGET => {
            if argp == 0 {
                return Err(SysErrNo::EFAULT);
            }
            let sector_size: i32 = 512;
            super::user::copy_object_to_user(argp, &sector_size)?;
            Ok(0)
        }
        _ => Err(SysErrNo::ENOTTY),
    }
}

pub fn sys_ioctl(fd: usize, request: usize, argp: usize) -> SyscallRet {
    let request = request as u32 as usize;
    let task = current_task().ok_or(SysErrNo::ESRCH)?;
    let inner = task.inner.lock();
    let mut fds = inner.fd_table.lock();

    if request == FIONBIO {
        if argp == 0 {
            return Err(SysErrNo::EFAULT);
        }
        let value = super::user::copy_object_from_user::<i32>(argp)?;
        return fds
            .get_mut(fd)
            .ok_or(SysErrNo::EBADF)?
            .ioctl_set_nonblocking(value != 0)
            .map(|_| 0);
    }

    if request == FIONREAD {
        if argp == 0 {
            return Err(SysErrNo::EFAULT);
        }
        let file_desc = fds.get(fd).ok_or(SysErrNo::EBADF)?;
        if let Some(available) = file_desc.pipe_buffered_len() {
            let available: i32 = available.min(i32::MAX as usize) as i32;
            super::user::copy_object_to_user(argp, &available)?;
            return Ok(0);
        }
    }

    let file_desc = fds.get(fd).ok_or(SysErrNo::EBADF)?;
    match file_desc {
        FileDescriptor::LoopControl => match request {
            LOOP_CTL_GET_FREE => crate::fs::block_dev::first_free_loop()
                .map(|index| Ok(index))
                .unwrap_or(Err(SysErrNo::ENODEV)),
            _ => Err(SysErrNo::ENOTTY),
        },
        FileDescriptor::LoopDevice { index, .. } => loop_device_ioctl(*index, request, argp, &fds),
        FileDescriptor::MemFile { .. }
        | FileDescriptor::Ext4Regular { .. }
        | FileDescriptor::Path { .. } => match request {
            FS_IOC_GETFLAGS => {
                if argp == 0 {
                    return Err(SysErrNo::EFAULT);
                }
                let flags =
                    super::with_kernel_page_table(|| crate::fs::file_flags_for_fd(file_desc))?;
                let out = flags as i32;
                super::user::copy_object_to_user(argp, &out)?;
                Ok(0)
            }
            FS_IOC_SETFLAGS => {
                if argp == 0 {
                    return Err(SysErrNo::EFAULT);
                }
                let flags = super::user::copy_object_from_user::<i32>(argp)? as u32;
                let meta = super::with_kernel_page_table(|| crate::fs::metadata_for_fd(file_desc))?;
                let credentials = task.credentials.lock().clone();
                if !credentials.is_root_capable() && credentials.fsuid != meta.uid {
                    return Err(SysErrNo::EPERM);
                }
                super::with_kernel_page_table(|| {
                    crate::fs::set_file_flags_for_fd(file_desc, flags)
                })?;
                Ok(0)
            }
            _ => Err(SysErrNo::ENOTTY),
        },
        FileDescriptor::Socket { state } => match request {
            FIONREAD => {
                if argp == 0 {
                    return Err(SysErrNo::EFAULT);
                }
                let socket = state.lock();
                let available = if socket.is_datagram() {
                    socket
                        .dgram_queue
                        .front()
                        .map(|packet| packet.data.len())
                        .unwrap_or(0)
                } else {
                    socket.rx_buf.len()
                };
                let available: i32 = available.min(i32::MAX as usize) as i32;
                super::user::copy_object_to_user(argp, &available)?;
                Ok(0)
            }
            _ => Err(SysErrNo::ENOTTY),
        },
        _ => Err(SysErrNo::ENOTTY),
    }
}

pub fn sys_readv(fd: usize, iov: *const u8, iovcnt: usize) -> SyscallRet {
    if iovcnt == 0 {
        return Ok(0);
    }
    if iovcnt > IOV_MAX {
        return Err(SysErrNo::EINVAL);
    }
    if iov.is_null() {
        return Err(SysErrNo::EFAULT);
    }

    let mut total = 0usize;
    let mut index = 0usize;
    while index < iovcnt {
        let remaining_limit = (isize::MAX as usize).saturating_sub(total);
        let (iovecs, next_index, want) =
            match load_user_iovecs_batch(iov, index, iovcnt, remaining_limit) {
                Ok(batch) => batch,
                Err(err) => return if total != 0 { Ok(total) } else { Err(err) },
            };
        index = next_index;
        if iovecs.is_empty() {
            break;
        }

        let n = match vectored_read_to_user(fd, &iovecs) {
            Ok(n) => n,
            Err(err) => return if total != 0 { Ok(total) } else { Err(err) },
        };
        total += n;
        if n < want {
            break;
        }
    }
    Ok(total)
}

pub fn sys_writev(fd: usize, iov: *const u8, iovcnt: usize) -> SyscallRet {
    if iovcnt == 0 {
        return Ok(0);
    }
    if iovcnt > IOV_MAX {
        return Err(SysErrNo::EINVAL);
    }
    if iov.is_null() {
        return Err(SysErrNo::EFAULT);
    }

    let mut total = 0usize;
    let mut index = 0usize;
    while index < iovcnt {
        let remaining_limit = (isize::MAX as usize).saturating_sub(total);
        let (iovecs, next_index, want) =
            match load_user_iovecs_batch(iov, index, iovcnt, remaining_limit) {
                Ok(batch) => batch,
                Err(err) => return if total != 0 { Ok(total) } else { Err(err) },
            };
        index = next_index;
        if iovecs.is_empty() {
            break;
        }

        let n = match vectored_write_from_user(fd, &iovecs) {
            Ok(n) => n,
            Err(err) => return if total != 0 { Ok(total) } else { Err(err) },
        };
        total += n;
        if n < want {
            break;
        }
    }
    Ok(total)
}

pub fn sys_preadv(fd: usize, iov: *const u8, iovcnt: usize, offset: usize) -> SyscallRet {
    if iovcnt == 0 {
        return Ok(0);
    }
    if iovcnt > IOV_MAX {
        return Err(SysErrNo::EINVAL);
    }
    if iov.is_null() {
        return Err(SysErrNo::EFAULT);
    }

    let mut total = 0usize;
    let mut index = 0usize;
    while index < iovcnt {
        let remaining_limit = (isize::MAX as usize).saturating_sub(total);
        let (iovecs, next_index, want) =
            match load_user_iovecs_batch(iov, index, iovcnt, remaining_limit) {
                Ok(batch) => batch,
                Err(err) => return if total != 0 { Ok(total) } else { Err(err) },
            };
        index = next_index;
        if iovecs.is_empty() {
            break;
        }

        let fixed_offset = checked_fixed_offset(offset, total)?;
        let n = match vectored_read_at_to_user(fd, &iovecs, fixed_offset) {
            Ok(n) => n,
            Err(err) => return if total != 0 { Ok(total) } else { Err(err) },
        };
        total += n;
        if n < want {
            break;
        }
    }
    Ok(total)
}

pub fn sys_pwritev(fd: usize, iov: *const u8, iovcnt: usize, offset: usize) -> SyscallRet {
    if iovcnt == 0 {
        return Ok(0);
    }
    if iovcnt > IOV_MAX {
        return Err(SysErrNo::EINVAL);
    }
    if iov.is_null() {
        return Err(SysErrNo::EFAULT);
    }

    let mut total = 0usize;
    let mut index = 0usize;
    while index < iovcnt {
        let remaining_limit = (isize::MAX as usize).saturating_sub(total);
        let (iovecs, next_index, want) =
            match load_user_iovecs_batch(iov, index, iovcnt, remaining_limit) {
                Ok(batch) => batch,
                Err(err) => return if total != 0 { Ok(total) } else { Err(err) },
            };
        index = next_index;
        if iovecs.is_empty() {
            break;
        }

        let fixed_offset = checked_fixed_offset(offset, total)?;
        let n = match with_fixed_io_fd_mut(fd, |file_desc| {
            vectored_write_at_from_user(file_desc, &iovecs, fixed_offset)
        }) {
            Ok(n) => n,
            Err(err) => return if total != 0 { Ok(total) } else { Err(err) },
        };
        total += n;
        if n < want {
            break;
        }
    }
    Ok(total)
}

fn epoll_state_for_fd(
    epfd: usize,
) -> Result<alloc::sync::Arc<spin::Mutex<fd::EpollState>>, SysErrNo> {
    let task = current_task().ok_or(SysErrNo::ESRCH)?;
    let inner = task.inner.lock();
    let fds = inner.fd_table.lock();
    match fds.get(epfd) {
        Some(FileDescriptor::Epoll { state }) => Ok(state.clone()),
        Some(_) => Err(SysErrNo::EINVAL),
        None => Err(SysErrNo::EBADF),
    }
}

fn epoll_ready_events(file_desc: &FileDescriptor, interest_events: u32) -> u32 {
    let mut revents = 0u32;
    if (interest_events & EPOLL_READ_EVENTS) != 0
        && super::with_kernel_page_table(|| file_desc.poll_read_ready())
    {
        revents |= interest_events & EPOLL_READ_EVENTS;
    }
    if (interest_events & EPOLL_WRITE_EVENTS) != 0
        && super::with_kernel_page_table(|| file_desc.poll_write_ready())
    {
        revents |= interest_events & EPOLL_WRITE_EVENTS;
    }
    if super::with_kernel_page_table(|| file_desc.poll_error()) {
        revents |= EPOLLERR;
    }
    if super::with_kernel_page_table(|| file_desc.poll_hup()) {
        revents |= EPOLLHUP;
    }
    revents
}

fn fd_supports_epoll(file_desc: &FileDescriptor) -> bool {
    matches!(
        file_desc,
        FileDescriptor::Stdin
            | FileDescriptor::Stdout
            | FileDescriptor::Stderr
            | FileDescriptor::PipeRead { .. }
            | FileDescriptor::PipeWrite { .. }
            | FileDescriptor::Socket { .. }
            | FileDescriptor::EventFd { .. }
    )
}

fn epoll_ready_count(state: &alloc::sync::Arc<spin::Mutex<fd::EpollState>>) -> usize {
    let epoll = state.lock();
    epoll
        .interests
        .iter()
        .filter(|interest| epoll_ready_events(&interest.file, interest.events) != 0)
        .count()
}

fn epoll_collect_ready(
    state: &alloc::sync::Arc<spin::Mutex<fd::EpollState>>,
    events: *mut EpollEvent,
    maxevents: usize,
) -> Result<usize, SysErrNo> {
    let mut ready_events = Vec::new();
    {
        let epoll = state.lock();
        for interest in epoll.interests.iter() {
            let revents = epoll_ready_events(&interest.file, interest.events);
            if revents == 0 {
                continue;
            }
            ready_events.push(EpollEvent::new(revents, interest.data));
            if ready_events.len() == maxevents {
                break;
            }
        }
    }

    for (index, event) in ready_events.iter().enumerate() {
        copy_object_to_user(unsafe { events.add(index) }, event)?;
    }
    Ok(ready_events.len())
}

fn sys_epoll_wait_deadline(
    epfd: usize,
    events: *mut EpollEvent,
    maxevents: usize,
    deadline: Option<usize>,
) -> SyscallRet {
    if maxevents == 0 || maxevents > fd::MAX_FD_NUM {
        return Err(SysErrNo::EINVAL);
    }
    if events.is_null() {
        return Err(SysErrNo::EFAULT);
    }

    let state = epoll_state_for_fd(epfd)?;
    loop {
        let ready = epoll_collect_ready(&state, events, maxevents)?;
        if ready != 0 {
            return Ok(ready);
        }
        if let Some(deadline) = deadline {
            if crate::timer::get_time_us() >= deadline {
                return Ok(0);
            }
            if sleep_on_io_if(Some(deadline), || Ok(epoll_ready_count(&state) == 0))?
                == WaitOutcome::TimedOut
            {
                return epoll_collect_ready(&state, events, maxevents);
            }
        } else {
            let _ = sleep_on_io_if(None, || Ok(epoll_ready_count(&state) == 0))?;
        }
    }
}

pub fn sys_epoll_create1(flags: usize) -> SyscallRet {
    if flags & !EPOLL_CLOEXEC != 0 {
        return Err(SysErrNo::EINVAL);
    }
    let task = current_task().ok_or(SysErrNo::ESRCH)?;
    let fd_flags = if flags & EPOLL_CLOEXEC != 0 {
        fd::FD_CLOEXEC
    } else {
        0
    };
    let epoll = FileDescriptor::Epoll {
        state: alloc::sync::Arc::new(spin::Mutex::new(fd::EpollState::new())),
    };
    let inner = task.inner.lock();
    let mut fds = inner.fd_table.lock();
    fds.alloc_with_flags(epoll, fd_flags)
        .ok_or(SysErrNo::EMFILE)
}

pub fn sys_epoll_ctl(
    epfd: usize,
    op: usize,
    target_fd: usize,
    event: *const EpollEvent,
) -> SyscallRet {
    let epoll_state = epoll_state_for_fd(epfd)?;
    if epfd == target_fd {
        return Err(SysErrNo::EINVAL);
    }

    let event = match op {
        EPOLL_CTL_ADD | EPOLL_CTL_MOD => {
            if event.is_null() {
                return Err(SysErrNo::EFAULT);
            }
            Some(copy_object_from_user(event)?)
        }
        EPOLL_CTL_DEL => None,
        _ => return Err(SysErrNo::EINVAL),
    };

    let target_file = {
        let task = current_task().ok_or(SysErrNo::ESRCH)?;
        let inner = task.inner.lock();
        let fds = inner.fd_table.lock();
        let file = fds.get(target_fd).ok_or(SysErrNo::EBADF)?;
        if matches!(file, FileDescriptor::Epoll { .. }) {
            return Err(SysErrNo::EINVAL);
        }
        if !fd_supports_epoll(file) {
            return Err(SysErrNo::EPERM);
        }
        if op == EPOLL_CTL_ADD {
            Some(file.clone())
        } else {
            None
        }
    };

    let mut epoll = epoll_state.lock();
    let index = epoll
        .interests
        .iter()
        .position(|interest| interest.fd == target_fd);
    match op {
        EPOLL_CTL_ADD => {
            if index.is_some() {
                return Err(SysErrNo::EEXIST);
            }
            let event = event.unwrap();
            epoll.interests.push(fd::EpollInterest {
                fd: target_fd,
                file: target_file.unwrap(),
                events: event.events(),
                data: event.data(),
            });
            Ok(0)
        }
        EPOLL_CTL_MOD => {
            let index = index.ok_or(SysErrNo::ENOENT)?;
            let event = event.unwrap();
            epoll.interests[index].events = event.events();
            epoll.interests[index].data = event.data();
            Ok(0)
        }
        EPOLL_CTL_DEL => {
            let index = index.ok_or(SysErrNo::ENOENT)?;
            epoll.interests.remove(index);
            Ok(0)
        }
        _ => Err(SysErrNo::EINVAL),
    }
}

pub fn sys_epoll_pwait(
    epfd: usize,
    events: *mut EpollEvent,
    maxevents: usize,
    timeout_ms: isize,
    _sigmask: usize,
    _sigsetsize: usize,
) -> SyscallRet {
    let deadline = if timeout_ms < 0 {
        None
    } else {
        Some(crate::timer::deadline_after_us(
            (timeout_ms as usize).saturating_mul(1000),
        ))
    };
    sys_epoll_wait_deadline(epfd, events, maxevents, deadline)
}

pub fn sys_epoll_pwait2(
    epfd: usize,
    events: *mut EpollEvent,
    maxevents: usize,
    timeout: usize,
    _sigmask: usize,
    _sigsetsize: usize,
) -> SyscallRet {
    let deadline = deadline_from_timespec_ptr(timeout)?;
    sys_epoll_wait_deadline(epfd, events, maxevents, deadline)
}

fn poll_once(fds: *mut PollFd, nfds: usize) -> Result<usize, SysErrNo> {
    if nfds > fd::MAX_FD_NUM {
        return Err(SysErrNo::EINVAL);
    }

    let task = current_task().ok_or(SysErrNo::ESRCH)?;
    let inner = task.inner.lock();
    let fd_table = inner.fd_table.lock();
    let mut ready = 0usize;

    for i in 0..nfds {
        let pollfd_ptr = unsafe { fds.add(i) };
        let mut pfd = copy_object_from_user(pollfd_ptr)?;
        pfd.revents = 0;

        if pfd.fd < 0 {
            copy_object_to_user(pollfd_ptr, &pfd)?;
            continue;
        }

        match fd_table.get(pfd.fd as usize) {
            Some(file_desc) => {
                if (pfd.events & POLL_READ_EVENTS) != 0
                    && super::with_kernel_page_table(|| file_desc.poll_read_ready())
                {
                    pfd.revents |= pfd.events & POLL_READ_EVENTS;
                }
                if (pfd.events & POLL_WRITE_EVENTS) != 0
                    && super::with_kernel_page_table(|| file_desc.poll_write_ready())
                {
                    pfd.revents |= pfd.events & POLL_WRITE_EVENTS;
                }
                if super::with_kernel_page_table(|| file_desc.poll_error()) {
                    pfd.revents |= POLLERR;
                }
                if super::with_kernel_page_table(|| file_desc.poll_hup()) {
                    pfd.revents |= POLLHUP;
                }
            }
            None => {
                pfd.revents = POLLNVAL;
            }
        }

        if pfd.revents != 0 {
            ready += 1;
        }
        copy_object_to_user(pollfd_ptr, &pfd)?;
    }

    Ok(ready)
}

fn push_unique_wait_key(keys: &mut Vec<WaitKey>, key: WaitKey) {
    if !keys.contains(&key) {
        keys.push(key);
    }
}

fn poll_wait_keys(fds: *mut PollFd, nfds: usize) -> Result<Vec<WaitKey>, SysErrNo> {
    let task = current_task().ok_or(SysErrNo::ESRCH)?;
    let inner = task.inner.lock();
    let fd_table = inner.fd_table.lock();

    let mut keys = Vec::new();
    for i in 0..nfds {
        let pfd = copy_object_from_user::<PollFd>(unsafe { fds.add(i) })?;
        if pfd.fd < 0 {
            continue;
        }
        let Some(file_desc) = fd_table.get(pfd.fd as usize) else {
            return Ok(Vec::new());
        };
        if super::with_kernel_page_table(|| file_desc.poll_error())
            || super::with_kernel_page_table(|| file_desc.poll_hup())
        {
            return Ok(Vec::new());
        }

        let wants_read = (pfd.events & POLL_READ_EVENTS) != 0;
        let wants_write = (pfd.events & POLL_WRITE_EVENTS) != 0;
        if wants_read && !super::with_kernel_page_table(|| file_desc.poll_read_ready()) {
            if let Some(key) = file_desc.pipe_read_wait_key() {
                push_unique_wait_key(&mut keys, key);
            } else if let Some(key) = file_desc.eventfd_read_wait_key() {
                push_unique_wait_key(&mut keys, key);
            } else if !wants_write {
                if let Some(key) = file_desc.socket_read_wait_key() {
                    push_unique_wait_key(&mut keys, key);
                } else {
                    return Ok(Vec::new());
                }
            } else {
                return Ok(Vec::new());
            }
        }
        if wants_write && !super::with_kernel_page_table(|| file_desc.poll_write_ready()) {
            if let Some(key) = file_desc.pipe_write_wait_key() {
                push_unique_wait_key(&mut keys, key);
            } else if let Some(key) = file_desc.eventfd_write_wait_key() {
                push_unique_wait_key(&mut keys, key);
            } else {
                return Ok(Vec::new());
            }
        }
        if !wants_read && !wants_write {
            return Ok(Vec::new());
        }
    }

    Ok(keys)
}

pub fn sys_ppoll(
    fds: *mut PollFd,
    nfds: usize,
    timeout: usize,
    _sigmask: usize,
    _sigsetsize: usize,
) -> SyscallRet {
    #[cfg(feature = "buildstorm-diagnostics")]
    crate::buildstorm_diagnostics::note_ppoll_call(nfds);
    let deadline = deadline_from_timespec_ptr(timeout)?;
    if nfds == 0 {
        if let Some(deadline) = deadline {
            let _ = crate::timer::sleep_until_us(deadline)?;
        }
        return Ok(0);
    }
    if fds.is_null() {
        return Err(SysErrNo::EFAULT);
    }

    loop {
        let ready = poll_once(fds, nfds)?;
        if ready != 0 {
            return Ok(ready);
        }
        let wait_keys = poll_wait_keys(fds, nfds)?;
        if let Some(deadline) = deadline {
            if crate::timer::get_time_us() >= deadline {
                return Ok(0);
            }
            let outcome = if !wait_keys.is_empty() {
                #[cfg(feature = "buildstorm-diagnostics")]
                crate::buildstorm_diagnostics::note_ppoll_sleep(true);
                sleep_on_io_keys_if(&wait_keys, Some(deadline), || Ok(poll_once(fds, nfds)? == 0))?
            } else {
                #[cfg(feature = "buildstorm-diagnostics")]
                crate::buildstorm_diagnostics::note_ppoll_sleep(false);
                sleep_on_io_if(Some(deadline), || Ok(poll_once(fds, nfds)? == 0))?
            };
            if outcome == WaitOutcome::TimedOut {
                return Ok(poll_once(fds, nfds)?);
            }
        } else {
            if !wait_keys.is_empty() {
                #[cfg(feature = "buildstorm-diagnostics")]
                crate::buildstorm_diagnostics::note_ppoll_sleep(true);
                let _ = sleep_on_io_keys_if(&wait_keys, None, || Ok(poll_once(fds, nfds)? == 0))?;
            } else {
                #[cfg(feature = "buildstorm-diagnostics")]
                crate::buildstorm_diagnostics::note_ppoll_sleep(false);
                let _ = sleep_on_io_if(None, || Ok(poll_once(fds, nfds)? == 0))?;
            }
        }
    }
}

fn load_fdset(base: usize, nfds: usize) -> Result<Vec<usize>, SysErrNo> {
    let bits = core::mem::size_of::<usize>() * 8;
    let words = nfds.div_ceil(bits);
    let mut set = alloc::vec![0usize; words];
    if base == 0 {
        return Ok(set);
    }
    for (i, word) in set.iter_mut().enumerate() {
        *word =
            super::user::copy_object_from_user::<usize>(base + i * core::mem::size_of::<usize>())?;
    }
    Ok(set)
}

fn fdset_contains(set: &[usize], fd: usize) -> bool {
    let bits = core::mem::size_of::<usize>() * 8;
    let word = fd / bits;
    let bit = fd % bits;
    set.get(word)
        .map(|value| (value & (1usize << bit)) != 0)
        .unwrap_or(false)
}

fn fdset_insert(set: &mut [usize], fd: usize) {
    let bits = core::mem::size_of::<usize>() * 8;
    let word = fd / bits;
    let bit = fd % bits;
    if let Some(value) = set.get_mut(word) {
        *value |= 1usize << bit;
    }
}

fn store_fdset(base: usize, set: &[usize]) -> Result<(), SysErrNo> {
    if base == 0 {
        return Ok(());
    }
    for (i, word) in set.iter().enumerate() {
        super::user::copy_object_to_user(base + i * core::mem::size_of::<usize>(), word)?;
    }
    Ok(())
}

struct PselectPollResult {
    ready: usize,
    read_out: Vec<usize>,
    write_out: Vec<usize>,
    except_out: Vec<usize>,
}

fn pselect_poll(
    nfds: usize,
    read_in: &[usize],
    write_in: &[usize],
    except_in: &[usize],
) -> Result<PselectPollResult, SysErrNo> {
    let mut read_out = alloc::vec![0usize; read_in.len()];
    let mut write_out = alloc::vec![0usize; write_in.len()];
    let except_out = alloc::vec![0usize; except_in.len()];
    let mut ready = 0usize;

    let task = current_task().ok_or(SysErrNo::ESRCH)?;
    let inner = task.inner.lock();
    let fd_table = inner.fd_table.lock();
    for fdno in 0..nfds {
        let want_read = fdset_contains(&read_in, fdno);
        let want_write = fdset_contains(&write_in, fdno);
        let want_except = fdset_contains(&except_in, fdno);
        if !want_read && !want_write && !want_except {
            continue;
        }

        let file_desc = fd_table.get(fdno).ok_or(SysErrNo::EBADF)?;
        let mut fd_ready = false;
        if want_read && super::with_kernel_page_table(|| file_desc.poll_read_ready()) {
            fdset_insert(&mut read_out, fdno);
            fd_ready = true;
        }
        if want_write && super::with_kernel_page_table(|| file_desc.poll_write_ready()) {
            fdset_insert(&mut write_out, fdno);
            fd_ready = true;
        }
        if fd_ready {
            ready += 1;
        }
    }
    drop(fd_table);
    drop(inner);

    Ok(PselectPollResult {
        ready,
        read_out,
        write_out,
        except_out,
    })
}

fn store_pselect_result(
    readfds: usize,
    writefds: usize,
    exceptfds: usize,
    result: &PselectPollResult,
) -> Result<(), SysErrNo> {
    store_fdset(readfds, &result.read_out)?;
    store_fdset(writefds, &result.write_out)?;
    store_fdset(exceptfds, &result.except_out)?;
    Ok(())
}

pub fn sys_pselect6(
    nfds: usize,
    readfds: usize,
    writefds: usize,
    exceptfds: usize,
    timeout: usize,
    _sigmask: usize,
) -> SyscallRet {
    let deadline = deadline_from_timespec_ptr(timeout)?;
    if nfds > fd::MAX_FD_NUM {
        return Err(SysErrNo::EINVAL);
    }
    if nfds == 0 {
        if let Some(deadline) = deadline {
            let _ = crate::timer::sleep_until_us(deadline)?;
        }
        return Ok(0);
    }

    let read_in = load_fdset(readfds, nfds)?;
    let write_in = load_fdset(writefds, nfds)?;
    let except_in = load_fdset(exceptfds, nfds)?;

    loop {
        let result = pselect_poll(nfds, &read_in, &write_in, &except_in)?;
        if result.ready != 0 {
            store_pselect_result(readfds, writefds, exceptfds, &result)?;
            return Ok(result.ready);
        }
        if let Some(deadline) = deadline {
            if crate::timer::get_time_us() >= deadline {
                store_pselect_result(readfds, writefds, exceptfds, &result)?;
                return Ok(0);
            }
            if sleep_on_io_if(Some(deadline), || {
                Ok(pselect_poll(nfds, &read_in, &write_in, &except_in)?.ready == 0)
            })? == WaitOutcome::TimedOut
            {
                let result = pselect_poll(nfds, &read_in, &write_in, &except_in)?;
                store_pselect_result(readfds, writefds, exceptfds, &result)?;
                return Ok(result.ready);
            }
        } else {
            let _ = sleep_on_io_if(None, || {
                Ok(pselect_poll(nfds, &read_in, &write_in, &except_in)?.ready == 0)
            })?;
        }
    }
}

pub fn sys_sendfile(out_fd: usize, in_fd: usize, offset: usize, count: usize) -> SyscallRet {
    let task = current_task().ok_or(SysErrNo::ESRCH)?;
    let mut copied = 0usize;
    let mut file_offset = if offset != 0 {
        Some(super::user::copy_object_from_user::<usize>(offset)?)
    } else {
        None
    };

    while copied < count {
        let chunk_len = (count - copied).min(4096);
        let mut kbuf = alloc::vec![0u8; chunk_len];
        let nread = {
            let inner = task.inner.lock();
            let mut fds = inner.fd_table.lock();
            let input = fds.get_mut(in_fd).ok_or(SysErrNo::EBADF)?;
            if let Some(off) = file_offset {
                super::with_kernel_page_table(|| input.read_at(off, &mut kbuf))?
            } else {
                super::with_kernel_page_table(|| input.read(&mut kbuf))?
            }
        };

        if nread == 0 {
            break;
        }
        kbuf.truncate(nread);

        let nwritten = {
            let inner = task.inner.lock();
            let mut fds = inner.fd_table.lock();
            let output = fds.get_mut(out_fd).ok_or(SysErrNo::EBADF)?;
            super::with_kernel_page_table(|| output.write(&kbuf))?
        };

        copied += nwritten;
        if let Some(off) = file_offset.as_mut() {
            *off += nwritten;
        }
        if nwritten < nread {
            break;
        }
    }

    if let Some(off) = file_offset {
        super::user::copy_object_to_user(offset, &off)?;
    }
    Ok(copied)
}

fn fd_is_pipe(file_desc: &FileDescriptor) -> bool {
    file_desc.is_pipe_read() || file_desc.is_pipe_write()
}

fn fd_has_append_mode(file_desc: &FileDescriptor) -> bool {
    matches!(
        file_desc,
        FileDescriptor::MemFile { append: true, .. }
            | FileDescriptor::Ext4Regular { append: true, .. }
    )
}

fn fd_is_splice_source(file_desc: &FileDescriptor) -> bool {
    matches!(
        file_desc,
        FileDescriptor::MemFile { readable: true, .. }
            | FileDescriptor::Ext4Regular { readable: true, .. }
            | FileDescriptor::PipeRead { .. }
    )
}

fn fd_is_splice_target(file_desc: &FileDescriptor) -> bool {
    matches!(
        file_desc,
        FileDescriptor::MemFile {
            writable: true,
            append: false,
            ..
        } | FileDescriptor::Ext4Regular {
            writable: true,
            append: false,
            ..
        } | FileDescriptor::PipeWrite { .. }
    )
}

fn fd_is_regular_without_read(file_desc: &FileDescriptor) -> bool {
    matches!(
        file_desc,
        FileDescriptor::MemFile {
            readable: false,
            ..
        } | FileDescriptor::Ext4Regular {
            readable: false,
            ..
        }
    )
}

fn fd_is_regular_without_write(file_desc: &FileDescriptor) -> bool {
    matches!(
        file_desc,
        FileDescriptor::MemFile {
            writable: false,
            ..
        } | FileDescriptor::Ext4Regular {
            writable: false,
            ..
        }
    )
}

fn fd_is_copy_file_regular_source(file_desc: &FileDescriptor) -> bool {
    matches!(
        file_desc,
        FileDescriptor::MemFile { readable: true, .. }
            | FileDescriptor::Ext4Regular { readable: true, .. }
    )
}

fn fd_is_copy_file_regular_target(file_desc: &FileDescriptor) -> bool {
    matches!(
        file_desc,
        FileDescriptor::MemFile {
            writable: true,
            append: false,
            ..
        } | FileDescriptor::Ext4Regular {
            writable: true,
            append: false,
            ..
        }
    )
}

fn fd_copy_file_backend(file_desc: &FileDescriptor) -> Option<u8> {
    match file_desc {
        FileDescriptor::MemFile { .. } => Some(0),
        FileDescriptor::Ext4Regular { .. } => Some(1),
        _ => None,
    }
}

fn load_splice_offset(ptr: usize) -> Result<Option<usize>, SysErrNo> {
    if ptr == 0 {
        return Ok(None);
    }
    let offset = super::user::copy_object_from_user::<i64>(ptr)?;
    usize::try_from(offset)
        .map(Some)
        .map_err(|_| SysErrNo::EINVAL)
}

fn store_splice_offset(ptr: usize, offset: usize) -> Result<(), SysErrNo> {
    if ptr == 0 {
        return Ok(());
    }
    let offset = i64::try_from(offset).map_err(|_| SysErrNo::EINVAL)?;
    super::user::copy_object_to_user(ptr, &offset)
}

pub fn sys_copy_file_range(
    fd_in: usize,
    off_in: usize,
    fd_out: usize,
    off_out: usize,
    len: usize,
    flags: usize,
) -> SyscallRet {
    if flags != 0 {
        return Err(SysErrNo::EINVAL);
    }
    checked_io_count(len)?;

    let task = current_task().ok_or(SysErrNo::ESRCH)?;
    let fd_table = task.inner.lock().fd_table.clone();

    {
        let fds = fd_table.lock();
        let input = fds.get(fd_in).ok_or(SysErrNo::EBADF)?;
        let output = fds.get(fd_out).ok_or(SysErrNo::EBADF)?;

        let input_meta = super::with_kernel_page_table(|| crate::fs::metadata_for_fd(input))?;
        let output_meta = super::with_kernel_page_table(|| crate::fs::metadata_for_fd(output))?;
        if input_meta.kind != crate::fs::VfsNodeKind::Regular
            || output_meta.kind != crate::fs::VfsNodeKind::Regular
        {
            return Err(if output_meta.kind == crate::fs::VfsNodeKind::Directory {
                SysErrNo::EISDIR
            } else {
                SysErrNo::EINVAL
            });
        }
        if fd_has_append_mode(output) {
            return Err(SysErrNo::EBADF);
        }
        if !fd_is_copy_file_regular_source(input) || !fd_is_copy_file_regular_target(output) {
            return Err(SysErrNo::EBADF);
        }
        if fd_copy_file_backend(input) != fd_copy_file_backend(output) {
            return Err(SysErrNo::EXDEV);
        }

        if input.same_file_identity(output) {
            let in_start = if off_in != 0 {
                usize::try_from(super::user::copy_object_from_user::<i64>(off_in)?)
                    .map_err(|_| SysErrNo::EINVAL)?
            } else {
                input_meta.size as usize
            };
            let out_start = if off_out != 0 {
                usize::try_from(super::user::copy_object_from_user::<i64>(off_out)?)
                    .map_err(|_| SysErrNo::EINVAL)?
            } else {
                output_meta.size as usize
            };
            let in_end = in_start.saturating_add(len);
            let out_end = out_start.saturating_add(len);
            if len != 0 && in_start < out_end && out_start < in_end {
                return Err(SysErrNo::EINVAL);
            }
        }
    }

    let mut input_offset = load_splice_offset(off_in)?;
    let mut output_offset = load_splice_offset(off_out)?;
    if len == 0 {
        store_splice_offset(off_in, input_offset.unwrap_or(0))?;
        store_splice_offset(off_out, output_offset.unwrap_or(0))?;
        return Ok(0);
    }

    let mut copied = 0usize;
    let mut kbuf = Vec::new();
    while copied < len {
        let want = (len - copied).min(COPY_FILE_RANGE_CHUNK);
        kbuf.resize(want, 0);

        let nread = {
            let mut fds = fd_table.lock();
            let input = fds.get_mut(fd_in).ok_or(SysErrNo::EBADF)?;
            if let Some(offset) = input_offset {
                let fixed_offset = checked_fixed_offset(offset, copied)?;
                read_fixed_at_into_kernel(input, fixed_offset, &mut kbuf[..want])?
            } else {
                read_fd_into_kernel(input, &mut kbuf[..want])?
            }
        };
        if nread == 0 {
            break;
        }

        let mut nwritten_total = 0usize;
        while nwritten_total < nread {
            let nwritten = {
                let mut fds = fd_table.lock();
                let output = fds.get_mut(fd_out).ok_or(SysErrNo::EBADF)?;
                let buf = &kbuf[nwritten_total..nread];
                if let Some(offset) = output_offset {
                    let fixed_offset = checked_fixed_offset(offset, copied + nwritten_total)?;
                    write_fixed_at_from_kernel(output, fixed_offset, buf)?
                } else {
                    write_fd_from_kernel(output, buf)?
                }
            };
            if nwritten == 0 {
                break;
            }
            nwritten_total += nwritten;
        }

        copied += nwritten_total;
        if nwritten_total == 0 || nwritten_total < nread {
            break;
        }
    }

    if let Some(offset) = input_offset.as_mut() {
        *offset = checked_fixed_offset(*offset, copied)?;
    }
    if let Some(offset) = output_offset.as_mut() {
        *offset = checked_fixed_offset(*offset, copied)?;
    }
    store_splice_offset(off_in, input_offset.unwrap_or(0))?;
    store_splice_offset(off_out, output_offset.unwrap_or(0))?;
    Ok(copied)
}

pub fn sys_splice(
    fd_in: usize,
    off_in: usize,
    fd_out: usize,
    off_out: usize,
    len: usize,
    flags: usize,
) -> SyscallRet {
    if flags & !SPLICE_F_ALL != 0 {
        return Err(SysErrNo::EINVAL);
    }
    checked_io_count(len)?;

    let task = current_task().ok_or(SysErrNo::ESRCH)?;
    let fd_table = task.inner.lock().fd_table.clone();

    {
        let fds = fd_table.lock();
        let input = fds.get(fd_in).ok_or(SysErrNo::EBADF)?;
        let output = fds.get(fd_out).ok_or(SysErrNo::EBADF)?;
        let input_is_pipe = fd_is_pipe(input);
        let output_is_pipe = fd_is_pipe(output);

        if !input_is_pipe && !output_is_pipe {
            return Err(SysErrNo::EINVAL);
        }
        if input_is_pipe && off_in != 0 {
            return Err(SysErrNo::ESPIPE);
        }
        if output_is_pipe && off_out != 0 {
            return Err(SysErrNo::ESPIPE);
        }
        if fd_has_append_mode(output) {
            return Err(SysErrNo::EINVAL);
        }
        if matches!(input, FileDescriptor::PipeWrite { .. }) {
            return Err(SysErrNo::EBADF);
        }
        if matches!(output, FileDescriptor::PipeRead { .. }) {
            return Err(SysErrNo::EBADF);
        }
        if fd_is_regular_without_read(input) || fd_is_regular_without_write(output) {
            return Err(SysErrNo::EBADF);
        }
        if !fd_is_splice_source(input) {
            return Err(SysErrNo::EINVAL);
        }
        if !fd_is_splice_target(output) {
            return Err(SysErrNo::EINVAL);
        }
    }

    let mut input_offset = load_splice_offset(off_in)?;
    let mut output_offset = load_splice_offset(off_out)?;
    if len == 0 {
        return Ok(0);
    }

    let nonblock = flags & SPLICE_F_NONBLOCK != 0;
    let mut copied = 0usize;
    let mut kbuf = Vec::new();

    while copied < len {
        let want = (len - copied).min(SPLICE_CHUNK);
        kbuf.resize(want, 0);

        let nread = loop {
            let res = {
                let mut fds = fd_table.lock();
                let input = fds.get_mut(fd_in).ok_or(SysErrNo::EBADF)?;
                if let Some(offset) = input_offset {
                    read_fixed_at_into_kernel(input, offset, &mut kbuf[..want])
                } else {
                    read_fd_into_kernel(input, &mut kbuf[..want])
                }
            };

            match res {
                Ok(n) => break n,
                Err(SysErrNo::EAGAIN) => {
                    if copied != 0 {
                        store_splice_offset(off_in, input_offset.unwrap_or(0))?;
                        store_splice_offset(off_out, output_offset.unwrap_or(0))?;
                        return Ok(copied);
                    }
                    if nonblock {
                        return Err(SysErrNo::EAGAIN);
                    }
                    let _ = wait_read_after_eagain(&fd_table, fd_in)?;
                }
                Err(err) => {
                    if copied != 0 {
                        store_splice_offset(off_in, input_offset.unwrap_or(0))?;
                        store_splice_offset(off_out, output_offset.unwrap_or(0))?;
                        return Ok(copied);
                    }
                    return Err(err);
                }
            }
        };

        if nread == 0 {
            break;
        }

        let mut nwritten_total = 0usize;
        while nwritten_total < nread {
            let res = {
                let mut fds = fd_table.lock();
                let output = fds.get_mut(fd_out).ok_or(SysErrNo::EBADF)?;
                let buf = &kbuf[nwritten_total..nread];
                if let Some(offset) = output_offset {
                    let fixed_offset = checked_fixed_offset(offset, nwritten_total)?;
                    write_fixed_at_from_kernel(output, fixed_offset, buf)
                } else {
                    write_fd_from_kernel(output, buf)
                }
            };

            match res {
                Ok(0) => break,
                Ok(n) => {
                    nwritten_total += n;
                    copied += n;
                }
                Err(SysErrNo::EAGAIN) => {
                    if nonblock {
                        store_splice_offset(off_in, input_offset.unwrap_or(0))?;
                        store_splice_offset(off_out, output_offset.unwrap_or(0))?;
                        return if copied != 0 {
                            Ok(copied)
                        } else {
                            Err(SysErrNo::EAGAIN)
                        };
                    }
                    let _ = wait_write_after_eagain(&fd_table, fd_out)?;
                }
                Err(SysErrNo::EPIPE) if copied == 0 => {
                    crate::syscall::signal::send_sigpipe_to_current();
                    return Err(SysErrNo::EPIPE);
                }
                Err(err) => {
                    if copied != 0 {
                        store_splice_offset(off_in, input_offset.unwrap_or(0))?;
                        store_splice_offset(off_out, output_offset.unwrap_or(0))?;
                        return Ok(copied);
                    }
                    return Err(err);
                }
            }
        }

        if let Some(offset) = input_offset.as_mut() {
            *offset = checked_fixed_offset(*offset, nwritten_total)?;
        }
        if let Some(offset) = output_offset.as_mut() {
            *offset = checked_fixed_offset(*offset, nwritten_total)?;
        }
        if nwritten_total == 0 || nwritten_total < nread {
            break;
        }
    }

    store_splice_offset(off_in, input_offset.unwrap_or(0))?;
    store_splice_offset(off_out, output_offset.unwrap_or(0))?;
    Ok(copied)
}

pub fn sys_truncate(pathname: *const u8, length: usize) -> SyscallRet {
    if length > isize::MAX as usize {
        return Err(SysErrNo::EINVAL);
    }
    let task = current_task().ok_or(SysErrNo::ESRCH)?;
    if length > task.inner.lock().rlimit_fsize {
        return Err(SysErrNo::EFBIG);
    }
    let (_logical_path, host_path) = resolve_host_path(AT_FDCWD, pathname)?;
    super::with_kernel_page_table(|| crate::fs::truncate_path(&host_path, length as u64))?;
    Ok(0)
}

pub fn sys_ftruncate(fd: usize, length: usize) -> SyscallRet {
    if length > isize::MAX as usize {
        return Err(SysErrNo::EINVAL);
    }
    let task = current_task().ok_or(SysErrNo::ESRCH)?;
    let inner = task.inner.lock();
    if length > inner.rlimit_fsize {
        return Err(SysErrNo::EFBIG);
    }
    let mut fds = inner.fd_table.lock();
    let file_desc = fds.get_mut(fd).ok_or(SysErrNo::EBADF)?;
    super::with_kernel_page_table(|| crate::fs::truncate_fd(file_desc, length as u64))?;
    Ok(0)
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
                let st = super::with_kernel_page_table(|| stat_for_fd(file_desc))?;
                copy_kstat_out(statbuf, &st)?;
                Ok(0)
            }
            None => Err(SysErrNo::EBADF),
        }
    } else {
        Err(SysErrNo::ESRCH)
    }
}

pub fn sys_statx(
    dirfd: isize,
    pathname: *const u8,
    flags: usize,
    mask: usize,
    statxbuf: *mut u8,
) -> SyscallRet {
    if statxbuf.is_null() {
        return Err(SysErrNo::EFAULT);
    }
    check_statx_flags(flags)?;
    check_statx_mask(mask)?;

    #[cfg(feature = "buildstorm-diagnostics")]
    let started_at = crate::timer::get_time_us();
    let path = read_user_path(pathname);
    #[cfg(feature = "buildstorm-diagnostics")]
    crate::buildstorm_diagnostics::note_phase(
        7,
        crate::timer::get_time_us().saturating_sub(started_at),
    );
    let path = path?;
    let st = if path.is_empty() {
        if flags & AT_EMPTY_PATH == 0 {
            return Err(SysErrNo::ENOENT);
        }
        stat_empty_path(dirfd)?
    } else {
        #[cfg(feature = "buildstorm-diagnostics")]
        let started_at = crate::timer::get_time_us();
        let resolved = resolve_host_path_str(dirfd, &path);
        #[cfg(feature = "buildstorm-diagnostics")]
        crate::buildstorm_diagnostics::note_phase(
            8,
            crate::timer::get_time_us().saturating_sub(started_at),
        );
        let (_logical_path, host_path) = resolved?;
        let follow = flags & AT_SYMLINK_NOFOLLOW == 0;
        #[cfg(feature = "buildstorm-diagnostics")]
        let started_at = crate::timer::get_time_us();
        let result = super::with_kernel_page_table(|| stat_for_path(&host_path, follow));
        #[cfg(feature = "buildstorm-diagnostics")]
        crate::buildstorm_diagnostics::note_phase(
            9,
            crate::timer::get_time_us().saturating_sub(started_at),
        );
        result?
    };

    let statx = make_statx(&st, mask);
    #[cfg(feature = "buildstorm-diagnostics")]
    let started_at = crate::timer::get_time_us();
    let copied = copy_statx_out(statxbuf, &statx);
    #[cfg(feature = "buildstorm-diagnostics")]
    crate::buildstorm_diagnostics::note_phase(
        10,
        crate::timer::get_time_us().saturating_sub(started_at),
    );
    copied?;
    Ok(0)
}

pub fn sys_newfstatat(
    dirfd: isize,
    pathname: *const u8,
    statbuf: *mut u8,
    flags: usize,
) -> SyscallRet {
    if statbuf.is_null() {
        return Err(SysErrNo::EFAULT);
    }
    check_fstatat_flags(flags)?;

    let path = read_user_path(pathname)?;
    let st = if path.is_empty() {
        if flags & AT_EMPTY_PATH == 0 {
            return Err(SysErrNo::ENOENT);
        }
        stat_empty_path(dirfd)?
    } else {
        let (_logical_path, host_path) = resolve_host_path_str(dirfd, &path)?;
        let follow = flags & AT_SYMLINK_NOFOLLOW == 0;
        super::with_kernel_page_table(|| stat_for_path(&host_path, follow))?
    };
    copy_kstat_out(statbuf, &st)?;
    Ok(0)
}

/// write 系统调用的安全版本
///
/// 用于从内核态调用，buf 是内核空间指针
pub fn sys_utimensat(
    dirfd: isize,
    pathname: *const u8,
    times: *const TimeSpec,
    flags: usize,
) -> SyscallRet {
    if flags & !(AT_SYMLINK_NOFOLLOW | AT_EMPTY_PATH) != 0 {
        return Err(SysErrNo::EINVAL);
    }
    let (atime, mtime, permission) = parse_utimens_times_with_permission(times)?;

    if pathname.is_null() {
        if dirfd == AT_FDCWD {
            return Err(SysErrNo::EFAULT);
        }
        let fd = usize::try_from(dirfd).map_err(|_| SysErrNo::EBADF)?;
        let task = current_task().ok_or(SysErrNo::ESRCH)?;
        let inner = task.inner.lock();
        let mut fds = inner.fd_table.lock();
        let file_desc = fds.get_mut(fd).ok_or(SysErrNo::EBADF)?;
        return super::with_kernel_page_table(|| {
            crate::fs::set_times_fd(file_desc, atime, mtime, permission)
        })
        .map(|_| 0);
    }

    let path = read_user_path(pathname)?;
    if path.is_empty() {
        if flags & AT_EMPTY_PATH == 0 {
            return Err(SysErrNo::ENOENT);
        }
        let fd = usize::try_from(dirfd).map_err(|_| SysErrNo::EBADF)?;
        let task = current_task().ok_or(SysErrNo::ESRCH)?;
        let inner = task.inner.lock();
        let mut fds = inner.fd_table.lock();
        let file_desc = fds.get_mut(fd).ok_or(SysErrNo::EBADF)?;
        return super::with_kernel_page_table(|| {
            crate::fs::set_times_fd(file_desc, atime, mtime, permission)
        })
        .map(|_| 0);
    }

    let (_logical_path, host_path) = resolve_host_path_str(dirfd, &path)?;
    let follow = flags & AT_SYMLINK_NOFOLLOW == 0;
    super::with_kernel_page_table(|| {
        crate::fs::set_times_path(&host_path, follow, atime, mtime, permission)
    })
    .map(|_| 0)
}

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

pub fn sys_fallocate(fd: usize, mode: usize, offset: usize, len: usize) -> SyscallRet {
    if mode & !FALLOC_FL_KEEP_SIZE != 0 {
        return Err(SysErrNo::EOPNOTSUPP);
    }
    if offset > isize::MAX as usize || len > isize::MAX as usize {
        return Err(SysErrNo::EINVAL);
    }

    let task = current_task().ok_or(SysErrNo::ESRCH)?;
    let inner = task.inner.lock();
    let mut fds = inner.fd_table.lock();
    let file_desc = fds.get_mut(fd).ok_or(SysErrNo::EBADF)?;
    super::with_kernel_page_table(|| {
        file_desc.allocate(offset, len, mode & FALLOC_FL_KEEP_SIZE != 0)
    })?;
    Ok(0)
}
