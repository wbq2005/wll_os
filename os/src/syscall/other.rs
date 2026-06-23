use super::SyscallRet;
use crate::task::wait_queue::WaitOutcome;
use crate::task::{
    current_task, manager, SCHED_BATCH, SCHED_DEADLINE, SCHED_FIFO, SCHED_IDLE, SCHED_OTHER,
    SCHED_RR,
};
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

const RLIMIT_NOFILE: usize = 7;
const RLIMIT_STACK: usize = 3;
const DEFAULT_STACK_LIMIT: usize = 256 * 1024;

#[repr(C)]
#[derive(Clone, Copy)]
struct TimeVal {
    tv_sec: usize,
    tv_usec: usize,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct RUsage {
    ru_utime: TimeVal,
    ru_stime: TimeVal,
    ru_maxrss: isize,
    ru_ixrss: isize,
    ru_idrss: isize,
    ru_isrss: isize,
    ru_minflt: isize,
    ru_majflt: isize,
    ru_nswap: isize,
    ru_inblock: isize,
    ru_oublock: isize,
    ru_msgsnd: isize,
    ru_msgrcv: isize,
    ru_nsignals: isize,
    ru_nvcsw: isize,
    ru_nivcsw: isize,
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
struct SchedParam {
    sched_priority: i32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct SchedAttr {
    size: u32,
    sched_policy: u32,
    sched_flags: u64,
    sched_nice: i32,
    sched_priority: u32,
    sched_runtime: u64,
    sched_deadline: u64,
    sched_period: u64,
    sched_util_min: u32,
    sched_util_max: u32,
}

const SCHED_ATTR_SIZE_VER0: usize = 48;
const SCHED_RESET_ON_FORK: usize = 0x4000_0000;
const SCHED_POLICY_UNCHANGED: usize = usize::MAX;

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
    Ok(Some(timespec_to_us(copy_object_from_user::<TimeSpec>(
        ptr,
    )?)?))
}

fn read_user_i32(addr: usize) -> Result<i32, SysErrNo> {
    let mut bytes = [0u8; core::mem::size_of::<i32>()];
    super::user::copy_from_user(addr, &mut bytes)?;
    Ok(i32::from_le_bytes(bytes))
}

fn write_user_i32(addr: usize, value: i32) -> Result<(), SysErrNo> {
    super::user::copy_to_user(addr, &value.to_le_bytes())
}

fn timeval_from_us(us: usize) -> TimeVal {
    TimeVal {
        tv_sec: us / 1_000_000,
        tv_usec: us % 1_000_000,
    }
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
    let mut sleep_us = duration_us_from_timespec(req)?;
    if sleep_us != 0 && crate::trap::foreground_driver_active() {
        sleep_us = sleep_us.max(crate::timer::TIME_SLICE_MS as usize * 1000);
    }
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

pub fn sys_clock_nanosleep(_clock_id: usize, flags: usize, req: usize, rem: usize) -> SyscallRet {
    const TIMER_ABSTIME: usize = 1;

    if req == 0 {
        return Err(SysErrNo::EFAULT);
    }
    let req_ts = copy_object_from_user::<TimeSpec>(req)?;
    let deadline_us = if flags & TIMER_ABSTIME != 0 {
        timespec_to_us(req_ts)?
    } else {
        timer::deadline_after_us(duration_us_from_timespec(req_ts)?)
    };
    if deadline_us > timer::get_time_us() {
        let _ = timer::sleep_until_us(deadline_us)?;
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
    let task = current_task().ok_or(SysErrNo::ESRCH)?;
    let uid = task.credentials.lock().real_uid;
    Ok(uid as usize)
}

pub fn sys_geteuid() -> SyscallRet {
    let task = current_task().ok_or(SysErrNo::ESRCH)?;
    let uid = task.credentials.lock().effective_uid;
    Ok(uid as usize)
}

pub fn sys_getgid() -> SyscallRet {
    let task = current_task().ok_or(SysErrNo::ESRCH)?;
    let gid = task.credentials.lock().real_gid;
    Ok(gid as usize)
}

pub fn sys_getegid() -> SyscallRet {
    let task = current_task().ok_or(SysErrNo::ESRCH)?;
    let gid = task.credentials.lock().effective_gid;
    Ok(gid as usize)
}

fn parse_id_arg(raw: usize) -> Result<Option<u32>, SysErrNo> {
    if raw == usize::MAX || raw == u32::MAX as usize {
        Ok(None)
    } else if raw > u32::MAX as usize {
        Err(SysErrNo::EINVAL)
    } else {
        Ok(Some(raw as u32))
    }
}

fn parse_required_id(raw: usize) -> Result<u32, SysErrNo> {
    if raw > u32::MAX as usize {
        Err(SysErrNo::EINVAL)
    } else {
        Ok(raw as u32)
    }
}

pub fn sys_setuid(uid: usize) -> SyscallRet {
    let uid = parse_required_id(uid)?;
    let task = current_task().ok_or(SysErrNo::ESRCH)?;
    let mut credentials = task.credentials.lock();
    if credentials.is_root_capable() {
        credentials.real_uid = uid;
        credentials.effective_uid = uid;
        credentials.saved_uid = uid;
        return Ok(0);
    }
    if credentials.has_uid(uid) {
        credentials.effective_uid = uid;
        Ok(0)
    } else {
        Err(SysErrNo::EPERM)
    }
}

pub fn sys_setgid(gid: usize) -> SyscallRet {
    let gid = parse_required_id(gid)?;
    let task = current_task().ok_or(SysErrNo::ESRCH)?;
    let mut credentials = task.credentials.lock();
    if credentials.is_root_capable() {
        credentials.real_gid = gid;
        credentials.effective_gid = gid;
        credentials.saved_gid = gid;
        return Ok(0);
    }
    if credentials.has_gid(gid) {
        credentials.effective_gid = gid;
        Ok(0)
    } else {
        Err(SysErrNo::EPERM)
    }
}

pub fn sys_setreuid(ruid: usize, euid: usize) -> SyscallRet {
    let new_ruid = parse_id_arg(ruid)?;
    let new_euid = parse_id_arg(euid)?;
    let task = current_task().ok_or(SysErrNo::ESRCH)?;
    let mut credentials = task.credentials.lock();
    let old = credentials.clone();
    if !old.is_root_capable() {
        if new_ruid
            .map(|uid| uid != old.real_uid && uid != old.effective_uid)
            .unwrap_or(false)
        {
            return Err(SysErrNo::EPERM);
        }
        if new_euid.map(|uid| !old.has_uid(uid)).unwrap_or(false) {
            return Err(SysErrNo::EPERM);
        }
    }
    if let Some(uid) = new_ruid {
        credentials.real_uid = uid;
    }
    if let Some(uid) = new_euid {
        credentials.effective_uid = uid;
    }
    if new_ruid.is_some() || new_euid.map(|uid| uid != old.real_uid).unwrap_or(false) {
        credentials.saved_uid = credentials.effective_uid;
    }
    Ok(0)
}

pub fn sys_setregid(rgid: usize, egid: usize) -> SyscallRet {
    let new_rgid = parse_id_arg(rgid)?;
    let new_egid = parse_id_arg(egid)?;
    let task = current_task().ok_or(SysErrNo::ESRCH)?;
    let mut credentials = task.credentials.lock();
    let old = credentials.clone();
    if !old.is_root_capable() {
        if new_rgid
            .map(|gid| gid != old.real_gid && gid != old.effective_gid)
            .unwrap_or(false)
        {
            return Err(SysErrNo::EPERM);
        }
        if new_egid.map(|gid| !old.has_gid(gid)).unwrap_or(false) {
            return Err(SysErrNo::EPERM);
        }
    }
    if let Some(gid) = new_rgid {
        credentials.real_gid = gid;
    }
    if let Some(gid) = new_egid {
        credentials.effective_gid = gid;
    }
    if new_rgid.is_some() || new_egid.map(|gid| gid != old.real_gid).unwrap_or(false) {
        credentials.saved_gid = credentials.effective_gid;
    }
    Ok(0)
}

pub fn sys_setresuid(ruid: usize, euid: usize, suid: usize) -> SyscallRet {
    let new_ruid = parse_id_arg(ruid)?;
    let new_euid = parse_id_arg(euid)?;
    let new_suid = parse_id_arg(suid)?;
    let task = current_task().ok_or(SysErrNo::ESRCH)?;
    let mut credentials = task.credentials.lock();
    let old = credentials.clone();
    if !old.is_root_capable()
        && [new_ruid, new_euid, new_suid]
            .iter()
            .copied()
            .flatten()
            .any(|uid| !old.has_uid(uid))
    {
        return Err(SysErrNo::EPERM);
    }
    if let Some(uid) = new_ruid {
        credentials.real_uid = uid;
    }
    if let Some(uid) = new_euid {
        credentials.effective_uid = uid;
    }
    if let Some(uid) = new_suid {
        credentials.saved_uid = uid;
    }
    Ok(0)
}

pub fn sys_setresgid(rgid: usize, egid: usize, sgid: usize) -> SyscallRet {
    let new_rgid = parse_id_arg(rgid)?;
    let new_egid = parse_id_arg(egid)?;
    let new_sgid = parse_id_arg(sgid)?;
    let task = current_task().ok_or(SysErrNo::ESRCH)?;
    let mut credentials = task.credentials.lock();
    let old = credentials.clone();
    if !old.is_root_capable()
        && [new_rgid, new_egid, new_sgid]
            .iter()
            .copied()
            .flatten()
            .any(|gid| !old.has_gid(gid))
    {
        return Err(SysErrNo::EPERM);
    }
    if let Some(gid) = new_rgid {
        credentials.real_gid = gid;
    }
    if let Some(gid) = new_egid {
        credentials.effective_gid = gid;
    }
    if let Some(gid) = new_sgid {
        credentials.saved_gid = gid;
    }
    Ok(0)
}

pub fn sys_getresuid(ruid: usize, euid: usize, suid: usize) -> SyscallRet {
    let task = current_task().ok_or(SysErrNo::ESRCH)?;
    let credentials = task.credentials.lock().clone();
    copy_object_to_user(ruid, &credentials.real_uid)?;
    copy_object_to_user(euid, &credentials.effective_uid)?;
    copy_object_to_user(suid, &credentials.saved_uid)?;
    Ok(0)
}

pub fn sys_getresgid(rgid: usize, egid: usize, sgid: usize) -> SyscallRet {
    let task = current_task().ok_or(SysErrNo::ESRCH)?;
    let credentials = task.credentials.lock().clone();
    copy_object_to_user(rgid, &credentials.real_gid)?;
    copy_object_to_user(egid, &credentials.effective_gid)?;
    copy_object_to_user(sgid, &credentials.saved_gid)?;
    Ok(0)
}

fn parse_group_count(raw: usize) -> Result<usize, SysErrNo> {
    if raw > i32::MAX as usize {
        Err(SysErrNo::EINVAL)
    } else {
        Ok(raw)
    }
}

pub fn sys_getgroups(size: usize, list: usize) -> SyscallRet {
    let size = parse_group_count(size)?;
    let task = current_task().ok_or(SysErrNo::ESRCH)?;
    let credentials = task.credentials.lock().clone();
    let groups = credentials.supplementary_groups();
    let count = groups.len();
    if size == 0 {
        return Ok(count);
    }
    if size < count {
        return Err(SysErrNo::EINVAL);
    }
    if list == 0 && count != 0 {
        return Err(SysErrNo::EFAULT);
    }
    for (index, group) in groups.iter().enumerate() {
        copy_object_to_user(list + index * core::mem::size_of::<u32>(), group)?;
    }
    Ok(count)
}

pub fn sys_setgroups(size: usize, list: usize) -> SyscallRet {
    let size = parse_group_count(size)?;
    if size > crate::task::LINUX_NGROUPS_MAX {
        return Err(SysErrNo::EINVAL);
    }
    let task = current_task().ok_or(SysErrNo::ESRCH)?;
    {
        let credentials = task.credentials.lock();
        if !credentials.is_root_capable() {
            return Err(SysErrNo::EPERM);
        }
    }
    if size != 0 && list == 0 {
        return Err(SysErrNo::EFAULT);
    }
    if size > crate::task::MAX_SUPPLEMENTARY_GROUPS {
        // Keep the modeled credential set bounded, but still fault a bad user
        // array within Linux's ABI limit before reporting the unsupported count.
        for index in 0..size {
            let _ = copy_object_from_user::<u32>(
                list + index * core::mem::size_of::<u32>(),
            )?;
        }
        return Err(SysErrNo::EINVAL);
    }
    let mut groups = Vec::new();
    for index in 0..size {
        groups.push(copy_object_from_user::<u32>(
            list + index * core::mem::size_of::<u32>(),
        )?);
    }
    task.credentials
        .lock()
        .set_supplementary_groups(&groups);
    Ok(0)
}

pub fn sys_setfsuid(_uid: usize) -> SyscallRet {
    Ok(0)
}

pub fn sys_setfsgid(_gid: usize) -> SyscallRet {
    Ok(0)
}

fn resource_limit_snapshot(resource: usize) -> Result<RLimit, SysErrNo> {
    match resource {
        RLIMIT_STACK => Ok(RLimit {
            rlim_cur: DEFAULT_STACK_LIMIT,
            rlim_max: DEFAULT_STACK_LIMIT,
        }),
        RLIMIT_NOFILE => {
            let task = current_task().ok_or(SysErrNo::ESRCH)?;
            let inner = task.inner.lock();
            Ok(RLimit {
                rlim_cur: inner.rlimit_nofile,
                rlim_max: inner.rlimit_nofile_max,
            })
        }
        _ => Ok(RLimit {
            rlim_cur: usize::MAX,
            rlim_max: usize::MAX,
        }),
    }
}

fn apply_resource_limit(resource: usize, limit_ptr: usize) -> Result<(), SysErrNo> {
    if limit_ptr == 0 {
        return Err(SysErrNo::EFAULT);
    }
    let limit = copy_object_from_user::<RLimit>(limit_ptr)?;
    if limit.rlim_cur > limit.rlim_max {
        return Err(SysErrNo::EINVAL);
    }
    if resource == RLIMIT_STACK {
        return Ok(());
    }
    if resource != RLIMIT_NOFILE {
        return Ok(());
    }

    let task = current_task().ok_or(SysErrNo::ESRCH)?;
    let mut inner = task.inner.lock();
    let hard = limit.rlim_max.min(crate::fs::fd::MAX_FD_NUM);
    let soft = limit.rlim_cur.min(hard);
    inner.rlimit_nofile = soft;
    inner.rlimit_nofile_max = hard;
    Ok(())
}

pub fn sys_getrlimit(resource: usize, old_limit: usize) -> SyscallRet {
    if old_limit == 0 {
        return Err(SysErrNo::EFAULT);
    }
    copy_object_to_user(old_limit, &resource_limit_snapshot(resource)?)?;
    Ok(0)
}

pub fn sys_setrlimit(resource: usize, new_limit: usize) -> SyscallRet {
    apply_resource_limit(resource, new_limit)?;
    Ok(0)
}

pub fn sys_prlimit64(
    pid: usize,
    resource: usize,
    new_limit: usize,
    old_limit: usize,
) -> SyscallRet {
    if pid != 0 {
        let current = current_task().ok_or(SysErrNo::ESRCH)?;
        if pid != current.pid.0 && pid != current.thread_group.tgid() {
            return Err(SysErrNo::ESRCH);
        }
    }
    let old = if old_limit != 0 {
        Some(resource_limit_snapshot(resource)?)
    } else {
        None
    };
    if new_limit != 0 {
        apply_resource_limit(resource, new_limit)?;
    }
    if let Some(old) = old {
        copy_object_to_user(old_limit, &old)?;
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

pub fn sys_get_robust_list(pid: usize, head_ptr: usize, len_ptr: usize) -> SyscallRet {
    if head_ptr == 0 || len_ptr == 0 {
        return Err(SysErrNo::EFAULT);
    }
    let task = if pid == 0 {
        current_task().ok_or(SysErrNo::ESRCH)?
    } else {
        crate::task::manager::find_task(pid).ok_or(SysErrNo::ESRCH)?
    };
    let (head, len) = {
        let inner = task.inner.lock();
        (inner.robust_list_head, inner.robust_list_len)
    };
    copy_to_user(head_ptr, &head.to_ne_bytes())?;
    copy_to_user(len_ptr, &len.to_ne_bytes())?;
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

pub fn sys_getrusage(who: usize, usage: usize) -> SyscallRet {
    const RUSAGE_CHILDREN: isize = -1;
    const RUSAGE_SELF: isize = 0;
    const RUSAGE_THREAD: isize = 1;

    if usage == 0 {
        return Err(SysErrNo::EFAULT);
    }

    let who = who as isize;
    let elapsed_us = match who {
        RUSAGE_SELF | RUSAGE_THREAD => current_task()
            .map(|task| timer::get_time_us().saturating_sub(task.start_time_us))
            .ok_or(SysErrNo::ESRCH)?,
        RUSAGE_CHILDREN => 0,
        _ => return Err(SysErrNo::EINVAL),
    };

    copy_object_to_user(
        usage,
        &RUsage {
            ru_utime: timeval_from_us(elapsed_us),
            ru_stime: TimeVal {
                tv_sec: 0,
                tv_usec: 0,
            },
            ru_maxrss: 0,
            ru_ixrss: 0,
            ru_idrss: 0,
            ru_isrss: 0,
            ru_minflt: 0,
            ru_majflt: 0,
            ru_nswap: 0,
            ru_inblock: 0,
            ru_oublock: 0,
            ru_msgsnd: 0,
            ru_msgrcv: 0,
            ru_nsignals: 0,
            ru_nvcsw: 0,
            ru_nivcsw: 0,
        },
    )?;
    Ok(0)
}

pub fn sys_umask(_mask: usize) -> SyscallRet {
    Ok(0o022)
}

pub fn sys_getpgid(pid: usize) -> SyscallRet {
    let task = if pid == 0 {
        current_task().ok_or(SysErrNo::ESRCH)?
    } else {
        manager::find_task(pid).ok_or(SysErrNo::ESRCH)?
    };
    let pgid = task.inner.lock().pgid;
    Ok(pgid)
}

pub fn sys_setpgid(pid: usize, pgid: usize) -> SyscallRet {
    let task = if pid == 0 {
        current_task().ok_or(SysErrNo::ESRCH)?
    } else {
        manager::find_task(pid).ok_or(SysErrNo::ESRCH)?
    };
    let new_pgid = if pgid == 0 {
        task.thread_group.tgid()
    } else {
        pgid
    };
    for member in task.thread_group.user_members() {
        member.inner.lock().pgid = new_pgid;
    }
    Ok(0)
}

pub fn sys_membarrier(_cmd: usize, _flags: usize) -> SyscallRet {
    Ok(0)
}

pub fn sys_sched_getaffinity(_pid: usize, cpusetsize: usize, mask: usize) -> SyscallRet {
    const KERNEL_CPUSET_BYTES: usize = 128;
    if mask == 0 {
        return Err(SysErrNo::EFAULT);
    }
    if cpusetsize < KERNEL_CPUSET_BYTES {
        return Err(SysErrNo::EINVAL);
    }
    let mut bytes = Vec::new();
    bytes.resize(KERNEL_CPUSET_BYTES, 0);
    bytes[0] = 1;
    copy_to_user(mask, &bytes)?;
    Ok(KERNEL_CPUSET_BYTES)
}

pub fn sys_sched_setaffinity(_pid: usize, cpusetsize: usize, mask: usize) -> SyscallRet {
    if cpusetsize != 0 && mask == 0 {
        return Err(SysErrNo::EFAULT);
    }
    if cpusetsize != 0 {
        let mut bytes = Vec::new();
        bytes.resize(cpusetsize.min(128), 0);
        super::user::copy_from_user(mask, &mut bytes)?;
    }
    Ok(0)
}

fn sched_task_for_pid(pid: usize) -> Result<Arc<crate::task::TaskControlBlock>, SysErrNo> {
    if pid == 0 {
        current_task().ok_or(SysErrNo::ESRCH)
    } else {
        manager::find_task(pid).ok_or(SysErrNo::ESRCH)
    }
}

fn base_sched_policy(policy: usize) -> usize {
    policy & !SCHED_RESET_ON_FORK
}

fn validate_sched_param(policy: usize, priority: i32) -> Result<usize, SysErrNo> {
    let policy = base_sched_policy(policy);
    match policy {
        SCHED_FIFO | SCHED_RR if (1..=99).contains(&priority) => Ok(priority as usize),
        SCHED_FIFO | SCHED_RR => Err(SysErrNo::EINVAL),
        SCHED_OTHER | SCHED_BATCH | SCHED_IDLE | SCHED_DEADLINE if priority == 0 => Ok(0),
        SCHED_OTHER | SCHED_BATCH | SCHED_IDLE | SCHED_DEADLINE => Err(SysErrNo::EINVAL),
        _ => Err(SysErrNo::EINVAL),
    }
}

pub fn sys_sched_getscheduler(pid: usize) -> SyscallRet {
    let task = sched_task_for_pid(pid)?;
    Ok(task
        .sched_policy
        .load(core::sync::atomic::Ordering::Relaxed))
}

pub fn sys_sched_setscheduler(pid: usize, policy: usize, param: usize) -> SyscallRet {
    if param == 0 {
        return Err(SysErrNo::EFAULT);
    }
    let sched_param = copy_object_from_user::<SchedParam>(param)?;
    if policy == SCHED_POLICY_UNCHANGED {
        let _ = sched_task_for_pid(pid)?;
        return Ok(0);
    }
    let base_policy = base_sched_policy(policy);
    let priority = validate_sched_param(base_policy, sched_param.sched_priority)?;
    let task = sched_task_for_pid(pid)?;
    task.set_sched_params(base_policy, priority);
    Ok(0)
}

pub fn sys_sched_getparam(_pid: usize, param: usize) -> SyscallRet {
    if param == 0 {
        return Err(SysErrNo::EFAULT);
    }
    let task = sched_task_for_pid(_pid)?;
    copy_object_to_user(
        param,
        &SchedParam {
            sched_priority: task
                .sched_priority
                .load(core::sync::atomic::Ordering::Relaxed) as i32,
        },
    )?;
    Ok(0)
}

pub fn sys_sched_setparam(pid: usize, param: usize) -> SyscallRet {
    if param == 0 {
        return Err(SysErrNo::EFAULT);
    }
    let sched_param = copy_object_from_user::<SchedParam>(param)?;
    let task = sched_task_for_pid(pid)?;
    let policy = task
        .sched_policy
        .load(core::sync::atomic::Ordering::Relaxed);
    let priority = validate_sched_param(policy, sched_param.sched_priority)?;
    task.set_sched_params(policy, priority);
    Ok(0)
}

fn sched_attr_from_task(task: &Arc<crate::task::TaskControlBlock>) -> SchedAttr {
    SchedAttr {
        size: core::mem::size_of::<SchedAttr>() as u32,
        sched_policy: task
            .sched_policy
            .load(core::sync::atomic::Ordering::Relaxed) as u32,
        sched_flags: 0,
        sched_nice: 0,
        sched_priority: task
            .sched_priority
            .load(core::sync::atomic::Ordering::Relaxed) as u32,
        sched_runtime: 0,
        sched_deadline: 0,
        sched_period: 0,
        sched_util_min: 0,
        sched_util_max: 0,
    }
}

pub fn sys_sched_getattr(pid: usize, attr: usize, size: usize, flags: usize) -> SyscallRet {
    if attr == 0 {
        return Err(SysErrNo::EFAULT);
    }
    if flags != 0 || size < SCHED_ATTR_SIZE_VER0 {
        return Err(SysErrNo::EINVAL);
    }
    let task = sched_task_for_pid(pid)?;
    let kernel_attr = sched_attr_from_task(&task);
    let bytes = unsafe {
        core::slice::from_raw_parts(
            &kernel_attr as *const SchedAttr as *const u8,
            core::mem::size_of::<SchedAttr>(),
        )
    };
    copy_to_user(attr, &bytes[..size.min(bytes.len())])?;
    Ok(0)
}

pub fn sys_sched_setattr(pid: usize, attr: usize, flags: usize) -> SyscallRet {
    if attr == 0 {
        return Err(SysErrNo::EFAULT);
    }
    if flags != 0 {
        return Err(SysErrNo::EINVAL);
    }
    let mut size_bytes = [0u8; 4];
    super::user::copy_from_user(attr, &mut size_bytes)?;
    let user_size = u32::from_le_bytes(size_bytes) as usize;
    if user_size < SCHED_ATTR_SIZE_VER0 {
        return Err(SysErrNo::EINVAL);
    }

    let mut kernel_attr = SchedAttr {
        size: user_size as u32,
        sched_policy: SCHED_OTHER as u32,
        sched_flags: 0,
        sched_nice: 0,
        sched_priority: 0,
        sched_runtime: 0,
        sched_deadline: 0,
        sched_period: 0,
        sched_util_min: 0,
        sched_util_max: 0,
    };
    let bytes = unsafe {
        core::slice::from_raw_parts_mut(
            &mut kernel_attr as *mut SchedAttr as *mut u8,
            core::mem::size_of::<SchedAttr>(),
        )
    };
    let copy_len = user_size.min(bytes.len());
    super::user::copy_from_user(attr, &mut bytes[..copy_len])?;

    let policy = base_sched_policy(kernel_attr.sched_policy as usize);
    let priority = validate_sched_param(policy, kernel_attr.sched_priority as i32)?;
    let task = sched_task_for_pid(pid)?;
    task.set_sched_params(policy, priority);
    Ok(0)
}

pub fn sys_sched_get_priority_max(policy: usize) -> SyscallRet {
    match policy {
        SCHED_FIFO | SCHED_RR => Ok(99),
        SCHED_OTHER | SCHED_BATCH | SCHED_IDLE | SCHED_DEADLINE => Ok(0),
        _ => Err(SysErrNo::EINVAL),
    }
}

pub fn sys_sched_get_priority_min(policy: usize) -> SyscallRet {
    match policy {
        SCHED_FIFO | SCHED_RR => Ok(1),
        SCHED_OTHER | SCHED_BATCH | SCHED_IDLE | SCHED_DEADLINE => Ok(0),
        _ => Err(SysErrNo::EINVAL),
    }
}

pub fn sys_sched_rr_get_interval(pid: usize, interval: usize) -> SyscallRet {
    if interval == 0 {
        return Err(SysErrNo::EFAULT);
    }
    let _ = sched_task_for_pid(pid)?;
    copy_object_to_user(
        interval,
        &TimeSpec {
            tv_sec: 0,
            tv_nsec: crate::timer::TIME_SLICE_MS as isize * 1_000_000,
        },
    )?;
    Ok(0)
}

pub fn sys_memory_lock_noop() -> SyscallRet {
    Ok(0)
}

pub(crate) fn futex_wake_addr_for_task(
    task: &Arc<crate::task::TaskControlBlock>,
    uaddr: usize,
    n: usize,
) -> usize {
    futex_wake_addr_private_and_shared(task, uaddr, n)
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
            let Some(index) = waiters.iter().position(|waiter| {
                waiter.uaddr == uaddr && waiter.key == key && (waiter.bitset & bitset) != 0
            }) else {
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

fn futex_requeue_addr(
    uaddr: usize,
    uaddr2: usize,
    private: bool,
    nr_wake: usize,
    nr_requeue: usize,
    cmp: Option<i32>,
) -> SyscallRet {
    validate_futex_uaddr(uaddr)?;
    if nr_requeue != 0 {
        validate_futex_uaddr(uaddr2)?;
    }
    if let Some(expected) = cmp {
        if read_user_i32(uaddr)? != expected {
            return Err(SysErrNo::EAGAIN);
        }
    }

    let task = current_task().ok_or(SysErrNo::ESRCH)?;
    let key = futex_key_for_op(&task, private);
    let mut to_wake = Vec::new();
    let mut requeued = 0usize;

    {
        let mut waiters = FUTEX_WAITERS.lock();
        let mut index = 0usize;
        while index < waiters.len() && to_wake.len() < nr_wake {
            if waiters[index].uaddr == uaddr && waiters[index].key == key {
                to_wake.push(waiters.remove(index));
            } else {
                index += 1;
            }
        }

        let mut index = 0usize;
        while index < waiters.len() && requeued < nr_requeue {
            if waiters[index].uaddr == uaddr && waiters[index].key == key {
                waiters[index].uaddr = uaddr2;
                waiters[index].key = key;
                requeued += 1;
            }
            index += 1;
        }
    }

    let mut woke = 0usize;
    for waiter in to_wake {
        if crate::task::wake_task_token_with(&waiter.task, waiter.token, WaitOutcome::Woken) {
            woke += 1;
        }
    }

    Ok(woke + requeued)
}

fn sign_extend_12(value: usize) -> i32 {
    let value = (value & 0x0fff) as i32;
    if (value & 0x0800) != 0 {
        value | !0x0fff
    } else {
        value
    }
}

fn futex_wake_op_arg(encoded: usize) -> Option<(i32, i32, i32, i32)> {
    let mut op = ((encoded >> 28) & 0x0f) as i32;
    let cmp = ((encoded >> 24) & 0x0f) as i32;
    let raw_oparg = (encoded >> 12) & 0x0fff;
    let cmparg = sign_extend_12(encoded);

    let shift_oparg = (op & 0x08) != 0;
    op &= !0x08;
    let oparg = if shift_oparg {
        if raw_oparg >= 32 {
            return None;
        }
        1i32.wrapping_shl(raw_oparg as u32)
    } else {
        sign_extend_12(raw_oparg)
    };

    Some((op, oparg, cmp, cmparg))
}

fn futex_wake_op(
    uaddr: usize,
    uaddr2: usize,
    private: bool,
    nr_wake: usize,
    nr_wake2: usize,
    encoded_op: usize,
) -> SyscallRet {
    validate_futex_uaddr(uaddr)?;
    validate_futex_uaddr(uaddr2)?;

    let Some((op, oparg, cmp, cmparg)) = futex_wake_op_arg(encoded_op) else {
        return Err(SysErrNo::EINVAL);
    };
    let old = read_user_i32(uaddr2)?;
    let new = match op {
        0 => oparg,
        1 => old.wrapping_add(oparg),
        2 => old | oparg,
        3 => old & !oparg,
        4 => old ^ oparg,
        _ => return Err(SysErrNo::EINVAL),
    };
    write_user_i32(uaddr2, new)?;

    let cmp_matches = match cmp {
        0 => old == cmparg,
        1 => old != cmparg,
        2 => old < cmparg,
        3 => old <= cmparg,
        4 => old > cmparg,
        5 => old >= cmparg,
        _ => return Err(SysErrNo::EINVAL),
    };

    let mut woke = futex_wake_addr_bitset(uaddr, private, nr_wake, usize::MAX);
    if cmp_matches {
        woke += futex_wake_addr_bitset(uaddr2, private, nr_wake2, usize::MAX);
    }
    Ok(woke)
}

fn futex_wait_addr(
    uaddr: usize,
    val: usize,
    deadline: Option<usize>,
    bitset: usize,
    private: bool,
) -> SyscallRet {
    let task = current_task().ok_or(SysErrNo::ESRCH)?;
    if let Some(ret) = finish_resumed_futex_wait(&task, uaddr, deadline, private) {
        return ret;
    }
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
            *task.wait_outcome.lock() = None;
            return Err(SysErrNo::EAGAIN);
        }
        Err(err) => {
            remove_futex_waiter(uaddr, key, task.pid.0, token);
            *task.block_reason.lock() = None;
            *task.wait_outcome.lock() = None;
            return Err(err);
        }
    }
    if crate::syscall::signal::current_has_unblocked_pending() {
        remove_futex_waiter(uaddr, key, task.pid.0, token);
        *task.block_reason.lock() = None;
        *task.wait_outcome.lock() = None;
        return Err(SysErrNo::EINTR);
    }
    if let Some(deadline) = deadline {
        timer::add_timeout(deadline, task.clone(), token);
    }

    crate::task::block_current_for_reason_until(
        crate::task::wait_queue::BlockReason::Futex,
        deadline,
    );
    if crate::trap::syscall_parked() {
        return Ok(0);
    }

    *task.block_reason.lock() = None;
    let still_waiting = remove_futex_waiter(uaddr, key, task.pid.0, token);
    if deadline.is_some() {
        timer::remove_timeout(task.pid.0, token);
    }
    let outcome = crate::task::wait_queue::finish_wait(&task, still_waiting, deadline);
    match outcome {
        WaitOutcome::TimedOut => Err(SysErrNo::ETIMEDOUT),
        WaitOutcome::Interrupted => Err(SysErrNo::EINTR),
        WaitOutcome::Woken => Ok(0),
    }
}

fn finish_resumed_futex_wait(
    task: &Arc<crate::task::TaskControlBlock>,
    uaddr: usize,
    deadline: Option<usize>,
    private: bool,
) -> Option<SyscallRet> {
    let outcome = task.wait_outcome.lock().take()?;
    let key = futex_key_for_op(task, private);
    let token = task.current_wait_token();
    *task.block_reason.lock() = None;
    remove_futex_waiter(uaddr, key, task.pid.0, token);
    if deadline.is_some() {
        timer::remove_timeout(task.pid.0, token);
    }
    Some(match outcome {
        WaitOutcome::TimedOut => Err(SysErrNo::ETIMEDOUT),
        WaitOutcome::Interrupted => Err(SysErrNo::EINTR),
        WaitOutcome::Woken => Ok(0),
    })
}

fn remove_futex_waiter(_uaddr: usize, _key: usize, pid: usize, token: usize) -> bool {
    let mut waiters = FUTEX_WAITERS.lock();
    if let Some(index) = waiters
        .iter()
        .position(|waiter| waiter.task.pid.0 == pid && waiter.token == token)
    {
        waiters.remove(index);
        true
    } else {
        false
    }
}

pub(crate) fn remove_futex_waiters_for_task(task: &Arc<crate::task::TaskControlBlock>) -> usize {
    let mut removed = 0usize;
    let mut waiters = FUTEX_WAITERS.lock();
    let mut index = 0usize;
    while index < waiters.len() {
        if Arc::ptr_eq(&waiters[index].task, task) || waiters[index].task.pid.0 == task.pid.0 {
            waiters.remove(index);
            removed += 1;
        } else {
            index += 1;
        }
    }
    removed
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

    let read_usize_at = |addr: usize| -> Result<usize, SysErrNo> {
        let mut bytes = [0u8; core::mem::size_of::<usize>()];
        let mut memory_set = task.memory_set.lock();
        super::user::copy_from_user_in_memory_set(&mut memory_set, addr, &mut bytes)?;
        Ok(usize::from_ne_bytes(bytes))
    };
    let read_i32_at = |addr: usize| -> Result<i32, SysErrNo> {
        let mut bytes = [0u8; core::mem::size_of::<i32>()];
        let mut memory_set = task.memory_set.lock();
        super::user::copy_from_user_in_memory_set(&mut memory_set, addr, &mut bytes)?;
        Ok(i32::from_ne_bytes(bytes))
    };
    let write_i32_at = |addr: usize, value: i32| -> Result<(), SysErrNo> {
        let mut memory_set = task.memory_set.lock();
        super::user::copy_to_user_in_memory_set(&mut memory_set, addr, &value.to_ne_bytes())
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
    const FUTEX_REQUEUE: usize = 3;
    const FUTEX_CMP_REQUEUE: usize = 4;
    const FUTEX_WAKE_OP: usize = 5;
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
        FUTEX_REQUEUE => futex_requeue_addr(uaddr, _uaddr2, private, val, timeout, None),
        FUTEX_CMP_REQUEUE => {
            futex_requeue_addr(uaddr, _uaddr2, private, val, timeout, Some(_val3 as i32))
        }
        FUTEX_WAKE_OP => futex_wake_op(uaddr, _uaddr2, private, val, timeout, _val3),
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
