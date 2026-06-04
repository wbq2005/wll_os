use super::SyscallRet;
use crate::mm::memory_set::MemorySet;
use crate::mm::page_table::PTEFlags;
use crate::task::wait_queue::WaitOutcome;
use crate::task::{current_task, TaskControlBlock, TaskStatus};
use crate::utils::error::SysErrNo;
use alloc::sync::Arc;
use polyhal::VirtAddr;
use polyhal_trap::trapframe::{TrapFrame, TrapFrameArgs};
use spin::Mutex;

pub const MAX_SIGNAL: i32 = 64;
const MAX_SIGNAL_USIZE: usize = MAX_SIGNAL as usize;
const KERNEL_SIGSET_SIZE: usize = core::mem::size_of::<usize>();

const SIG_DFL: usize = 0;
const SIG_IGN: usize = 1;

const SIGKILL: i32 = 9;
const SIGSEGV: i32 = 11;
const SIGCHLD: i32 = 17;
const SIGCONT: i32 = 18;
const SIGSTOP: i32 = 19;
const SIGTSTP: i32 = 20;
const SIGTTIN: i32 = 21;
const SIGTTOU: i32 = 22;
const SIGURG: i32 = 23;
const SIGWINCH: i32 = 28;

const SIG_BLOCK: i32 = 0;
const SIG_UNBLOCK: i32 = 1;
const SIG_SETMASK: i32 = 2;

const SA_NODEFER: usize = 0x4000_0000;
const SA_RESETHAND: usize = 0x8000_0000;

const SIGNAL_FRAME_MAGIC: usize = 0x574c_4c5f_5349_4746; // "WLL_SIGF"

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
    pub mask: usize,
}

const DEFAULT_SIGACTION: KernelSigAction = KernelSigAction {
    handler: SIG_DFL,
    flags: 0,
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
}

impl SignalState {
    pub fn new() -> Self {
        Self {
            blocked: 0,
            pending: 0,
        }
    }

    pub fn fork_from(blocked: usize) -> Self {
        Self {
            blocked: sanitize_mask(blocked),
            pending: 0,
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
    mask: usize,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct SignalFrame {
    magic: usize,
    frame_size: usize,
    signo: usize,
    old_mask: usize,
    context: ArchSignalContext,
}

#[cfg(target_arch = "riscv64")]
#[repr(C)]
#[derive(Clone, Copy)]
struct ArchSignalContext {
    x: [usize; 32],
    sstatus: usize,
    sepc: usize,
    fsx: [usize; 2],
}

#[cfg(target_arch = "loongarch64")]
#[repr(C)]
#[derive(Clone, Copy)]
struct ArchSignalContext {
    regs: [usize; 32],
    prmd: usize,
    era: usize,
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

fn cannot_catch_or_ignore(signum: i32) -> bool {
    signum == SIGKILL || signum == SIGSTOP
}

fn default_ignored(signum: i32) -> bool {
    matches!(
        signum,
        SIGCHLD | SIGCONT | SIGTSTP | SIGTTIN | SIGTTOU | SIGURG | SIGWINCH
    )
}

fn default_exit_code(signum: i32) -> i32 {
    128 + signum
}

fn user_action(action: KernelSigAction) -> UserSigAction {
    UserSigAction {
        handler: action.handler,
        flags: action.flags,
        mask: action.mask,
    }
}

fn kernel_action(action: UserSigAction) -> KernelSigAction {
    KernelSigAction {
        handler: action.handler,
        flags: action.flags,
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

pub fn sys_kill(pid: i32, sig: i32) -> SyscallRet {
    if sig != 0 && !valid_signal(sig) {
        return Err(SysErrNo::EINVAL);
    }

    let targets = if pid > 0 {
        crate::task::manager::find_thread_group(pid as usize)
    } else if pid == 0 {
        let current = current_task().ok_or(SysErrNo::ESRCH)?;
        crate::task::manager::find_thread_group(current.thread_group.tgid())
    } else if pid == -1 {
        crate::task::manager::all_user_tasks()
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

    deliver_to_process(&targets, sig);
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
        queue_signal(&task, sig);
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
        queue_signal(&task, sig);
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
    task.signal_state.lock().blocked = sanitize_mask(frame.old_mask);
    if !crate::trap::update_current_trapframe(|tf| restore_trapframe(&frame.context, tf)) {
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

pub fn handle_pending_for_user(ctx: &mut TrapFrame) -> bool {
    let Some(task) = current_task() else {
        return true;
    };
    if task.is_kernel || task.status() == TaskStatus::Zombie {
        return false;
    }

    loop {
        let Some(signum) = next_unblocked_pending(&task) else {
            return true;
        };
        let action = task.signal_actions.lock().get(signum);

        if action.handler == SIG_IGN || (action.handler == SIG_DFL && default_ignored(signum)) {
            task.signal_state.lock().pending &= !signal_bit(signum);
            continue;
        }

        if action.handler == SIG_DFL {
            task.signal_state.lock().pending &= !signal_bit(signum);
            crate::task::terminate_task_group(&task, default_exit_code(signum));
            return false;
        }

        let old_mask = {
            let mut state = task.signal_state.lock();
            let old_mask = state.blocked;
            let mut blocked = old_mask | action.mask;
            if (action.flags & SA_NODEFER) == 0 {
                blocked |= signal_bit(signum);
            }
            state.pending &= !signal_bit(signum);
            state.blocked = sanitize_mask(blocked);
            old_mask
        };

        if (action.flags & SA_RESETHAND) != 0 && !cannot_catch_or_ignore(signum) {
            task.signal_actions.lock().set(signum, DEFAULT_SIGACTION);
        }

        if setup_signal_frame(ctx, signum, action.handler, old_mask).is_err() {
            crate::task::terminate_task_group(&task, default_exit_code(SIGSEGV));
            return false;
        }
        return true;
    }
}

fn deliver_to_process(targets: &[Arc<TaskControlBlock>], signum: i32) {
    if let Some(task) = targets
        .iter()
        .find(|task| task.status() != TaskStatus::Zombie)
        .or_else(|| targets.first())
    {
        queue_signal(task, signum);
    }
}

fn queue_signal(task: &Arc<TaskControlBlock>, signum: i32) {
    task.signal_state.lock().pending |= signal_bit(signum);
    wake_for_signal(task);
}

fn wake_for_signal(task: &Arc<TaskControlBlock>) {
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
    handler: usize,
    old_mask: usize,
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
        context: arch_context_from_trapframe(ctx),
    };
    super::user::copy_object_to_user(frame_addr, &frame)?;

    ctx[TrapFrameArgs::SP] = frame_addr;
    ctx[TrapFrameArgs::SEPC] = handler;
    ctx[TrapFrameArgs::RA] = signal_trampoline_addr();
    ctx[TrapFrameArgs::ARG0] = signum as usize;
    ctx[TrapFrameArgs::ARG1] = 0;
    ctx[TrapFrameArgs::ARG2] = frame_addr;
    Ok(())
}

#[cfg(target_arch = "riscv64")]
fn arch_context_from_trapframe(tf: &TrapFrame) -> ArchSignalContext {
    ArchSignalContext {
        x: tf.x,
        sstatus: unsafe { core::mem::transmute::<_, usize>(tf.sstatus) },
        sepc: tf.sepc,
        fsx: tf.fsx,
    }
}

#[cfg(target_arch = "riscv64")]
fn restore_trapframe(saved: &ArchSignalContext, tf: &mut TrapFrame) {
    tf.x = saved.x;
    tf.sstatus = unsafe { core::mem::transmute(saved.sstatus) };
    tf.sepc = saved.sepc;
    tf.fsx = saved.fsx;
}

#[cfg(target_arch = "loongarch64")]
fn arch_context_from_trapframe(tf: &TrapFrame) -> ArchSignalContext {
    ArchSignalContext {
        regs: tf.regs,
        prmd: tf.prmd,
        era: tf.era,
    }
}

#[cfg(target_arch = "loongarch64")]
fn restore_trapframe(saved: &ArchSignalContext, tf: &mut TrapFrame) {
    tf.regs = saved.regs;
    tf.prmd = saved.prmd;
    tf.era = saved.era;
}
