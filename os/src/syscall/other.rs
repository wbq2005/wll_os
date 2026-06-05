use super::SyscallRet;
use crate::task::current_task;
use crate::task::wait_queue::WaitOutcome;
use crate::timer;
use crate::utils::error::SysErrNo;
use alloc::sync::Arc;
use alloc::vec::Vec;
use lazy_static::lazy_static;
use spin::Mutex;

#[repr(C)]
#[derive(Clone, Copy)]
struct TimeSpec {
    tv_sec: isize,
    tv_nsec: isize,
}

#[derive(Clone, Copy)]
pub(crate) struct RobustExitState {
    head: usize,
    len: usize,
}

struct FutexWaiter {
    uaddr: usize,
    key: usize,
    bitset: usize,
    task: Arc<crate::task::TaskControlBlock>,
    token: usize,
}

lazy_static! {
    static ref FUTEX_WAITERS: Mutex<Vec<FutexWaiter>> = Mutex::new(Vec::new());
}

#[repr(C)]
#[derive(Clone, Copy)]
struct RLimit {
    rlim_cur: usize,
    rlim_max: usize,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct TimeVal {
    tv_sec: usize,
    tv_usec: usize,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct Tms {
    tms_utime: isize,
    tms_stime: isize,
    tms_cutime: isize,
    tms_cstime: isize,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct UtsName {
    sysname: [u8; 65],
    nodename: [u8; 65],
    release: [u8; 65],
    version: [u8; 65],
    machine: [u8; 65],
    domainname: [u8; 65],
}

fn write_c_string(dst: &mut [u8; 65], value: &str) {
    let bytes = value.as_bytes();
    let len = bytes.len().min(64);
    dst[..len].copy_from_slice(&bytes[..len]);
    dst[len] = 0;
}

/// nanosleep 系统调用
fn copy_to_user(dst: usize, src: &[u8]) -> Result<(), SysErrNo> {
    super::user::copy_to_user(dst, src)
}

fn copy_object_to_user<T>(dst: usize, obj: &T) -> Result<(), SysErrNo> {
    super::user::copy_object_to_user(dst, obj)
}

fn copy_object_from_user<T: Copy>(src: usize) -> Result<T, SysErrNo> {
    super::user::copy_object_from_user(src)
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

fn timespec_to_us(ts: TimeSpec) -> Result<usize, SysErrNo> {
    duration_us_from_timespec(ts)
}

fn deadline_from_timespec_ptr(ptr: usize) -> Result<Option<usize>, SysErrNo> {
    if ptr == 0 {
        return Ok(None);
    }
    let duration_us = duration_us_from_timespec(copy_object_from_user::<TimeSpec>(ptr)?)?;
    Ok(Some(timer::deadline_after_us(duration_us)))
}

fn deadline_from_absolute_timespec_ptr(ptr: usize) -> Result<Option<usize>, SysErrNo> {
    if ptr == 0 {
        return Ok(None);
    }
    Ok(Some(timespec_to_us(copy_object_from_user::<TimeSpec>(ptr)?)?))
}

fn read_user_i32(addr: usize) -> Result<i32, SysErrNo> {
    let mut bytes = [0u8; core::mem::size_of::<i32>()];
    super::user::copy_from_user(addr, &mut bytes)?;
    Ok(i32::from_le_bytes(bytes))
}

fn validate_futex_uaddr(uaddr: usize) -> Result<(), SysErrNo> {
    if uaddr == 0 {
        return Err(SysErrNo::EFAULT);
    }
    if uaddr % core::mem::align_of::<i32>() != 0 {
        return Err(SysErrNo::EINVAL);
    }
    let mut bytes = [0u8; core::mem::size_of::<i32>()];
    super::user::copy_from_user(uaddr, &mut bytes)?;
    Ok(())
}

fn futex_key_for_task(task: &Arc<crate::task::TaskControlBlock>) -> usize {
    Arc::as_ptr(&task.memory_set) as usize
}

fn futex_key_for_op(task: &Arc<crate::task::TaskControlBlock>, private: bool) -> usize {
    if private {
        futex_key_for_task(task)
    } else {
        0
    }
}

pub fn sys_nanosleep(req: usize, rem: usize) -> SyscallRet {
    if req == 0 {
        return Err(SysErrNo::EFAULT);
    }

    let req = copy_object_from_user::<TimeSpec>(req)?;
    let sleep_us = duration_us_from_timespec(req)?;
    if sleep_us != 0 {
        let _ = timer::sleep_until_us(timer::deadline_after_us(sleep_us))?;
    }

    if rem != 0 {
        copy_object_to_user(
            rem,
            &TimeSpec {
                tv_sec: 0,
                tv_nsec: 0,
            },
        )?;
    }

    Ok(0)
}

/// gettimeofday 系统调用
pub fn sys_gettimeofday(tv: usize, tz: usize) -> SyscallRet {
    if tv == 0 {
        return Err(SysErrNo::EFAULT);
    }
    let (sec, usec) = timer::get_timeval();
    copy_object_to_user(
        tv,
        &TimeVal {
            tv_sec: sec,
            tv_usec: usec,
        },
    )?;
    if tz != 0 {
        copy_to_user(tz, &[0; 8])?;
    }
    Ok(0)
}

pub fn sys_clock_gettime(_clock_id: usize, tp: usize) -> SyscallRet {
    if tp == 0 {
        return Err(SysErrNo::EFAULT);
    }
    let time_us = timer::get_time_us();
    copy_object_to_user(
        tp,
        &TimeSpec {
            tv_sec: (time_us / 1_000_000) as isize,
            tv_nsec: ((time_us % 1_000_000) * 1000) as isize,
        },
    )?;
    Ok(0)
}

pub fn sys_clock_getres(_clock_id: usize, tp: usize) -> SyscallRet {
    if tp == 0 {
        return Err(SysErrNo::EFAULT);
    }
    copy_object_to_user(
        tp,
        &TimeSpec {
            tv_sec: 0,
            tv_nsec: 1_000,
        },
    )?;
    Ok(0)
}

/// uname 系统调用
pub fn sys_uname(buf: usize) -> SyscallRet {
    if buf == 0 {
        return Err(SysErrNo::EFAULT);
    }
    let mut uts = UtsName {
        sysname: [0; 65],
        nodename: [0; 65],
        release: [0; 65],
        version: [0; 65],
        machine: [0; 65],
        domainname: [0; 65],
    };
    write_c_string(&mut uts.sysname, "wll_OS");
    write_c_string(&mut uts.nodename, "os-contest");
    write_c_string(&mut uts.release, "5.10.0");
    write_c_string(&mut uts.version, "2026");
    #[cfg(target_arch = "riscv64")]
    write_c_string(&mut uts.machine, "riscv64");
    #[cfg(target_arch = "loongarch64")]
    write_c_string(&mut uts.machine, "loongarch64");
    write_c_string(&mut uts.domainname, "localdomain");
    copy_object_to_user(buf, &uts)?;
    Ok(0)
}

pub fn sys_times(buf: usize) -> SyscallRet {
    if buf != 0 {
        let ticks = timer::get_time() as isize;
        copy_object_to_user(
            buf,
            &Tms {
                tms_utime: ticks,
                tms_stime: 0,
                tms_cutime: 0,
                tms_cstime: 0,
            },
        )?;
    }
    Ok(timer::get_time())
}

pub fn sys_gettid() -> SyscallRet {
    current_task().map(|task| task.pid.0).ok_or(SysErrNo::ESRCH)
}

pub fn sys_getuid() -> SyscallRet {
    Ok(0)
}

pub fn sys_geteuid() -> SyscallRet {
    Ok(0)
}

pub fn sys_getgid() -> SyscallRet {
    Ok(0)
}

pub fn sys_getegid() -> SyscallRet {
    Ok(0)
}

pub fn sys_prlimit64(
    _pid: usize,
    _resource: usize,
    new_limit: usize,
    old_limit: usize,
) -> SyscallRet {
    let _ = new_limit;
    if old_limit != 0 {
        copy_object_to_user(
            old_limit,
            &RLimit {
                rlim_cur: usize::MAX,
                rlim_max: usize::MAX,
            },
        )?;
    }
    Ok(0)
}

pub fn sys_set_tid_address(tidptr: usize) -> SyscallRet {
    let task = current_task().ok_or(SysErrNo::ESRCH)?;
    // set_tid_address only records the user word to clear on exit; it does not
    // write immediately.  clone(CLONE_CHILD_CLEARTID) uses the same field.
    task.inner.lock().clear_child_tid = tidptr;
    Ok(task.pid.0)
}

pub fn sys_set_robust_list(head: usize, len: usize) -> SyscallRet {
    const ROBUST_LIST_HEAD_SIZE: usize = 24;
    if len != ROBUST_LIST_HEAD_SIZE {
        return Err(SysErrNo::EINVAL);
    }
    let task = current_task().ok_or(SysErrNo::ESRCH)?;
    let mut inner = task.inner.lock();
    inner.robust_list_head = head;
    inner.robust_list_len = len;
    Ok(0)
}

pub(crate) fn take_robust_exit_state(
    task: &Arc<crate::task::TaskControlBlock>,
) -> Option<RobustExitState> {
    let mut inner = task.inner.lock();
    let state = if inner.robust_list_head != 0 {
        Some(RobustExitState {
            head: inner.robust_list_head,
            len: inner.robust_list_len,
        })
    } else {
        None
    };
    inner.robust_list_head = 0;
    inner.robust_list_len = 0;
    state
}

pub fn sys_getrandom(buf: usize, buflen: usize, _flags: usize) -> SyscallRet {
    if buf == 0 {
        return Err(SysErrNo::EFAULT);
    }
    let time = timer::get_time();
    let mut bytes = alloc::vec![0u8; buflen];
    for (i, byte) in bytes.iter_mut().enumerate() {
        *byte = ((time.wrapping_mul(1103515245).wrapping_add(12345 + i)) >> 16) as u8;
    }
    copy_to_user(buf, &bytes)?;
    Ok(buflen)
}

pub fn sys_sysinfo(info: usize) -> SyscallRet {
    if info == 0 {
        return Err(SysErrNo::EFAULT);
    }
    #[repr(C)]
    struct SysInfo {
        uptime: isize,
        loads: [usize; 3],
        totalram: usize,
        freeram: usize,
        sharedram: usize,
        bufferram: usize,
        totalswap: usize,
        freeswap: usize,
        procs: u16,
        pad: u16,
        pad2: u32,
        totalhigh: usize,
        freehigh: usize,
        mem_unit: u32,
        _pad: [u8; 4],
    }
    let si = SysInfo {
        uptime: (timer::get_time_us() / 1_000_000) as isize,
        loads: [0; 3],
        totalram: 128 * 1024 * 1024,
        freeram: 64 * 1024 * 1024,
        sharedram: 0,
        bufferram: 0,
        totalswap: 0,
        freeswap: 0,
        procs: 1,
        pad: 0,
        pad2: 0,
        totalhigh: 0,
        freehigh: 0,
        mem_unit: 1,
        _pad: [0; 4],
    };
    copy_object_to_user(info, &si)?;
    Ok(0)
}

pub fn sys_syslog(action: usize, buf: usize, len: usize) -> SyscallRet {
    const SYSLOG_ACTION_READ: usize = 2;
    const SYSLOG_ACTION_READ_ALL: usize = 3;
    const SYSLOG_ACTION_READ_CLEAR: usize = 4;
    const SYSLOG_ACTION_CLEAR: usize = 5;
    const SYSLOG_ACTION_CONSOLE_OFF: usize = 6;
    const SYSLOG_ACTION_CONSOLE_ON: usize = 7;
    const SYSLOG_ACTION_CONSOLE_LEVEL: usize = 8;
    const SYSLOG_ACTION_SIZE_UNREAD: usize = 9;
    const SYSLOG_ACTION_SIZE_BUFFER: usize = 10;

    match action {
        SYSLOG_ACTION_READ | SYSLOG_ACTION_READ_ALL | SYSLOG_ACTION_READ_CLEAR => {
            if buf == 0 && len != 0 {
                return Err(SysErrNo::EFAULT);
            }
            Ok(0)
        }
        SYSLOG_ACTION_CLEAR
        | SYSLOG_ACTION_CONSOLE_OFF
        | SYSLOG_ACTION_CONSOLE_ON
        | SYSLOG_ACTION_CONSOLE_LEVEL
        | SYSLOG_ACTION_SIZE_UNREAD
        | SYSLOG_ACTION_SIZE_BUFFER => Ok(0),
        _ => Err(SysErrNo::EINVAL),
    }
}

pub fn sys_getrusage(_who: usize, usage: usize) -> SyscallRet {
    if usage != 0 {
        copy_to_user(usage, &[0; 144])?;
    }
    Ok(0)
}

pub fn sys_umask(_mask: usize) -> SyscallRet {
    Ok(0o022)
}

pub fn sys_getpgid(_pid: usize) -> SyscallRet {
    current_task()
        .map(|task| task.thread_group.tgid())
        .ok_or(SysErrNo::ESRCH)
}

pub fn sys_setpgid(_pid: usize, _pgid: usize) -> SyscallRet {
    Ok(0)
}

pub fn sys_membarrier(_cmd: usize, _flags: usize) -> SyscallRet {
    Ok(0)
}

pub fn sys_sched_stub() -> SyscallRet {
    Ok(0)
}

pub(crate) fn futex_wake_addr_for_task(
    task: &Arc<crate::task::TaskControlBlock>,
    uaddr: usize,
    n: usize,
) -> usize {
    futex_wake_addr_key(uaddr, futex_key_for_task(task), n, usize::MAX)
}

fn futex_wake_addr_private_and_shared(
    task: &Arc<crate::task::TaskControlBlock>,
    uaddr: usize,
    n: usize,
) -> usize {
    let private_woke = futex_wake_addr_key(uaddr, futex_key_for_task(task), n, usize::MAX);
    let remaining = n.saturating_sub(private_woke);
    private_woke + futex_wake_addr_key(uaddr, 0, remaining, usize::MAX)
}

fn futex_wake_addr_bitset(uaddr: usize, private: bool, n: usize, bitset: usize) -> usize {
    if uaddr == 0 || n == 0 {
        return 0;
    }
    let Some(task) = current_task() else {
        return 0;
    };
    let key = futex_key_for_op(&task, private);
    futex_wake_addr_key(uaddr, key, n, bitset)
}

fn futex_wake_addr_key(uaddr: usize, key: usize, n: usize, bitset: usize) -> usize {
    if uaddr == 0 || n == 0 {
        return 0;
    }
    let mut woke = 0usize;
    while woke < n {
        let waiter = {
            let mut waiters = FUTEX_WAITERS.lock();
            let Some(index) = waiters
                .iter()
                .position(|waiter| {
                    waiter.uaddr == uaddr && waiter.key == key && (waiter.bitset & bitset) != 0
                })
            else {
                break;
            };
            waiters.remove(index)
        };
        if crate::task::wake_task_token_with(&waiter.task, waiter.token, WaitOutcome::Woken) {
            woke += 1;
        }
    }
    woke
}

fn futex_wait_addr(
    uaddr: usize,
    val: usize,
    deadline: Option<usize>,
    bitset: usize,
    private: bool,
) -> SyscallRet {
    if bitset == 0 {
        return Err(SysErrNo::EINVAL);
    }
    validate_futex_uaddr(uaddr)?;
    if read_user_i32(uaddr)? != val as i32 {
        return Err(SysErrNo::EAGAIN);
    }
    if deadline
        .map(|deadline| timer::get_time_us() >= deadline)
        .unwrap_or(false)
    {
        return Err(SysErrNo::ETIMEDOUT);
    }

    let task = current_task().ok_or(SysErrNo::ESRCH)?;
    let key = futex_key_for_op(&task, private);
    let token = task.next_wait_token();
    *task.wait_outcome.lock() = None;
    *task.block_reason.lock() = Some(crate::task::wait_queue::BlockReason::Futex);
    FUTEX_WAITERS.lock().push(FutexWaiter {
        uaddr,
        key,
        bitset,
        task: task.clone(),
        token,
    });
    match read_user_i32(uaddr) {
        Ok(current) if current == val as i32 => {}
        Ok(_) => {
            remove_futex_waiter(uaddr, key, task.pid.0, token);
            *task.block_reason.lock() = None;
            return Err(SysErrNo::EAGAIN);
        }
        Err(err) => {
            remove_futex_waiter(uaddr, key, task.pid.0, token);
            *task.block_reason.lock() = None;
            return Err(err);
        }
    }
    if crate::syscall::signal::current_has_unblocked_pending() {
        remove_futex_waiter(uaddr, key, task.pid.0, token);
        *task.block_reason.lock() = None;
        return Err(SysErrNo::EINTR);
    }
    if let Some(deadline) = deadline {
        timer::add_timeout(deadline, task.clone(), token);
    }

    crate::task::block_current_for_reason_until(crate::task::wait_queue::BlockReason::Futex, deadline);

    *task.block_reason.lock() = None;
    let still_waiting = remove_futex_waiter(uaddr, key, task.pid.0, token);
    match crate::task::wait_queue::finish_wait(&task, still_waiting, deadline) {
        WaitOutcome::TimedOut => Err(SysErrNo::ETIMEDOUT),
        WaitOutcome::Interrupted => Err(SysErrNo::EINTR),
        WaitOutcome::Woken => Ok(0),
    }
}

fn remove_futex_waiter(uaddr: usize, key: usize, pid: usize, token: usize) -> bool {
    let mut waiters = FUTEX_WAITERS.lock();
    if let Some(index) = waiters.iter().position(|waiter| {
        waiter.uaddr == uaddr
            && waiter.key == key
            && waiter.task.pid.0 == pid
            && waiter.token == token
    }) {
        waiters.remove(index);
        true
    } else {
        false
    }
}

pub(crate) fn process_robust_list_on_exit(task: &Arc<crate::task::TaskControlBlock>) {
    const ROBUST_LIST_HEAD_SIZE: usize = 24;
    const FUTEX_TID_MASK: i32 = 0x3fff_ffff;
    const FUTEX_OWNER_DIED: i32 = 0x4000_0000;
    const FUTEX_WAITERS: i32 = 0x8000_0000u32 as i32;
    const MAX_ROBUST_ENTRIES: usize = 2048;

    let Some(state) = take_robust_exit_state(task) else {
        return;
    };
    if state.len != ROBUST_LIST_HEAD_SIZE {
        return;
    }

    let memory_set = task.memory_set.lock();
    let read_usize_at = |addr: usize| -> Result<usize, SysErrNo> {
        let mut bytes = [0u8; core::mem::size_of::<usize>()];
        super::user::copy_from_user_in_memory_set(&memory_set, addr, &mut bytes)?;
        Ok(usize::from_ne_bytes(bytes))
    };
    let read_i32_at = |addr: usize| -> Result<i32, SysErrNo> {
        let mut bytes = [0u8; core::mem::size_of::<i32>()];
        super::user::copy_from_user_in_memory_set(&memory_set, addr, &mut bytes)?;
        Ok(i32::from_ne_bytes(bytes))
    };
    let write_i32_at = |addr: usize, value: i32| -> Result<(), SysErrNo> {
        super::user::copy_to_user_in_memory_set(&memory_set, addr, &value.to_ne_bytes())
    };

    let futex_offset = match read_usize_at(state.head + core::mem::size_of::<usize>()) {
        Ok(raw) => raw as isize,
        Err(_) => return,
    };
    let pending = read_usize_at(state.head + 2 * core::mem::size_of::<usize>()).unwrap_or(0);
    let mut next = match read_usize_at(state.head) {
        Ok(next) => next,
        Err(_) => return,
    };
    let tid = task.pid.0 as i32;

    let mark_entry = |entry: usize| {
        if entry == 0 || entry == state.head {
            return;
        }
        let Some(futex_addr) = (entry as isize)
            .checked_add(futex_offset)
            .and_then(|addr| usize::try_from(addr).ok())
        else {
            return;
        };
        let Ok(value) = read_i32_at(futex_addr) else {
            return;
        };
        if (value & FUTEX_TID_MASK) != tid {
            return;
        }
        let new_value = (value & FUTEX_WAITERS) | FUTEX_OWNER_DIED;
        if write_i32_at(futex_addr, new_value).is_ok() {
            futex_wake_addr_private_and_shared(task, futex_addr, 1);
        }
    };

    mark_entry(pending);
    for _ in 0..MAX_ROBUST_ENTRIES {
        if next == 0 || next == state.head {
            break;
        }
        mark_entry(next);
        match read_usize_at(next) {
            Ok(new_next) => next = new_next,
            Err(_) => break,
        }
    }
}

pub fn sys_futex_stub(
    uaddr: usize,
    futex_op: usize,
    val: usize,
    timeout: usize,
    _uaddr2: usize,
    _val3: usize,
) -> SyscallRet {
    const FUTEX_WAIT: usize = 0;
    const FUTEX_WAKE: usize = 1;
    const FUTEX_WAIT_BITSET: usize = 9;
    const FUTEX_WAKE_BITSET: usize = 10;
    const FUTEX_CMD_MASK: usize = 0x7f;
    const FUTEX_PRIVATE_FLAG: usize = 0x80;
    let op = futex_op & FUTEX_CMD_MASK;
    let private = (futex_op & FUTEX_PRIVATE_FLAG) != 0;
    match op {
        FUTEX_WAIT => {
            let deadline = deadline_from_timespec_ptr(timeout)?;
            futex_wait_addr(uaddr, val, deadline, usize::MAX, private)
        }
        FUTEX_WAKE => {
            validate_futex_uaddr(uaddr)?;
            Ok(futex_wake_addr_bitset(uaddr, private, val, usize::MAX))
        }
        FUTEX_WAIT_BITSET => {
            let deadline = deadline_from_absolute_timespec_ptr(timeout)?;
            futex_wait_addr(uaddr, val, deadline, _val3, private)
        }
        FUTEX_WAKE_BITSET => {
            validate_futex_uaddr(uaddr)?;
            if _val3 == 0 {
                return Err(SysErrNo::EINVAL);
            }
            Ok(futex_wake_addr_bitset(uaddr, private, val, _val3))
        }
        _ => Err(SysErrNo::ENOSYS),
    }
}
