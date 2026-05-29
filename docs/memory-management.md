# 内存管理

## 概述

wll_OS 采用页式内存管理，支持虚拟内存。每个进程拥有独立的地址空间，内核空间在所有进程间共享。

## 核心组件

### 1. 物理页帧分配器 (frame_allocator.rs)

使用 `buddy_system_allocator` 库实现伙伴系统分配器。

```rust
pub struct PhysPageNum(pub usize);

pub struct FrameTracker {
    pub ppn: PhysPageNum,
}
```

- `alloc_frame()`: 分配一个物理页帧
- `dealloc_frame(ppn)`: 释放物理页帧
- `FrameTracker`: RAII 自动释放页帧

### 2. 页表 (page_table.rs)

封装 polyhal 的 `PageTableWrapper`，提供页表操作接口。

```rust
bitflags! {
    pub struct PTEFlags: u16 {
        const V = 1 << 0;   // Valid
        const R = 1 << 1;   // Readable
        const W = 1 << 2;   // Writable
        const X = 1 << 3;   // Executable
        const U = 1 << 4;   // User accessible
        const G = 1 << 5;   // Global
        const A = 1 << 6;   // Accessed
        const D = 1 << 7;   // Dirty
    }
}
```

### 3. 地址空间 (memory_set.rs)

```rust
pub struct MemorySet {
    pub page_table: PageTableWrapper,
    pub areas: Vec<MapArea>,
}
```

**关键方法**:

- `new_bare()`: 创建空地址空间
- `insert_framed_area(start_va, end_va, permission)`: 插入映射区域
- `from_kernel(kernel_ms)`: 从内核页表复制（用于用户进程）
- `activate()`: 激活此地址空间
- `translate(vaddr)`: 虚拟地址到物理地址转换

### 4. 虚拟内存区域 (map_area.rs)

```rust
pub struct MapArea {
    pub start_va: VirtAddr,
    pub end_va: VirtAddr,
    pub flags: PTEFlags,
}
```

## 内存分配流程

### 用户栈分配

```
1. 确定栈顶地址 (USER_STACK_TOP = 0x8000_0000)
2. 计算栈底地址 (USER_STACK_TOP - USER_STACK_SIZE)
3. 调用 insert_framed_area() 分配物理页帧
4. 建立虚拟地址到物理地址的映射
5. 设置权限: U | R | W | V
```

### ELF 段加载

```
1. 解析 ELF 程序头 (Program Header)
2. 对于每个 PT_LOAD 段:
   a. 计算虚拟地址范围 (p_vaddr ~ p_vaddr + p_memsz)
   b. 确定权限 (R/W/X/U)
   c. 调用 insert_framed_area() 分配物理页帧
   d. 复制文件内容到物理内存
3. 分配用户栈
4. 返回 (MemorySet, user_stack_top, entry_point)
```

## 地址转换

### 虚拟地址 → 物理地址

```rust
pub fn translate(&self, vaddr: VirtAddr) -> Option<PhysAddr> {
    self.page_table.translate(vaddr).map(|(paddr, _)| paddr)
}
```

### 页表结构 (RISC-V SV39)

```
VPN[2] (9 bits) → 页目录 (L2)
VPN[1] (9 bits) → 页目录 (L1)
VPN[0] (9 bits) → 页表项 (L0)
Offset (12 bits) → 页内偏移
```

## 内存安全

1. **Rust 所有权系统**: 防止 use-after-free 和 double-free
2. **FrameTracker**: RAII 自动释放物理页帧
3. **地址检查**: 系统调用中检查用户提供的地址是否合法
4. **权限检查**: 页表项中的 U 位控制用户态访问权限

## 配置常量

### RISC-V

```rust
pub const KERNEL_BASE: usize = 0x8020_0000;
pub const PAGE_SIZE: usize = 4096;
pub const VIRT_ADDR_START: usize = 0xffff_ffff_0000_0000;
pub const USER_STACK_TOP: usize = 0x8000_0000;
pub const USER_STACK_SIZE: usize = 0x10_0000; // 1MB
```

### LoongArch

```rust
pub const KERNEL_BASE: usize = 0x9000_0000_9000_0000;
pub const PAGE_SIZE: usize = 4096;
pub const VIRT_ADDR_START: usize = 0x9000_0000_0000_0000;
pub const USER_STACK_TOP: usize = 0x8000_0000;
pub const USER_STACK_SIZE: usize = 0x10_0000; // 1MB
```
