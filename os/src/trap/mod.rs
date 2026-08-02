pub mod interrupts;

use lazy_static::lazy_static;
use polyhal_trap::trap::TrapType;
use polyhal_trap::trapframe::{TrapFrame, TrapFrameArgs};
use spin::Mutex;

use crate::cpu::CpuLocal;
use crate::syscall::syscall;
use crate::task::exit_current_and_run_next;
use crate::task::TaskStatus;
use crate::timer::set_next_trigger;

const SIGILL: i32 = 4;
const SIGSEGV: i32 = 11;
lazy_static! {
    /// 存 `TrapFrame` 裸指针（单核）；用 `usize` 避免 `*mut TrapFrame: !Send` 与 `lazy_static` 冲突。
    pub static ref CURRENT_SYSCALL_CTX_PTR: CpuLocal<usize> = CpuLocal::new_with(|_| 0);

    /// 标记当前是否处于 execve 调用上下文中。
    /// 置位时 handle_syscall 跳过 syscall_ok() PC 前进，让 execve 直接返回到新程序入口。
    static ref EXECVE_COMPLETED: CpuLocal<bool> = CpuLocal::new_with(|_| false);
    static ref SIGRETURN_COMPLETED: CpuLocal<bool> = CpuLocal::new_with(|_| false);

    /// 标记前台驱动模式：当此标志为 true 时，exit/suspend/timer 不要调用 run_next_task()，
    /// 而是将当前任务置为 Zombie 后直接返回，由前台驱动负责收尾。
    static ref FOREGROUND_DRIVER_ACTIVE: Mutex<bool> = Mutex::new(false);
    static ref SYSCALL_PARKED: CpuLocal<bool> = CpuLocal::new_with(|_| false);

}

pub fn enter_foreground_driver() {
    *FOREGROUND_DRIVER_ACTIVE.lock() = true;
    crate::timer::set_next_foreground_trigger();
}

pub fn leave_foreground_driver() {
    *FOREGROUND_DRIVER_ACTIVE.lock() = false;
    crate::timer::set_next_trigger();
}

pub fn foreground_driver_active() -> bool {
    *FOREGROUND_DRIVER_ACTIVE.lock()
}

pub fn signal_syscall_parked() {
    *SYSCALL_PARKED.lock() = true;
}

pub fn syscall_parked() -> bool {
    *SYSCALL_PARKED.lock()
}

fn take_syscall_parked() -> bool {
    let mut guard = SYSCALL_PARKED.lock();
    let was = *guard;
    *guard = false;
    was
}

fn exit_user_thread_group_for_signal(signum: i32) {
    crate::task::exit_thread_group_and_run_next(crate::task::signal_exit_code(signum));
}

pub fn prepare_user_trapframe(tf: &mut TrapFrame) {
    #[cfg(target_arch = "riscv64")]
    {
        let bits = unsafe { core::mem::transmute::<_, usize>(tf.sstatus) };
        let bits = (bits & !(1 << 8)) | (1 << 5) | (3 << 13);
        tf.sstatus = unsafe { core::mem::transmute(bits) };
    }
    #[cfg(target_arch = "loongarch64")]
    {
        tf.prmd = (tf.prmd & !0b111) | 0b111;
    }
}

pub fn clone_current_trapframe() -> Option<TrapFrame> {
    let ptr = *CURRENT_SYSCALL_CTX_PTR.lock();
    if ptr == 0 {
        None
    } else {
        unsafe { Some((*(ptr as *mut TrapFrame)).clone()) }
    }
}

pub fn update_current_trapframe(f: impl FnOnce(&mut TrapFrame)) -> bool {
    let ptr = *CURRENT_SYSCALL_CTX_PTR.lock();
    if ptr == 0 {
        false
    } else {
        unsafe {
            f(&mut *(ptr as *mut TrapFrame));
        }
        true
    }
}

pub fn save_current_trapframe(tf: &TrapFrame) {
    if let Some(task) = crate::task::current_task() {
        *task.trap_frame.lock() = Some(tf.clone());
    }
}

/// 向 trap 处理层发送信号：当前 execve 已替换地址空间，
/// handle_syscall 应跳过 syscall_ok() PC 前进，直接返回到新程序入口。
pub fn signal_execve_done() {
    *EXECVE_COMPLETED.lock() = true;
}

pub fn signal_rt_sigreturn_done() {
    *SIGRETURN_COMPLETED.lock() = true;
}

/// Returns true if the current trap was caused by execve completing.
/// Consumes the flag so it can only be observed once per trap.
fn take_execve_done() -> bool {
    let mut guard = EXECVE_COMPLETED.lock();
    let was = *guard;
    *guard = false;
    was
}

fn take_sigreturn_done() -> bool {
    let mut guard = SIGRETURN_COMPLETED.lock();
    let was = *guard;
    *guard = false;
    was
}

/// 初始化 Trap/中断处理（trap 向量，不含定时器）
///
/// RISC-V: trap 向量已在 ctor 中初始化（polyhal-trap::TRAP_INIT），无需重复。
/// LoongArch: trap 向量已在 ctor 中初始化。
pub fn init() {
    // trap 向量已在 polyhal-trap 的 ctor 中初始化
    #[cfg(target_arch = "riscv64")]
    polyhal_trap::trap::init_trap_only();
    #[cfg(target_arch = "loongarch64")]
    {
        polyhal_trap::trap::init();
        // LoongArch glibc binaries in the tests use the double-float ABI.  Keep
        // the base FPU enabled so user FP instructions do not trap as FPD after
        // libc has finished its integer-only startup path.
        loongArch64::register::euen::set_fpe(true);
        // The official LoongArch glibc advertises and uses LSX.  Keep it
        // enabled only because the user trap frame saves the complete 128-bit
        // vector register file across every user/kernel transition.
        loongArch64::register::euen::set_sxe(true);
        log::info!(
            "[trap] loongarch eentry={:#x}",
            loongArch64::register::eentry::read().eentry()
        );
    }
    log::info!("[trap] Trap handler initialized");
}

/// 初始化定时器组件（必须在 trap 向量设置后才可调用）
///
/// RISC-V: 调用 sie::set_stimer() 启用 SIE[STIE]。
/// 必须在 trap handler 设置后调用，否则 M-mode OpenSBI 访问 SIE CSR 0x14d 会崩溃。
pub fn init_timer() {
    polyhal_trap::trap::init_timer();
    log::info!("[timer] Timer initialized");
}

/// 内核中断处理函数
pub fn kernel_interrupt(ctx: &mut TrapFrame, trap_type: TrapType) {
    match trap_type {
        TrapType::SysCall => {
            // 系统调用处理
            handle_syscall(ctx);
        }
        TrapType::StorePageFault(vaddr)
        | TrapType::LoadPageFault(vaddr)
        | TrapType::PagePrivilegeFault(vaddr) => {
            log::error!(
                "[trap] Kernel page fault at {:#x}, sepc={:#x}, sp={:#x}",
                vaddr,
                ctx[TrapFrameArgs::SEPC],
                ctx[TrapFrameArgs::SP]
            );
            exit_current_and_run_next(-2);
        }
        TrapType::InstructionPageFault(vaddr) => {
            log::error!(
                "[trap] Instruction page fault at {:#x}, sepc={:#x}, sp={:#x}",
                vaddr,
                ctx[TrapFrameArgs::SEPC],
                ctx[TrapFrameArgs::SP]
            );
            exit_current_and_run_next(-2);
        }
        TrapType::Timer => {
            crate::timer::rearm_kernel_tick();
        }
        TrapType::Ipi(_) => {
            let action = crate::platform::acknowledge_local_ipi();
            crate::platform::handle_ipi(action);
        }
        TrapType::IllegalInstruction(vaddr) => {
            log::error!("[trap] Illegal instruction at {:#x}", vaddr);
            exit_current_and_run_next(-2);
        }
        TrapType::Breakpoint => {
            log::info!("[trap] Breakpoint hit");
            // 处理断点，通常是调试器使用
        }
        TrapType::SupervisorExternal => {
            log::info!("[trap] Supervisor external interrupt");
        }
        TrapType::Irq(irq) => {
            log::info!("[trap] IRQ {:?} received", irq);
        }
        TrapType::Unknown => {
            log::warn!("[trap] Unknown trap type");
        }
    }
}

/// 用户态中断处理函数
///
/// 用户态陷入内核时的处理入口
pub fn user_interrupt(ctx: &mut TrapFrame, trap_type: TrapType) {
    // LoongArch keeps its conservative kernel-root trap boundary. RISC-V user
    // roots already share the kernel RAM mappings, so ordinary syscall work can
    // retain the current ASID; low-address MMIO drivers switch roots explicitly.
    // Scheduling, blocking, exit, and exec still restore the kernel root.
    #[cfg(target_arch = "loongarch64")]
    restore_kernel_page_table();

    // 获取当前任务的 trap 上下文
    // 对于用户态中断，需要保存用户上下文并处理

    match trap_type {
        TrapType::SysCall => {
            handle_syscall(ctx);
        }
        TrapType::Timer => {
            // The foreground runner owns saving the returned trap frame and
            // requeueing the current task.  Calling suspend_current_and_run_next
            // here as well would enqueue the same task twice and leave one queue
            // entry without a trap frame.  Returning from run_user_task is enough
            // to give the runner a scheduling boundary.
            crate::timer::wake_expired_timers();
            if foreground_driver_active() {
                crate::timer::set_next_foreground_trigger();
                let _ = crate::syscall::signal::handle_pending_for_user(ctx);
            } else {
                set_next_trigger();
                let _ = crate::syscall::signal::handle_pending_for_user(ctx);
            }
        }
        TrapType::Ipi(_) => {
            let action = crate::platform::acknowledge_local_ipi();
            crate::platform::handle_ipi(action);
        }
        trap @ (TrapType::StorePageFault(vaddr)
        | TrapType::LoadPageFault(vaddr)
        | TrapType::InstructionPageFault(vaddr)
        | TrapType::PagePrivilegeFault(vaddr)) => {
            if let Some(task) = crate::task::current_task() {
                let is_privilege = matches!(trap, TrapType::PagePrivilegeFault(_));
                let is_store = matches!(trap, TrapType::StorePageFault(_));
                let is_exec = matches!(trap, TrapType::InstructionPageFault(_));
                if !is_privilege
                    && task
                        .memory_set
                        .lock()
                        .handle_page_fault(vaddr, is_store, is_exec)
                        .is_ok()
                {
                    return;
                }

                let sepc = ctx[TrapFrameArgs::SEPC];
                let sp = ctx[TrapFrameArgs::SP];
                let ms = task.memory_set.lock();
                let fault_pa = ms.translate(polyhal::VirtAddr::new(vaddr));
                let sepc_pa = ms.translate(polyhal::VirtAddr::new(sepc));
                log::error!(
                    "[trap] User page fault {:?} pid={} at {:#x}, sepc={:#x}, sp={:#x}, ra={:#x}, tp={:#x}, fault_pa={:?}, sepc_pa={:?}",
                    trap,
                    task.pid.0,
                    vaddr,
                    sepc,
                    sp,
                    ctx[TrapFrameArgs::RA],
                    ctx[TrapFrameArgs::TLS],
                    fault_pa,
                    sepc_pa
                );
            } else {
                log::error!(
                    "[trap] User page fault {:?} at {:#x}, killing process",
                    trap,
                    vaddr
                );
            }
            exit_user_thread_group_for_signal(SIGSEGV);
        }
        TrapType::IllegalInstruction(vaddr) => {
            log::error!(
                "[trap] User illegal instruction at {:#x}, killing process",
                vaddr
            );
            exit_user_thread_group_for_signal(SIGILL);
        }
        _ => {
            log::warn!(
                "[trap] Unhandled user trap {:?}, killing process",
                trap_type
            );
            exit_user_thread_group_for_signal(SIGSEGV);
        }
    }
}

/// 处理系统调用
///
/// 从 TrapFrame 中提取系统调用参数并分发到对应的处理函数
fn handle_syscall(ctx: &mut TrapFrame) {
    // 获取系统调用号（RISC-V: a7/x[17], LoongArch: r11）
    let syscall_id = ctx[TrapFrameArgs::SYSCALL];
    // 获取系统调用参数（a0-a5）
    let args = ctx.args();

    log::debug!("[syscall] id: {}, args: {:?}", syscall_id, args);

    // 暴露当前 syscall 上下文，供 fork/clone 等复制寄存器上下文使用
    *CURRENT_SYSCALL_CTX_PTR.lock() = ctx as *mut TrapFrame as usize;

    // 调用系统调用分发函数
    let syscall_task = crate::task::current_task();
    #[cfg(feature = "buildstorm-diagnostics")]
    crate::buildstorm_diagnostics::note_syscall_enter(syscall_id);
    let result = syscall(syscall_id, args);
    #[cfg(feature = "buildstorm-diagnostics")]
    crate::buildstorm_diagnostics::note_syscall_exit(syscall_id);

    *CURRENT_SYSCALL_CTX_PTR.lock() = 0;
    if take_syscall_parked() {
        return;
    }

    // 设置返回值到 a0/x[10]
    // 检查是否是 execve 刚完成——如果是，跳过 syscall_ok() 的 PC 前进，
    // 让 CPU sret 到新程序的入口地址（sepc 已在 sys_execve 中设为 entry）。
    let execve_done = take_execve_done();
    if execve_done {
        return;
    }
    let sigreturn_done = take_sigreturn_done();
    if sigreturn_done {
        return;
    }
    if syscall_task
        .as_ref()
        .map(|task| task.status() == TaskStatus::Zombie)
        .unwrap_or(false)
    {
        return;
    }
    crate::timer::wake_expired_timers();
    match result {
        Ok(ret) => {
            ctx[TrapFrameArgs::RET] = ret;
        }
        Err(errno) => {
            // 错误时返回负的错误码
            ctx[TrapFrameArgs::RET] = errno.as_ret();
        }
    }
    // 普通系统调用：PC 需要前进（跳过 ecall 指令）
    ctx.syscall_ok();
    let should_return = syscall_task
        .as_ref()
        .map(|task| !matches!(task.status(), TaskStatus::Zombie | TaskStatus::Stopped))
        .unwrap_or(true);
    if should_return {
        let _ = crate::syscall::signal::handle_pending_for_user(ctx);
    }
}

/// 处理进程退出系统调用
///
/// 当用户进程调用 exit 时，切换到下一个任务
pub fn handle_exit(exit_code: i32) {
    log::info!("[trap] Process exit with code {}", exit_code);
    exit_current_and_run_next(exit_code);
}

/// 恢复内核页表
///
/// 在用户任务返回后调用，因为 SATP 在用户态运行时被切换到了用户页表。
/// 内核代码无法通过高虚拟地址访问，必须先恢复内核页表。
pub fn restore_kernel_page_table() {
    crate::platform::clear_current_address_space();
    if let Some(ref kpt) = *crate::mm::page_table::kernel_page_table().lock() {
        if polyhal::pagetable::PageTable::current().root() == kpt.root() {
            return;
        }
        crate::perf_counters::note_kernel_page_table_restore();
        let reused_kernel_asid = polyhal::pagetable::PageTable::current_asid() == 0;
        kpt.change_with_asid(0);
        if reused_kernel_asid {
            polyhal::pagetable::TLB::flush_all();
        }
    }
}
