# 2026 决赛任务驱动内核路线：BuildStorm、性能与真机

## 总目标与不可逾越的边界

总目标是在 RISC-V64 与 LoongArch64 上，以通用 Linux ABI 和真实硬件抽象为基础，最大化 CAgent 与 BuildStorm 得分，并让同一套内核逐步运行在 VisionFive 2（JH7110）和龙芯 2K1000LA 开发板上。

所有阶段遵守以下规则：

1. 不根据测试名称、命令行、文件名或预期输出在 syscall/VFS/调度器中返回特定结果。
2. 不伪造命令输出、构建产物、CPU 数量、系统时间、`/proc/uptime` 或编译耗时。
3. 性能数据来自 guest 内真实执行；保留原始串口日志、构建命令、镜像版本和 QEMU 参数。
4. 修复必须能用标准 ABI 解释，并至少由一个非目标程序或独立微测试交叉验证。
5. 测试 harness 只负责发现、启动和记录任务，不参与被测功能计算，也不修改结果。
6. QEMU 与真机共享上层内核实现；平台差异只允许存在于 DT/ACPI 解析、中断控制器、时钟、串口、块设备、网卡和启动协议等平台层。

## 已完成门槛：CAgent 双架构能力闭环

当前 public-equivalent 10 项任务已经通过前台用户任务 harness 在两个架构真实执行：

- RISC-V64：10/10。
- LoongArch64：10/10。
- 覆盖 shell 算术、BusyBox/Lua 动态程序、`fork/exec/wait`、时间、`uname`、`/proc`、TCP 状态、目录与文件读写、`statfs` 和递归目录搜索。
- LoongArch 首用户切换、TrapFrame、页表与 UART 接收已单独验证；此前停滞来自交互 shell 启动方式，不是用户态切换失败。

这只是能力门槛，不等同于官方 CAgent 满分。下一次 CAgent 工作应直接运行官方 glibc 脚本并记录每项耗时，确认时间奖励，而不是继续扩充等价脚本。

## 下一主阶段：BuildStorm 从 0 到可编译

### 阶段 A：建立不可伪造的基线与故障采集

目标是先获得 `rustc --version`、`cargo --version` 和最小 `cargo new && cargo build && run` 的真实结果。建立 RISC-V64、LoongArch64 两套固定配置：`-smp 8 -m 8G`、固定磁盘镜像、release 内核、关闭全局 debug。runner 只采集串口、退出状态、guest `/proc/uptime` 起止值、峰值内存和失败前最后若干 syscall。

实现一个可编译关闭的通用 syscall 统计设施：按 syscall 编号统计调用量、失败 errno、累计时间和最大延迟；不得识别 rustc/cargo。为缺失 syscall 输出限频诊断，记录 PID、ABI 参数和返回值。第一轮按“动态加载失败、工具链启动失败、最小项目失败、主构建失败”四层保存检查点，避免每次都重跑完整构建。

验收：两个架构均能稳定复现相同阶段的结果；计时与 `/proc/uptime` 单调一致；关闭诊断后行为不变。

### 阶段 B：工具链启动与动态链接 ABI

优先让官方镜像中的 glibc `rustc` 和 `cargo` 原样运行。按真实错误补齐通用能力，预计重点包括：

- ELF `PT_INTERP`、TLS、auxv、随机数、VDSO 缺失时的合法回退；
- `clone/clone3`、`set_tid_address`、`set_robust_list`、futex、信号掩码和线程退出；
- `openat/openat2`、`statx`、`readlinkat`、`getdents64`、`fcntl`、`ioctl` 的标准语义；
- `mmap/mprotect/munmap/mremap/brk` 的页对齐、权限、共享/私有映射和文件尾页行为；
- `poll/ppoll/epoll`、pipe、eventfd 以及非阻塞 I/O；
- `/proc/self/*`、`/proc/cpuinfo`、`/proc/meminfo`、`/proc/uptime`、`/proc/self/exe` 的动态且真实内容。

每项修复配独立 ABI 回归，例如 futex 用多线程计数器验证，mmap 用随机映射/保护组合验证，文件语义用并发 rename/unlink/open 验证。禁止以 rustc 路径触发兼容分支。

验收：两架构打印真实工具链版本；无未知 syscall 风暴；反复运行 20 次无泄漏、死锁或随机崩溃。

### 阶段 C：最小 Rust 构建全链路

运行官方 `cargo new` 最小项目，依次通过依赖解析、rustc 前端、LLVM/codegen、链接器和产物执行。故障按资源类型处理：

- 进程：正确的父子关系、wait 状态、进程组、信号和 `execve` 后状态清理；
- 文件系统：硬链接、符号链接、rename 原子性、unlink 后存活 inode、文件锁、临时文件和目录缓存一致性；
- 内存：按需分配、COW、文件映射回写、并发缺页、OOM 返回与回收；
- 时间：单调时钟、实时钟、sleep/futex timeout，不修改 uptime 计算来获取分数；
- 多线程：真正让 8 个 vCPU 上的可运行线程并行，而非仅报告 8 核。

验收：清空目标目录后可重复构建，产物真实存在且可执行输出 `Hello, world!`；两个架构各连续通过 10 次。

### 阶段 D：arceos-helloworld 首次完整构建

从官方离线缓存和源码执行未修改的 `cargo xtask arceos build`。先以“正确完成”为唯一目标，不做未经数据支持的优化。采用二分检查点定位数百 crate 中的首个失败，记录失败进程树、errno、内存映射和磁盘状态。重点验证大目录遍历、并发 rustc、静态库归档、链接大文件、临时文件 rename、子进程管道与高并发 futex。

验收：官方命令退出 0；目标产物真实存在且大小达到规则要求；重启后从干净构建目录再次成功；RISC-V64 和 LoongArch64 都通过。只有达到这个门槛，BuildStorm 编译成功分才算具备。

## 性能阶段：以 Linux 基线为标尺优化

### 阶段 E：测量与火焰图式归因

建立低扰动计数：上下文切换次数、用户/内核时间、主缺页/次缺页、COW 页数、页分配失败、块读写字节和请求数、VFS cache 命中率、futex 睡眠/唤醒、锁竞争、各 syscall 总耗时。分别测冷缓存与热缓存，至少 5 次取中位数，并与同机同镜像 Linux 基线对比。

优化优先级由占比决定，不凭直觉。预期候选包括：

1. 去除前台 harness 的单任务串行假设，使普通调度器真正承载并行编译。
2. 改进就绪队列和每 CPU 调度，降低全局锁与无效轮询。
3. 页缓存、目录项/inode 缓存和 readahead，合并小块 I/O，减少 ext4 全局锁。
4. COW fork、批量页表操作和 TLB shootdown，减少 rustc 子进程启动成本。
5. futex 哈希等待队列与精确唤醒，避免广播惊群。
6. pipe 环形缓冲与批量 copy，减少 cargo/rustc/linker 管道开销。
7. 用户缓冲区复制的分段校验与批量路径，降低大量小 syscall 成本。

每次优化只合入有 A/B 数据支持的变化，并回归 CAgent、minibuild、LTP 相关子集及双架构 release 构建。性能提升不能以放松同步、跳过落盘语义或伪造计时换取。

验收：完整构建稳定成功；给出修改前后中位数、波动范围、加速比及瓶颈计数变化；逐步逼近 Linux 基线 B，优先保证低于 2B，再争取接近或低于 B。

## 真机并行路线

真机适配不等 BuildStorm 完成后才开始。每个通用子系统变化都要保持平台层边界，并建立以下门槛。

### VisionFive 2 / JH7110

使用真实 JH7110 平台信息和设备树，不采用 FU740 假设。依次完成 OpenSBI/U-Boot 启动、DTB 内存与保留区解析、PLIC/CLINT 或实际中断与时钟接口、串口、SD/eMMC 或 NVMe 根文件系统、PCIe/USB 所需枚举、板载网卡、SMP 四核启动、缓存一致性和真实 RTC/网络校时。HDMI 不进入首轮拿分关键路径，待存储、网络和 SMP 稳定后处理。

### 龙芯 2K1000LA 星云板

隔离 QEMU virt 与 `board=2k1000` 平台实现，完成固件启动参数、设备树/固件表、串口、中断控制器、稳定时钟源、SMP、PCIe、SATA/块设备或板载启动介质、双千兆网卡和 USB。重点验证 LoongArch 缓存维护、I/O 映射属性、跨核中断和页表一致性，避免 QEMU 强内存模型掩盖问题。

### 真机共同验收阶梯

1. 串口启动 100 次，内存探测和时钟稳定。
2. 多核压力与跨核唤醒，运行 24 小时无死锁。
3. 根文件系统反复写入、断电恢复测试和大文件校验。
4. 网络 DHCP、TCP 长连接、并发传输和校验。
5. CAgent 原始 glibc 测试全过。
6. 工具链与 minibuild 通过；资源允许时运行 BuildStorm。

## 文档、审计与最终交付

从第一轮 BuildStorm 起维护实验台账：提交号、架构、QEMU/真机参数、镜像哈希、测试命令、原始日志、成功率、耗时和结论。最终优化文档按规则覆盖根因、设计实现、前后数据、AI 使用说明和完整复现步骤。

最终提交前进行反作弊审计：搜索测试名、预期输出、固定时间值和特定命令分支；检查 `/proc/uptime` 与时钟实现；确认镜像测试脚本未被提交内核修改；用官方原始镜像重新跑双架构。任何无法用通用 ABI 或平台规范解释的改动不得进入提交版本。

## 下一次立即执行的任务包

1. 把官方 BuildStorm 镜像与脚本接入只读 runner，固定 `-smp 8 -m 8G` 和日志格式。
2. 双架构运行工具链检查，收集首个真实失败与缺失 syscall 统计。
3. 以首个阻塞点为单位修复，附独立 ABI 回归；直到 `TOOLCHAIN_RESULT status=OK`。
4. 推进 minibuild，完成后建立连续运行稳定性门槛。
5. 启动完整构建，先拿成功门槛，再进入性能剖析。
6. 同步建立 JH7110 与 2K1000LA 的平台 capability matrix，确保每个新子系统都有 QEMU 和真机实现位置。
