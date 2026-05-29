use crate::arch::consts::VIRT_ADDR_START;

#[inline]
pub fn ebreak() {
    unsafe {
        core::arch::asm!("break 2");
    }
}

#[inline]
pub fn shutdown() -> ! {
    let ged_addr = (0x100E001C | VIRT_ADDR_START) as *mut u8;
    log::info!("Shutting down...");
    unsafe { ged_addr.write_volatile(0x34) };
    unsafe { loongArch64::asm::idle() };
    log::warn!("It should shutdown!");
    unreachable!()
}
