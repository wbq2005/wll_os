use polyhal::timer::current_time;

/// 时钟频率
///
/// QEMU virt 机器通常使用 10MHz 的时钟
pub const CLOCK_FREQ: usize = 10000000; // 10MHz for QEMU virt

/// 时间片大小（毫秒）
pub const TIME_SLICE_MS: u64 = 10;
/// Foreground judge runs one user task tree synchronously.  A longer tick keeps
/// static libc startup from spending most of its time bouncing through the
/// harness, while normal scheduler time slices stay at 10ms.
pub const FOREGROUND_TIME_SLICE_MS: u64 = 50;

/// 获取当前时间戳（毫秒）
pub fn get_time() -> usize {
    current_time().as_millis() as usize
}

/// 获取当前时间戳（微秒）
pub fn get_time_us() -> usize {
    current_time().as_micros() as usize
}

/// 设置下一次定时器中断
///
/// 设置一个 10ms 后的定时器中断
pub fn set_next_trigger() {
    polyhal::timer::set_next_timer(core::time::Duration::from_millis(TIME_SLICE_MS));
}

pub fn set_next_foreground_trigger() {
    polyhal::timer::set_next_timer(core::time::Duration::from_millis(
        FOREGROUND_TIME_SLICE_MS,
    ));
}

/// 初始化定时器
///
/// 设置第一次定时器中断
#[cfg(target_arch = "riscv64")]
pub fn init() {
    log::info!("[timer] Initializing timer, clock freq: {} Hz", CLOCK_FREQ);
    // Defer timer initialization to trap::init_timer(), which calls
    // polyhal_trap::trap::init_timer() -> polyhal::timer::init() -> sie::set_stimer().
    // This ensures the trap handler is fully set up before enabling SIE[STIE].
    crate::trap::init_timer();
    set_next_trigger();
    log::info!(
        "[timer] Timer initialized, time slice: {} ms",
        TIME_SLICE_MS
    );
}

#[cfg(target_arch = "loongarch64")]
pub fn init() {
    log::info!("[timer] Initializing timer, clock freq: {} Hz", CLOCK_FREQ);
    polyhal::timer::init();
    set_next_trigger();
    log::info!(
        "[timer] loongarch ecfg={:?} tcfg={:?}",
        loongArch64::register::ecfg::read(),
        loongArch64::register::tcfg::read()
    );
    log::info!(
        "[timer] Timer initialized, time slice: {} ms",
        TIME_SLICE_MS
    );
}

/// 获取当前时间（秒和微秒）
///
/// 用于 gettimeofday 系统调用
pub fn get_timeval() -> (usize, usize) {
    let time_us = get_time_us();
    let sec = time_us / 1_000_000;
    let usec = time_us % 1_000_000;
    (sec, usec)
}

/// 延时指定毫秒数（忙等待）
///
/// 注意：这是一个简单的忙等待实现，会占用 CPU
/// 实际实现中应该使用定时器中断
pub fn sleep_ms(ms: usize) {
    let start = get_time();
    while get_time() < start + ms {
        core::hint::spin_loop();
    }
}
