use super::SyscallRet;
use crate::mm::memory_set::MemorySet;
use crate::mm::page_table::PTEFlags;
use crate::task::wait_queue::{BlockReason, WaitOutcome, WaitQueue};
use crate::task::{current_task, TaskControlBlock, TaskStatus};
use crate::timer;
use crate::utils::error::SysErrNo;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::Ordering;
use lazy_static::lazy_static;
use polyhal::VirtAddr;
use polyhal_trap::trapframe::{TrapFrame, TrapFrameArgs};
use spin::Mutex;

pub const MAX_SIGNAL: i32 = 64;
const MAX_SIGNAL_USIZE: usize = MAX_SIGNAL as usize;
const KERNEL_SIGSET_SIZE: usize = core::mem::size_of::<usize>();

const SIG_DFL: usize = 0;
const SIG_IGN: usize = 1;

const SIGKILL: i32 = 9;
const SIGALRM: i32 = 14;
const SIGPIPE: i32 = 13;
const SIGSEGV: i32 = 11;
const SIGCHLD: i32 = 17;
const SIGPROF: i32 = 27;
const SIGCONT: i32 = 18;
const SIGSTOP: i32 = 19;
const SIGTSTP: i32 = 20;
const SIGTTIN: i32 = 21;
const SIGTTOU: i32 = 22;
const SIGURG: i32 = 23;
const SIGVTALRM: i32 = 26;
const SIGWINCH: i32 = 28;
const SIGCANCEL: i32 = 32;
const SIGSETXID: i32 = 33;

const SIG_BLOCK: i32 = 0;
const SIG_UNBLOCK: i32 = 1;
const SIG_SETMASK: i32 = 2;

const SA_NOCLDSTOP: usize = 0x0000_0001;
const SA_NOCLDWAIT: usize = 0x0000_0002;
const SA_SIGINFO: usize = 0x0000_0004;
const SA_UNSUPPORTED: usize = 0x0000_0400;
const SA_EXPOSE_TAGBITS: usize = 0x0000_0800;
const SA_RESTORER: usize = 0x0400_0000;
const SA_ONSTACK: usize = 0x0800_0000;
const SA_RESTART: usize = 0x1000_0000;
const SA_INTERRUPT: usize = 0x2000_0000;
const SA_NODEFER: usize = 0x4000_0000;
const SA_RESETHAND: usize = 0x8000_0000;
const KNOWN_SIGACTION_FLAGS: usize = SA_NOCLDSTOP
    | SA_NOCLDWAIT
    | SA_SIGINFO
    | SA_EXPOSE_TAGBITS
    | SA_RESTORER
    | SA_ONSTACK
    | SA_RESTART
    | SA_INTERRUPT
    | SA_NODEFER
    | SA_RESETHAND;

const SIGNAL_FRAME_MAGIC: usize = 0x574c_4c5f_5349_4746; // "WLL_SIGF"
const SI_USER: i32 = 0;
const SI_KERNEL: i32 = 0x80;
const SI_TKILL: i32 = -6;
const CLD_EXITED: i32 = 1;

#[repr(C)]
#[derive(Clone, Copy)]
struct TimeSpec {
    tv_sec: isize,
    tv_nsec: isize,
}

lazy_static! {
    static ref SIGNAL_WAIT_QUEUE: WaitQueue = WaitQueue::new(BlockReason::Signal);
}

#[cfg(target_arch = "riscv64")]
const SIGNAL_TRAMPOLINE_CODE: &[u8] = &[
    0x93, 0x08, 0xb0, 0x08, // li a7, 139
    0x73, 0x00, 0x00, 0x00, // ecall
    0x73, 0x00, 0x10, 0x00, // ebreak
];

#[cfg(target_arch = "loongarch64")]
const SIGNAL_TRAMPOLINE_CODE: &[u8] = &[
    0x0b, 0x2c, 0x82, 0x03, // li.w $a7, 139
    0x00, 0x00, 0x2b, 0x00, // syscall 0
    0x00, 0x00, 0x2a, 0x00, // break 0
];

#[derive(Clone, Copy)]
pub struct KernelSigAction {
    pub handler: usize,
    pub flags: usize,
    pub restorer: usize,
    pub mask: usize,
}

const DEFAULT_SIGACTION: KernelSigAction = KernelSigAction {
    handler: SIG_DFL,
    flags: 0,
    restorer: 0,
    mask: 0,
};

#[derive(Clone)]
pub struct SignalActions {
    table: [KernelSigAction; MAX_SIGNAL_USIZE + 1],
}

impl SignalActions {
    pub fn new() -> Self {
        Self {
            table: [DEFAULT_SIGACTION; MAX_SIGNAL_USIZE + 1],
        }
    }

    fn get(&self, signum: i32) -> KernelSigAction {
        self.table[signum as usize]
    }

    fn set(&mut self, signum: i32, action: KernelSigAction) {
        self.table[signum as usize] = action;
    }
}

#[derive(Clone, Copy)]
pub struct SignalState {
    pub blocked: usize,
    pub pending: usize,
    pub pending_info: [PendingSignalInfo; MAX_SIGNAL_USIZE + 1],
    pub suspend_old_mask: Option<usize>,
}

impl SignalState {
    pub fn new() -> Self {
        Self {
            blocked: 0,
            pending: 0,
            pending_info: [PendingSignalInfo::empty(); MAX_SIGNAL_USIZE + 1],
            suspend_old_mask: None,
        }
    }

    pub fn fork_from(blocked: usize) -> Self {
        Self {
            blocked: sanitize_mask(blocked),
            pending: 0,
            pending_info: [PendingSignalInfo::empty(); MAX_SIGNAL_USIZE + 1],
            suspend_old_mask: None,
        }
    }
}

pub type SharedSignalActions = Arc<Mutex<SignalActions>>;

pub fn new_shared_signal_actions() -> SharedSignalActions {
    Arc::new(Mutex::new(SignalActions::new()))
}

pub fn dup_signal_actions(src: &SharedSignalActions) -> SharedSignalActions {
    Arc::new(Mutex::new(src.lock().clone()))
}

#[repr(C)]
#[derive(Clone, Copy)]
struct UserSigAction {
    handler: usize,
    flags: usize,
    restorer: usize,
    mask: usize,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct UserSigInfo {
    signo: i32,
    errno: i32,
    code: i32,
    _align: i32,
    pid: i32,
    uid: u32,
    _reserved: [u8; 104],
}

#[derive(Clone, Copy)]
pub struct PendingSignalInfo {
    code: i32,
    sender_pid: i32,
    sender_uid: u32,
}

impl PendingSignalInfo {
    const fn empty() -> Self {
        Self {
            code: SI_USER,
            sender_pid: 0,
            sender_uid: 0,
        }
    }

    fn from_current(code: i32) -> Self {
        Self {
            code,
            sender_pid: current_task()
                .map(|task| task.thread_group.tgid() as i32)
                .unwrap_or(0),
            sender_uid: 0,
        }
    }

    fn user_siginfo(self, signum: i32) -> UserSigInfo {
        UserSigInfo {
            signo: signum,
            errno: 0,
            code: self.code,
            _align: 0,
            pid: self.sender_pid,
            uid: self.sender_uid,
            _reserved: [0; 104],
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
struct SignalFrame {
    magic: usize,
    frame_size: usize,
    signo: usize,
    old_mask: usize,
    siginfo: UserSigInfo,
    ucontext: UserUContext,
    arch_extra: ArchSignalExtra,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct UserSigAltStack {
    ss_sp: usize,
    ss_flags: i32,
    _pad: i32,
    ss_size: usize,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct UserSigSet {
    bits: [usize; 16],
}

impl UserSigSet {
    fn from_kernel(mask: usize) -> Self {
        let mut bits = [0usize; 16];
        bits[0] = mask;
        Self { bits }
    }

    fn to_kernel_mask(self) -> usize {
        self.bits[0]
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
struct UserUContext {
    uc_flags: usize,
    uc_link: usize,
    uc_stack: UserSigAltStack,
    uc_sigmask: UserSigSet,
    uc_mcontext: UserMContext,
}

#[cfg(target_arch = "riscv64")]
#[repr(C, align(16))]
#[derive(Clone, Copy)]
struct UserMContext {
    gregs: [usize; 32],
    fpregs: [u64; 66],
}

#[cfg(target_arch = "riscv64")]
#[repr(C)]
#[derive(Clone, Copy)]
struct ArchSignalExtra {
    sstatus: usize,
    fsx: [usize; 2],
}

#[cfg(target_arch = "loongarch64")]
#[repr(C)]
#[derive(Clone, Copy)]
struct UserMContext {
    regs: [usize; 32],
    prmd: usize,
    era: usize,
}

#[cfg(target_arch = "loongarch64")]
#[repr(C)]
#[derive(Clone, Copy)]
struct ArchSignalExtra {
    f: [u64; 32],
    fcc: u64,
    fcsr: u64,
}

fn valid_signal(signum: i32) -> bool {
    signum > 0 && signum <= MAX_SIGNAL
}

fn signal_bit(signum: i32) -> usize {
    1usize << ((signum as usize) - 1)
}

fn unblockable_mask() -> usize {
    signal_bit(SIGKILL) | signal_bit(SIGSTOP)
}

fn sanitize_mask(mask: usize) -> usize {
    mask & !unblockable_mask()
}

fn sanitize_flags(flags: usize) -> usize {
    flags & KNOWN_SIGACTION_FLAGS & !SA_UNSUPPORTED
}

fn cannot_catch_or_ignore(signum: i32) -> bool {
    signum == SIGKILL || signum == SIGSTOP
}

fn default_ignored(signum: i32) -> bool {
    matches!(signum, SIGCHLD | SIGCONT | SIGURG | SIGWINCH)
}

fn default_stops(signum: i32) -> bool {
    matches!(signum, SIGSTOP | SIGTSTP | SIGTTIN | SIGTTOU)
}

fn default_exit_code(signum: i32) -> i32 {
    crate::task::signal_exit_code(signum)
}

fn read_sigset(addr: usize) -> Result<usize, SysErrNo> {
    let mut bytes = [0u8; KERNEL_SIGSET_SIZE];
    super::user::copy_from_user(addr, &mut bytes)?;
    Ok(sanitize_mask(usize::from_ne_bytes(bytes)))
}

fn duration_us_from_timespec(ts: TimeSpec) -> Result<usize, SysErrNo> {
    if ts.tv_sec < 0 || ts.tv_nsec < 0 || ts.tv_nsec >= 1_000_000_000 {
        return Err(SysErrNo::EINVAL);
    }
    Ok((ts.tv_sec as usize)
        .saturating_mul(1_000_000)
        .saturating_add((ts.tv_nsec as usize).div_ceil(1000)))
}

fn user_action(action: KernelSigAction) -> UserSigAction {
    UserSigAction {
        handler: action.handler,
        flags: action.flags,
        restorer: action.restorer,
        mask: action.mask,
    }
}

fn kernel_action(action: UserSigAction) -> KernelSigAction {
    KernelSigAction {
        handler: action.handler,
        flags: sanitize_flags(action.flags),
        restorer: action.restorer,
        mask: sanitize_mask(action.mask),
    }
}

pub fn signal_trampoline_addr() -> usize {
    crate::config::USER_STACK_TOP
}

pub fn install_signal_trampoline(memory_set: &mut MemorySet) -> Result<(), SysErrNo> {
    let start = signal_trampoline_addr();
    if !memory_set.is_mapped(VirtAddr::new(start)) {
        memory_set.insert_framed_area(
            VirtAddr::new(start),
            VirtAddr::new(start + crate::config::PAGE_SIZE),
            PTEFlags::U | PTEFlags::R | PTEFlags::X | PTEFlags::V,
        )?;
    }
    memory_set.write_bytes(start, SIGNAL_TRAMPOLINE_CODE)
}

pub fn sys_sigaction(signum: i32, act: usize, oldact: usize, sigset_size: usize) -> SyscallRet {
    if !valid_signal(signum) || cannot_catch_or_ignore(signum) {
        return Err(SysErrNo::EINVAL);
    }
    if sigset_size != KERNEL_SIGSET_SIZE {
        return Err(SysErrNo::EINVAL);
    }

    let new_action = if act != 0 {
        Some(kernel_action(super::user::copy_object_from_user::<
            UserSigAction,
        >(act)?))
    } else {
        None
    };

    let task = current_task().ok_or(SysErrNo::ESRCH)?;
    let old_action = task.signal_actions.lock().get(signum);
    if oldact != 0 {
        super::user::copy_object_to_user(oldact, &user_action(old_action))?;
    }
    if let Some(action) = new_action {
        task.signal_actions.lock().set(signum, action);
    }
    Ok(0)
}

pub fn sys_sigprocmask(how: i32, set: usize, oldset: usize, sigset_size: usize) -> SyscallRet {
    if sigset_size != KERNEL_SIGSET_SIZE {
        return Err(SysErrNo::EINVAL);
    }

    let task = current_task().ok_or(SysErrNo::ESRCH)?;
    let new_mask = if set != 0 {
        let mut bytes = [0u8; KERNEL_SIGSET_SIZE];
        super::user::copy_from_user(set, &mut bytes)?;
        Some(usize::from_ne_bytes(bytes))
    } else {
        None
    };

    let mut state = task.signal_state.lock();
    let old_mask = state.blocked;
    if oldset != 0 {
        super::user::copy_to_user(oldset, &old_mask.to_ne_bytes())?;
    }

    if let Some(mask) = new_mask {
        state.blocked = match how {
            SIG_BLOCK => sanitize_mask(state.blocked | mask),
            SIG_UNBLOCK => sanitize_mask(state.blocked & !mask),
            SIG_SETMASK => sanitize_mask(mask),
            _ => return Err(SysErrNo::EINVAL),
        };
    }
    Ok(0)
}

pub fn sys_sigtimedwait(set: usize, info: usize, timeout: usize, sigset_size: usize) -> SyscallRet {
    if set == 0 {
        return Err(SysErrNo::EFAULT);
    }
    if sigset_size != KERNEL_SIGSET_SIZE {
        return Err(SysErrNo::EINVAL);
    }

    let wait_mask = read_sigset(set)?;
    let deadline_us = if timeout != 0 {
        let timeout = super::user::copy_object_from_user::<TimeSpec>(timeout)?;
        Some(timer::deadline_after_us(duration_us_from_timespec(
            timeout,
        )?))
    } else {
        None
    };

    loop {
        let task = current_task().ok_or(SysErrNo::ESRCH)?;
        if let Some((signum, pending_info)) = take_pending_from_mask(&task, wait_mask) {
            if info != 0 {
                super::user::copy_object_to_user(info, &pending_info.user_siginfo(signum))?;
            }
            return Ok(signum as usize);
        }

        match SIGNAL_WAIT_QUEUE.sleep_until_if(deadline_us, || {
            let task = current_task().ok_or(SysErrNo::ESRCH)?;
            Ok(!has_pending_in_mask(&task, wait_mask))
        }) {
            Ok(WaitOutcome::TimedOut) => return Err(SysErrNo::EAGAIN),
            Ok(_) => continue,
            Err(SysErrNo::EINTR) => return Err(SysErrNo::EINTR),
            Err(err) => return Err(err),
        }
    }
}

pub fn sys_sigsuspend(mask: usize, sigset_size: usize) -> SyscallRet {
    if mask == 0 {
        return Err(SysErrNo::EFAULT);
    }
    if sigset_size != KERNEL_SIGSET_SIZE {
        return Err(SysErrNo::EINVAL);
    }

    let suspend_mask = read_sigset(mask)?;
    let task = current_task().ok_or(SysErrNo::ESRCH)?;
    {
        let mut state = task.signal_state.lock();
        state.suspend_old_mask = Some(state.blocked);
        state.blocked = suspend_mask;
    }

    loop {
        if has_deliverable_pending(&task) {
            return Err(SysErrNo::EINTR);
        }
        match SIGNAL_WAIT_QUEUE.sleep_until_if(None, || Ok(!has_deliverable_pending(&task))) {
            Ok(_) => continue,
            Err(SysErrNo::EINTR) => return Err(SysErrNo::EINTR),
            Err(err) => {
                let mut state = task.signal_state.lock();
                if let Some(old_mask) = state.suspend_old_mask.take() {
                    state.blocked = old_mask;
                }
                return Err(err);
            }
        }
    }
}

pub fn sys_kill(pid: i32, sig: i32) -> SyscallRet {
    if sig != 0 && !valid_signal(sig) {
        return Err(SysErrNo::EINVAL);
    }

    let targets = if pid > 0 {
        crate::task::manager::find_thread_group(pid as usize)
    } else if pid == 0 {
        let current = current_task().ok_or(SysErrNo::ESRCH)?;
        let pgid = current.inner.lock().pgid;
        crate::task::manager::find_process_group(pgid)
    } else if pid == -1 {
        crate::task::manager::all_user_tasks()
    } else if pid < -1 {
        crate::task::manager::find_process_group((-(pid as isize)) as usize)
    } else {
        return Err(SysErrNo::ESRCH);
    };

    if targets.is_empty() {
        if pid > 0 && crate::task::manager::was_thread_group_seen(pid as usize) {
            return Ok(0);
        }
        return Err(SysErrNo::ESRCH);
    }
    if sig == 0 {
        return Ok(0);
    }

    deliver_to_processes(&targets, sig, PendingSignalInfo::from_current(SI_USER));
    Ok(0)
}

pub fn sys_tkill(tid: i32, sig: i32) -> SyscallRet {
    if tid <= 0 {
        return Err(SysErrNo::EINVAL);
    }
    if sig != 0 && !valid_signal(sig) {
        return Err(SysErrNo::EINVAL);
    }
    let task = crate::task::manager::find_task(tid as usize).ok_or(SysErrNo::ESRCH)?;
    if task.is_kernel {
        return Err(SysErrNo::ESRCH);
    }
    if sig != 0 {
        queue_signal(&task, sig, PendingSignalInfo::from_current(SI_TKILL));
    }
    Ok(0)
}

pub fn sys_tgkill(tgid: i32, tid: i32, sig: i32) -> SyscallRet {
    if tgid <= 0 || tid <= 0 {
        return Err(SysErrNo::EINVAL);
    }
    if sig != 0 && !valid_signal(sig) {
        return Err(SysErrNo::EINVAL);
    }
    let task = crate::task::manager::find_task(tid as usize).ok_or(SysErrNo::ESRCH)?;
    if task.is_kernel || task.thread_group.tgid() != tgid as usize {
        return Err(SysErrNo::ESRCH);
    }
    if sig != 0 {
        queue_signal(&task, sig, PendingSignalInfo::from_current(SI_TKILL));
    }
    Ok(0)
}

pub fn sys_sigreturn() -> SyscallRet {
    let ctx = crate::trap::clone_current_trapframe().ok_or(SysErrNo::EINVAL)?;
    let frame_addr = ctx[TrapFrameArgs::SP];
    let frame = super::user::copy_object_from_user::<SignalFrame>(frame_addr)?;
    if frame.magic != SIGNAL_FRAME_MAGIC || frame.frame_size != core::mem::size_of::<SignalFrame>()
    {
        return Err(SysErrNo::EINVAL);
    }

    let task = current_task().ok_or(SysErrNo::ESRCH)?;
    task.signal_state.lock().blocked = sanitize_mask(frame.ucontext.uc_sigmask.to_kernel_mask());
    if !crate::trap::update_current_trapframe(|tf| {
        restore_trapframe(&frame.ucontext.uc_mcontext, &frame.arch_extra, tf)
    }) {
        return Err(SysErrNo::EINVAL);
    }
    crate::trap::signal_rt_sigreturn_done();
    Ok(0)
}

pub fn reset_signal_handlers_for_exec(task: &Arc<TaskControlBlock>) {
    let mut actions = task.signal_actions.lock();
    for signum in 1..=MAX_SIGNAL {
        let action = actions.get(signum);
        if action.handler > SIG_IGN {
            actions.set(signum, DEFAULT_SIGACTION);
        }
    }
}

pub fn current_has_unblocked_pending() -> bool {
    current_task()
        .as_ref()
        .map(has_deliverable_pending)
        .unwrap_or(false)
}

pub(crate) fn remove_signal_waiters_for_task(task: &Arc<TaskControlBlock>) -> usize {
    SIGNAL_WAIT_QUEUE.remove_task_waiters(task)
}

pub(crate) fn notify_child_exit(child: &Arc<TaskControlBlock>) {
    let parent = child.inner.lock().parent.clone();
    let Some(parent) = parent else {
        return;
    };
    if parent.is_kernel || parent.status() == TaskStatus::Zombie {
        return;
    }

    queue_signal(
        &parent,
        SIGCHLD,
        PendingSignalInfo {
            code: CLD_EXITED,
            sender_pid: child.thread_group.tgid() as i32,
            sender_uid: 0,
        },
    );
}

pub(crate) fn send_sigpipe_to_current() {
    if let Some(task) = current_task() {
        queue_signal(&task, SIGPIPE, PendingSignalInfo::from_current(SI_USER));
    }
}

pub(crate) fn send_interval_timer_signal(task: &Arc<TaskControlBlock>, which: usize) {
    let signum = match which {
        0 => SIGALRM,
        1 => SIGVTALRM,
        2 => SIGPROF,
        _ => return,
    };
    queue_signal(task, signum, PendingSignalInfo::from_current(SI_USER));
}

pub(crate) fn handle_pending_for_task(ctx: &mut TrapFrame, task: &Arc<TaskControlBlock>) -> bool {
    if task.is_kernel {
        return false;
    }
    if task.signal_pending_hint.load(Ordering::Acquire) == 0 {
        return true;
    }
    if task.status() == TaskStatus::Zombie {
        return false;
    }

    loop {
        let Some(signum) = next_unblocked_pending(&task) else {
            return true;
        };
        let action = task.signal_actions.lock().get(signum);

        if action.handler == SIG_IGN || (action.handler == SIG_DFL && default_ignored(signum)) {
            clear_pending_signal(&task, signum);
            continue;
        }

        if action.handler == SIG_DFL && default_stops(signum) {
            clear_pending_signal(&task, signum);
            crate::task::stop_task_group(&task, signum);
            return false;
        }

        if action.handler == SIG_DFL {
            clear_pending_signal(&task, signum);
            crate::task::terminate_task_group(&task, default_exit_code(signum));
            return false;
        }

        let (old_mask, info) = {
            let mut state = task.signal_state.lock();
            let old_mask = state.suspend_old_mask.take().unwrap_or(state.blocked);
            let info = state.pending_info[signum as usize];
            let mut blocked = old_mask | action.mask;
            if (action.flags & SA_NODEFER) == 0 {
                blocked |= signal_bit(signum);
            }
            state.pending &= !signal_bit(signum);
            state.pending_info[signum as usize] = PendingSignalInfo::empty();
            state.blocked = sanitize_mask(blocked);
            task.signal_pending_hint
                .store(usize::from(state.pending != 0), Ordering::Release);
            (old_mask, info)
        };

        if (action.flags & SA_RESETHAND) != 0 && !cannot_catch_or_ignore(signum) {
            task.signal_actions.lock().set(signum, DEFAULT_SIGACTION);
        }

        if setup_signal_frame(ctx, signum, action, old_mask, info).is_err() {
            crate::task::terminate_task_group(&task, default_exit_code(SIGSEGV));
            return false;
        }
        return true;
    }
}

pub fn handle_pending_for_user(ctx: &mut TrapFrame) -> bool {
    let Some(task) = current_task() else {
        return true;
    };
    if task.status() == TaskStatus::Zombie {
        return false;
    }
    handle_pending_for_task(ctx, &task)
}

/// Deliver a synchronous CPU exception through the Linux signal ABI.
///
/// A caught SIGILL/SIGSEGV must expose the faulting context to userspace so
/// probes can update the saved PC and resume. Returning to the same faulting
/// instruction when the signal is blocked or ignored would livelock, so those
/// cases retain the default terminating behavior.
pub(crate) fn handle_synchronous_fault_for_user(ctx: &mut TrapFrame, signum: i32) -> bool {
    let Some(task) = current_task() else {
        return false;
    };
    if task.is_kernel || task.status() == TaskStatus::Zombie || !valid_signal(signum) {
        return false;
    }

    let action = task.signal_actions.lock().get(signum);
    if action.handler == SIG_IGN || !signal_is_unblocked(&task, signum) {
        crate::task::terminate_task_group(&task, default_exit_code(signum));
        return false;
    }

    queue_signal(
        &task,
        signum,
        PendingSignalInfo {
            code: SI_KERNEL,
            sender_pid: 0,
            sender_uid: 0,
        },
    );
    handle_pending_for_user(ctx)
}

fn deliver_to_process(targets: &[Arc<TaskControlBlock>], signum: i32, info: PendingSignalInfo) {
    if let Some(task) = targets
        .iter()
        .find(|task| task.status() != TaskStatus::Zombie && signal_is_unblocked(task, signum))
        .or_else(|| {
            targets
                .iter()
                .find(|task| task.status() != TaskStatus::Zombie)
        })
        .or_else(|| targets.first())
    {
        queue_signal(task, signum, info);
    }
}

fn deliver_to_processes(targets: &[Arc<TaskControlBlock>], signum: i32, info: PendingSignalInfo) {
    let mut delivered_tgids = Vec::new();
    for task in targets {
        let tgid = task.thread_group.tgid();
        if delivered_tgids.contains(&tgid) {
            continue;
        }
        delivered_tgids.push(tgid);
        let mut members = Vec::new();
        for member in targets {
            if member.thread_group.tgid() == tgid {
                members.push(member.clone());
            }
        }
        deliver_to_process(&members, signum, info);
    }
}

fn queue_signal(task: &Arc<TaskControlBlock>, signum: i32, info: PendingSignalInfo) {
    if signum == SIGCONT {
        crate::task::continue_task_group(task, SIGCONT);
    }
    let action = task.signal_actions.lock().get(signum);
    if action.handler == SIG_DFL
        && default_stops(signum)
        && (signum == SIGSTOP || signal_is_unblocked(task, signum))
    {
        crate::task::stop_task_group(task, signum);
        return;
    }
    if action.handler == SIG_DFL && !default_ignored(signum) && signal_is_unblocked(task, signum) {
        crate::task::terminate_task_group(task, default_exit_code(signum));
        return;
    }
    if signum == SIGCONT && action.handler == SIG_DFL {
        return;
    }
    let mut state = task.signal_state.lock();
    state.pending |= signal_bit(signum);
    state.pending_info[signum as usize] = info;
    task.signal_pending_hint.store(1, Ordering::Release);
    drop(state);
    SIGNAL_WAIT_QUEUE.wake_all();
    wake_for_signal(task);
}

fn wake_for_signal(task: &Arc<TaskControlBlock>) {
    if task.status() == TaskStatus::Stopped {
        return;
    }
    if has_deliverable_pending(task) {
        crate::task::wake_blocked_task(task, WaitOutcome::Interrupted);
    }
}

fn next_unblocked_pending(task: &Arc<TaskControlBlock>) -> Option<i32> {
    let state = task.signal_state.lock();
    let pending = state.pending & !state.blocked;
    if pending == 0 {
        None
    } else {
        Some(pending.trailing_zeros() as i32 + 1)
    }
}

fn signal_is_unblocked(task: &Arc<TaskControlBlock>, signum: i32) -> bool {
    let state = task.signal_state.lock();
    (state.blocked & signal_bit(signum)) == 0
}

fn clear_pending_signal(task: &Arc<TaskControlBlock>, signum: i32) {
    let mut state = task.signal_state.lock();
    state.pending &= !signal_bit(signum);
    state.pending_info[signum as usize] = PendingSignalInfo::empty();
    task.signal_pending_hint
        .store(usize::from(state.pending != 0), Ordering::Release);
}

fn has_pending_in_mask(task: &Arc<TaskControlBlock>, mask: usize) -> bool {
    let state = task.signal_state.lock();
    (state.pending & mask) != 0
}

fn take_pending_from_mask(
    task: &Arc<TaskControlBlock>,
    mask: usize,
) -> Option<(i32, PendingSignalInfo)> {
    let mut state = task.signal_state.lock();
    let pending = state.pending & mask;
    if pending == 0 {
        return None;
    }
    let signum = pending.trailing_zeros() as i32 + 1;
    let info = state.pending_info[signum as usize];
    state.pending &= !signal_bit(signum);
    state.pending_info[signum as usize] = PendingSignalInfo::empty();
    task.signal_pending_hint
        .store(usize::from(state.pending != 0), Ordering::Release);
    Some((signum, info))
}

fn has_deliverable_pending(task: &Arc<TaskControlBlock>) -> bool {
    let mut mask = {
        let state = task.signal_state.lock();
        state.pending & !state.blocked
    };
    while mask != 0 {
        let signum = mask.trailing_zeros() as i32 + 1;
        let bit = signal_bit(signum);
        let action = task.signal_actions.lock().get(signum);
        if action.handler > SIG_IGN || (action.handler == SIG_DFL && !default_ignored(signum)) {
            return true;
        }
        mask &= !bit;
    }
    false
}

fn setup_signal_frame(
    ctx: &mut TrapFrame,
    signum: i32,
    action: KernelSigAction,
    old_mask: usize,
    info: PendingSignalInfo,
) -> Result<(), SysErrNo> {
    let frame_size = core::mem::size_of::<SignalFrame>();
    let frame_addr = ctx[TrapFrameArgs::SP]
        .checked_sub(frame_size)
        .ok_or(SysErrNo::EFAULT)?
        & !0xf;
    let frame = SignalFrame {
        magic: SIGNAL_FRAME_MAGIC,
        frame_size,
        signo: signum as usize,
        old_mask,
        siginfo: info.user_siginfo(signum),
        ucontext: user_ucontext_from_trapframe(ctx, old_mask),
        arch_extra: arch_extra_from_trapframe(ctx),
    };
    super::user::copy_object_to_user(frame_addr, &frame)?;

    ctx[TrapFrameArgs::SP] = frame_addr;
    ctx[TrapFrameArgs::SEPC] = action.handler;
    ctx[TrapFrameArgs::RA] = if (action.flags & SA_RESTORER) != 0 && action.restorer != 0 {
        action.restorer
    } else {
        signal_trampoline_addr()
    };
    ctx[TrapFrameArgs::ARG0] = signum as usize;
    ctx[TrapFrameArgs::ARG1] = if (action.flags & SA_SIGINFO) != 0 {
        frame_addr + core::mem::offset_of!(SignalFrame, siginfo)
    } else {
        0
    };
    ctx[TrapFrameArgs::ARG2] = frame_addr + core::mem::offset_of!(SignalFrame, ucontext);
    Ok(())
}

fn user_ucontext_from_trapframe(tf: &TrapFrame, old_mask: usize) -> UserUContext {
    UserUContext {
        uc_flags: 0,
        uc_link: 0,
        uc_stack: UserSigAltStack {
            ss_sp: 0,
            ss_flags: 0,
            _pad: 0,
            ss_size: 0,
        },
        uc_sigmask: UserSigSet::from_kernel(old_mask),
        uc_mcontext: user_mcontext_from_trapframe(tf),
    }
}

#[cfg(target_arch = "riscv64")]
fn user_mcontext_from_trapframe(tf: &TrapFrame) -> UserMContext {
    let mut gregs = tf.x;
    gregs[0] = tf.sepc;
    let mut fpregs = [0; 66];
    fpregs[..32].copy_from_slice(&tf.f);
    fpregs[32] = tf.fcsr;
    UserMContext { gregs, fpregs }
}

#[cfg(target_arch = "riscv64")]
fn arch_extra_from_trapframe(tf: &TrapFrame) -> ArchSignalExtra {
    ArchSignalExtra {
        sstatus: unsafe { core::mem::transmute::<_, usize>(tf.sstatus) },
        fsx: tf.fsx,
    }
}

#[cfg(target_arch = "riscv64")]
fn restore_trapframe(saved: &UserMContext, extra: &ArchSignalExtra, tf: &mut TrapFrame) {
    tf.x = saved.gregs;
    tf.x[0] = 0;
    tf.sstatus = unsafe { core::mem::transmute(extra.sstatus) };
    tf.sepc = saved.gregs[0];
    tf.fsx = extra.fsx;
    tf.f.copy_from_slice(&saved.fpregs[..32]);
    tf.fcsr = saved.fpregs[32];
}

#[cfg(target_arch = "loongarch64")]
fn user_mcontext_from_trapframe(tf: &TrapFrame) -> UserMContext {
    UserMContext {
        regs: tf.regs,
        prmd: tf.prmd,
        era: tf.era,
    }
}

#[cfg(target_arch = "loongarch64")]
fn arch_extra_from_trapframe(tf: &TrapFrame) -> ArchSignalExtra {
    ArchSignalExtra {
        f: tf.f,
        fcc: tf.fcc,
        fcsr: tf.fcsr,
    }
}

#[cfg(target_arch = "loongarch64")]
fn restore_trapframe(saved: &UserMContext, extra: &ArchSignalExtra, tf: &mut TrapFrame) {
    tf.regs = saved.regs;
    tf.prmd = saved.prmd;
    tf.era = saved.era;
    tf.f = extra.f;
    tf.fcc = extra.fcc;
    tf.fcsr = extra.fcsr;
}
