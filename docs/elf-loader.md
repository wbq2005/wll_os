# ELF 加载器

## 概述

wll_OS 实现了 ELF64 文件格式的解析和加载器，用于从 ELF 可执行文件创建用户进程。

## ELF 格式简介

ELF (Executable and Linkable Format) 是 Unix/Linux 系统的标准可执行文件格式。

### ELF 文件结构

```
┌─────────────────┐
│   ELF Header    │  文件头，包含基本信息
├─────────────────┤
│ Program Header  │  程序头表，描述加载信息
│     Table       │
├─────────────────┤
│     Section 1   │
│     Section 2   │  各节数据
│       ...       │
├─────────────────┤
│ Section Name    │
│ String Table    │
└─────────────────┘
```

### ELF Header

```rust
#[repr(C)]
pub struct ElfHeader {
    pub magic: [u8; 4],        // 魔数: 0x7f, 'E', 'L', 'F'
    pub class: u8,             // 位宽: 1=32位, 2=64位
    pub data: u8,              // 字节序: 1=小端, 2=大端
    pub version: u8,           // ELF 版本
    pub os_abi: u8,            // ABI 类型
    pub abi_version: u8,       // ABI 版本
    pub pad: [u8; 7],          // 填充
    pub e_type: u16,           // 文件类型: 2=可执行, 3=共享对象
    pub e_machine: u16,        // 目标架构: 243=RISC-V, 258=LoongArch
    pub e_version: u32,        // ELF 版本
    pub e_entry: usize,        // 程序入口地址
    pub e_phoff: usize,        // 程序头表偏移
    pub e_shoff: usize,        // 节头表偏移
    pub e_flags: u32,          // 标志
    pub e_ehsize: u16,         // ELF 头大小
    pub e_phentsize: u16,      // 程序头表项大小
    pub e_phnum: u16,          // 程序头表项数量
    pub e_shentsize: u16,      // 节头表项大小
    pub e_shnum: u16,          // 节头表项数量
    pub e_shstrndx: u16,       // 节名字符串表索引
}
```

### Program Header

```rust
#[repr(C)]
pub struct ProgramHeader {
    pub p_type: u32,           // 段类型
    pub p_flags: u32,          // 标志 (R/W/X)
    pub p_offset: usize,       // 段在文件中的偏移
    pub p_vaddr: usize,        // 段在内存中的虚拟地址
    pub p_paddr: usize,        // 段的物理地址（未使用）
    pub p_filesz: usize,       // 段在文件中的大小
    pub p_memsz: usize,        // 段在内存中的大小
    pub p_align: usize,        // 对齐方式
}
```

### 段类型

| 类型 | 值 | 说明 |
|------|-----|------|
| PT_NULL | 0 | 忽略 |
| PT_LOAD | 1 | 可加载段 |
| PT_DYNAMIC | 2 | 动态链接信息 |
| PT_INTERP | 3 | 解释器路径 |
| PT_NOTE | 4 | 辅助信息 |
| PT_SHLIB | 5 | 保留 |
| PT_PHDR | 6 | 程序头表 |

## 加载流程

### 1. 解析 ELF

```rust
let elf = ElfFile::parse(elf_data)?;
```

检查项：
- 魔数是否正确
- 是否为 64 位
- 是否为可执行文件或共享对象
- 目标架构是否匹配

### 2. 加载程序段

```rust
let (memory_set, user_stack_top, entry) = elf.load()?;
```

对于每个 `PT_LOAD` 段：

1. **计算虚拟地址范围**
   ```
   start_va = p_vaddr
   end_va = p_vaddr + p_memsz
   ```

2. **确定权限**
   ```rust
   let mut flags = PTEFlags::U | PTEFlags::V; // 用户可访问
   if p_flags & 1 != 0 { flags |= PTEFlags::X; } // 可执行
   if p_flags & 2 != 0 { flags |= PTEFlags::W; } // 可写
   if p_flags & 4 != 0 { flags |= PTEFlags::R; } // 可读
   ```

3. **分配物理页帧并建立映射**
   ```rust
   memory_set.insert_framed_area(start_va, end_va, flags);
   ```

4. **复制文件内容到内存**
   ```rust
   for (i, &byte) in src.iter().enumerate() {
       let vaddr = VirtAddr::new(ph.p_vaddr + i);
       if let Some(paddr) = memory_set.translate(vaddr) {
           unsafe { *(paddr.raw() as *mut u8) = byte; }
       }
   }
   ```

### 3. 分配用户栈

```rust
let user_stack_top = USER_STACK_TOP; // 0x8000_0000
let user_stack_bottom = user_stack_top - USER_STACK_SIZE; // 0x7F00_0000

memory_set.insert_framed_area(
    VirtAddr::new(user_stack_bottom),
    VirtAddr::new(user_stack_top),
    PTEFlags::U | PTEFlags::R | PTEFlags::W | PTEFlags::V,
);
```

### 4. 创建任务

```rust
let mut trap_frame = TrapFrame::new();
trap_frame[TrapFrameArgs::SP] = user_stack_top;
trap_frame[TrapFrameArgs::SEPC] = entry;
```

## BSS 段处理

BSS 段在文件中不占空间（`p_filesz < p_memsz`），但在内存中需要清零：

```rust
// 文件内容只复制 filesz 字节
let src = &data[p_offset..p_offset + p_filesz];

// p_filesz 到 p_memsz 之间的内存自动为 0
// 因为 insert_framed_area 分配的物理页帧由 buddy allocator 清零
```

## 架构检查

```rust
#[cfg(target_arch = "riscv64")]
if header.e_machine != 243 {
    return Err(SysErrNo::ENOEXEC);
}

#[cfg(target_arch = "loongarch64")]
if header.e_machine != 258 {
    return Err(SysErrNo::ENOEXEC);
}
```

## 位置无关代码 (PIC)

当前实现假设 ELF 文件使用绝对地址（非 PIC）。如需支持 PIC：

1. 在加载时进行重定位
2. 解析 `.rela` 重定位表
3. 根据实际加载地址修正引用

## 使用示例

```rust
// 添加用户程序到文件系统
fs::add_user_program("init", include_bytes!("../user/init"));

// 加载并执行
if let Some(elf_data) = fs::read_file("init") {
    let task = TaskControlBlock::new_user(&elf_data)?;
    manager::add_task(task);
}
```
