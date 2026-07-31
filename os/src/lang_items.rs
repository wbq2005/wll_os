use core::panic::PanicInfo;
use core::sync::atomic::{AtomicBool, Ordering};

static PANIC_REPORTED: AtomicBool = AtomicBool::new(false);

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    if !PANIC_REPORTED.swap(true, Ordering::SeqCst) {
        crate::println!(
            "Kernel panic on cpu {} (hardware {}): {}",
            crate::platform::current_cpu_index(),
            crate::platform::current_hardware_cpu_id(),
            info
        );
    }
    loop {
        #[cfg(target_arch = "riscv64")]
        unsafe {
            core::arch::asm!("wfi");
        }
        #[cfg(target_arch = "loongarch64")]
        unsafe {
            core::arch::asm!("idle 0");
        }
    }
}
