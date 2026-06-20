/// Call SBI_SHUTDOWN to close the machine. Exit qemu if you are using qemu.
#[inline]
pub fn shutdown() -> ! {
    let _ = sbi_rt::system_reset(sbi_rt::Shutdown, sbi_rt::NoReason);

    unsafe {
        core::arch::asm!(
            "ecall",
            in("a7") sbi_rt::legacy::LEGACY_SHUTDOWN,
            lateout("a0") _,
        );
    }

    loop {
        unsafe {
            core::arch::asm!("wfi");
        }
    }
}
 
