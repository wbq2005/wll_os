use core::arch::naked_asm;

macro_rules! init_dwm {
    () => {
        "
        ori         $t0, $zero, 0x1     # CSR_DMW1_PLV0
        lu52i.d     $t0, $t0, -2048     # UC, PLV0, 0x8000 xxxx xxxx xxxx
        csrwr       $t0, 0x180          # LOONGARCH_CSR_DMWIN0
        ori         $t0, $zero, 0x11    # CSR_DMW1_MAT | CSR_DMW1_PLV0
        lu52i.d     $t0, $t0, -1792     # CA, PLV0, 0x9000 xxxx xxxx xxxx
        csrwr       $t0, 0x181          # LOONGARCH_CSR_DMWIN1
        "
    };
}

/// The earliest entry point for the primary CPU.
#[unsafe(naked)]
#[no_mangle]
#[link_section = ".text.entry"]
unsafe extern "C" fn _start() -> ! {
    naked_asm!(
        init_dwm!(),
        "# Set PGDL to 0 (use DMW, not page tables)
        li.d        $t0, 0x0
        csrwr       $t0, 0x18        # LOONGARCH_CSR_PGDL = 0

        li.w        $t0, 0xb0       # PLV=0, IE=0, PG=1
        csrwr       $t0, 0x0        # LOONGARCH_CSR_CRMD
        li.w        $t0, 0x00       # PLV=0, PIE=0, PWE=0
        csrwr       $t0, 0x1        # LOONGARCH_CSR_PRMD
        li.w        $t0, 0x00       # FPE=0, SXE=0, ASXE=0, BTE=0
        csrwr       $t0, 0x2        # LOONGARCH_CSR_EUEN

        la.global   $sp, bstack_top
        csrrd       $a0, 0x20           # cpuid
        la.global   $t0, {entry}
        jirl        $zero,$t0,0
        ",
        entry = sym rust_tmp_main,
    )
}

/// Rust temporary entry point - MINIMAL TEST VERSION
pub fn rust_tmp_main(_hart_id: usize) {
    // UART at physical 0x1FE002E0 (ns16550-compatible, QEMU virt)
    // LSR at offset 5, bit 5 = THR empty (ready to send)
    let uart_lsr = 0x900000001FE002E5_u64 as *const u8;
    let uart_thr = 0x900000001FE002E0_u64 as *mut u8;
    
    // Print "BOOT\n" to UART
    let msg = b"BOOT\n";
    for b in msg {
        // Wait for TX empty
        loop {
            let lsr = unsafe { core::ptr::read_volatile(uart_lsr) };
            if (lsr & 0x20) != 0 {
                break;
            }
        }
        unsafe { core::ptr::write_volatile(uart_thr, *b); }
    }
    
    // Infinite loop to confirm we reached here
    loop {
        core::hint::spin_loop();
    }
}
