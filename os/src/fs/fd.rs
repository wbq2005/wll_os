use alloc::collections::VecDeque;
/// 文件描述符管理
///
/// 管理进程打开的文件，实现 POSIX 风格的文件描述符表
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use lazy_static::lazy_static;
use spin::Mutex;

use crate::fs::block_dev;
use crate::fs::ext4_vol;
use crate::fs::vfs::VfsNodeKind;
use crate::fs::MEM_FS;
use crate::fs::{FileTimes, MemNodeMetadata};
use crate::task::wait_queue::WaitKey;
use crate::utils::error::SysErrNo;

/// 最大文件描述符数量
pub const MAX_FD_NUM: usize = 1024;

/// 标准文件描述符
pub const STDIN_FD: usize = 0;
pub const STDOUT_FD: usize = 1;
pub const STDERR_FD: usize = 2;
pub const FD_CLOEXEC: usize = 1;

/// 文件描述符偏移量
pub type FileOffset = usize;

const CONSOLE_LINE_CAP: usize = 512;
const MAX_FILE_OFFSET: usize = isize::MAX as usize;
const PIPE_CAPACITY: usize = 64 * 1024;
const MEM_FILE_INLINE_LIMIT: usize = 1024 * 1024;
const MEM_FILE_CHUNK_SIZE: usize = 64 * 1024;
const SOCK_STREAM: usize = 1;
const SOCK_DGRAM: usize = 2;
const PIPE_WAIT_READABLE: usize = 1;
const PIPE_WAIT_WRITABLE: usize = 2;
const EVENTFD_WAIT_READABLE: usize = 1;
const EVENTFD_WAIT_WRITABLE: usize = 2;
const PIPE_SMALL_COPY: usize = 64;

fn pipe_wait_key(state: &Arc<Mutex<PipeState>>, event: usize) -> WaitKey {
    WaitKey::new(Arc::as_ptr(state) as usize, event)
}

fn wake_pipe_readers(state: &Arc<Mutex<PipeState>>) {
    crate::task::wait_queue::wake_io_keyed_waiters(pipe_wait_key(state, PIPE_WAIT_READABLE));
}

fn wake_pipe_writers(state: &Arc<Mutex<PipeState>>) {
    crate::task::wait_queue::wake_io_keyed_waiters(pipe_wait_key(state, PIPE_WAIT_WRITABLE));
}

fn eventfd_wait_key(state: &Arc<Mutex<EventFdState>>, event: usize) -> WaitKey {
    WaitKey::new(Arc::as_ptr(state) as usize, event)
}

fn wake_eventfd_readers(state: &Arc<Mutex<EventFdState>>) {
    crate::task::wait_queue::wake_io_keyed_waiters(eventfd_wait_key(state, EVENTFD_WAIT_READABLE));
}

fn wake_eventfd_writers(state: &Arc<Mutex<EventFdState>>) {
    crate::task::wait_queue::wake_io_keyed_waiters(eventfd_wait_key(state, EVENTFD_WAIT_WRITABLE));
}

fn is_dev_null_path(path: &str) -> bool {
    matches!(path, "/dev/null" | "/glibc/dev/null" | "/musl/dev/null")
}

fn is_dev_zero_path(path: &str) -> bool {
    matches!(path, "/dev/zero" | "/glibc/dev/zero" | "/musl/dev/zero")
}

#[derive(Debug, Clone)]
pub enum MemFileContent {
    Inline(Vec<u8>),
    Chunked {
        len: usize,
        chunks: Vec<Option<Vec<u8>>>,
    },
}

impl MemFileContent {
    pub fn new() -> Self {
        Self::Inline(Vec::new())
    }

    pub fn from_slice(content: &[u8]) -> Self {
        if content.len() <= MEM_FILE_INLINE_LIMIT {
            return Self::Inline(content.to_vec());
        }
        let mut chunks = Vec::new();
        for part in content.chunks(MEM_FILE_CHUNK_SIZE) {
            if part.iter().all(|byte| *byte == 0) {
                chunks.push(None);
            } else {
                chunks.push(Some(part.to_vec()));
            }
        }
        Self::Chunked {
            len: content.len(),
            chunks,
        }
    }

    pub fn len(&self) -> usize {
        match self {
            Self::Inline(content) => content.len(),
            Self::Chunked { len, .. } => *len,
        }
    }

    pub fn clear(&mut self) {
        *self = Self::new();
    }

    pub fn is_elf_image(&self) -> bool {
        let mut magic = [0u8; 4];
        self.read_at(0, &mut magic) == 4 && magic == [0x7f, b'E', b'L', b'F']
    }

    pub fn read_at(&self, offset: usize, buf: &mut [u8]) -> usize {
        let len = self.len();
        if offset >= len || buf.is_empty() {
            return 0;
        }
        let to_read = buf.len().min(len - offset);
        match self {
            Self::Inline(content) => {
                buf[..to_read].copy_from_slice(&content[offset..offset + to_read]);
            }
            Self::Chunked { chunks, .. } => {
                buf[..to_read].fill(0);
                let mut copied = 0usize;
                while copied < to_read {
                    let pos = offset + copied;
                    let chunk_idx = pos / MEM_FILE_CHUNK_SIZE;
                    let chunk_off = pos % MEM_FILE_CHUNK_SIZE;
                    let n = (to_read - copied).min(MEM_FILE_CHUNK_SIZE - chunk_off);
                    if let Some(Some(chunk)) = chunks.get(chunk_idx) {
                        if chunk_off < chunk.len() {
                            let copy_len = n.min(chunk.len() - chunk_off);
                            buf[copied..copied + copy_len]
                                .copy_from_slice(&chunk[chunk_off..chunk_off + copy_len]);
                        }
                    }
                    copied += n;
                }
            }
        }
        to_read
    }

    pub fn write_at(&mut self, offset: usize, buf: &[u8]) -> usize {
        let end = offset + buf.len();
        self.resize(end);
        match self {
            Self::Inline(content) => {
                content[offset..end].copy_from_slice(buf);
            }
            Self::Chunked { chunks, .. } => {
                let mut copied = 0usize;
                while copied < buf.len() {
                    let pos = offset + copied;
                    let chunk_idx = pos / MEM_FILE_CHUNK_SIZE;
                    let chunk_off = pos % MEM_FILE_CHUNK_SIZE;
                    let n = (buf.len() - copied).min(MEM_FILE_CHUNK_SIZE - chunk_off);
                    while chunks.len() <= chunk_idx {
                        chunks.push(None);
                    }
                    let chunk = chunks[chunk_idx].get_or_insert_with(Vec::new);
                    if chunk.len() < chunk_off + n {
                        chunk.resize(chunk_off + n, 0);
                    }
                    chunk[chunk_off..chunk_off + n].copy_from_slice(&buf[copied..copied + n]);
                    copied += n;
                }
            }
        }
        end
    }

    pub fn resize(&mut self, new_len: usize) {
        match self {
            Self::Inline(content) if new_len <= MEM_FILE_INLINE_LIMIT => {
                content.resize(new_len, 0);
            }
            Self::Inline(content) => {
                let old = core::mem::take(content);
                let mut chunks = Vec::new();
                for part in old.chunks(MEM_FILE_CHUNK_SIZE) {
                    if part.iter().all(|byte| *byte == 0) {
                        chunks.push(None);
                    } else {
                        chunks.push(Some(part.to_vec()));
                    }
                }
                *self = Self::Chunked {
                    len: old.len(),
                    chunks,
                };
                self.resize(new_len);
            }
            Self::Chunked { len, chunks } => {
                let needed = if new_len == 0 {
                    0
                } else {
                    (new_len + MEM_FILE_CHUNK_SIZE - 1) / MEM_FILE_CHUNK_SIZE
                };
                while chunks.len() < needed {
                    chunks.push(None);
                }
                chunks.truncate(needed);
                if let Some(Some(last)) = chunks.last_mut() {
                    let start = (needed - 1) * MEM_FILE_CHUNK_SIZE;
                    let target = (new_len - start).min(MEM_FILE_CHUNK_SIZE);
                    last.truncate(target);
                }
                *len = new_len;
                if new_len <= MEM_FILE_INLINE_LIMIT {
                    let flattened = self.to_vec();
                    *self = Self::Inline(flattened);
                }
            }
        }
    }

    pub fn to_vec(&self) -> Vec<u8> {
        match self {
            Self::Inline(content) => content.clone(),
            Self::Chunked { len, .. } => {
                let mut out = alloc::vec![0u8; *len];
                self.read_at(0, &mut out);
                out
            }
        }
    }
}

fn refresh_mem_file(
    name: &str,
    content: &mut MemFileContent,
    times: &mut FileTimes,
    linked: &mut bool,
) -> bool {
    if is_dev_null_path(name) || is_dev_zero_path(name) {
        return true;
    }
    if !*linked {
        return false;
    }
    if let Some(file) = MEM_FS.lock().get_file(name) {
        let (snapshot, file_times) = file.snapshot();
        *content = snapshot;
        *times = file_times;
        true
    } else {
        *linked = false;
        false
    }
}

fn write_mem_content(content: &mut MemFileContent, offset: usize, buf: &[u8]) -> usize {
    content.write_at(offset, buf)
}

lazy_static! {
    static ref CONSOLE_LINE_BUFFERS: Mutex<Vec<(usize, Vec<u8>)>> = Mutex::new(Vec::new());
}

fn write_console_buffered(buf: &[u8]) {
    let writer = crate::task::current_task()
        .map(|task| task.thread_group.tgid())
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
    pub const O_NOFOLLOW: u32 = 0o00400000;
    pub const O_NOATIME: u32 = 0o01000000;
    pub const O_TMPFILE: u32 = 0o20200000;
    pub const O_PATH: u32 = 0o10000000;
    pub const O_CLOEXEC: u32 = 0o2000000;
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
        content: MemFileContent,
        times: FileTimes,
        offset: FileOffset,
        readable: bool,
        writable: bool,
        append: bool,
        linked: bool,
        node: Option<MemNodeMetadata>,
    },
    /// 内存目录
    MemDir {
        path: String,
        host_path: String,
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
    Ext4Dir {
        path: String,
        ino: u32,
        offset: usize,
    },
    Path {
        logical_path: String,
        host_path: String,
        kind: VfsNodeKind,
        flags: u32,
    },
    LoopControl,
    LoopDevice {
        index: usize,
        offset: usize,
        readable: bool,
        writable: bool,
    },
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
    Socket {
        state: Arc<Mutex<SocketState>>,
    },
    EventFd {
        state: Arc<Mutex<EventFdState>>,
    },
    Epoll {
        state: Arc<Mutex<EpollState>>,
    },
}

#[derive(Debug)]
pub struct PipeState {
    buf: VecDeque<u8>,
    readers: usize,
    writers: usize,
}

impl PipeState {
    fn new() -> Self {
        Self {
            buf: VecDeque::with_capacity(PIPE_CAPACITY),
            readers: 1,
            writers: 1,
        }
    }
}

fn pipe_read_buffer(pipe: &mut PipeState, buf: &mut [u8]) -> usize {
    let n = buf.len().min(pipe.buf.len());
    if n <= PIPE_SMALL_COPY {
        for dst in &mut buf[..n] {
            *dst = pipe.buf.pop_front().unwrap_or(0);
        }
        return n;
    }

    {
        let (front, back) = pipe.buf.as_slices();
        let front_len = n.min(front.len());
        buf[..front_len].copy_from_slice(&front[..front_len]);
        let back_len = n - front_len;
        if back_len != 0 {
            buf[front_len..n].copy_from_slice(&back[..back_len]);
        }
    }
    pipe.buf.drain(..n);
    n
}

fn pipe_write_buffer(pipe: &mut PipeState, buf: &[u8], available: usize) -> usize {
    let n = buf.len().min(available);
    if n <= PIPE_SMALL_COPY {
        for &byte in &buf[..n] {
            pipe.buf.push_back(byte);
        }
    } else {
        pipe.buf.extend(buf[..n].iter().copied());
    }
    n
}

#[derive(Debug)]
pub struct SocketPacket {
    pub data: Vec<u8>,
    pub addr: Vec<u8>,
}

#[derive(Debug)]
pub struct SocketState {
    pub domain: i32,
    pub sock_type: usize,
    pub protocol: i32,
    pub nonblock: bool,
    pub bound: bool,
    pub listening: bool,
    pub connected: bool,
    pub shutdown_read: bool,
    pub shutdown_write: bool,
    pub local_addr: Option<Vec<u8>>,
    pub peer_addr: Option<Vec<u8>>,
    pub peer: Option<Arc<Mutex<SocketState>>>,
    pub rx_buf: VecDeque<u8>,
    pub dgram_queue: VecDeque<SocketPacket>,
    pub pending: VecDeque<Arc<Mutex<SocketState>>>,
    pub backlog: usize,
    pub reuse_addr: bool,
    pub reuse_port: bool,
    pub keepalive: bool,
    pub broadcast: bool,
    pub tcp_nodelay: bool,
    pub sndbuf: usize,
    pub rcvbuf: usize,
    pub send_timeout_us: Option<usize>,
    pub recv_timeout_us: Option<usize>,
    pub error: i32,
}

#[derive(Debug)]
pub struct EventFdState {
    pub counter: u64,
    pub semaphore: bool,
    pub nonblock: bool,
}

#[derive(Debug, Clone)]
pub struct EpollInterest {
    pub fd: usize,
    pub file: FileDescriptor,
    pub events: u32,
    pub data: u64,
}

#[derive(Debug)]
pub struct EpollState {
    pub interests: Vec<EpollInterest>,
}

impl EpollState {
    pub fn new() -> Self {
        Self {
            interests: Vec::new(),
        }
    }
}

impl SocketState {
    pub fn new(domain: i32, sock_type: usize, protocol: i32, nonblock: bool) -> Self {
        Self {
            domain,
            sock_type,
            protocol,
            nonblock,
            bound: false,
            listening: false,
            connected: false,
            shutdown_read: false,
            shutdown_write: false,
            local_addr: None,
            peer_addr: None,
            peer: None,
            rx_buf: VecDeque::new(),
            dgram_queue: VecDeque::new(),
            pending: VecDeque::new(),
            backlog: 0,
            reuse_addr: false,
            reuse_port: false,
            keepalive: false,
            broadcast: false,
            tcp_nodelay: false,
            sndbuf: 64 * 1024,
            rcvbuf: 64 * 1024,
            send_timeout_us: None,
            recv_timeout_us: None,
            error: 0,
        }
    }

    pub fn is_stream(&self) -> bool {
        self.sock_type == SOCK_STREAM
    }

    pub fn is_datagram(&self) -> bool {
        self.sock_type == SOCK_DGRAM
    }
}

impl EventFdState {
    pub fn new(counter: u64, semaphore: bool, nonblock: bool) -> Self {
        Self {
            counter,
            semaphore,
            nonblock,
        }
    }
}

#[derive(Debug, Clone)]
pub struct DirEntryRecord {
    pub name: String,
    pub is_dir: bool,
}

impl FileDescriptor {
    fn checked_file_end(offset: usize, len: usize) -> Result<usize, SysErrNo> {
        let end = offset.checked_add(len).ok_or(SysErrNo::EFBIG)?;
        if end > MAX_FILE_OFFSET {
            return Err(SysErrNo::EFBIG);
        }
        Ok(end)
    }

    fn checked_seek_from(base: usize, delta: isize) -> Result<usize, SysErrNo> {
        let next = if delta >= 0 {
            base.checked_add(delta as usize)
        } else {
            base.checked_sub(delta.unsigned_abs())
        }
        .ok_or(SysErrNo::EINVAL)?;
        if next > MAX_FILE_OFFSET {
            return Err(SysErrNo::EINVAL);
        }
        Ok(next)
    }

    pub fn pipe_read_nonblocking(&self) -> bool {
        matches!(self, FileDescriptor::PipeRead { nonblock: true, .. })
    }

    pub fn is_pipe_read(&self) -> bool {
        matches!(self, FileDescriptor::PipeRead { .. })
    }

    pub fn pipe_write_nonblocking(&self) -> bool {
        matches!(self, FileDescriptor::PipeWrite { nonblock: true, .. })
    }

    pub fn is_pipe_write(&self) -> bool {
        matches!(self, FileDescriptor::PipeWrite { .. })
    }

    pub fn same_file_identity(&self, other: &Self) -> bool {
        match (self, other) {
            (
                FileDescriptor::MemFile { name: left, .. },
                FileDescriptor::MemFile { name: right, .. },
            ) => left == right,
            (
                FileDescriptor::Ext4Regular { ino: left, .. },
                FileDescriptor::Ext4Regular { ino: right, .. },
            ) => left == right,
            (
                FileDescriptor::Path {
                    host_path: left, ..
                },
                FileDescriptor::Path {
                    host_path: right, ..
                },
            ) => left == right,
            _ => false,
        }
    }

    pub fn socket_state(&self) -> Option<Arc<Mutex<SocketState>>> {
        match self {
            FileDescriptor::Socket { state } => Some(state.clone()),
            _ => None,
        }
    }

    pub fn pipe_read_would_block(&self) -> bool {
        match self {
            FileDescriptor::PipeRead { state, .. } => {
                let pipe = state.lock();
                pipe.buf.is_empty() && pipe.writers > 0
            }
            _ => false,
        }
    }

    pub fn pipe_read_wait_key(&self) -> Option<WaitKey> {
        match self {
            FileDescriptor::PipeRead { state, .. } => {
                Some(pipe_wait_key(state, PIPE_WAIT_READABLE))
            }
            _ => None,
        }
    }

    pub fn pipe_write_would_block(&self) -> bool {
        match self {
            FileDescriptor::PipeWrite { state, .. } => {
                let pipe = state.lock();
                pipe.readers > 0 && pipe.buf.len() >= PIPE_CAPACITY
            }
            _ => false,
        }
    }

    pub fn pipe_write_wait_key(&self) -> Option<WaitKey> {
        match self {
            FileDescriptor::PipeWrite { state, .. } => {
                Some(pipe_wait_key(state, PIPE_WAIT_WRITABLE))
            }
            _ => None,
        }
    }

    pub fn eventfd_read_nonblocking(&self) -> bool {
        matches!(self, FileDescriptor::EventFd { state } if state.lock().nonblock)
    }

    pub fn eventfd_read_would_block(&self) -> bool {
        match self {
            FileDescriptor::EventFd { state } => state.lock().counter == 0,
            _ => false,
        }
    }

    pub fn eventfd_read_wait_key(&self) -> Option<WaitKey> {
        match self {
            FileDescriptor::EventFd { state } => {
                Some(eventfd_wait_key(state, EVENTFD_WAIT_READABLE))
            }
            _ => None,
        }
    }

    pub fn eventfd_write_nonblocking(&self) -> bool {
        matches!(self, FileDescriptor::EventFd { state } if state.lock().nonblock)
    }

    pub fn eventfd_write_would_block(&self) -> bool {
        match self {
            FileDescriptor::EventFd { state } => state.lock().counter != 0,
            _ => false,
        }
    }

    pub fn eventfd_write_wait_key(&self) -> Option<WaitKey> {
        match self {
            FileDescriptor::EventFd { state } => {
                Some(eventfd_wait_key(state, EVENTFD_WAIT_WRITABLE))
            }
            _ => None,
        }
    }

    pub fn set_status_flags(&mut self, flags: usize) {
        let append = (flags & (open_flags::O_APPEND as usize)) != 0;
        let nonblock = (flags & pipe_flags::O_NONBLOCK) != 0;
        match self {
            FileDescriptor::MemFile {
                append: current, ..
            }
            | FileDescriptor::Ext4Regular {
                append: current, ..
            } => {
                *current = append;
            }
            FileDescriptor::PipeRead {
                nonblock: current, ..
            }
            | FileDescriptor::PipeWrite {
                nonblock: current, ..
            } => {
                *current = nonblock;
            }
            FileDescriptor::Socket { state } => {
                state.lock().nonblock = nonblock;
            }
            FileDescriptor::EventFd { state } => {
                state.lock().nonblock = nonblock;
            }
            _ => {}
        }
    }

    pub fn poll_hup(&self) -> bool {
        match self {
            FileDescriptor::PipeRead { state, .. } => state.lock().writers == 0,
            FileDescriptor::Socket { state } => {
                let socket = state.lock();
                socket.shutdown_read && socket.shutdown_write
            }
            _ => false,
        }
    }

    pub fn poll_error(&self) -> bool {
        match self {
            FileDescriptor::PipeWrite { state, .. } => state.lock().readers == 0,
            FileDescriptor::Socket { state } => state.lock().error != 0,
            _ => false,
        }
    }

    pub fn poll_read_ready(&self) -> bool {
        match self {
            FileDescriptor::Stdin => true,
            FileDescriptor::MemFile { readable, .. } => *readable,
            FileDescriptor::Ext4Regular { readable, .. } => *readable,
            FileDescriptor::LoopDevice { readable, .. } => *readable,
            FileDescriptor::PipeRead { state, .. } => {
                let pipe = state.lock();
                !pipe.buf.is_empty() || pipe.writers == 0
            }
            FileDescriptor::EventFd { state } => state.lock().counter != 0,
            FileDescriptor::Socket { state } => {
                let socket = state.lock();
                socket.shutdown_read
                    || !socket.rx_buf.is_empty()
                    || !socket.dgram_queue.is_empty()
                    || !socket.pending.is_empty()
            }
            _ => false,
        }
    }

    pub fn poll_write_ready(&self) -> bool {
        match self {
            FileDescriptor::Stdout | FileDescriptor::Stderr => true,
            FileDescriptor::MemFile { writable, .. } => *writable,
            FileDescriptor::Ext4Regular { writable, .. } => *writable,
            FileDescriptor::LoopDevice { writable, .. } => *writable,
            FileDescriptor::PipeWrite { state, .. } => {
                let pipe = state.lock();
                pipe.readers > 0 && pipe.buf.len() < PIPE_CAPACITY
            }
            FileDescriptor::EventFd { state } => state.lock().counter < u64::MAX - 1,
            FileDescriptor::Socket { state } => {
                let socket = state.lock();
                (socket.connected || socket.is_datagram())
                    && !socket.shutdown_write
                    && socket.error == 0
            }
            _ => false,
        }
    }

    /// 检查文件是否可读
    pub fn readable(&self) -> bool {
        match self {
            FileDescriptor::Stdin => true,
            FileDescriptor::Stdout => false,
            FileDescriptor::Stderr => false,
            FileDescriptor::MemFile { readable, .. } => *readable,
            FileDescriptor::MemDir { .. } => false,
            FileDescriptor::Ext4Regular { readable, .. } => *readable,
            FileDescriptor::Ext4Dir { .. } => false,
            FileDescriptor::Path { .. } => false,
            FileDescriptor::LoopControl => false,
            FileDescriptor::LoopDevice { readable, .. } => *readable,
            FileDescriptor::PipeRead { .. } => true,
            FileDescriptor::PipeWrite { .. } => false,
            FileDescriptor::Socket { .. } => true,
            FileDescriptor::EventFd { .. } => true,
            FileDescriptor::Epoll { .. } => false,
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
            FileDescriptor::Path { .. } => false,
            FileDescriptor::LoopControl => false,
            FileDescriptor::LoopDevice { writable, .. } => *writable,
            FileDescriptor::PipeRead { .. } => false,
            FileDescriptor::PipeWrite { .. } => true,
            FileDescriptor::Socket { .. } => true,
            FileDescriptor::EventFd { .. } => true,
            FileDescriptor::Epoll { .. } => false,
        }
    }

    /// 读取数据
    pub fn read(&mut self, buf: &mut [u8]) -> Result<usize, SysErrNo> {
        match self {
            FileDescriptor::Stdin => {
                if interactive_stdin_enabled() {
                    return read_interactive_stdin(buf);
                }
                trace_stdin("[stdin-trace] read enter\n");
                let c = crate::console::getchar();
                if let Some(c) = c {
                    if buf.is_empty() {
                        return Ok(0);
                    }
                    trace_stdin_byte(c);
                    buf[0] = c;
                    Ok(1)
                } else {
                    // No input available. In the evaluation environment there is no
                    // interactive terminal, so return EOF (0) immediately instead
                    // of EAGAIN. EAGAIN would cause blocking reads to spin forever.
                    Ok(0)
                }
            }
            FileDescriptor::MemFile {
                name,
                content,
                times,
                offset,
                readable,
                linked,
                ..
            } => {
                if !*readable {
                    return Err(SysErrNo::EBADF);
                }
                if is_dev_null_path(name) {
                    return Ok(0);
                }
                if is_dev_zero_path(name) {
                    buf.fill(0);
                    *offset = (*offset).saturating_add(buf.len());
                    return Ok(buf.len());
                }
                refresh_mem_file(name, content, times, linked);
                if *offset >= content.len() {
                    return Ok(0);
                }
                let to_read = content.read_at(*offset, buf);
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
            FileDescriptor::Path { .. } => Err(SysErrNo::EBADF),
            FileDescriptor::LoopControl => Err(SysErrNo::ENXIO),
            FileDescriptor::LoopDevice {
                index,
                offset,
                readable,
                ..
            } => {
                if !*readable {
                    return Err(SysErrNo::EBADF);
                }
                let n = block_dev::loop_read_at(*index, *offset, buf)?;
                *offset = offset.saturating_add(n);
                Ok(n)
            }
            FileDescriptor::PipeRead { state, .. } => {
                if buf.is_empty() {
                    return Ok(0);
                }
                let (n, wake_writers) = {
                    let mut pipe = state.lock();
                    if pipe.buf.is_empty() {
                        if pipe.writers == 0 {
                            return Ok(0);
                        }
                        return Err(SysErrNo::EAGAIN);
                    }
                    let was_full = pipe.buf.len() == PIPE_CAPACITY;
                    let n = pipe_read_buffer(&mut pipe, buf);
                    (n, was_full && pipe.buf.len() < PIPE_CAPACITY)
                };
                if wake_writers {
                    wake_pipe_writers(state);
                }
                Ok(n)
            }
            FileDescriptor::PipeWrite { .. } => Err(SysErrNo::EBADF),
            FileDescriptor::EventFd { state } => {
                if buf.len() < core::mem::size_of::<u64>() {
                    return Err(SysErrNo::EINVAL);
                }
                let (value, wake_writers) = {
                    let mut eventfd = state.lock();
                    if eventfd.counter == 0 {
                        return Err(SysErrNo::EAGAIN);
                    }
                    if eventfd.semaphore {
                        eventfd.counter -= 1;
                        (1u64, true)
                    } else {
                        let value = eventfd.counter;
                        eventfd.counter = 0;
                        (value, true)
                    }
                };
                buf[..core::mem::size_of::<u64>()].copy_from_slice(&value.to_ne_bytes());
                if wake_writers {
                    wake_eventfd_writers(state);
                }
                Ok(core::mem::size_of::<u64>())
            }
            FileDescriptor::Socket { state } => {
                if buf.is_empty() {
                    return Ok(0);
                }
                let mut socket = state.lock();
                if socket.shutdown_read {
                    return Ok(0);
                }
                if socket.is_stream() && !socket.connected {
                    return Err(SysErrNo::ENOTCONN);
                }
                let n = if socket.is_datagram() {
                    let Some(packet) = socket.dgram_queue.pop_front() else {
                        return Err(SysErrNo::EAGAIN);
                    };
                    let n = buf.len().min(packet.data.len());
                    buf[..n].copy_from_slice(&packet.data[..n]);
                    n
                } else {
                    if socket.rx_buf.is_empty() {
                        return Err(SysErrNo::EAGAIN);
                    }
                    let mut n = 0usize;
                    while n < buf.len() {
                        if let Some(b) = socket.rx_buf.pop_front() {
                            buf[n] = b;
                            n += 1;
                        } else {
                            break;
                        }
                    }
                    n
                };
                if n > 0 {
                    crate::task::wait_queue::wake_io_waiters();
                }
                Ok(n)
            }
            _ => Err(SysErrNo::EBADF),
        }
    }

    /// 写入数据
    /// Read from a regular file at a fixed offset without changing the fd offset.
    pub fn read_at(&mut self, offset: usize, buf: &mut [u8]) -> Result<usize, SysErrNo> {
        match self {
            FileDescriptor::MemFile {
                name,
                content,
                times,
                readable,
                linked,
                ..
            } => {
                if !*readable {
                    return Err(SysErrNo::EBADF);
                }
                if is_dev_null_path(name) {
                    return Ok(0);
                }
                if is_dev_zero_path(name) {
                    buf.fill(0);
                    return Ok(buf.len());
                }
                refresh_mem_file(name, content, times, linked);
                if offset >= content.len() {
                    return Ok(0);
                }
                let to_read = content.read_at(offset, buf);
                Ok(to_read)
            }
            FileDescriptor::Ext4Regular { ino, readable, .. } => {
                if !*readable {
                    return Err(SysErrNo::EBADF);
                }
                ext4_vol::ext4_read_at(*ino, offset, buf)
            }
            FileDescriptor::MemDir { .. } | FileDescriptor::Ext4Dir { .. } => Err(SysErrNo::EISDIR),
            FileDescriptor::Path { .. } => Err(SysErrNo::EBADF),
            FileDescriptor::LoopControl => Err(SysErrNo::ENXIO),
            FileDescriptor::LoopDevice {
                index, readable, ..
            } => {
                if !*readable {
                    return Err(SysErrNo::EBADF);
                }
                block_dev::loop_read_at(*index, offset, buf)
            }
            FileDescriptor::PipeRead { .. } | FileDescriptor::PipeWrite { .. } => {
                Err(SysErrNo::ESPIPE)
            }
            FileDescriptor::Socket { .. } => Err(SysErrNo::ESPIPE),
            _ => Err(SysErrNo::EBADF),
        }
    }

    pub fn write_at(&mut self, offset: usize, buf: &[u8]) -> Result<usize, SysErrNo> {
        match self {
            FileDescriptor::MemFile {
                name,
                content,
                times,
                writable,
                linked,
                ..
            } => {
                if !*writable {
                    return Err(SysErrNo::EBADF);
                }
                if is_dev_null_path(name) || is_dev_zero_path(name) {
                    return Ok(buf.len());
                }
                if buf.is_empty() {
                    return Ok(0);
                }
                let mem_live = refresh_mem_file(name, content, times, linked);
                Self::checked_file_end(offset, buf.len())?;
                write_mem_content(content, offset, buf);
                times.touch_modified();
                if mem_live {
                    MEM_FS
                        .lock()
                        .write_file_content(name, content.clone(), *times);
                }
                Ok(buf.len())
            }
            FileDescriptor::Ext4Regular { ino, writable, .. } => {
                if !*writable {
                    return Err(SysErrNo::EBADF);
                }
                Self::checked_file_end(offset, buf.len())?;
                ext4_vol::ext4_write_at(*ino, offset, buf)
            }
            FileDescriptor::MemDir { .. } | FileDescriptor::Ext4Dir { .. } => Err(SysErrNo::EISDIR),
            FileDescriptor::Path { .. } => Err(SysErrNo::EBADF),
            FileDescriptor::LoopControl => Err(SysErrNo::ENXIO),
            FileDescriptor::LoopDevice {
                index, writable, ..
            } => {
                if !*writable {
                    return Err(SysErrNo::EBADF);
                }
                Self::checked_file_end(offset, buf.len())?;
                block_dev::loop_write_at(*index, offset, buf)
            }
            FileDescriptor::PipeRead { .. } | FileDescriptor::PipeWrite { .. } => {
                Err(SysErrNo::ESPIPE)
            }
            FileDescriptor::Socket { .. } => Err(SysErrNo::ESPIPE),
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
                times,
                offset,
                writable,
                append,
                linked,
                ..
            } => {
                if !*writable {
                    return Err(SysErrNo::EBADF);
                }
                if is_dev_null_path(name) || is_dev_zero_path(name) {
                    return Ok(buf.len());
                }
                if buf.is_empty() {
                    return Ok(0);
                }
                let mem_live = refresh_mem_file(name, content, times, linked);
                if *append {
                    *offset = content.len();
                }
                let start = *offset;
                let end = Self::checked_file_end(start, buf.len())?;
                write_mem_content(content, start, buf);
                *offset = end;
                times.touch_modified();
                if mem_live {
                    MEM_FS
                        .lock()
                        .write_file_content(name, content.clone(), *times);
                }

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
                Self::checked_file_end(*offset, buf.len())?;
                let n = ext4_vol::ext4_write_at(*ino, *offset, buf)?;
                *offset += n;
                Ok(n)
            }
            FileDescriptor::Ext4Dir { .. } => Err(SysErrNo::EISDIR),
            FileDescriptor::Path { .. } => Err(SysErrNo::EBADF),
            FileDescriptor::LoopControl => Err(SysErrNo::ENXIO),
            FileDescriptor::LoopDevice {
                index,
                offset,
                writable,
                ..
            } => {
                if !*writable {
                    return Err(SysErrNo::EBADF);
                }
                if buf.is_empty() {
                    return Ok(0);
                }
                Self::checked_file_end(*offset, buf.len())?;
                let n = block_dev::loop_write_at(*index, *offset, buf)?;
                *offset = offset.saturating_add(n);
                Ok(n)
            }
            FileDescriptor::PipeWrite { state, .. } => {
                if buf.is_empty() {
                    return Ok(0);
                }
                let (written, wake_readers) = {
                    let mut pipe = state.lock();
                    if pipe.readers == 0 {
                        return Err(SysErrNo::EPIPE);
                    }
                    let available = PIPE_CAPACITY.saturating_sub(pipe.buf.len());
                    if available == 0 {
                        return Err(SysErrNo::EAGAIN);
                    }
                    let was_empty = pipe.buf.is_empty();
                    let written = pipe_write_buffer(&mut pipe, buf, available);
                    (written, was_empty && written != 0)
                };
                if wake_readers {
                    wake_pipe_readers(state);
                }
                Ok(written)
            }
            FileDescriptor::PipeRead { .. } => Err(SysErrNo::EBADF),
            FileDescriptor::EventFd { state } => {
                if buf.len() < core::mem::size_of::<u64>() {
                    return Err(SysErrNo::EINVAL);
                }
                let mut raw = [0u8; core::mem::size_of::<u64>()];
                raw.copy_from_slice(&buf[..core::mem::size_of::<u64>()]);
                let value = u64::from_ne_bytes(raw);
                if value == u64::MAX {
                    return Err(SysErrNo::EINVAL);
                }
                let wake_readers = {
                    let mut eventfd = state.lock();
                    let available = (u64::MAX - 1).saturating_sub(eventfd.counter);
                    if value > available {
                        return Err(SysErrNo::EAGAIN);
                    }
                    let was_empty = eventfd.counter == 0;
                    eventfd.counter += value;
                    was_empty && value != 0
                };
                if wake_readers {
                    wake_eventfd_readers(state);
                }
                Ok(core::mem::size_of::<u64>())
            }
            FileDescriptor::Socket { state } => {
                if buf.is_empty() {
                    return Ok(0);
                }
                let socket = state.lock();
                if socket.shutdown_write {
                    return Err(SysErrNo::EPIPE);
                }
                if socket.is_stream() && !socket.connected {
                    return Err(SysErrNo::ENOTCONN);
                }
                let peer = socket.peer.clone().ok_or(SysErrNo::ENOTCONN)?;
                drop(socket);
                let mut peer_socket = peer.lock();
                if peer_socket.shutdown_read {
                    return Err(SysErrNo::EPIPE);
                }
                peer_socket.rx_buf.extend(buf.iter().copied());
                drop(peer_socket);
                crate::task::wait_queue::wake_io_waiters();
                Ok(buf.len())
            }
            _ => Err(SysErrNo::EBADF),
        }
    }

    /// lseek with signed offset
    pub fn seek_signed(&mut self, offset: isize, whence: usize) -> Result<FileOffset, SysErrNo> {
        match self {
            FileDescriptor::MemFile {
                content,
                offset: current_offset,
                ..
            } => {
                let new_off = match whence {
                    0 => Self::checked_seek_from(0, offset),
                    1 => Self::checked_seek_from(*current_offset, offset),
                    2 => Self::checked_seek_from(content.len(), offset),
                    _ => return Err(SysErrNo::EINVAL),
                }?;
                *current_offset = new_off;
                Ok(new_off)
            }
            FileDescriptor::Ext4Regular {
                ino,
                offset: current_offset,
                ..
            } => {
                let sz = ext4_vol::regular_file_size(*ino)?;
                let new_off = match whence {
                    0 => Self::checked_seek_from(0, offset),
                    1 => Self::checked_seek_from(*current_offset, offset),
                    2 => Self::checked_seek_from(sz, offset),
                    _ => return Err(SysErrNo::EINVAL),
                }?;
                *current_offset = new_off;
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
                let end = entries.len();
                let new_offset = match whence {
                    0 => Self::checked_seek_from(0, offset),
                    1 => Self::checked_seek_from(*current_offset, offset),
                    2 => Self::checked_seek_from(end, offset),
                    _ => return Err(SysErrNo::EINVAL),
                }?;
                if new_offset > end {
                    return Err(SysErrNo::EINVAL);
                }
                *current_offset = new_offset;
                Ok(*current_offset)
            }
            FileDescriptor::Ext4Dir { .. } => Err(SysErrNo::ESPIPE),
            FileDescriptor::Path { .. } => Err(SysErrNo::EBADF),
            FileDescriptor::LoopControl => Err(SysErrNo::ESPIPE),
            FileDescriptor::LoopDevice {
                index,
                offset: current_offset,
                ..
            } => {
                let sz = block_dev::loop_size(*index)?;
                let new_off = match whence {
                    0 => Self::checked_seek_from(0, offset),
                    1 => Self::checked_seek_from(*current_offset, offset),
                    2 => Self::checked_seek_from(sz, offset),
                    _ => return Err(SysErrNo::EINVAL),
                }?;
                *current_offset = new_off;
                Ok(*current_offset)
            }
            _ => Err(SysErrNo::ESPIPE),
        }
    }

    /// 设置文件偏移
    pub fn seek(&mut self, offset: FileOffset, whence: usize) -> Result<FileOffset, SysErrNo> {
        if offset > MAX_FILE_OFFSET {
            return Err(SysErrNo::EINVAL);
        }
        self.seek_signed(offset as isize, whence)
    }

    /// 获取文件大小
    pub fn size(&self) -> usize {
        match self {
            FileDescriptor::MemFile {
                name,
                content,
                linked,
                ..
            } => {
                if is_dev_null_path(name) || is_dev_zero_path(name) {
                    0
                } else if !*linked {
                    content.len()
                } else {
                    MEM_FS
                        .lock()
                        .get_file(name)
                        .map(|file| file.size())
                        .unwrap_or(content.len())
                }
            }
            FileDescriptor::MemDir { entries, .. } => entries.len(),
            FileDescriptor::Ext4Regular { ino, .. } => {
                ext4_vol::regular_file_size(*ino).unwrap_or(0)
            }
            FileDescriptor::Ext4Dir { .. } => 0,
            FileDescriptor::Path { .. } => 0,
            FileDescriptor::LoopDevice { index, .. } => block_dev::loop_size(*index).unwrap_or(0),
            FileDescriptor::PipeRead { state, .. } | FileDescriptor::PipeWrite { state, .. } => {
                state.lock().buf.len()
            }
            FileDescriptor::EventFd { .. } => core::mem::size_of::<u64>(),
            _ => 0,
        }
    }

    fn truncate_with_readonly_errno(
        &mut self,
        new_len: usize,
        readonly_errno: SysErrNo,
    ) -> Result<(), SysErrNo> {
        if new_len > MAX_FILE_OFFSET {
            return Err(SysErrNo::EFBIG);
        }
        match self {
            FileDescriptor::MemFile {
                name,
                content,
                times,
                writable,
                linked,
                ..
            } => {
                if !*writable {
                    return Err(readonly_errno);
                }
                if is_dev_null_path(name) || is_dev_zero_path(name) {
                    return Ok(());
                }
                let mem_live = refresh_mem_file(name, content, times, linked);
                content.resize(new_len);
                times.touch_modified();
                if mem_live {
                    MEM_FS
                        .lock()
                        .write_file_content(name, content.clone(), *times);
                }
                Ok(())
            }
            FileDescriptor::Ext4Regular { ino, writable, .. } => {
                if !*writable {
                    return Err(readonly_errno);
                }
                ext4_vol::truncate_regular_ino(*ino, new_len as u64)
            }
            FileDescriptor::MemDir { .. } | FileDescriptor::Ext4Dir { .. } => Err(SysErrNo::EISDIR),
            FileDescriptor::Path { .. } => Err(SysErrNo::EBADF),
            FileDescriptor::LoopControl | FileDescriptor::LoopDevice { .. } => {
                Err(SysErrNo::EINVAL)
            }
            _ => Err(SysErrNo::EINVAL),
        }
    }

    pub fn truncate(&mut self, new_len: usize) -> Result<(), SysErrNo> {
        self.truncate_with_readonly_errno(new_len, SysErrNo::EBADF)
    }

    pub fn ftruncate(&mut self, new_len: usize) -> Result<(), SysErrNo> {
        self.truncate_with_readonly_errno(new_len, SysErrNo::EINVAL)
    }

    pub fn allocate(&mut self, offset: usize, len: usize, keep_size: bool) -> Result<(), SysErrNo> {
        if len == 0 {
            return Err(SysErrNo::EINVAL);
        }
        let end = Self::checked_file_end(offset, len)?;
        match self {
            FileDescriptor::MemFile { .. } | FileDescriptor::Ext4Regular { .. } => {
                let current_size = self.size();
                let target_size = if keep_size {
                    current_size
                } else {
                    end.max(current_size)
                };
                self.truncate(target_size)?;
                Ok(())
            }
            FileDescriptor::MemDir { .. } | FileDescriptor::Ext4Dir { .. } => Err(SysErrNo::EISDIR),
            FileDescriptor::Path { .. } => Err(SysErrNo::EBADF),
            FileDescriptor::LoopControl | FileDescriptor::LoopDevice { .. } => {
                Err(SysErrNo::EINVAL)
            }
            FileDescriptor::PipeRead { .. } | FileDescriptor::PipeWrite { .. } => {
                Err(SysErrNo::ESPIPE)
            }
            _ => Err(SysErrNo::EINVAL),
        }
    }

    pub fn sync(&self, _data_only: bool) -> Result<(), SysErrNo> {
        match self {
            FileDescriptor::Ext4Regular { ino, .. } => ext4_vol::flush_cached_ino(*ino),
            FileDescriptor::LoopDevice { .. } => Ok(()),
            FileDescriptor::PipeRead { .. } | FileDescriptor::PipeWrite { .. } => {
                Err(SysErrNo::EINVAL)
            }
            _ => Ok(()),
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
            FileDescriptor::Ext4Dir { ino, offset, .. } => {
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
            FileDescriptor::Path { .. } => Err(SysErrNo::EBADF),
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
                times,
                offset,
                readable,
                writable,
                append,
                linked,
                node,
            } => FileDescriptor::MemFile {
                name: name.clone(),
                content: content.clone(),
                times: *times,
                offset: *offset,
                readable: *readable,
                writable: *writable,
                append: *append,
                linked: *linked,
                node: *node,
            },
            FileDescriptor::MemDir {
                path,
                host_path,
                entries,
                offset,
            } => FileDescriptor::MemDir {
                path: path.clone(),
                host_path: host_path.clone(),
                entries: entries.clone(),
                offset: *offset,
            },
            FileDescriptor::Ext4Regular {
                ino,
                offset,
                readable,
                writable,
                append,
            } => {
                crate::fs::ext4_vol::open_regular_ino(*ino);
                FileDescriptor::Ext4Regular {
                    ino: *ino,
                    offset: *offset,
                    readable: *readable,
                    writable: *writable,
                    append: *append,
                }
            }
            FileDescriptor::Ext4Dir { path, ino, offset } => FileDescriptor::Ext4Dir {
                path: path.clone(),
                ino: *ino,
                offset: *offset,
            },
            FileDescriptor::Path {
                logical_path,
                host_path,
                kind,
                flags,
            } => FileDescriptor::Path {
                logical_path: logical_path.clone(),
                host_path: host_path.clone(),
                kind: *kind,
                flags: *flags,
            },
            FileDescriptor::LoopControl => FileDescriptor::LoopControl,
            FileDescriptor::LoopDevice {
                index,
                offset,
                readable,
                writable,
            } => FileDescriptor::LoopDevice {
                index: *index,
                offset: *offset,
                readable: *readable,
                writable: *writable,
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
            FileDescriptor::Socket { state } => FileDescriptor::Socket {
                state: state.clone(),
            },
            FileDescriptor::EventFd { state } => FileDescriptor::EventFd {
                state: state.clone(),
            },
            FileDescriptor::Epoll { state } => FileDescriptor::Epoll {
                state: state.clone(),
            },
        }
    }
}

impl Drop for FileDescriptor {
    fn drop(&mut self) {
        let mut wake_io = false;
        match self {
            FileDescriptor::PipeRead { state, .. } => {
                let mut last_reader = false;
                let mut closed_reader = false;
                let mut pipe = state.lock();
                if pipe.readers > 0 {
                    pipe.readers -= 1;
                    last_reader = pipe.readers == 0;
                    closed_reader = true;
                }
                drop(pipe);
                if last_reader {
                    wake_pipe_readers(state);
                    wake_pipe_writers(state);
                } else if closed_reader {
                    wake_pipe_readers(state);
                }
            }
            FileDescriptor::PipeWrite { state, .. } => {
                let mut last_writer = false;
                let mut closed_writer = false;
                let mut pipe = state.lock();
                if pipe.writers > 0 {
                    pipe.writers -= 1;
                    last_writer = pipe.writers == 0;
                    closed_writer = true;
                }
                drop(pipe);
                if last_writer {
                    wake_pipe_readers(state);
                    wake_pipe_writers(state);
                } else if closed_writer {
                    wake_pipe_writers(state);
                }
            }
            FileDescriptor::Socket { .. } => {
                wake_io = true;
            }
            FileDescriptor::EventFd { state } => {
                wake_eventfd_readers(state);
                wake_eventfd_writers(state);
                wake_io = true;
            }
            FileDescriptor::Ext4Regular { ino, .. } => {
                crate::fs::ext4_vol::close_regular_ino(*ino);
            }
            _ => {}
        }
        if wake_io {
            crate::task::wait_queue::wake_io_waiters();
        }
    }
}

/// 文件描述符表
#[derive(Clone)]
pub struct FileDescriptorTable {
    fds: Vec<Option<FileDescriptor>>,
    fd_flags: Vec<usize>,
    next_fd_hint: usize,
    cloexec_count: usize,
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

        Self {
            fds,
            fd_flags: alloc::vec![0; MAX_FD_NUM],
            next_fd_hint: 3,
            cloexec_count: 0,
        }
    }

    fn count_cloexec(flags: usize) -> usize {
        usize::from((flags & FD_CLOEXEC) != 0)
    }

    fn set_slot(&mut self, index: usize, fd: FileDescriptor, flags: usize) {
        self.cloexec_count = self
            .cloexec_count
            .saturating_sub(Self::count_cloexec(self.fd_flags[index]));
        let old = self.fds[index].take();
        self.fds[index] = Some(fd);
        self.fd_flags[index] = flags & FD_CLOEXEC;
        self.cloexec_count += Self::count_cloexec(self.fd_flags[index]);
        if self.next_fd_hint == MAX_FD_NUM || index <= self.next_fd_hint {
            self.next_fd_hint = index.saturating_add(1).min(MAX_FD_NUM);
        }
        drop(old);
    }

    fn clear_slot(&mut self, index: usize) -> Option<FileDescriptor> {
        self.cloexec_count = self
            .cloexec_count
            .saturating_sub(Self::count_cloexec(self.fd_flags[index]));
        let old = self.fds[index].take();
        self.fd_flags[index] = 0;
        if index < self.next_fd_hint {
            self.next_fd_hint = index;
        }
        old
    }

    fn alloc_slot_from(
        &mut self,
        start: usize,
        limit: usize,
        fd: FileDescriptor,
        flags: usize,
    ) -> Option<usize> {
        let limit = limit.min(MAX_FD_NUM);
        if start >= limit {
            return None;
        }
        let scan_start = if start == 0 {
            self.next_fd_hint.min(limit)
        } else {
            start
        };
        if let Some(index) = self
            .fds
            .iter()
            .enumerate()
            .take(limit)
            .skip(scan_start)
            .find_map(|(i, slot)| if slot.is_none() { Some(i) } else { None })
        {
            self.set_slot(index, fd, flags);
            return Some(index);
        }
        if scan_start > start {
            if let Some(index) = self
                .fds
                .iter()
                .enumerate()
                .take(scan_start)
                .skip(start)
                .find_map(|(i, slot)| if slot.is_none() { Some(i) } else { None })
            {
                self.set_slot(index, fd, flags);
                return Some(index);
            }
        }
        None
    }

    pub fn alloc(&mut self, fd: FileDescriptor) -> Option<usize> {
        self.alloc_with_flags(fd, 0)
    }

    pub fn alloc_with_flags(&mut self, fd: FileDescriptor, flags: usize) -> Option<usize> {
        self.alloc_with_flags_below(fd, flags, MAX_FD_NUM)
    }

    pub fn alloc_with_flags_below(
        &mut self,
        fd: FileDescriptor,
        flags: usize,
        limit: usize,
    ) -> Option<usize> {
        self.alloc_slot_from(0, limit, fd, flags)
    }

    pub fn alloc_from(&mut self, start: usize, fd: FileDescriptor) -> Option<usize> {
        self.alloc_from_with_flags(start, fd, 0)
    }

    pub fn alloc_from_with_flags(
        &mut self,
        start: usize,
        fd: FileDescriptor,
        flags: usize,
    ) -> Option<usize> {
        self.alloc_from_with_flags_below(start, fd, flags, MAX_FD_NUM)
    }

    pub fn alloc_from_with_flags_below(
        &mut self,
        start: usize,
        fd: FileDescriptor,
        flags: usize,
        limit: usize,
    ) -> Option<usize> {
        self.alloc_slot_from(start, limit, fd, flags)
    }

    pub fn alloc_at(&mut self, index: usize, fd: FileDescriptor) -> Result<(), SysErrNo> {
        if index >= MAX_FD_NUM {
            return Err(SysErrNo::EBADF);
        }
        self.set_slot(index, fd, 0);
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
        crate::trap::restore_kernel_page_table();
        drop(self.remove(fd)?);
        if let Some(task) = crate::task::current_task() {
            if !task.is_kernel {
                task.memory_set.lock().activate();
            }
        }
        Ok(())
    }

    pub fn remove(&mut self, fd: usize) -> Result<FileDescriptor, SysErrNo> {
        if fd >= MAX_FD_NUM {
            return Err(SysErrNo::EBADF);
        }
        if self.fds[fd].is_none() {
            return Err(SysErrNo::EBADF);
        }
        self.clear_slot(fd).ok_or(SysErrNo::EBADF)
    }

    pub fn fd_flags(&self, fd: usize) -> Result<usize, SysErrNo> {
        if fd >= MAX_FD_NUM || self.fds[fd].is_none() {
            return Err(SysErrNo::EBADF);
        }
        Ok(self.fd_flags[fd])
    }

    pub fn set_fd_flags(&mut self, fd: usize, flags: usize) -> Result<(), SysErrNo> {
        if fd >= MAX_FD_NUM || self.fds[fd].is_none() {
            return Err(SysErrNo::EBADF);
        }
        let old = self.fd_flags[fd];
        self.fd_flags[fd] = flags & FD_CLOEXEC;
        self.cloexec_count = self.cloexec_count.saturating_sub(Self::count_cloexec(old))
            + Self::count_cloexec(self.fd_flags[fd]);
        Ok(())
    }

    pub fn close_on_exec(&mut self) -> Vec<FileDescriptor> {
        let mut closed = Vec::new();
        if self.cloexec_count == 0 {
            return closed;
        }
        for index in 0..MAX_FD_NUM {
            if self.fds[index].is_some() && (self.fd_flags[index] & FD_CLOEXEC) != 0 {
                if let Some(file) = self.clear_slot(index) {
                    closed.push(file);
                }
            }
        }
        closed
    }

    pub fn close_all(&mut self) {
        for index in 0..MAX_FD_NUM {
            self.fds[index] = None;
            self.fd_flags[index] = 0;
        }
        self.next_fd_hint = 0;
        self.cloexec_count = 0;
    }

    pub fn dup(&mut self, old_fd: usize) -> Result<usize, SysErrNo> {
        self.dup_below(old_fd, MAX_FD_NUM)
    }

    pub fn dup_below(&mut self, old_fd: usize, limit: usize) -> Result<usize, SysErrNo> {
        if old_fd >= MAX_FD_NUM || self.fds[old_fd].is_none() {
            return Err(SysErrNo::EBADF);
        }
        let fd = self.fds[old_fd].clone().unwrap();
        match self.alloc_with_flags_below(fd, 0, limit) {
            Some(new_fd) => Ok(new_fd),
            None => Err(SysErrNo::EMFILE),
        }
    }

    pub fn dup2(&mut self, old_fd: usize, new_fd: usize) -> Result<usize, SysErrNo> {
        self.dup2_below(old_fd, new_fd, MAX_FD_NUM)
    }

    pub fn dup2_below(
        &mut self,
        old_fd: usize,
        new_fd: usize,
        limit: usize,
    ) -> Result<usize, SysErrNo> {
        if old_fd >= MAX_FD_NUM || self.fds[old_fd].is_none() {
            return Err(SysErrNo::EBADF);
        }
        if new_fd >= limit.min(MAX_FD_NUM) {
            return Err(SysErrNo::EBADF);
        }
        if old_fd == new_fd {
            return Ok(new_fd);
        }
        if self.fds[new_fd].is_some() {
            let _ = self.free(new_fd);
        }
        let fd = self.fds[old_fd].clone().unwrap();
        self.set_slot(new_fd, fd, 0);
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

fn interactive_stdin_enabled() -> bool {
    matches!(
        option_env!("WLL_INTERACTIVE"),
        Some("1") | Some("true") | Some("TRUE") | Some("yes") | Some("YES") | Some("on") | Some("ON")
    )
}

/// 交互模式下的标准输入读取。
///
/// 默认评测路径中没有真人终端，stdin 在无输入时会立即返回 EOF，避免评测
/// 程序因为阻塞读而卡死。录制演示视频时则需要 BusyBox shell 等待键盘，
/// 因此 WLL_INTERACTIVE=1 时这里会轮询 UART，直到拿到一个字节再返回。
///
/// 这里故意一次只返回 1 字节。BusyBox shell 在脚本、重定向和 fd 恢复过程
/// 中会多次 read(0)，如果内核侧做全局行缓冲，脚本结束后容易让 shell 的
/// fd 0 状态和内核缓冲状态脱节，表现为 prompt 出现但后续按键没有反应。
/// 逐字返回牺牲了大段粘贴体验，但人工演示最稳定。
fn read_interactive_stdin(buf: &mut [u8]) -> Result<usize, SysErrNo> {
    if buf.is_empty() {
        return Ok(0);
    }

    trace_stdin("[stdin-trace] read interactive byte\n");
    let mut byte = loop {
        if let Some(byte) = crate::console::getchar() {
            break byte;
        }
        core::hint::spin_loop();
    };
    if byte == b'\r' {
        // PowerShell/QEMU 串口通常把 Enter 传成 CR；BusyBox shell 期望 LF。
        byte = b'\n';
    }
    trace_stdin_byte(byte);
    // 本内核没有完整 tty line discipline，这里做最小回显，保证录屏时能看见输入。
    crate::console::putchar(byte);
    buf[0] = byte;
    Ok(1)
}

fn stdin_trace_enabled() -> bool {
    matches!(
        option_env!("WLL_STDIN_TRACE"),
        Some("1") | Some("true") | Some("TRUE") | Some("yes") | Some("YES") | Some("on") | Some("ON")
    )
}

fn trace_stdin(msg: &str) {
    if stdin_trace_enabled() {
        for byte in msg.bytes() {
            crate::console::putchar(byte);
        }
    }
}

fn trace_stdin_byte(byte: u8) {
    if !stdin_trace_enabled() {
        return;
    }
    const HEX: &[u8; 16] = b"0123456789abcdef";
    trace_stdin("[stdin-trace] got 0x");
    crate::console::putchar(HEX[(byte >> 4) as usize]);
    crate::console::putchar(HEX[(byte & 0x0f) as usize]);
    crate::console::putchar(b'\n');
}

/// 打开路径：`flags`/`mode` 语义对齐 Linux `openat` 子集。
pub fn open_file(
    host_path: &str,
    logical_path: &str,
    flags: u32,
    mode: u32,
) -> Result<FileDescriptor, SysErrNo> {
    crate::fs::open_path(host_path, logical_path, flags, mode)
}

#[cfg(any())]
fn open_file_legacy_unused(
    host_path: &str,
    logical_path: &str,
    flags: u32,
    mode: u32,
) -> Result<FileDescriptor, SysErrNo> {
    use open_flags::*;

    let path_norm = fs::normalize_path(host_path);
    let logical_norm = fs::normalize_path(logical_path);
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
        if !want_create {
            return Err(SysErrNo::ENOENT);
        }
    }

    let mem = fs::MEM_FS.lock();
    let mem_has_file = mem.get_file(&path_norm).is_some();
    drop(mem);

    let ext_path_norm = if !removed {
        match ext4_vol::lookup_kind(&path_norm) {
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
        let source = fs::MEM_FS
            .lock()
            .get_file(&path_norm)
            .map(|file| file.snapshot());
        let (mut content, mut times) =
            source.unwrap_or_else(|| (MemFileContent::new(), FileTimes::now()));
        if want_trunc && write_ok {
            content.clear();
            fs::MEM_FS.lock().truncate_file(&path_norm, 0)?;
            if let Some(file) = fs::MEM_FS.lock().get_file(&path_norm) {
                times = file.times();
            }
        }
        let base_off = if append && write_ok { content.len() } else { 0 };
        return Ok(FileDescriptor::MemFile {
            name: path_norm,
            content,
            times,
            offset: base_off,
            readable: read_ok,
            writable: write_ok,
            append,
            linked: true,
            node: None,
        });
    }

    if fs::MEM_FS.lock().is_dir(&path_norm) || ext4_vol::ext4_dir_path_exists(&ext_path_norm) {
        if write_ok || want_trunc || want_create {
            return Err(SysErrNo::EISDIR);
        }
        let open_path = if fs::MEM_FS.lock().is_dir(&path_norm) {
            path_norm.clone()
        } else {
            ext_path_norm.clone()
        };
        return open_dir_descriptor(&open_path, &logical_norm);
    }

    if want_dir {
        if !fs::MEM_FS.lock().is_dir(&path_norm) && !ext4_vol::ext4_dir_path_exists(&ext_path_norm)
        {
            return Err(SysErrNo::ENOENT);
        }
        // MemFS 目录优先走 MemDir（预载已全部加载到内存）。
        // ext4 独有目录走 Ext4Dir（实时读取目录项，支持 getdents64）。
        if MEM_FS.lock().is_dir(&path_norm) {
            let entries = fs::list_dir(&path_norm)?;
            return Ok(FileDescriptor::MemDir {
                path: logical_norm,
                host_path: path_norm,
                entries,
                offset: 0,
            });
        } else {
            // ext4 目录：通过 lookup_path 获取 inode 并返回 Ext4Dir
            let Some((ino, _)) = ext4_vol::lookup_path(&ext_path_norm) else {
                return Err(SysErrNo::ENOENT);
            };
            return Ok(FileDescriptor::Ext4Dir {
                path: logical_norm,
                ino,
                offset: 0,
            });
        }
    }

    if fs::MEM_FS.lock().is_dir(&path_norm) || ext4_vol::ext4_dir_path_exists(&ext_path_norm) {
        return Err(SysErrNo::EISDIR);
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
        crate::fs::ext4_vol::open_regular_ino(ino);
        return Ok(FileDescriptor::Ext4Regular {
            ino,
            offset: base_off,
            readable: read_ok,
            writable: write_ok,
            append,
        });
    }

    if want_create {
        let parent = fs::parent_path(&path_norm);
        let mem_parent = fs::MEM_FS.lock().is_dir(&parent);
        let ext_parent = ext4_vol::ext4_dir_path_exists(&parent);
        if mem_parent && (fs::is_memfs_volatile_dir(&parent) || !ext_parent) {
            fs::MEM_FS
                .lock()
                .add_file_with_mode(&path_norm, Vec::new(), mode);
            let times = fs::MEM_FS
                .lock()
                .get_file(&path_norm)
                .map(|file| file.times())
                .unwrap_or_else(FileTimes::now);
            return Ok(FileDescriptor::MemFile {
                name: path_norm,
                content: MemFileContent::new(),
                times,
                offset: 0,
                readable: read_ok,
                writable: write_ok,
                append,
                linked: true,
                node: None,
            });
        }
        if ext_parent {
            let ino = ext4_vol::create_regular_ext4_with_mode(&path_norm, mode)?;
            crate::fs::ext4_vol::open_regular_ino(ino);
            return Ok(FileDescriptor::Ext4Regular {
                ino,
                offset: 0,
                readable: read_ok,
                writable: write_ok,
                append,
                linked: true,
                node: None,
            });
        }
        if mem_parent {
            fs::MEM_FS
                .lock()
                .add_file_with_mode(&path_norm, Vec::new(), mode);
            let times = fs::MEM_FS
                .lock()
                .get_file(&path_norm)
                .map(|file| file.times())
                .unwrap_or_else(FileTimes::now);
            return Ok(FileDescriptor::MemFile {
                name: path_norm,
                content: MemFileContent::new(),
                times,
                offset: 0,
                readable: read_ok,
                writable: write_ok,
                append,
                linked: true,
            });
        }
        return Err(SysErrNo::ENOENT);
    }

    Err(SysErrNo::ENOENT)
}

pub fn create_pipe(nonblock: bool) -> (FileDescriptor, FileDescriptor) {
    let state = Arc::new(Mutex::new(PipeState::new()));
    (
        FileDescriptor::PipeRead {
            state: state.clone(),
            nonblock,
        },
        FileDescriptor::PipeWrite { state, nonblock },
    )
}

pub fn create_eventfd(counter: u64, semaphore: bool, nonblock: bool) -> FileDescriptor {
    FileDescriptor::EventFd {
        state: Arc::new(Mutex::new(EventFdState::new(
            counter, semaphore, nonblock,
        ))),
    }
}
