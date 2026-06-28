use crate::debug_console::DebugConsole;

// QEMU virt 机器默认把第一路串口暴露为 16550 UART，MMIO 基址为
// 0x1000_0000。输出仍走 SBI legacy console_putchar，输入则需要直接读
// UART 寄存器；否则内核只能打印日志，无法在 -serial stdio 下接收键盘。
const UART_ADDR: usize = 0x1000_0000;
// Receiver Buffer Register：LSR 表示有数据后，从这里取 1 字节输入。
const UART_RBR: usize = 0;
// Line Status Register：用于轮询串口是否收到新字节。
const UART_LSR: usize = 5;
// Data Ready bit：置位表示 RBR 中至少有 1 字节可读。
const UART_LSR_DR: u8 = 0x01;

/// Debug console using SBI legacy console_putchar.
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
        unsafe {
            // 这里必须使用 volatile 读，防止编译器把 MMIO 寄存器访问优化掉。
            // 没有数据时返回 None，由上层决定是非阻塞返回 EOF，还是交互模式
            // 下自旋等待用户输入。
            let lsr = core::ptr::read_volatile((UART_ADDR + UART_LSR) as *const u8);
            if (lsr & UART_LSR_DR) != 0 {
                Some(core::ptr::read_volatile((UART_ADDR + UART_RBR) as *const u8))
            } else {
                None
            }
        }
    }
}
