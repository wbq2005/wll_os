/// RISC-V 架构配置
pub const KERNEL_BASE: usize = 0x8020_0000;
pub const PAGE_SIZE: usize = 4096;
pub const PAGE_SIZE_BITS: usize = 12;

// SV39: 内核虚拟地址从 0xFFFF_FFFF_8000_0000 开始（高 2GB）
// 物理地址 0x8020_0000 → 虚拟地址 0xFFFF_FFFF_8020_0000
pub const VIRT_ADDR_START: usize = 0xffff_ffff_8000_0000;

// SV39 地址空间边界
// 用户空间: 0x0000_0000 ~ 0x0000_7FFF_FFFF_FFFF (256GB)
// 内核空间: 0xFFFF_FFFF_8000_0000 ~ 0xFFFF_FFFF_FFFF_FFFF (2GB)
pub const USER_VADDR_START: usize = 0;
pub const USER_VADDR_END: usize = 0x0000_7fff_ffff_ffff;
pub const KERNEL_VADDR_START: usize = 0xffff_ffff_8000_0000;
pub const KERNEL_VADDR_END: usize = 0xffff_ffff_ffffffff;

// 用户空间配置
/// 用户程序起始地址
pub const USER_START_ADDR: usize = 0x1000;
/// 用户栈大小 (默认 1MB)
pub const USER_STACK_SIZE: usize = 0x10_0000;
/// 用户栈顶地址（初始 SP，栈从高地址向下生长）
/// 使用用户空间高地址区域
pub const USER_STACK_TOP: usize = 0x7fff_f000;
/// 用户堆起始地址
pub const USER_HEAP_START: usize = 0x1000_0000;
