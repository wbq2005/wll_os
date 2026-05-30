use polyhal_trap::trap::TrapType;
/// 中断处理辅助函数
use polyhal_trap::trapframe::TrapFrame;

/// 架构相关中断处理入口
///
/// 这个函数被 polyhal 的 trap 处理代码调用
/// 是内核态和用户态中断的统一入口点
#[no_mangle]
#[allow(improper_ctypes_definitions)]
pub extern "C" fn _interrupt_for_arch(ctx: &mut TrapFrame, trap_type: TrapType, _token: usize) {
    // 判断是内核态还是用户态中断
    // 通过 TrapFrame 中的信息来判断
    let from_user = is_from_user(ctx);

    if from_user {
        // 用户态中断 - 需要保存用户上下文
        crate::trap::user_interrupt(ctx, trap_type);
    } else {
        // 内核态中断
        crate::trap::kernel_interrupt(ctx, trap_type);
    }
}

/// 判断中断是否来自用户态
///
/// 通过检查 TrapFrame 中的特权级信息来判断
#[inline]
fn is_from_user(ctx: &TrapFrame) -> bool {
    // 使用 TrapFrame 自带的 from_user 方法
    ctx.from_user()
}

/// 使能中断
#[inline]
pub fn enable_interrupt() {
    #[cfg(target_arch = "riscv64")]
    unsafe {
        riscv::register::sstatus::set_sie();
    }

    #[cfg(all(target_arch = "loongarch64", feature = "loongarch"))]
    {
        use loongArch64::register::prmd;
        prmd::set_pie(true);
    }
}

/// 禁用中断
#[inline]
pub fn disable_interrupt() {
    #[cfg(target_arch = "riscv64")]
    unsafe {
        riscv::register::sstatus::clear_sie();
    }

    #[cfg(all(target_arch = "loongarch64", feature = "loongarch"))]
    {
        use loongArch64::register::prmd;
        prmd::set_pie(false);
    }
}

/// 检查中断是否使能
#[inline]
pub fn is_interrupt_enabled() -> bool {
    #[cfg(target_arch = "riscv64")]
    {
        riscv::register::sstatus::read().sie()
    }

    #[cfg(all(target_arch = "loongarch64", feature = "loongarch"))]
    {
        use loongArch64::register::prmd;
        prmd::read().pie()
    }

    #[cfg(not(any(
        target_arch = "riscv64",
        all(target_arch = "loongarch64", feature = "loongarch")
    )))]
    {
        false
    }
}
