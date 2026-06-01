use alloc::sync::Arc;
use alloc::vec::Vec;
use lazy_static::lazy_static;
use polyhal::timer::current_time;
use spin::Mutex;

use crate::task::wait_queue::{WaitOutcome, WaitQueue};
use crate::task::TaskControlBlock;

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

struct TimerWaiter {
    deadline_us: usize,
    task: Arc<TaskControlBlock>,
    token: usize,
}

lazy_static! {
    static ref TIMER_WAITERS: Mutex<Vec<TimerWaiter>> = Mutex::new(Vec::new());
    static ref SLEEP_QUEUE: WaitQueue = WaitQueue::new();
}

/// 获取当前时间戳（毫秒）
pub fn get_time() -> usize {
    current_time().as_millis() as usize
}

/// 获取当前时间戳（微秒）
pub fn get_time_us() -> usize {
    current_time().as_micros() as usize
}

fn next_timer_delay_us(now_us: usize, default_us: usize) -> usize {
    let waiters = TIMER_WAITERS.lock();
    let next_deadline = waiters.iter().map(|waiter| waiter.deadline_us).min();
    match next_deadline {
        Some(deadline) if deadline <= now_us => 1,
        Some(deadline) => default_us.min(deadline.saturating_sub(now_us)).max(1),
        None => default_us,
    }
}

fn program_next_timer(default_ms: u64) {
    let default_us = (default_ms as usize).saturating_mul(1000);
    let delay_us = next_timer_delay_us(get_time_us(), default_us);
    polyhal::timer::set_next_timer(core::time::Duration::from_micros(delay_us as u64));
}

/// 设置下一次定时器中断
///
/// 设置一个 10ms 后的定时器中断
pub fn set_next_trigger() {
    program_next_timer(TIME_SLICE_MS);
}

pub fn set_next_foreground_trigger() {
    program_next_timer(FOREGROUND_TIME_SLICE_MS);
}

pub fn add_timeout(deadline_us: usize, task: Arc<TaskControlBlock>, token: usize) {
    TIMER_WAITERS.lock().push(TimerWaiter {
        deadline_us,
        task,
        token,
    });
    set_next_trigger();
}

pub fn wake_expired_timers() {
    let now = get_time_us();
    let mut expired = Vec::new();
    {
        let mut waiters = TIMER_WAITERS.lock();
        let mut index = 0usize;
        while index < waiters.len() {
            if waiters[index].deadline_us <= now {
                expired.push(waiters.remove(index));
            } else {
                index += 1;
            }
        }
    }
    for waiter in expired {
        crate::task::wake_task_token(&waiter.task, waiter.token);
    }
}

pub fn deadline_after_us(duration_us: usize) -> usize {
    get_time_us().saturating_add(duration_us)
}

pub fn sleep_until_us(deadline_us: usize) -> Result<WaitOutcome, crate::utils::error::SysErrNo> {
    SLEEP_QUEUE.sleep_until(Some(deadline_us))
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

/// Sleep for the requested number of milliseconds using the timer wait queue.
pub fn sleep_ms(ms: usize) {
    let _ = sleep_until_us(deadline_after_us(ms.saturating_mul(1000)));
}
