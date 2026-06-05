use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use core::sync::atomic::{AtomicUsize, Ordering};
use lazy_static::lazy_static;
use spin::Mutex;

use super::SyscallRet;
use crate::fs::fd::{self, FileDescriptor, SocketPacket, SocketState};
use crate::task::current_task;
use crate::utils::error::SysErrNo;

const AF_UNIX: i32 = 1;
const AF_INET: i32 = 2;
const AF_INET6: i32 = 10;

const SOCK_STREAM: usize = 1;
const SOCK_DGRAM: usize = 2;
const SOCK_NONBLOCK: usize = fd::pipe_flags::O_NONBLOCK;
const SOCK_CLOEXEC: usize = fd::pipe_flags::O_CLOEXEC;
const SOCK_FLAG_MASK: usize = SOCK_NONBLOCK | SOCK_CLOEXEC;

const IPPROTO_TCP: i32 = 6;
const IPPROTO_UDP: i32 = 17;
const SOL_SOCKET: usize = 1;

const SO_REUSEADDR: usize = 2;
const SO_TYPE: usize = 3;
const SO_ERROR: usize = 4;
const SO_BROADCAST: usize = 6;
const SO_SNDBUF: usize = 7;
const SO_RCVBUF: usize = 8;
const SO_KEEPALIVE: usize = 9;
const SO_REUSEPORT: usize = 15;
const SO_RCVTIMEO: usize = 20;
const SO_SNDTIMEO: usize = 21;
const SO_PROTOCOL: usize = 38;

const TCP_NODELAY: usize = 1;

const MSG_DONTWAIT: usize = 0x40;
const MSG_NOSIGNAL: usize = 0x4000;
const MSG_SUPPORTED: usize = MSG_DONTWAIT | MSG_NOSIGNAL;

const SHUT_RD: usize = 0;
const SHUT_WR: usize = 1;
const SHUT_RDWR: usize = 2;

const MAX_SOCKADDR_LEN: usize = 128;
static NEXT_EPHEMERAL_PORT: AtomicUsize = AtomicUsize::new(49152);

#[repr(C)]
#[derive(Clone, Copy)]
struct TimeVal {
    tv_sec: isize,
    tv_usec: isize,
}

struct BoundSocket {
    domain: i32,
    sock_type: usize,
    addr: Vec<u8>,
    state: Weak<Mutex<SocketState>>,
}

lazy_static! {
    static ref SOCKET_BINDINGS: Mutex<Vec<BoundSocket>> = Mutex::new(Vec::new());
}

fn socket_type(raw_type: usize) -> Result<(usize, bool, usize), SysErrNo> {
    let flags = raw_type & SOCK_FLAG_MASK;
    if raw_type & !SOCK_FLAG_MASK & !0xf != 0 {
        return Err(SysErrNo::EINVAL);
    }
    let base = raw_type & !SOCK_FLAG_MASK;
    match base {
        SOCK_STREAM | SOCK_DGRAM => Ok((base, (flags & SOCK_NONBLOCK) != 0, flags)),
        _ => Err(SysErrNo::EOPNOTSUPP),
    }
}

fn validate_domain(domain: i32) -> Result<(), SysErrNo> {
    match domain {
        AF_UNIX | AF_INET | AF_INET6 => Ok(()),
        _ => Err(SysErrNo::EAFNOSUPPORT),
    }
}

fn validate_protocol(sock_type: usize, protocol: i32) -> Result<(), SysErrNo> {
    match (sock_type, protocol) {
        (SOCK_STREAM, 0 | IPPROTO_TCP) => Ok(()),
        (SOCK_DGRAM, 0 | IPPROTO_UDP) => Ok(()),
        _ => Err(SysErrNo::EPROTONOSUPPORT),
    }
}

fn socket_state_for_fd(fd: usize) -> Result<Arc<Mutex<SocketState>>, SysErrNo> {
    let task = current_task().ok_or(SysErrNo::ESRCH)?;
    let inner = task.inner.lock();
    let fds = inner.fd_table.lock();
    let file_desc = fds.get(fd).ok_or(SysErrNo::EBADF)?;
    file_desc.socket_state().ok_or(SysErrNo::ENOTSOCK)
}

fn purge_dead_bindings(bindings: &mut Vec<BoundSocket>) {
    bindings.retain(|entry| entry.state.upgrade().is_some());
}

fn register_bound_socket(
    domain: i32,
    sock_type: usize,
    addr: &[u8],
    state: &Arc<Mutex<SocketState>>,
    reuse_addr: bool,
) -> Result<(), SysErrNo> {
    let mut bindings = SOCKET_BINDINGS.lock();
    purge_dead_bindings(&mut bindings);
    let in_use = bindings.iter().any(|entry| {
        entry.domain == domain
            && entry.sock_type == sock_type
            && sockaddr_addr_matches(&entry.addr, addr)
            && entry.state.upgrade().is_some()
    });
    if in_use && !reuse_addr {
        return Err(SysErrNo::EADDRINUSE);
    }
    bindings.push(BoundSocket {
        domain,
        sock_type,
        addr: addr.to_vec(),
        state: Arc::downgrade(state),
    });
    Ok(())
}

fn find_bound_socket(domain: i32, sock_type: usize, addr: &[u8]) -> Option<Arc<Mutex<SocketState>>> {
    let mut bindings = SOCKET_BINDINGS.lock();
    purge_dead_bindings(&mut bindings);
    bindings
        .iter()
        .find(|entry| {
            entry.domain == domain
                && entry.sock_type == sock_type
                && sockaddr_addr_matches(&entry.addr, addr)
        })
        .and_then(|entry| entry.state.upgrade())
}

fn copy_sockaddr_from_user(addr: usize, addrlen: usize) -> Result<Vec<u8>, SysErrNo> {
    if addr == 0 || addrlen < core::mem::size_of::<u16>() || addrlen > MAX_SOCKADDR_LEN {
        return Err(SysErrNo::EINVAL);
    }
    let mut buf = alloc::vec![0u8; addrlen];
    super::user::copy_from_user(addr, &mut buf)?;
    Ok(buf)
}

fn sockaddr_family(addr: &[u8]) -> i32 {
    if addr.len() < core::mem::size_of::<u16>() {
        return 0;
    }
    u16::from_ne_bytes([addr[0], addr[1]]) as i32
}

fn sockaddr_matches_domain(socket: &SocketState, addr: &[u8]) -> bool {
    let family = sockaddr_family(addr);
    family == 0 || family == socket.domain
}

fn sockaddr_port(addr: &[u8]) -> Option<u16> {
    match sockaddr_family(addr) {
        AF_INET | AF_INET6 if addr.len() >= 4 => Some(u16::from_be_bytes([addr[2], addr[3]])),
        _ => None,
    }
}

fn set_sockaddr_port(addr: &mut [u8], port: u16) {
    if matches!(sockaddr_family(addr), AF_INET | AF_INET6) && addr.len() >= 4 {
        addr[2..4].copy_from_slice(&port.to_be_bytes());
    }
}

fn sockaddr_addr_matches(bound: &[u8], target: &[u8]) -> bool {
    let family = sockaddr_family(bound);
    if family != sockaddr_family(target) || sockaddr_port(bound) != sockaddr_port(target) {
        return false;
    }
    match family {
        AF_INET if bound.len() >= 8 && target.len() >= 8 => {
            bound[4..8].iter().all(|byte| *byte == 0) || bound[4..8] == target[4..8]
        }
        AF_INET6 if bound.len() >= 24 && target.len() >= 24 => {
            bound[8..24].iter().all(|byte| *byte == 0) || bound[8..24] == target[8..24]
        }
        _ => bound == target,
    }
}

fn assign_ephemeral_port_if_needed(addr: &mut [u8]) {
    if sockaddr_port(addr) == Some(0) {
        let next = NEXT_EPHEMERAL_PORT.fetch_add(1, Ordering::Relaxed);
        let port = 49152 + (next % (65535 - 49152));
        set_sockaddr_port(addr, port as u16);
    }
}

fn default_sockaddr(domain: i32) -> Vec<u8> {
    let len = match domain {
        AF_INET => 16,
        AF_INET6 => 28,
        AF_UNIX => 2,
        _ => 2,
    };
    let mut addr = alloc::vec![0u8; len];
    addr[..2].copy_from_slice(&(domain as u16).to_ne_bytes());
    addr
}

fn copy_sockaddr_to_user(addr: usize, addrlen: usize, stored: &[u8]) -> Result<(), SysErrNo> {
    if addr == 0 || addrlen == 0 {
        return Err(SysErrNo::EFAULT);
    }
    let user_len = super::user::copy_object_from_user::<u32>(addrlen)? as usize;
    if user_len != 0 {
        let copy_len = user_len.min(stored.len());
        super::user::copy_to_user(addr, &stored[..copy_len])?;
    }
    let stored_len = stored.len() as u32;
    super::user::copy_object_to_user(addrlen, &stored_len)
}

fn validate_msg_flags(flags: usize) -> Result<(), SysErrNo> {
    if flags & !MSG_SUPPORTED != 0 {
        Err(SysErrNo::EOPNOTSUPP)
    } else {
        Ok(())
    }
}

fn copy_i32_option_out(optval: usize, optlen: usize, value: i32) -> Result<(), SysErrNo> {
    if optval == 0 || optlen == 0 {
        return Err(SysErrNo::EFAULT);
    }
    let user_len = super::user::copy_object_from_user::<u32>(optlen)? as usize;
    let bytes = value.to_ne_bytes();
    let copy_len = user_len.min(bytes.len());
    if copy_len != 0 {
        super::user::copy_to_user(optval, &bytes[..copy_len])?;
    }
    let out_len = bytes.len() as u32;
    super::user::copy_object_to_user(optlen, &out_len)
}

fn copy_i32_option_in(optval: usize, optlen: usize) -> Result<i32, SysErrNo> {
    if optval == 0 || optlen < core::mem::size_of::<i32>() {
        return Err(SysErrNo::EINVAL);
    }
    super::user::copy_object_from_user::<i32>(optval)
}

fn copy_timeval_option_in(optval: usize, optlen: usize) -> Result<Option<usize>, SysErrNo> {
    if optval == 0 || optlen < core::mem::size_of::<TimeVal>() {
        return Err(SysErrNo::EINVAL);
    }
    let tv = super::user::copy_object_from_user::<TimeVal>(optval)?;
    if tv.tv_sec < 0 || tv.tv_usec < 0 || tv.tv_usec >= 1_000_000 {
        return Err(SysErrNo::EINVAL);
    }
    let us = (tv.tv_sec as usize)
        .saturating_mul(1_000_000)
        .saturating_add(tv.tv_usec as usize);
    if us == 0 {
        Ok(None)
    } else {
        Ok(Some(us))
    }
}

pub fn sys_socket(domain: usize, raw_type: usize, protocol: usize) -> SyscallRet {
    let domain = domain as i32;
    validate_domain(domain)?;
    let (sock_type, nonblock, flags) = socket_type(raw_type)?;
    let protocol = protocol as i32;
    validate_protocol(sock_type, protocol)?;

    let task = current_task().ok_or(SysErrNo::ESRCH)?;
    let inner = task.inner.lock();
    let mut fds = inner.fd_table.lock();
    let fd_flags = if (flags & SOCK_CLOEXEC) != 0 {
        fd::FD_CLOEXEC
    } else {
        0
    };
    let socket = FileDescriptor::Socket {
        state: Arc::new(Mutex::new(SocketState::new(
            domain, sock_type, protocol, nonblock,
        ))),
    };
    fds.alloc_with_flags(socket, fd_flags).ok_or(SysErrNo::EMFILE)
}

pub fn sys_socketpair(
    _domain: usize,
    _raw_type: usize,
    _protocol: usize,
    _sv: usize,
) -> SyscallRet {
    Err(SysErrNo::EOPNOTSUPP)
}

pub fn sys_bind(fd: usize, addr: usize, addrlen: usize) -> SyscallRet {
    let mut sockaddr = copy_sockaddr_from_user(addr, addrlen)?;
    let state = socket_state_for_fd(fd)?;
    let mut socket = state.lock();
    if !sockaddr_matches_domain(&socket, &sockaddr) {
        return Err(SysErrNo::EAFNOSUPPORT);
    }
    if socket.bound {
        return Err(SysErrNo::EINVAL);
    }
    assign_ephemeral_port_if_needed(&mut sockaddr);
    register_bound_socket(
        socket.domain,
        socket.sock_type,
        &sockaddr,
        &state,
        socket.reuse_addr,
    )?;
    socket.local_addr = Some(sockaddr);
    socket.bound = true;
    Ok(0)
}

pub fn sys_listen(fd: usize, backlog: usize) -> SyscallRet {
    let state = socket_state_for_fd(fd)?;
    let mut socket = state.lock();
    if !socket.is_stream() {
        return Err(SysErrNo::EOPNOTSUPP);
    }
    if socket.connected {
        return Err(SysErrNo::EINVAL);
    }
    socket.listening = true;
    socket.bound = true;
    socket.backlog = (backlog as isize).max(0) as usize;
    Ok(0)
}

pub fn sys_accept(fd: usize, addr: usize, addrlen: usize) -> SyscallRet {
    sys_accept4(fd, addr, addrlen, 0)
}

pub fn sys_accept4(fd: usize, addr: usize, addrlen: usize, flags: usize) -> SyscallRet {
    if flags & !SOCK_FLAG_MASK != 0 {
        return Err(SysErrNo::EINVAL);
    }
    let state = socket_state_for_fd(fd)?;
    let mut socket = state.lock();
    if !socket.is_stream() || !socket.listening {
        return Err(SysErrNo::EINVAL);
    }
    if let Some(accepted) = socket.pending.pop_front() {
        if addr != 0 {
            let peer_addr = accepted
                .lock()
                .peer_addr
                .clone()
                .unwrap_or_else(|| default_sockaddr(socket.domain));
            copy_sockaddr_to_user(addr, addrlen, &peer_addr)?;
        }
        if (flags & SOCK_NONBLOCK) != 0 {
            accepted.lock().nonblock = true;
        }
        let fd_flags = if (flags & SOCK_CLOEXEC) != 0 {
            fd::FD_CLOEXEC
        } else {
            0
        };
        drop(socket);
        let task = current_task().ok_or(SysErrNo::ESRCH)?;
        let inner = task.inner.lock();
        let mut fds = inner.fd_table.lock();
        let desc = FileDescriptor::Socket { state: accepted };
        return fds.alloc_with_flags(desc, fd_flags).ok_or(SysErrNo::EMFILE);
    }
    if socket.nonblock || (flags & SOCK_NONBLOCK) != 0 {
        return Err(SysErrNo::EAGAIN);
    }
    Err(SysErrNo::EOPNOTSUPP)
}

pub fn sys_connect(fd: usize, addr: usize, addrlen: usize) -> SyscallRet {
    let peer = copy_sockaddr_from_user(addr, addrlen)?;
    let state = socket_state_for_fd(fd)?;
    let mut socket = state.lock();
    if !sockaddr_matches_domain(&socket, &peer) {
        return Err(SysErrNo::EAFNOSUPPORT);
    }
    if socket.connected {
        return Err(SysErrNo::EISCONN);
    }
    if socket.is_datagram() {
        socket.peer_addr = Some(peer);
        socket.connected = true;
        return Ok(0);
    }
    drop(socket);
    let listener = find_bound_socket(AF_INET, SOCK_STREAM, &peer)
        .or_else(|| find_bound_socket(AF_INET6, SOCK_STREAM, &peer))
        .or_else(|| find_bound_socket(AF_UNIX, SOCK_STREAM, &peer))
        .ok_or(SysErrNo::ECONNREFUSED)?;
    let mut listener_socket = listener.lock();
    if !listener_socket.listening {
        return Err(SysErrNo::ECONNREFUSED);
    }
    let local_addr = listener_socket
        .local_addr
        .clone()
        .unwrap_or_else(|| default_sockaddr(listener_socket.domain));
    let client_addr = default_sockaddr(listener_socket.domain);
    let accepted = Arc::new(Mutex::new(SocketState::new(
        listener_socket.domain,
        SOCK_STREAM,
        IPPROTO_TCP,
        false,
    )));
    {
        let mut accepted_socket = accepted.lock();
        accepted_socket.bound = true;
        accepted_socket.connected = true;
        accepted_socket.local_addr = Some(local_addr.clone());
        accepted_socket.peer_addr = Some(client_addr.clone());
        accepted_socket.peer = Some(state.clone());
    }
    listener_socket.pending.push_back(accepted.clone());
    drop(listener_socket);
    let mut client_socket = state.lock();
    client_socket.bound = true;
    client_socket.connected = true;
    client_socket.local_addr = Some(client_addr);
    client_socket.peer_addr = Some(local_addr);
    client_socket.peer = Some(accepted);
    drop(client_socket);
    crate::task::wait_queue::wake_io_waiters();
    Ok(0)
}

pub fn sys_getsockname(fd: usize, addr: usize, addrlen: usize) -> SyscallRet {
    let state = socket_state_for_fd(fd)?;
    let socket = state.lock();
    let sockaddr = socket
        .local_addr
        .clone()
        .unwrap_or_else(|| default_sockaddr(socket.domain));
    drop(socket);
    copy_sockaddr_to_user(addr, addrlen, &sockaddr)?;
    Ok(0)
}

pub fn sys_getpeername(fd: usize, addr: usize, addrlen: usize) -> SyscallRet {
    let state = socket_state_for_fd(fd)?;
    let socket = state.lock();
    let sockaddr = socket.peer_addr.clone().ok_or(SysErrNo::ENOTCONN)?;
    drop(socket);
    copy_sockaddr_to_user(addr, addrlen, &sockaddr)?;
    Ok(0)
}

pub fn sys_sendto(
    fd: usize,
    buf: usize,
    len: usize,
    flags: usize,
    dest_addr: usize,
    addrlen: usize,
) -> SyscallRet {
    validate_msg_flags(flags)?;
    if len != 0 && buf == 0 {
        return Err(SysErrNo::EFAULT);
    }
    let dest = if dest_addr != 0 {
        Some(copy_sockaddr_from_user(dest_addr, addrlen)?)
    } else {
        None
    };
    if len == 0 {
        return Ok(0);
    }
    let state = socket_state_for_fd(fd)?;
    let socket = state.lock();
    if socket.shutdown_write {
        return Err(SysErrNo::EPIPE);
    }
    if socket.is_stream() && !socket.connected {
        return Err(SysErrNo::ENOTCONN);
    }
    let mut data = alloc::vec![0u8; len];
    super::user::copy_from_user(buf, &mut data)?;
    if socket.is_datagram() {
        let target_addr = match dest.or_else(|| socket.peer_addr.clone()) {
            Some(addr) => addr,
            None => return Err(SysErrNo::EDESTADDRREQ),
        };
        let source_addr = socket
            .local_addr
            .clone()
            .unwrap_or_else(|| default_sockaddr(socket.domain));
        let domain = socket.domain;
        drop(socket);
        let peer =
            find_bound_socket(domain, SOCK_DGRAM, &target_addr).ok_or(SysErrNo::ECONNREFUSED)?;
        peer.lock().dgram_queue.push_back(SocketPacket {
            data,
            addr: source_addr,
        });
        crate::task::wait_queue::wake_io_waiters();
        return Ok(len);
    }
    let peer = socket.peer.clone().ok_or(SysErrNo::ENOTCONN)?;
    drop(socket);
    peer.lock().rx_buf.extend(data.iter().copied());
    crate::task::wait_queue::wake_io_waiters();
    Ok(len)
}

pub fn sys_recvfrom(
    fd: usize,
    buf: usize,
    len: usize,
    flags: usize,
    src_addr: usize,
    addrlen: usize,
) -> SyscallRet {
    validate_msg_flags(flags)?;
    if len != 0 && buf == 0 {
        return Err(SysErrNo::EFAULT);
    }
    if len == 0 {
        return Ok(0);
    }
    let state = socket_state_for_fd(fd)?;
    let mut socket = state.lock();
    if socket.shutdown_read {
        return Ok(0);
    }
    if socket.is_stream() && !socket.connected {
        return Err(SysErrNo::ENOTCONN);
    }
    if socket.is_datagram() {
        let Some(packet) = socket.dgram_queue.pop_front() else {
            return Err(SysErrNo::EAGAIN);
        };
        let n = len.min(packet.data.len());
        super::user::copy_to_user(buf, &packet.data[..n])?;
        if src_addr != 0 {
            copy_sockaddr_to_user(src_addr, addrlen, &packet.addr)?;
        }
        crate::task::wait_queue::wake_io_waiters();
        return Ok(n);
    }
    if socket.rx_buf.is_empty() {
        return Err(SysErrNo::EAGAIN);
    }
    let mut out = alloc::vec![0u8; len];
    let mut n = 0usize;
    while n < len {
        if let Some(byte) = socket.rx_buf.pop_front() {
            out[n] = byte;
            n += 1;
        } else {
            break;
        }
    }
    drop(socket);
    super::user::copy_to_user(buf, &out[..n])?;
    crate::task::wait_queue::wake_io_waiters();
    Ok(n)
}

pub fn sys_setsockopt(
    fd: usize,
    level: usize,
    optname: usize,
    optval: usize,
    optlen: usize,
) -> SyscallRet {
    let state = socket_state_for_fd(fd)?;
    let mut socket = state.lock();
    match (level, optname) {
        (SOL_SOCKET, SO_REUSEADDR) => {
            socket.reuse_addr = copy_i32_option_in(optval, optlen)? != 0;
            Ok(0)
        }
        (SOL_SOCKET, SO_REUSEPORT) => {
            socket.reuse_port = copy_i32_option_in(optval, optlen)? != 0;
            Ok(0)
        }
        (SOL_SOCKET, SO_KEEPALIVE) => {
            socket.keepalive = copy_i32_option_in(optval, optlen)? != 0;
            Ok(0)
        }
        (SOL_SOCKET, SO_BROADCAST) => {
            socket.broadcast = copy_i32_option_in(optval, optlen)? != 0;
            Ok(0)
        }
        (SOL_SOCKET, SO_SNDBUF) => {
            socket.sndbuf = copy_i32_option_in(optval, optlen)?.max(1) as usize;
            Ok(0)
        }
        (SOL_SOCKET, SO_RCVBUF) => {
            socket.rcvbuf = copy_i32_option_in(optval, optlen)?.max(1) as usize;
            Ok(0)
        }
        (SOL_SOCKET, SO_RCVTIMEO) => {
            socket.recv_timeout_us = copy_timeval_option_in(optval, optlen)?;
            Ok(0)
        }
        (SOL_SOCKET, SO_SNDTIMEO) => {
            socket.send_timeout_us = copy_timeval_option_in(optval, optlen)?;
            Ok(0)
        }
        (level, TCP_NODELAY) if level == IPPROTO_TCP as usize && socket.is_stream() => {
            socket.tcp_nodelay = copy_i32_option_in(optval, optlen)? != 0;
            Ok(0)
        }
        _ => Err(SysErrNo::ENOPROTOOPT),
    }
}

pub fn sys_getsockopt(
    fd: usize,
    level: usize,
    optname: usize,
    optval: usize,
    optlen: usize,
) -> SyscallRet {
    let state = socket_state_for_fd(fd)?;
    let socket = state.lock();
    let value = match (level, optname) {
        (SOL_SOCKET, SO_TYPE) => socket.sock_type as i32,
        (SOL_SOCKET, SO_ERROR) => socket.error,
        (SOL_SOCKET, SO_PROTOCOL) => socket.protocol,
        (SOL_SOCKET, SO_REUSEADDR) => socket.reuse_addr as i32,
        (SOL_SOCKET, SO_REUSEPORT) => socket.reuse_port as i32,
        (SOL_SOCKET, SO_KEEPALIVE) => socket.keepalive as i32,
        (SOL_SOCKET, SO_BROADCAST) => socket.broadcast as i32,
        (SOL_SOCKET, SO_SNDBUF) => socket.sndbuf as i32,
        (SOL_SOCKET, SO_RCVBUF) => socket.rcvbuf as i32,
        (level, TCP_NODELAY) if level == IPPROTO_TCP as usize && socket.is_stream() => {
            socket.tcp_nodelay as i32
        }
        _ => return Err(SysErrNo::ENOPROTOOPT),
    };
    drop(socket);
    copy_i32_option_out(optval, optlen, value)?;
    Ok(0)
}

pub fn sys_shutdown(fd: usize, how: usize) -> SyscallRet {
    let state = socket_state_for_fd(fd)?;
    let mut socket = state.lock();
    match how {
        SHUT_RD => socket.shutdown_read = true,
        SHUT_WR => socket.shutdown_write = true,
        SHUT_RDWR => {
            socket.shutdown_read = true;
            socket.shutdown_write = true;
        }
        _ => return Err(SysErrNo::EINVAL),
    }
    drop(socket);
    crate::task::wait_queue::wake_io_waiters();
    Ok(0)
}
