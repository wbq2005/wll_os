pub mod interrupts;

use polyhal_trap::trap::TrapType;
use polyhal_trap::trapframe::{TrapFrame, TrapFrameArgs};
use lazy_static::lazy_static;
use spin::Mutex;

use crate::syscall::syscall;
use crate::task::{suspend_current_and_run_next, exit_current_and_run_next};
use crate::timer::set_next_trigger;

lazy_static! {
    /// 存 `TrapFrame` 裸指针（单核）；用 `usize` 避免 `*mut TrapFrame: !Send` 与 `lazy_static` 冲突。
    pub static ref CURRENT_SYSCALL_CTX_PTR: Mutex<usize> = Mutex::new(0);

    /// 标记当前是否处于 execve 调用上下文中。
    /// 置位时 handle_syscall 跳过 syscall_ok() PC 前进，让 execve 直接返回到新程序入口。
    pub static ref EXECVE_IN_PROGRESS: Mutex<bool> = Mutex::new(false);

    /// 标记前台驱动模式：当此标志为 true 时，exit/suspend/timer 不要调用 run_next_task()，
    /// 而是将当前任务置为 Zombie 后直接返回，由前台驱动负责收尾。
    pub static ref FOREGROUND_MODE: Mutex<bool> = Mutex::new(false);
}

pub fn clone_current_trapframe() -> Option<TrapFrame> {
    let ptr = *CURRENT_SYSCALL_CTX_PTR.lock();
    if ptr == 0 {
        None
    } else {
        unsafe { Some((*(ptr as *mut TrapFrame)).clone()) }
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
    *EXECVE_IN_PROGRESS.lock() = true;
}

/// Returns true if the current trap was caused by execve completing.
/// Consumes the flag so it can only be observed once per trap.
pub fn take_execve_done() -> bool {
    let mut guard = EXECVE_IN_PROGRESS.lock();
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
        TrapType::StorePageFault(vaddr) | TrapType::LoadPageFault(vaddr) => {
            log::error!("[trap] Kernel page fault at {:#x}", vaddr);
            exit_current_and_run_next(-2);
        }
        TrapType::InstructionPageFault(vaddr) => {
            log::error!("[trap] Instruction page fault at {:#x}", vaddr);
            exit_current_and_run_next(-2);
        }
        TrapType::Timer => {
            // 定时器中断 - 设置下一次定时器并触发调度
            set_next_trigger();
            if !*FOREGROUND_MODE.lock() {
                suspend_current_and_run_next();
            }
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
    // 获取当前任务的 trap 上下文
    // 对于用户态中断，需要保存用户上下文并处理

    match trap_type {
        TrapType::SysCall => {
            handle_syscall(ctx);
        }
        TrapType::Timer => {
            set_next_trigger();
            // Foreground mode: don't suspend. Trap frame is unchanged, foreground
            // loop will re-run the task immediately. Only break if FOREGROUND_MODE.
            if *FOREGROUND_MODE.lock() {
                // just return, trap frame unchanged, foreground loop re-runs
            } else {
                suspend_current_and_run_next();
            }
        }
        TrapType::StorePageFault(vaddr) | TrapType::LoadPageFault(vaddr)
        | TrapType::InstructionPageFault(vaddr) => {
            log::error!(
                "[trap] User page fault at {:#x}, killing process",
                vaddr
            );
            exit_current_and_run_next(-2);
        }
        TrapType::IllegalInstruction(vaddr) => {
            log::error!(
                "[trap] User illegal instruction at {:#x}, killing process",
                vaddr
            );
            exit_current_and_run_next(-2);
        }
        _ => {
            log::warn!("[trap] Unhandled user trap {:?}, killing process", trap_type);
            exit_current_and_run_next(-2);
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
    let result = syscall(syscall_id, args);

    *CURRENT_SYSCALL_CTX_PTR.lock() = 0;

    // 设置返回值到 a0/x[10]
    match result {
        Ok(ret) => {
            ctx[TrapFrameArgs::RET] = ret;
        }
        Err(errno) => {
            // 错误时返回负的错误码
            ctx[TrapFrameArgs::RET] = errno.as_ret();
        }
    }

    // 检查是否是 execve 刚完成——如果是，跳过 syscall_ok() 的 PC 前进，
    // 让 CPU sret 到新程序的入口地址（sepc 已在 sys_execve 中设为 entry）。
    if take_execve_done() {
        // EXECVE_IN_PROGRESS 已被 take_execve_done() 消费，sepc 已是新程序入口
    } else {
        // 普通系统调用：PC 需要前进（跳过 ecall 指令）
        ctx.syscall_ok();
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
    if let Some(ref kpt) = *crate::mm::page_table::kernel_page_table().lock() {
        kpt.change();
    }
}
