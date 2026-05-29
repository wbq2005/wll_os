# wll_OS - 操作系统大赛 2026

**队伍ID**: T2026105749910208  
**队伍名称**: wll_OS  
**学校**: 华南师范大学

---

## 项目简介

本项目是参加2026年全国大学生计算机系统能力大赛操作系统内核赛道（"内核实现"赛道）的参赛作品。

wll_OS 是一个基于 Rust 语言开发的操作系统内核，支持 RISC-V64 和 LoongArch64 两种架构。内核采用微内核设计风格，使用 polyhal 作为硬件抽象层，实现了基本的进程管理、内存管理、文件系统和系统调用。

## 设计思路

### 整体架构

wll_OS 采用分层架构设计：

- **硬件抽象层 (HAL)**: 使用 polyhal 库提供跨架构的硬件抽象
- **内核核心层**: 实现内存管理、进程调度、中断处理、系统调用等核心功能
- **系统调用层**: 提供 POSIX 风格的系统调用接口
- **文件系统层**: 基于内存的文件系统，支持基本的文件操作

### 关键设计决策

1. **Rust 语言**: 利用 Rust 的所有权系统和类型安全特性，减少内存安全问题
2. **多架构支持**: 通过 polyhal 抽象层，同时支持 RISC-V64 和 LoongArch64
3. **内存文件系统**: 初赛阶段使用内存文件系统，简化实现同时满足测试需求
4. **ELF 程序加载**: 实现 ELF64 解析器，支持用户程序的加载和执行

## 实现重点

### 1. 内存管理

- **页式内存管理**: 基于 4KB 页面的虚拟内存管理
- **物理页帧分配**: 使用伙伴系统分配器 (buddy_system_allocator)
- **地址空间管理**: 每个进程拥有独立的地址空间 (MemorySet)
- **内核/用户空间隔离**: 内核空间映射到高地址，用户空间使用低地址

### 2. 进程管理

- **任务控制块 (TCB)**: 管理进程状态、地址空间、文件描述符等
- **调度器**: 基于时间片的轮转调度
- **上下文切换**: 支持内核态和用户态的上下文切换
- **ELF 加载器**: 解析 ELF64 文件并加载到用户地址空间

### 3. 文件系统

- **内存文件系统 (MemFS)**: 基于内存的简单文件系统
- **文件描述符表**: POSIX 风格的文件描述符管理
- **支持的系统调用**:
  - `sys_openat` - 打开文件
  - `sys_read` - 读取文件
  - `sys_write` - 写入文件
  - `sys_close` - 关闭文件
  - `sys_lseek` - 设置文件偏移
  - `sys_dup/sys_dup3` - 复制文件描述符
  - `sys_fstat` - 获取文件状态

### 4. 系统调用

实现了以下系统调用：

| 系统调用 | 功能 | 状态 |
|---------|------|------|
| sys_exit | 进程退出 | 已实现 |
| sys_getpid | 获取进程ID | 已实现 |
| sys_sched_yield | 主动让出CPU | 已实现 |
| sys_execve | 执行新程序 | 已实现 |
| sys_openat | 打开文件 | 已实现 |
| sys_read | 读取数据 | 已实现 |
| sys_write | 写入数据 | 已实现 |
| sys_close | 关闭文件 | 已实现 |
| sys_lseek | 文件定位 | 已实现 |
| sys_dup | 复制fd | 已实现 |
| sys_dup3 | 复制fd到指定位置 | 已实现 |
| sys_fstat | 文件状态 | 已实现 |

## 开发过程中遇到的问题和解决方法

### 问题1: ELF 加载时的地址空间映射

**问题**: ELF 文件的段需要映射到用户地址空间，但不同段的虚拟地址可能不连续。

**解决**: 为每个 LOAD 段独立分配物理页帧并建立映射，使用 `insert_framed_area` 方法按需分配。

### 问题2: 用户栈的初始化

**问题**: 用户程序需要栈来执行，但栈的初始化和位置需要仔细设计。

**解决**: 在用户地址空间的高地址区域 (0x8000_0000) 分配用户栈，大小为 1MB，向下增长。

### 问题3: 文件描述符管理

**问题**: 需要管理进程打开的文件，支持标准IO和内存文件。

**解决**: 设计 `FileDescriptorTable` 结构，预分配标准输入(0)、标准输出(1)、标准错误(2)，支持动态分配和释放。

### 问题4: TrapFrame 的初始化

**问题**: 新创建的用户任务需要正确的 TrapFrame 才能从内核态正确返回到用户态。

**解决**: 在 `TaskControlBlock::new_user` 中初始化 TrapFrame，设置 SP（栈顶）、SEPC（程序入口）、RET（返回值）等寄存器。

## 非本队来源说明

### 第三方库

1. **polyhal / polyhal-boot / polyhal-trap**
   - 来源: https://github.com/[polyhal-repo]
   - 用途: 硬件抽象层，提供跨架构支持
   - 许可证: MIT/Apache-2.0

2. **buddy_system_allocator**
   - 来源: https://github.com/rcore-os/buddy_system_allocator
   - 用途: 物理页帧分配器
   - 许可证: MIT

3. **spin**
   - 来源: https://github.com/mvdnes/spin-rs
   - 用途: 自旋锁实现
   - 许可证: MIT

4. **lazy_static**
   - 来源: https://github.com/rust-lang-nursery/lazy-static.rs
   - 用途: 延迟初始化全局变量
   - 许可证: MIT/Apache-2.0

5. **bitflags**
   - 来源: https://github.com/bitflags/bitflags
   - 用途: 位标志宏
   - 许可证: MIT/Apache-2.0

6. **log**
   - 来源: https://github.com/rust-lang/log
   - 用途: 日志框架
   - 许可证: MIT/Apache-2.0

### 参考资源

- 《操作系统导论》(Operating Systems: Three Easy Pieces)
- rCore-Tutorial: https://github.com/rcore-os/rCore-Tutorial-v3
- Linux 内核文档
- **OSKernel2025-Nonix**（如 `newtestloongarch` 等分支）：README 与代码体现了 RISC-V VirtIO-MMIO 块设备、LoongArch PCI VirtIO 块设备，以及基于 `lwext4_rust` 的 ext4 集成路径。**本项目未拷贝其源码**，仅作架构与模块划分层面的对照；本项目 ext4 使用 `ext4_rs`，VFS 采用 MemFS 与运行时 ext4 **叠加**（见 [`vfs.rs`](os/src/fs/vfs.rs)）。

### 与 Nonix 的对照与创新（记录）

- **不照搬栈**：Nonix 中 `lwext4_rust` + 较重 inode 封装；本项目保持 **`ext4_rs` + [`ext4_vol.rs`](os/src/fs/ext4_vol.rs)** 与 **MemFS 预载 + 运行时 ext4 同名叠加**，评测/本地可分离「编译期镜像」与「运行时 virtio 盘」。
- **块设备与双架构**：可参考 Nonix 在 **`os/src/drivers`/`virtio_blk`** 上的 **RISC-V MMIO** 与 **LoongArch PCI** 分工；本项目若补齐 LoongArch 运行时根盘，宜单独实现 PCI 传输层而非复制其 fs 层。
- **fd 层**：Nonix 仓库中的 `os/src/fs/fstruct.rs`（保留完整 `OpenFlags`）与错误语义可作对照；本项目在完善 `O_CLOEXEC` / `O_NONBLOCK` / 访问模式时可对照其边界，但保持现有 `FileDescriptorTable` 结构演进。

## 编译和运行

### 编译

```bash
make all
```

说明：
- `make all` / `make build` 在发现 `sdcard-*.img` 缺失且存在对应 `sdcard-*.img.xz`、且系统有 `xz` 时，会**自动解压**生成 `.img`。
- 若仅有 `.xz` 但无 `xz` 命令，或 `.img` 与 `.xz` 均不存在：**仅打印警告，仍继续 `cargo build`**，以便评测机在仅有内核源码、由运行时 virtio 盘提供测试文件的环境中通过编译。
- 本地可执行 `make unpack-sdcard` 提前解压或排查环境。

这会生成两个内核文件：
- `kernel-rv` - RISC-V64 版本
- `kernel-la` - LoongArch64 版本

### 评测机与 sdcard 镜像

- 评测环境常为 **干净的 `git clone`**，不包含 `sdcard-rv.img` / `sdcard-la.img`（且仓库 `.gitignore` 通常忽略 `*.img`）。旧版 `Makefile` 曾在 `cargo build` 前强制要求镜像，导致 **Compile Error**；现已与 `os/build.rs` 对齐：缺镜像时 **MemFS 预载为空**，由 **VirtIO-blk 上的 ext4** 在运行时提供 `/init` 与用例文件。
- **注意**：若评测/本地 QEMU **未**附加含 ext4 的 virtio 磁盘，则编译可通过，但启动后仍可能找不到 `/init`（见下方 `[boot-error]` 行为）。

### 预加载镜像（可选，本地调试）

- `os/build.rs` 若读到仓库根目录的 `sdcard-rv.img` / `sdcard-la.img`，会把 `/init` 及从测试脚本解析出的相关 ELF 预载入 MemFS，便于本地无额外参数或快速复现。
- 若既无预载、运行时也未能挂载含 `/init` 的 ext4，`add_initproc` 可能在打印 `[boot-error]` 后 **关机**（可用 `--features no-init-idle` 改为空闲循环，便于调试）。

### Phase 2 验收要点（进程 / IPC）

- **已实现语义概要**：`fork`/`clone`（进程克隆，`clone` 对 `CLONE_VM`/`CLONE_FILES`/`CLONE_SIGHAND`/`CLONE_THREAD` 返回 `EINVAL`）、`getppid`、`wait4`（支持 `pid=-1`、`pid=0`、阻塞、`WNOHANG`）、`pipe2`（支持 `O_NONBLOCK`，`O_CLOEXEC` 保留）、调度与合作式阻塞路径。
- **建议自检**：在 `basic` 用例中覆盖 `fork / clone / pipe / yield / wait / waitpid / exit` 子集；`pipe2(..., O_NONBLOCK)` 下阻塞读应返回 `-EAGAIN` 给用户而非内核死循环。
- **已知边界**：pthread 级线程模型尚未接入（共用地址空间的 clone）。

### Phase 3 运行时根盘（ext4 + VirtIO，RISC-V）

- **启动链路**：`rust_main` 在 RISC-V 上解析 DTB 枚举 **VirtIO-MMIO blk**，挂载后用 [`fs/ext4_vol.rs`](os/src/fs/ext4_vol.rs) 打开 **ext4**；[`vfs.rs`](os/src/fs/vfs.rs) 将 MemFS 预载与 ext4 **叠加**（同名路径 MemFS 优先）。
- **`openat` 子集**：支持 `O_CREAT`/`O_TRUNC`/`O_APPEND`/`O_DIRECTORY`；可写路径可走 **ext4**（或退回 MemFS 仅内存新建的匿名文件）。
- **`mount` / `umount2`**：`/` + fstype `ext4` 在已成功 virtio 挂载时可视为确认；`umount2("/")` 会卸载运行时 ext4（回退 MemFS 读路径）。
- **评测脚本来源**：无 `/init` 时 harness 通过 [`task::list_files()`](os/src/task/mod.rs)（含 ext4 扫描）发现 `*_testcode.sh` / `run-all.sh`，与 Phase 3「运行时扫描」一致。
- **示例 QEMU（附加 virtio 磁盘）**：在原有命令上增加磁盘与 virtio-blk 设备，例如：

```bash
qemu-system-riscv64 -machine virt -kernel kernel-rv -m 128M -nographic -smp 1 \
  -bios default -no-reboot \
  -drive file=sdcard-rv.img,if=none,format=raw,id=hd0 \
  -device virtio-blk-device,drive=hd0
```

（镜像路径按本机实际仓库根目录调整。）

### 运行 RISC-V 版本

```bash
qemu-system-riscv64 -machine virt -kernel kernel-rv -m 128M -nographic -smp 1 -bios default -no-reboot
```

### 运行 LoongArch 版本

```bash
qemu-system-loongarch64 -kernel kernel-la -m 128M -nographic -smp 1 -no-reboot
```

## 项目结构

```
wll_os/
├── os/                     # 操作系统内核代码
│   ├── src/
│   │   ├── main.rs         # 内核入口
│   │   ├── config/         # 架构配置
│   │   ├── mm/             # 内存管理
│   │   │   ├── elf_loader.rs  # ELF 加载器
│   │   │   ├── memory_set.rs  # 地址空间管理
│   │   │   └── ...
│   │   ├── task/           # 进程管理
│   │   ├── syscall/        # 系统调用
│   │   ├── fs/             # 文件系统
│   │   └── trap/           # 中断处理
│   └── Cargo.toml
├── patch/                  # polyhal 本地补丁
├── docs/                   # 设计文档
├── Makefile               # 编译脚本
└── Dockerfile             # 编译环境
```

## 开发记录

| 日期 | 内容 |
|------|------|
| 2026-05-04 | 实现用户程序加载和文件系统 syscalls |
| 2026-05-05 | `check-sdcard` 改为缺镜像仅警告：修复评测机无 sdcard 时的 Compile Error；启动错误提示与文档同步 |

## 许可证

本项目代码采用 MIT 许可证。
