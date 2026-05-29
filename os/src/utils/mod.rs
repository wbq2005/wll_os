pub mod error;

/// 字符串处理工具
pub fn str_from_raw_ptr(ptr: *const u8) -> &'static str {
    let mut len = 0;
    unsafe {
        while *ptr.add(len) != 0 {
            len += 1;
        }
        core::str::from_utf8(core::slice::from_raw_parts(ptr, len)).unwrap_or("")
    }
}
