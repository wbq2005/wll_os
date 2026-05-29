# 文件系统

## 概述

wll_OS 实现了一个简单的内存文件系统 (MemFS)，用于初赛阶段的用户程序加载和基本文件操作。

## 架构

```
┌─────────────────────────────────────┐
│         系统调用层 (syscall/fs.rs)    │
│    sys_openat, sys_read, sys_write   │
├─────────────────────────────────────┤
│      文件描述符层 (fs/fd.rs)          │
│   FileDescriptorTable, FileDescriptor │
├─────────────────────────────────────┤
│      文件系统层 (fs/mod.rs)           │
│       MemFS, MemFile                  │
└─────────────────────────────────────┘
```

## 内存文件系统 (MemFS)

### 数据结构

```rust
pub struct MemFileSystem {
    files: Vec<MemFile>,
}

pub struct MemFile {
    pub name: String,
    pub content: Vec<u8>,
}
```

### 操作

- `add_file(name, content)`: 添加文件
- `get_file(name)`: 获取文件
- `exists(name)`: 检查文件是否存在
- `read_file(name)`: 读取文件内容

### 使用方式

在内核初始化时添加用户程序：

```rust
fs::add_user_program("init", include_bytes!("../user/init"));
```

## 文件描述符管理

### 文件描述符表

```rust
pub struct FileDescriptorTable {
    fds: Vec<Option<FileDescriptor>>,
}
```

- 最大文件描述符数量: 1024 (MAX_FD_NUM)
- 预分配标准 IO: 0=stdin, 1=stdout, 2=stderr

### 文件描述符类型

```rust
pub enum FileDescriptor {
    Stdin,
    Stdout,
    Stderr,
    MemFile {
        name: String,
        content: Vec<u8>,
        offset: usize,  // 当前读写位置
    },
}
```

### 操作

| 方法 | 说明 |
|------|------|
| `alloc(fd)` | 分配新的文件描述符 |
| `get(fd)` | 获取文件描述符引用 |
| `free(fd)` | 释放文件描述符 |
| `dup(old_fd)` | 复制文件描述符 |
| `dup2(old_fd, new_fd)` | 复制到指定位置 |

## 支持的系统调用

### sys_openat

```rust
pub fn sys_openat(dirfd: isize, pathname: *const u8, flags: u32, mode: u32) -> SyscallRet
```

从内存文件系统打开文件，返回文件描述符。

### sys_read

```rust
pub fn sys_read(fd: usize, buf: *mut u8, count: usize) -> SyscallRet
```

- stdin: 从串口读取字符
- 文件: 从当前 offset 读取，更新 offset

### sys_write

```rust
pub fn sys_write(fd: usize, buf: *const u8, count: usize) -> SyscallRet
```

- stdout/stderr: 输出到串口控制台

### sys_close

```rust
pub fn sys_close(fd: usize) -> SyscallRet
```

释放文件描述符。

### sys_lseek

```rust
pub fn sys_lseek(fd: usize, offset: isize, whence: usize) -> SyscallRet
```

- `whence=0` (SEEK_SET): 从文件开头
- `whence=1` (SEEK_CUR): 从当前位置
- `whence=2` (SEEK_END): 从文件末尾

### sys_dup / sys_dup3

```rust
pub fn sys_dup(old_fd: usize) -> SyscallRet
pub fn sys_dup3(old_fd: usize, new_fd: usize, flags: usize) -> SyscallRet
```

复制文件描述符。

### sys_fstat

```rust
pub fn sys_fstat(fd: usize, statbuf: *mut u8) -> SyscallRet
```

获取文件状态（当前为简化实现）。

## 未来扩展

1. **磁盘文件系统**: 支持 EXT4 等真实文件系统
2. **VFS 层**: 虚拟文件系统，支持多种文件系统类型
3. **缓存机制**: 页缓存，提高文件访问性能
4. **文件权限**: 实现完整的权限检查
