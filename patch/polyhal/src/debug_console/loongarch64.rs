use spin::Mutex;

use super::DebugConsole;

#[cfg(not(board = "2k1000"))]
const UART_ADDR: usize = 0x8000_0000_0000_0000 | 0x01FE_001E0;
#[cfg(board = "2k1000")]
const UART_ADDR: usize = 0x800000001fe20000;

/// NS16550A UART 寄存器偏移
const UART_THR: usize = 0; // 发送保持寄存器
const UART_RBR: usize = 0; // 接收缓冲寄存器
const UART_LSR: usize = 5; // 线路状态寄存器
const UART_LSR_THRE: u8 = 0x20; // 发送保持寄存器空
const UART_LSR_DR: u8 = 0x01;   // 数据就绪

struct Uart {
    base: usize,
}

impl Uart {
    const fn new(base: usize) -> Self {
        Self { base }
    }

    fn write_reg(&self, offset: usize, value: u8) {
        unsafe {
            core::ptr::write_volatile((self.base + offset) as *mut u8, value);
        }
    }

    fn read_reg(&self, offset: usize) -> u8 {
        unsafe {
            core::ptr::read_volatile((self.base + offset) as *const u8)
        }
    }

    fn put(&mut self, ch: u8) {
        // 等待发送缓冲区为空
        while (self.read_reg(UART_LSR) & UART_LSR_THRE) == 0 {}
        self.write_reg(UART_THR, ch);
    }

    fn get(&mut self) -> Option<u8> {
        if (self.read_reg(UART_LSR) & UART_LSR_DR) != 0 {
            Some(self.read_reg(UART_RBR))
        } else {
            None
        }
    }
}

static COM1: Mutex<Uart> = Mutex::new(Uart::new(UART_ADDR));

impl DebugConsole {
    /// Writes a byte to the console.
    #[inline]
    pub fn putchar(ch: u8) {
        COM1.lock().put(ch);
    }

    /// read a byte, return -1 if nothing exists.
    #[inline]
    pub fn getchar() -> Option<u8> {
        COM1.lock().get()
    }

    /// Print a number in hexadecimal format
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
}
