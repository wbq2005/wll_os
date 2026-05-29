# 开发日志

## 2026-05-04

### 实现用户程序加载和文件系统 syscalls

#### 已完成工作

1. **ELF 加载器** (`os/src/mm/elf_loader.rs`)
   - ELF64 文件格式解析
   - 程序段加载到虚拟内存
   - 用户栈分配
   - 架构检查（RISC-V: 243, LoongArch: 258）

2. **内存文件系统** (`os/src/fs/`)
   - 简单内存文件系统 (MemFS)
   - 文件描述符表管理
   - 支持标准 IO (stdin/stdout/stderr)

3. **系统调用完善** (`os/src/syscall/`)
   - `sys_openat` - 打开文件
   - `sys_read` - 读取数据
   - `sys_write` - 写入数据
   - `sys_close` - 关闭文件
   - `sys_lseek` - 文件定位
   - `sys_dup` / `sys_dup3` - 复制文件描述符
   - `sys_fstat` - 获取文件状态
   - `sys_execve` - 执行新程序

4. **进程管理** (`os/src/task/`)
   - `TaskControlBlock::new_user()` - 从 ELF 创建用户任务
   - 添加 `fd_table` 到任务控制块
   - Init 进程加载支持

5. **配置更新**
   - 添加用户空间常量 (USER_STACK_TOP, USER_STACK_SIZE)

#### 技术难点

1. **TrapFrame 初始化**: 需要正确设置 SP、SEPC 等寄存器
2. **文件描述符管理**: 设计合适的 FDT 结构，支持动态分配
3. **ELF 段映射**: 处理不同段的虚拟地址和权限

#### 代码统计

- 新增文件：5 个
- 修改文件：10+ 个
- 代码行数：约 1000 行

---

## 开发计划

### 近期（本周）

- [ ] 实现磁盘文件系统（EXT4 或简单块设备驱动）
- [ ] 支持从磁盘加载测试程序
- [ ] 实现 sys_clone（进程/线程创建）
- [ ] 实现 sys_wait4（等待子进程）

### 中期（未来两周）

- [ ] 完善内存管理（sys_brk, sys_mmap）
- [ ] 实现信号处理（sys_sigaction, sys_kill）
- [ ] 实现时间相关系统调用（sys_nanosleep, sys_gettimeofday）
- [ ] 实现管道（sys_pipe2）

### 长期（初赛截止前）

- [ ] 通过网络 socket 支持
- [ ] 支持多核 SMP
- [ ] 性能优化
- [ ] 完整通过测试用例

---

## 遇到的问题记录

| 日期 | 问题 | 解决方法 |
|------|------|----------|
| 2026-05-04 | TrapFrame::default() 不存在 | 使用 TrapFrame::new() |
| 2026-05-04 | 编译错误：未找到 SysErrNo | 添加 use crate::utils::error::SysErrNo |
| 2026-05-04 | Git 推送失败 | 使用 SSH 方式配置 remote |

---

## 参考资料

- [rCore-Tutorial](https://github.com/rcore-os/rCore-Tutorial-v3)
- [Linux 内核文档](https://www.kernel.org/doc/html/latest/)
- [RISC-V 特权指令集手册](https://riscv.org/technical/specifications/)
- [LoongArch 参考手册](https://loongson.github.io/LoongArch-Documentation/)
