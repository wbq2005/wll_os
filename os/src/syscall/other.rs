use super::SyscallRet;
use crate::task::current_task;
use crate::timer;
use crate::utils::error::SysErrNo;
use alloc::sync::Arc;
use alloc::vec::Vec;
use lazy_static::lazy_static;
use spin::Mutex;

#[repr(C)]
#[derive(Clone, Copy)]
struct TimeSpec {
    tv_sec: usize,
    tv_nsec: usize,
}

struct FutexWaiter {
    uaddr: usize,
    key: usize,
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
    if ts.tv_nsec >= 1_000_000_000 {
        return Err(SysErrNo::EINVAL);
    }
    Ok(ts
        .tv_sec
        .saturating_mul(1_000_000)
        .saturating_add(ts.tv_nsec.div_ceil(1000)))
}

fn deadline_from_timespec_ptr(ptr: usize) -> Result<Option<usize>, SysErrNo> {
    if ptr == 0 {
        return Ok(None);
    }
    let duration_us = duration_us_from_timespec(copy_object_from_user::<TimeSpec>(ptr)?)?;
    Ok(Some(timer::deadline_after_us(duration_us)))
}

fn read_user_i32(addr: usize) -> Result<i32, SysErrNo> {
    let mut bytes = [0u8; core::mem::size_of::<i32>()];
    super::user::copy_from_user(addr, &mut bytes)?;
    Ok(i32::from_le_bytes(bytes))
}

fn futex_key_for_task(task: &Arc<crate::task::TaskControlBlock>) -> usize {
    Arc::as_ptr(&task.memory_set) as usize
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
            tv_sec: time_us / 1_000_000,
            tv_nsec: (time_us % 1_000_000) * 1000,
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

pub fn sys_set_robust_list(_head: usize, _len: usize) -> SyscallRet {
    Ok(0)
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

pub fn futex_wake_addr(uaddr: usize, n: usize) -> usize {
    if uaddr == 0 || n == 0 {
        return 0;
    }
    let Some(task) = current_task() else {
        return 0;
    };
    let key = futex_key_for_task(&task);
    let mut woke = 0usize;
    while woke < n {
        let waiter = {
            let mut waiters = FUTEX_WAITERS.lock();
            let Some(index) = waiters
                .iter()
                .position(|waiter| waiter.uaddr == uaddr && waiter.key == key)
            else {
                break;
            };
            waiters.remove(index)
        };
        if crate::task::wake_task_token(&waiter.task, waiter.token) {
            woke += 1;
        }
    }
    woke
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
    const FUTEX_CMD_MASK: usize = 0x7f;
    let op = futex_op & FUTEX_CMD_MASK;
    match op {
        FUTEX_WAIT => {
            if uaddr == 0 {
                return Err(SysErrNo::EFAULT);
            }
            if read_user_i32(uaddr)? != val as i32 {
                return Err(SysErrNo::EAGAIN);
            }
            let deadline = deadline_from_timespec_ptr(timeout)?;
            if deadline
                .map(|deadline| timer::get_time_us() >= deadline)
                .unwrap_or(false)
            {
                return Err(SysErrNo::ETIMEDOUT);
            }

            let task = current_task().ok_or(SysErrNo::ESRCH)?;
            let key = futex_key_for_task(&task);
            let token = task.next_wait_token();
            FUTEX_WAITERS.lock().push(FutexWaiter {
                uaddr,
                key,
                task: task.clone(),
                token,
            });
            if let Some(deadline) = deadline {
                timer::add_timeout(deadline, task.clone(), token);
            }

            crate::task::wait_queue::block_current_for(crate::task::wait_queue::BlockReason::Futex);

            let still_waiting = remove_futex_waiter(uaddr, key, task.pid.0, token);
            if crate::syscall::signal::current_has_unblocked_pending() {
                return Err(SysErrNo::EINTR);
            }
            if still_waiting
                && deadline
                    .map(|deadline| timer::get_time_us() >= deadline)
                    .unwrap_or(false)
            {
                Err(SysErrNo::ETIMEDOUT)
            } else {
                Ok(0)
            }
        }
        FUTEX_WAKE => Ok(futex_wake_addr(uaddr, val)),
        _ => Ok(0),
    }
}
