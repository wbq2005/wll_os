use crate::println;
use log::{self, Level, LevelFilter, Log, Metadata, Record};

/// 内核日志实现
struct SimpleLogger;

impl Log for SimpleLogger {
    fn enabled(&self, _metadata: &Metadata) -> bool {
        true
    }

    fn log(&self, record: &Record) {
        if !self.enabled(record.metadata()) {
            return;
        }

        let color = match record.level() {
            Level::Error => 31, // Red
            Level::Warn => 33,  // Yellow
            Level::Info => 32,  // Green
            Level::Debug => 34, // Blue
            Level::Trace => 35, // Magenta
        };

        println!(
            "\x1b[{}m[{:>5}] {}\x1b[0m",
            color,
            record.level(),
            record.args()
        );
    }

    fn flush(&self) {}
}

/// 初始化日志系统
pub fn init(level: Option<&str>) {
    let filter = match level {
        Some("error") => LevelFilter::Error,
        Some("warn") => LevelFilter::Warn,
        Some("info") => LevelFilter::Info,
        Some("debug") => LevelFilter::Debug,
        Some("trace") => LevelFilter::Trace,
        _ => LevelFilter::Off,
    };

    log::set_logger(&SimpleLogger).unwrap();
    log::set_max_level(filter);
}
