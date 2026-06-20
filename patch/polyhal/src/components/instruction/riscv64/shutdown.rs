/// Call SBI_SHUTDOWN to close the machine. Exit qemu if you are using qemu.
#[inline]
pub fn shutdown() -> ! {
    qemu_virt_finisher_pass();

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

#[inline]
fn qemu_virt_finisher_pass() {
    const QEMU_VIRT_TEST_FINISHER: *mut u32 = 0x100000 as *mut u32;
    const FINISHER_PASS: u32 = 0x5555;

    unsafe {
        QEMU_VIRT_TEST_FINISHER.write_volatile(FINISHER_PASS);
    }
}
 
