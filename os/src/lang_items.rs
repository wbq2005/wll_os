use core::panic::PanicInfo;

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    #[cfg(target_arch = "riscv64")]
    {
        unsafe {
            core::arch::asm!("li a7, 0x01", "li a0, 0x50", "ecall",);
        }
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
