#[macro_use]
mod macros;

use super::{EscapeReason, TrapType};
use crate::trapframe::TrapFrame;
use core::arch::naked_asm;
use core::sync::atomic::{AtomicU8, Ordering};
use polyhal::consts::VIRT_ADDR_START;
use riscv::{
    interrupt::{Exception, Interrupt},
    register::{
        scause::{self, Trap},
        stval,
        stvec::{self, Stvec},
    },
};

/// Global flag to signal that the trap handler should skip sret.
/// This is set when a task switch occurred (e.g., from exit()).
/// The kernelvec assembly checks this flag and jumps to skip_sret if set.
static SKIP_SRET_FLAG: AtomicU8 = AtomicU8::new(0);

/// Pointer to the trap frame being processed (saved before switch_to).
static mut TRAP_FRAME_PTR: usize = 0;

// Initialize trap vectors only (no timer). Call this early during boot.
pub fn init_trap_only() {
    unsafe {
        let mut stvec = Stvec::from_bits(0);
        stvec.set_address(kernelvec as *const () as usize);
        stvec.set_trap_mode(stvec::TrapMode::Direct);
        stvec::write(stvec);
    }
}

// Initialize trap vectors AND timer. Call this once the kernel is ready
// to handle timer interrupts (trap handler must be set up first).
pub fn init() {
    init_trap_only();
    polyhal::timer::init();
}

// Initialize the timer component. Must be called AFTER the trap handler
// is set up (via init_trap_only or init), otherwise the kernel may crash
// when OpenSBI tries to access SIE CSR 0x14d.
pub fn init_timer() {
    polyhal::timer::init();
}

// 内核中断回调
#[no_mangle]
fn kernel_callback(context: &mut TrapFrame) -> TrapType {
    let scause = scause::read();
    let stval = stval::read();

    let trap_type = match scause.cause().try_into().unwrap() {
        // 中断异常
        Trap::Exception(Exception::Breakpoint) => {
            context.sepc += 2;
            TrapType::Breakpoint
        }
        Trap::Exception(Exception::LoadFault) => {
            if stval > VIRT_ADDR_START {
                panic!("kernel error: {:#x}", stval);
            }
            TrapType::Unknown
        }
        Trap::Exception(Exception::UserEnvCall) => TrapType::SysCall,
        // 时钟中断
        Trap::Interrupt(Interrupt::SupervisorTimer) => TrapType::Timer,
        Trap::Exception(Exception::StorePageFault) => TrapType::StorePageFault(stval),
        Trap::Exception(Exception::StoreFault) => TrapType::StorePageFault(stval),
        Trap::Exception(Exception::InstructionPageFault) => TrapType::InstructionPageFault(stval),
        Trap::Exception(Exception::IllegalInstruction) => TrapType::IllegalInstruction(stval),
        Trap::Exception(Exception::LoadPageFault) => TrapType::LoadPageFault(stval),
        Trap::Interrupt(Interrupt::SupervisorExternal) => TrapType::SupervisorExternal,
        _ => {
            log::error!(
                "kernel trap {:#x} {:?}  stval {:#x}  sepc: {:#x}",
                scause.bits(),
                scause.cause(),
                stval,
                context.sepc
            );
            panic!("unknown trap: {:#x?}", context);
        }
    };

    // Check SKIP_SRET flag: if set, the trap handler has performed a task switch
    // and we should NOT execute sret. Instead, we'll jump to skip_sret.
    // The flag is checked again by the assembly after this function returns.
    if SKIP_SRET_FLAG.load(Ordering::SeqCst) == 1 {
        // Just let the assembly check the flag and skip sret
    }

    unsafe { super::_interrupt_for_arch(context, trap_type, 0) };
    trap_type
}

#[naked]
#[allow(named_asm_labels)]
pub unsafe extern "C" fn kernelvec() {
    naked_asm!(
        includes_trap_macros!(),
        // 宏定�?
        r"
            .align 4
            .altmacro

            csrrw   sp, sscratch, sp
            bnez    sp, uservec
            csrr    sp, sscratch

            addi    sp, sp, -{cx_size}

            SAVE_GENERAL_REGS
            csrw    sscratch, x0

            mv      a0, sp

            call kernel_callback

            // Check SKIP_SRET_FLAG before restoring registers.
            // If a task switch occurred, skip restoring from the corrupted sp.
            // Use la (PC-relative) instead of lui+offset (absolute).
            la      t0, {skip_sret_flag}
            lb      t0, 0(t0)
            bnez    t0, skip_sret_label

            LOAD_GENERAL_REGS
            sret

        skip_sret_label:
            // Clear the flag and jump to the skip handler
            .option push
            .option norelax
            la      t0, {skip_sret_flag}
            sb      x0, 0(t0)
            .option pop
            tail kernel_skip_sret
        ",
        cx_size = const crate::trapframe::TRAPFRAME_SIZE,
        skip_sret_flag = sym SKIP_SRET_FLAG,
    )
}

// Called when we need to skip sret (after a task switch in exit).
// The OS-level scheduler lives outside this crate, so this low-level fallback
// cannot call it directly. Current user-task returns do not use this path.
#[no_mangle]
extern "C" fn kernel_skip_sret() {
    SKIP_SRET_FLAG.store(0, Ordering::SeqCst);
    loop {
        unsafe { core::arch::asm!("wfi"); }
    }
}

#[naked]
#[no_mangle]
unsafe extern "C" fn user_restore(context: *mut TrapFrame) {
    naked_asm!(
            includes_trap_macros!(),
            // 在内核态栈中开一个空间来存储内核态信�?
            // 下次发生中断必然会进入中断入口然后恢复这个上下文.
            // 仅保�?Callee-saved regs、gp、tp、ra.
            ".align 4
                addi    sp, sp, -18*8

                STR      sp,  1
                STR      gp,  2
                STR      tp,  3
                STR      s0,  4
                STR      s1,  5
                STR      s2,  6
                STR      s3,  7
                STR      s4,  8
                STR      s5,  9
                STR      s6,  10
                STR      s7,  11
                STR      s8,  12
                STR      s9,  13
                STR      s10, 14
                STR      s11, 15
                STR      a0,  16
                STR      ra,  17
            ",
            // 将栈信息保存到用户栈.
            // a0 是传入的Context, 然后下面会再次恢�?sp 地址.
            "   sd       sp, 8*0(a0)
                csrw     sscratch, a0
                mv       sp, a0

                LOAD_GENERAL_REGS
                sret
            ",
        )
}

#[naked]
#[no_mangle]
#[allow(named_asm_labels)]
pub unsafe extern "C" fn uservec() {
    naked_asm!(
        includes_trap_macros!(),
        // 保存 general registers, 除了 sp
        "
        SAVE_GENERAL_REGS
        csrw    sscratch, x0

        mv      a0, sp
        ld      sp, 0*8(a0)
        sd      x0, 0*8(a0)
    ",
        // 恢复内核上下文信�? 仅恢�?callee-saved 寄存器和 ra、gp、tp
        "  
        LDR      gp,  2
        LDR      tp,  3
        LDR      s0,  4
        LDR      s1,  5
        LDR      s2,  6
        LDR      s3,  7
        LDR      s4,  8
        LDR      s5,  9
        LDR      s6,  10
        LDR      s7,  11
        LDR      s8,  12
        LDR      s9,  13
        LDR      s10, 14
        LDR      s11, 15
        LDR      ra,  17
        
        LDR      sp,  1
    ",
        // 回收�?
        "addi sp, sp, 18*8
        ret
    ",
    );
}

/// Return EscapeReson related to interrupt type.
/// Signal that the next kernelvec should skip sret (because a task switch occurred).
/// Call this when a task exit or context switch needs to bypass sret.
pub fn signal_skip_sret() {
    SKIP_SRET_FLAG.store(1, Ordering::SeqCst);
}

pub fn run_user_task(context: &mut TrapFrame) -> EscapeReason {
    unsafe { user_restore(context) };
    kernel_callback(context).into()
}

/// Run user task until interrupt is received.
pub fn run_user_task_forever(context: &mut TrapFrame) -> ! {
    loop {
        unsafe { user_restore(context) };
        kernel_callback(context);
    }
}
