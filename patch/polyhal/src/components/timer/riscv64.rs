use core::time::Duration;

// TODO: Get CLOCK_FREQUENCY CLOCK_FREQ
use riscv::register::{sie, time};

const CLOCK_FREQ: u64 = 10000000;

/// Get ticks from system clock
///
/// # Return
///
/// - [u64] clock ticks
#[inline]
pub fn get_ticks() -> u64 {
    time::read64()
}

/// Get frequency of the system clock
///
/// # Return
///
/// - [u64] n ticks per second
#[inline]
pub fn get_freq() -> u64 {
    CLOCK_FREQ
}

/// Set the next timer
///
/// # parameters
///
/// - next [Duration] next time from system boot#[inline]
pub fn set_next_timer(next: Duration) {
    let delta = next.as_secs() * CLOCK_FREQ + next.subsec_nanos() as u64 * CLOCK_FREQ / 1_000_000_000;
    sbi_rt::set_timer(
        get_ticks().saturating_add(delta),
    );
}

// Initialize the Timer
pub fn init() {
    unsafe {
        sie::set_stimer();
    }
    // NOTE: Do NOT call set_next_timer() here. The kernel's scheduler
    // should call set_next_timer() when it's ready to handle timer interrupts.
    // Calling set_next_timer(Duration::ZERO) during boot can cause immediate
    // timer interrupts that crash before the trap handler is fully set up.
    log::info!("initialize timer interrupt (stimer enabled, scheduling deferred)");
}
