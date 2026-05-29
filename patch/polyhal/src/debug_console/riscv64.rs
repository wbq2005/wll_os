use crate::debug_console::DebugConsole;

/// Debug console using SBI legacy console_putchar (HTIF)
impl DebugConsole {
    #[inline]
    pub fn putchar(ch: u8) {
        #[cfg(target_arch = "riscv64")]
        {
            Self::sbi_putchar(ch);
        }
        #[cfg(not(target_arch = "riscv64"))]
        {
            let _ = ch;
        }
    }

    #[cfg(target_arch = "riscv64")]
    fn sbi_putchar(ch: u8) {
        if ch == b'\n' {
            Self::sbi_putchar(b'\r');
        }
        unsafe {
            core::arch::asm!(
                "li a7, 0x01",
                "mv a0, {0}",
                "ecall",
                in(reg) ch as usize,
                out("a7") _,
                out("a0") _
            );
        }
    }

    #[inline]
    pub fn puthex(mut value: usize) {
        if value == 0 {
            Self::putchar(b'0');
            return;
        }
        let digits: [u8; 16] = *b"0123456789abcdef";
        let mut buf = [0u8; 16];
        let mut len = 0;
        while value > 0 && len < 16 {
            len += 1;
            buf[16 - len] = digits[value & 0xF];
            value >>= 4;
        }
        for i in (16 - len)..16 {
            Self::putchar(buf[i]);
        }
    }

    #[inline]
    pub fn getchar() -> Option<u8> {
        None
    }
}
