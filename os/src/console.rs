use core::fmt::{self, Write};
use polyhal::debug_console::DebugConsole;

/// 串口输出结构体
struct Stdout;

impl Write for Stdout {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for c in s.bytes() {
            DebugConsole::putchar(c);
        }
        Ok(())
    }
}

/// 向串口输出一个字符
pub fn putchar(c: u8) {
    DebugConsole::putchar(c);
}

/// 打印格式化字符串到串口
#[macro_export]
macro_rules! print {
    ($($arg:tt)*) => ({
        $crate::console::_print(format_args!($($arg)*));
    });
}

/// 打印格式化字符串到串口并换行
#[macro_export]
macro_rules! println {
    () => ($crate::print!("\n"));
    ($($arg:tt)*) => ({
        $crate::print!("{}\n", format_args!($($arg)*));
    });
}

/// 从串口读取一个字符
///
/// 返回 Some(c) 如果读取到字符，None 如果没有数据可读
pub fn getchar() -> Option<u8> {
    DebugConsole::getchar()
}

/// 内部打印函数
pub fn _print(args: fmt::Arguments) {
    let mut stdout = Stdout;
    stdout.write_fmt(args).unwrap();
}
