/// 任务上下文 - 用于上下文切换
///
/// 保存任务切换时需要保存的寄存器状态
/// 注意：这是任务级别的上下文，与 TrapFrame（中断上下文）不同
#[derive(Debug, Clone)]
#[repr(C)]
pub struct TaskContext {
    pub ra: usize,      // 返回地址 (x1)
    pub sp: usize,      // 栈指针 (x2)
    pub s: [usize; 12], // 被调用者保存的寄存器 s0-s11 (x8-x9, x18-x27)
}

impl TaskContext {
    /// 创建零初始化的上下文
    pub fn zero_init() -> Self {
        Self {
            ra: 0,
            sp: 0,
            s: [0; 12],
        }
    }

    /// 创建用于恢复任务的上下文
    ///
    /// 用于创建初始任务上下文，设置返回地址为任务入口
    pub fn goto_restore() -> Self {
        Self {
            ra: 0, // 稍后设置为任务入口地址
            sp: 0, // 稍后设置为任务栈顶
            s: [0; 12],
        }
    }

    /// 设置返回地址
    pub fn set_ra(&mut self, ra: usize) {
        self.ra = ra;
    }

    /// 设置栈指针
    pub fn set_sp(&mut self, sp: usize) {
        self.sp = sp;
    }
}

/// 上下文切换
///
/// 保存当前任务的上下文到 `current_task_ctx`，
/// 从 `next_task_ctx` 恢复下一个任务的上下文
#[cfg(target_arch = "riscv64")]
#[naked]
#[no_mangle]
pub unsafe extern "C" fn switch_to(_current_task_ctx: *mut TaskContext, _next_task_ctx: *const TaskContext) {
    core::arch::naked_asm!(
        // 保存当前任务的上下文
        // current_task_ctx 在 a0 寄存器
        "sd ra, 0(a0)",
        "sd sp, 8(a0)",
        "sd s0, 16(a0)",
        "sd s1, 24(a0)",
        "sd s2, 32(a0)",
        "sd s3, 40(a0)",
        "sd s4, 48(a0)",
        "sd s5, 56(a0)",
        "sd s6, 64(a0)",
        "sd s7, 72(a0)",
        "sd s8, 80(a0)",
        "sd s9, 88(a0)",
        "sd s10, 96(a0)",
        "sd s11, 104(a0)",

        // 恢复下一个任务的上下文
        // next_task_ctx 在 a1 寄存器
        "ld ra, 0(a1)",
        "ld sp, 8(a1)",
        "ld s0, 16(a1)",
        "ld s1, 24(a1)",
        "ld s2, 32(a1)",
        "ld s3, 40(a1)",
        "ld s4, 48(a1)",
        "ld s5, 56(a1)",
        "ld s6, 64(a1)",
        "ld s7, 72(a1)",
        "ld s8, 80(a1)",
        "ld s9, 88(a1)",
        "ld s10, 96(a1)",
        "ld s11, 104(a1)",

        // 返回到下一个任务
        "ret",
    )
}

/// 上下文切换 (LoongArch)
#[cfg(target_arch = "loongarch64")]
#[naked]
#[no_mangle]
pub unsafe extern "C" fn switch_to(_current_task_ctx: *mut TaskContext, _next_task_ctx: *const TaskContext) {
    core::arch::naked_asm!(
        // 保存当前任务的上下文
        // current_task_ctx 在 a0 寄存器 ($4)
        "st.d $ra, $a0, 0",    // ra -> 0(a0)
        "st.d $sp, $a0, 8",    // sp -> 8(a0)
        "st.d $s0, $a0, 16",   // s0 -> 16(a0)
        "st.d $s1, $a0, 24",   // s1 -> 24(a0)
        "st.d $s2, $a0, 32",   // s2 -> 32(a0)
        "st.d $s3, $a0, 40",   // s3 -> 40(a0)
        "st.d $s4, $a0, 48",   // s4 -> 48(a0)
        "st.d $s5, $a0, 56",   // s5 -> 56(a0)
        "st.d $s6, $a0, 64",   // s6 -> 64(a0)
        "st.d $s7, $a0, 72",   // s7 -> 72(a0)
        "st.d $s8, $a0, 80",   // s8 -> 80(a0)
        "st.d $fp, $a0, 88",   // fp(s9) -> 88(a0)
        // LoongArch64 只有 s0-s8 (9个 callee-saved 寄存器)
        // 不需要保存 s10, s11

        // 恢复下一个任务的上下文
        // next_task_ctx 在 a1 寄存器 ($5)
        "ld.d $ra, $a1, 0",    // 0(a1) -> ra
        "ld.d $sp, $a1, 8",    // 8(a1) -> sp
        "ld.d $s0, $a1, 16",   // 16(a1) -> s0
        "ld.d $s1, $a1, 24",   // 24(a1) -> s1
        "ld.d $s2, $a1, 32",   // 32(a1) -> s2
        "ld.d $s3, $a1, 40",   // 40(a1) -> s3
        "ld.d $s4, $a1, 48",   // 48(a1) -> s4
        "ld.d $s5, $a1, 56",   // 56(a1) -> s5
        "ld.d $s6, $a1, 64",   // 64(a1) -> s6
        "ld.d $s7, $a1, 72",   // 72(a1) -> s7
        "ld.d $s8, $a1, 80",   // 80(a1) -> s8
        "ld.d $fp, $a1, 88",   // 88(a1) -> fp(s9)

        // 返回到下一个任务
        "jr $ra",
    )
}
