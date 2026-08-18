pub mod interrupts;

use lazy_static::lazy_static;
use polyhal_trap::trap::TrapType;
use polyhal_trap::trapframe::{TrapFrame, TrapFrameArgs};
use spin::Mutex;

use core::sync::atomic::{AtomicUsize, Ordering};

use crate::config::MAX_CPUS;
use crate::syscall::syscall;
use crate::task::exit_current_and_run_next;
use crate::task::TaskStatus;
use crate::timer::set_next_trigger;

const SIGILL: i32 = 4;
const SIGBUS: i32 = 7;
const SIGSEGV: i32 = 11;
const SYSCALL_OUTCOME_PARKED: usize = 1 << 0;
const SYSCALL_OUTCOME_EXECVE: usize = 1 << 1;
const SYSCALL_OUTCOME_SIGRETURN: usize = 1 << 2;

static CURRENT_SYSCALL_CTX_PTR: [AtomicUsize; MAX_CPUS] =
    [const { AtomicUsize::new(0) }; MAX_CPUS];
static SYSCALL_OUTCOME: [AtomicUsize; MAX_CPUS] =
    [const { AtomicUsize::new(0) }; MAX_CPUS];
#[cfg(feature = "buildstorm-diagnostics")]
static UNALIGNED_ACCESS_OK: AtomicUsize = AtomicUsize::new(0);

#[inline]
fn syscall_cpu() -> usize {
    crate::platform::current_cpu_index().min(MAX_CPUS - 1)
}

lazy_static! {
    /// 标记前台驱动模式：当此标志为 true 时，exit/suspend/timer 不要调用 run_next_task()，
    /// 而是将当前任务置为 Zombie 后直接返回，由前台驱动负责收尾。
    static ref FOREGROUND_DRIVER_ACTIVE: Mutex<bool> = Mutex::new(false);
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
    SYSCALL_OUTCOME[syscall_cpu()].fetch_or(SYSCALL_OUTCOME_PARKED, Ordering::Release);
}

pub fn syscall_parked() -> bool {
    SYSCALL_OUTCOME[syscall_cpu()].load(Ordering::Acquire) & SYSCALL_OUTCOME_PARKED != 0
}

fn exit_user_thread_group_for_signal(signum: i32) {
    crate::task::exit_thread_group_and_run_next(crate::task::signal_exit_code(signum));
}

#[cfg(feature = "buildstorm-diagnostics")]
fn note_terminal_user_trap(
    kind: usize,
    task: Option<&alloc::sync::Arc<crate::task::TaskControlBlock>>,
    vaddr: usize,
    ctx: &TrapFrame,
) {
    let sepc = ctx[TrapFrameArgs::SEPC];
    let sp = ctx[TrapFrameArgs::SP];
    let ra = ctx[TrapFrameArgs::RA];
    let tp = ctx[TrapFrameArgs::TLS];
    let Some(task) = task else {
        crate::buildstorm_diagnostics::note_first_terminal_user_trap(
            kind, 0, vaddr, sepc, sp, ra, tp, 0, 0, 0, 0, 0, 0, 0, 0,
        );
        return;
    };
    let ms = crate::buildstorm_memory_set_lock!(
        crate::buildstorm_diagnostics::MemorySetLockSite::Other,
        &task.memory_set,
    );
    let fault_pa = ms
        .translate(polyhal::VirtAddr::new(vaddr))
        .map(|pa| pa.raw())
        .unwrap_or(0);
    let sepc_pa = ms
        .translate(polyhal::VirtAddr::new(sepc))
        .map(|pa| pa.raw())
        .unwrap_or(0);
    let (vma_start, vma_end, vma_flags, backing, resident, page_state) = ms
        .areas
        .iter()
        .find(|area| area.contains(polyhal::VirtAddr::new(vaddr)))
        .map(|area| {
            let (backing, resident, page_state) = area.diagnostic_page_snapshot(vaddr);
            (
                area.start_va.raw(),
                area.end_va.raw(),
                area.flags.bits() as usize,
                backing,
                resident,
                page_state,
            )
        })
        .unwrap_or((0, 0, 0, 0, 0, 0));
    crate::buildstorm_diagnostics::note_first_terminal_user_trap(
        kind, task.pid.0, vaddr, sepc, sp, ra, tp, fault_pa, sepc_pa, vma_start, vma_end,
        vma_flags, backing, resident, page_state,
    );
}

pub fn prepare_user_trapframe(tf: &mut TrapFrame) {
    #[cfg(target_arch = "riscv64")]
    {
        let bits = unsafe { core::mem::transmute::<_, usize>(tf.sstatus) };
        // `user_restore` writes this value before it has completed restoring
        // registers and executing `sret`.  SIE must stay clear throughout
        // that transition: an IPI in the middle is entered through `uservec`
        // (sscratch already names the user frame) and would treat a partial
        // restore as a user trap.  SPIE is set so `sret` enables interrupts
        // only after the CPU has actually entered the user context.
        let bits = (bits & !((1 << 8) | (1 << 1))) | (1 << 5) | (3 << 13);
        tf.sstatus = unsafe { core::mem::transmute(bits) };
    }
    #[cfg(target_arch = "loongarch64")]
    {
        tf.prmd = (tf.prmd & !0b111) | 0b111;
    }
}

pub fn clone_current_trapframe() -> Option<TrapFrame> {
    let ptr = CURRENT_SYSCALL_CTX_PTR[syscall_cpu()].load(Ordering::Acquire);
    if ptr == 0 {
        None
    } else {
        unsafe { Some((*(ptr as *mut TrapFrame)).clone()) }
    }
}

pub fn update_current_trapframe(f: impl FnOnce(&mut TrapFrame)) -> bool {
    let ptr = CURRENT_SYSCALL_CTX_PTR[syscall_cpu()].load(Ordering::Acquire);
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
    SYSCALL_OUTCOME[syscall_cpu()].fetch_or(SYSCALL_OUTCOME_EXECVE, Ordering::Release);
}

pub fn signal_rt_sigreturn_done() {
    SYSCALL_OUTCOME[syscall_cpu()].fetch_or(SYSCALL_OUTCOME_SIGRETURN, Ordering::Release);
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
            #[cfg(feature = "buildstorm-diagnostics")]
            crate::buildstorm_diagnostics::note_kernel_tick();
            // A timed syscall can be parked in kernel context (notably the
            // foreground driver's blocked-owner loop).  Such a task cannot
            // reach user_interrupt(), so process timer waiters here as well
            // as on the user trap path.  Without this, nanosleep children
            // remain blocked forever while their parent waits in wait4.
            crate::timer::wake_expired_timers();
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
        TrapType::UnalignedAccess => {
            log::error!("[trap] kernel unaligned access at sepc={:#x}", ctx[TrapFrameArgs::SEPC]);
            exit_current_and_run_next(-2);
        }
        TrapType::UnalignedAccessFault(vaddr) => {
            log::error!("[trap] kernel unaligned emulation failed at {:#x}", vaddr);
            exit_current_and_run_next(-2);
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
    #[cfg(feature = "buildstorm-diagnostics")]
    let _trap_scope = crate::buildstorm_diagnostics::note_user_trap_enter(match &trap_type {
        TrapType::SysCall => 1,
        TrapType::Timer => 2,
        TrapType::Ipi(_) => 3,
        TrapType::StorePageFault(_) => 4,
        TrapType::LoadPageFault(_) => 5,
        TrapType::InstructionPageFault(_) => 6,
        TrapType::PagePrivilegeFault(_) => 7,
        _ => 8,
    });
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
            #[cfg(feature = "buildstorm-diagnostics")]
            crate::buildstorm_diagnostics::note_user_tick();
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
        // The LoongArch trap layer has already translated and emulated the
        // fault while the user page table was active. Restoring the kernel
        // root above is the only common-layer work needed here.
        TrapType::UnalignedAccess => {
            #[cfg(feature = "buildstorm-diagnostics")]
            {
                let count = UNALIGNED_ACCESS_OK.fetch_add(1, Ordering::Relaxed) + 1;
                if count <= 8 || count.is_power_of_two() {
                    crate::println!(
                        "BUILDSTORM_DIAG unaligned_ok count={} sepc={:#x}",
                        count,
                        ctx[TrapFrameArgs::SEPC],
                    );
                }
            }
        }
        TrapType::UnalignedAccessFault(vaddr) => {
            #[cfg(feature = "smp-regression")]
            crate::smp_regression::note_user_memory_lifecycle_terminal_trap(
                5,
                vaddr,
                ctx[TrapFrameArgs::SEPC],
            );
            #[cfg(feature = "buildstorm-diagnostics")]
            note_terminal_user_trap(5, crate::task::current_task().as_ref(), vaddr, ctx);
            #[cfg(feature = "buildstorm-diagnostics")]
            crate::println!(
                "BUILDSTORM_DIAG unaligned_failure sepc={:#x} badv={:#x} signal={}",
                ctx[TrapFrameArgs::SEPC],
                vaddr,
                SIGBUS,
            );
            log::error!(
                "[trap] User unaligned emulation failed at {:#x}, killing process",
                vaddr
            );
            if crate::syscall::signal::handle_synchronous_fault_for_user(ctx, SIGBUS) {
                return;
            }
            exit_user_thread_group_for_signal(SIGBUS);
        }
        trap @ (TrapType::StorePageFault(vaddr)
        | TrapType::LoadPageFault(vaddr)
        | TrapType::InstructionPageFault(vaddr)
        | TrapType::PagePrivilegeFault(vaddr)) => {
            if let Some(task) = crate::task::current_task() {
                let is_privilege = matches!(trap, TrapType::PagePrivilegeFault(_));
                let is_store = matches!(trap, TrapType::StorePageFault(_));
                let is_exec = matches!(trap, TrapType::InstructionPageFault(_));
                let page_fault_handled = if is_privilege {
                    false
                } else {
                    let mut memory_set = crate::buildstorm_memory_set_lock!(
                        crate::buildstorm_diagnostics::MemorySetLockSite::HardwarePageFault,
                        &task.memory_set,
                    );
                    #[cfg(feature = "buildstorm-diagnostics")]
                    let _fault_source =
                        crate::buildstorm_diagnostics::PageFaultSourceScope::new(if is_exec {
                            crate::buildstorm_diagnostics::PageFaultSource::HardwareExec
                        } else if is_store {
                            crate::buildstorm_diagnostics::PageFaultSource::HardwareStore
                        } else {
                            crate::buildstorm_diagnostics::PageFaultSource::HardwareLoad
                        });
                    memory_set
                        .handle_page_fault(vaddr, is_store, is_exec)
                        .is_ok()
                };
                if page_fault_handled {
                    return;
                }

                let sepc = ctx[TrapFrameArgs::SEPC];
                #[cfg(feature = "buildstorm-diagnostics")]
                let terminal_kind = match trap {
                    TrapType::LoadPageFault(_) => 1,
                    TrapType::StorePageFault(_) => 2,
                    TrapType::InstructionPageFault(_) => 3,
                    TrapType::PagePrivilegeFault(_) => 4,
                    _ => unreachable!(),
                };
                #[cfg(feature = "smp-regression")]
                crate::smp_regression::note_user_memory_lifecycle_terminal_trap(1, vaddr, sepc);
                #[cfg(feature = "buildstorm-diagnostics")]
                note_terminal_user_trap(terminal_kind, Some(&task), vaddr, ctx);
                let sp = ctx[TrapFrameArgs::SP];
                let ms = crate::buildstorm_memory_set_lock!(
                    crate::buildstorm_diagnostics::MemorySetLockSite::Other,
                    &task.memory_set,
                );
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
                #[cfg(feature = "buildstorm-diagnostics")]
                note_terminal_user_trap(
                    match trap {
                        TrapType::LoadPageFault(_) => 1,
                        TrapType::StorePageFault(_) => 2,
                        TrapType::InstructionPageFault(_) => 3,
                        TrapType::PagePrivilegeFault(_) => 4,
                        _ => unreachable!(),
                    },
                    None,
                    vaddr,
                    ctx,
                );
                log::error!(
                    "[trap] User page fault {:?} at {:#x}, killing process",
                    trap,
                    vaddr
                );
            }
            exit_user_thread_group_for_signal(SIGSEGV);
        }
        TrapType::IllegalInstruction(vaddr) => {
            if crate::syscall::signal::handle_synchronous_fault_for_user(ctx, SIGILL) {
                return;
            }
            #[cfg(feature = "smp-regression")]
            crate::smp_regression::note_user_memory_lifecycle_terminal_trap(
                2,
                vaddr,
                ctx[TrapFrameArgs::SEPC],
            );
            #[cfg(feature = "buildstorm-diagnostics")]
            note_terminal_user_trap(2, crate::task::current_task().as_ref(), vaddr, ctx);
            log::error!(
                "[trap] User illegal instruction at {:#x}, killing process",
                vaddr
            );
            let needs_exit = crate::task::current_task()
                .map(|task| task.status() != TaskStatus::Zombie)
                .unwrap_or(true);
            if needs_exit {
                exit_user_thread_group_for_signal(SIGILL);
            }
        }
        _ => {
            #[cfg(feature = "smp-regression")]
            crate::smp_regression::note_user_memory_lifecycle_terminal_trap(
                3,
                0,
                ctx[TrapFrameArgs::SEPC],
            );
            #[cfg(feature = "buildstorm-diagnostics")]
            note_terminal_user_trap(3, crate::task::current_task().as_ref(), 0, ctx);
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
    let cpu = syscall_cpu();
    SYSCALL_OUTCOME[cpu].store(0, Ordering::Release);
    CURRENT_SYSCALL_CTX_PTR[cpu].store(ctx as *mut TrapFrame as usize, Ordering::Release);

    // 调用系统调用分发函数
    #[cfg(feature = "buildstorm-diagnostics")]
    crate::buildstorm_diagnostics::note_active_syscall_enter(
        syscall_id,
        ctx[TrapFrameArgs::SEPC],
    );
    #[cfg(feature = "buildstorm-diagnostics")]
    let syscall_scope = crate::buildstorm_diagnostics::note_syscall_enter(syscall_id);
    let result = syscall(syscall_id, args);
    #[cfg(feature = "buildstorm-diagnostics")]
    drop(syscall_scope);
    #[cfg(feature = "buildstorm-diagnostics")]
    crate::buildstorm_diagnostics::note_syscall_exit(syscall_id);

    CURRENT_SYSCALL_CTX_PTR[cpu].store(0, Ordering::Release);
    let outcome = SYSCALL_OUTCOME[cpu].swap(0, Ordering::AcqRel);
    if outcome & SYSCALL_OUTCOME_PARKED != 0 {
        return;
    }

    // 设置返回值到 a0/x[10]
    // 检查是否是 execve 刚完成——如果是，跳过 syscall_ok() 的 PC 前进，
    // 让 CPU sret 到新程序的入口地址（sepc 已在 sys_execve 中设为 entry）。
    if outcome & SYSCALL_OUTCOME_EXECVE != 0 {
        return;
    }
    if outcome & SYSCALL_OUTCOME_SIGRETURN != 0 {
        return;
    }
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
        #[cfg(feature = "buildstorm-diagnostics")]
        crate::buildstorm_diagnostics::note_root_activation(false, false);
        kpt.change_with_asid(0);
        #[cfg(target_arch = "loongarch64")]
        {
            // LoongArch changes PGDL and ASID independently. Switching from
            // a user root back to the kernel root therefore needs a local
            // invalidation even when the previous ASID was nonzero.
            polyhal::pagetable::TLB::flush_all();
        }
        if reused_kernel_asid {
            #[cfg(feature = "buildstorm-diagnostics")]
            crate::buildstorm_diagnostics::note_local_tlb_flush();
            polyhal::pagetable::TLB::flush_all();
        }
    }
}
