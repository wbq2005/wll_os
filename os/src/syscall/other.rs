use crate::utils::error::SysErrNo;
use super::SyscallRet;
use crate::timer;
use crate::task::current_task;
use polyhal::VirtAddr;

#[repr(C)]
#[derive(Clone, Copy)]
struct TimeSpec {
    tv_sec: usize,
    tv_nsec: usize,
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
    if dst == 0 {
        return Err(SysErrNo::EFAULT);
    }
    let task = current_task().ok_or(SysErrNo::ESRCH)?;
    let memory_set = task.memory_set.lock();
    let mut addr = dst;
    for &byte in src {
        let pa = memory_set
            .translate(VirtAddr::new(addr))
            .ok_or(SysErrNo::EFAULT)?;
        unsafe { *(pa.raw() as *mut u8) = byte; }
        addr += 1;
    }
    Ok(())
}

fn copy_from_user(src: usize, dst: &mut [u8]) -> Result<(), SysErrNo> {
    if src == 0 {
        return Err(SysErrNo::EFAULT);
    }
    let task = current_task().ok_or(SysErrNo::ESRCH)?;
    let memory_set = task.memory_set.lock();
    let mut addr = src;
    for byte in dst {
        let pa = memory_set
            .translate(VirtAddr::new(addr))
            .ok_or(SysErrNo::EFAULT)?;
        *byte = unsafe { *(pa.raw() as *const u8) };
        addr += 1;
    }
    Ok(())
}

fn copy_object_to_user<T>(dst: usize, obj: &T) -> Result<(), SysErrNo> {
    let bytes = unsafe {
        core::slice::from_raw_parts(obj as *const T as *const u8, core::mem::size_of::<T>())
    };
    copy_to_user(dst, bytes)
}

fn copy_object_from_user<T: Copy>(src: usize) -> Result<T, SysErrNo> {
    let mut obj = core::mem::MaybeUninit::<T>::uninit();
    let bytes = unsafe {
        core::slice::from_raw_parts_mut(
            obj.as_mut_ptr() as *mut u8,
            core::mem::size_of::<T>(),
        )
    };
    copy_from_user(src, bytes)?;
    Ok(unsafe { obj.assume_init() })
}

pub fn sys_nanosleep(req: usize, rem: usize) -> SyscallRet {
    if req == 0 {
        return Err(SysErrNo::EFAULT);
    }

    let req = copy_object_from_user::<TimeSpec>(req)?;
    let sleep_ms = req.tv_sec.saturating_mul(1000).saturating_add(req.tv_nsec.div_ceil(1_000_000));
    timer::sleep_ms(sleep_ms);

    if rem != 0 {
        unsafe {
            copy_object_to_user(rem, &TimeSpec { tv_sec: 0, tv_nsec: 0 })?;
        }
    }

    Ok(0)
}

/// gettimeofday 系统调用
pub fn sys_gettimeofday(tv: usize, tz: usize) -> SyscallRet {
    if tv == 0 {
        return Err(SysErrNo::EFAULT);
    }
    let (sec, usec) = timer::get_timeval();
    unsafe {
        copy_object_to_user(tv, &TimeVal { tv_sec: sec, tv_usec: usec })?;
    }
    if tz != 0 {
        unsafe {
            copy_to_user(tz, &[0; 8])?;
        }
    }
    Ok(0)
}

pub fn sys_clock_gettime(_clock_id: usize, tp: usize) -> SyscallRet {
    if tp == 0 {
        return Err(SysErrNo::EFAULT);
    }
    let time_us = timer::get_time_us();
    unsafe {
        copy_object_to_user(tp, &TimeSpec {
            tv_sec: time_us / 1_000_000,
            tv_nsec: (time_us % 1_000_000) * 1000,
        })?;
    }
    Ok(0)
}

pub fn sys_clock_getres(_clock_id: usize, tp: usize) -> SyscallRet {
    if tp == 0 {
        return Err(SysErrNo::EFAULT);
    }
    unsafe {
        copy_object_to_user(tp, &TimeSpec {
            tv_sec: 0,
            tv_nsec: 1_000,
        })?;
    }
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
    unsafe {
        copy_object_to_user(buf, &uts)?;
    }
    Ok(0)
}

pub fn sys_times(buf: usize) -> SyscallRet {
    if buf != 0 {
        let ticks = timer::get_time() as isize;
        unsafe {
            copy_object_to_user(buf, &Tms {
                tms_utime: ticks,
                tms_stime: 0,
                tms_cutime: 0,
                tms_cstime: 0,
            })?;
        }
    }
    Ok(timer::get_time())
}

pub fn sys_gettid() -> SyscallRet {
    current_task()
        .map(|task| task.pid.0)
        .ok_or(SysErrNo::ESRCH)
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

pub fn sys_prlimit64(_pid: usize, _resource: usize, new_limit: usize, old_limit: usize) -> SyscallRet {
    let _ = new_limit;
    if old_limit != 0 {
        unsafe {
            copy_object_to_user(old_limit, &RLimit {
                rlim_cur: usize::MAX,
                rlim_max: usize::MAX,
            })?;
        }
    }
    Ok(0)
}

pub fn sys_set_tid_address(_tidptr: usize) -> SyscallRet {
    current_task()
        .map(|task| task.pid.0)
        .ok_or(SysErrNo::ESRCH)
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
        .map(|task| task.pid.0)
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

pub fn sys_futex_stub(
    _uaddr: usize,
    futex_op: usize,
    _val: usize,
    _timeout: usize,
    _uaddr2: usize,
    _val3: usize,
) -> SyscallRet {
    const FUTEX_WAIT: usize = 0;
    const FUTEX_WAKE: usize = 1;
    const FUTEX_PRIVATE_FLAG: usize = 128;
    let op = futex_op & !(FUTEX_PRIVATE_FLAG);
    match op {
        FUTEX_WAIT => {
            crate::task::suspend_current_and_run_next();
            Ok(0)
        }
        FUTEX_WAKE => Ok(0),
        _ => Ok(0),
    }
}
