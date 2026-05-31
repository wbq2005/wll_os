/// LoongArch 架构配置
pub const KERNEL_BASE: usize = 0x9000_0000_9000_0000;
pub const PAGE_SIZE: usize = 4096;
pub const PAGE_SIZE_BITS: usize = 12;
pub const VIRT_ADDR_START: usize = 0x9000_0000_0000_0000;

// 用户空间配置
/// 用户程序起始地址
pub const USER_START_ADDR: usize = 0x1000;
/// 用户栈顶部地址 (低 128MB 区域)
pub const USER_STACK_TOP: usize = 0x8000_0000;
/// 用户栈大小 (64KB)
pub const USER_STACK_SIZE: usize = 0x1_0000;
/// 用户堆起始地址
pub const USER_HEAP_START: usize = 0x1000_0000;
