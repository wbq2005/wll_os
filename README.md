# wll_OS - 操作系统大赛 2026

| 项目 | 内容 |
| --- | --- |
| 队伍 ID | T2026105749910208 |
| 队伍名称 | 12345 |
| 学校 | 华南师范大学 |

## 项目简介

wll_OS 是一个面向全国大学生计算机系统能力大赛操作系统内核赛道的教学与竞赛型 Rust 内核项目。项目目标是在受限时间内构建一个结构清晰、可持续演进、能够真实运行评测程序的类 Unix 内核，并尽量让每一项功能都可以通过 QEMU 串口日志、judge parser 输出和回归脚本进行验证。

当前内核支持 RISC-V64 与 LoongArch64 两种架构，围绕进程管理、虚拟内存、文件系统、系统调用、信号、同步原语和评测 harness 等核心模块展开实现。项目不追求用脚本包装“看起来通过”的结果，而是将 libc、BusyBox、benchmark 与 bounded LTP slice 作为主要驱动力，逐步补齐真实用户态程序所依赖的内核语义。

在大赛场景中，wll_OS 的重点包括：

- **可复现**：固定 Rust 工具链和构建入口，保留每次评测的 kernel/sdcard hash、串口日志与 judge summary。
- **可移植**：通过 polyhal 抽象架构差异，在同一套内核主体上支持 RISC-V64 与 LoongArch64。
- **可验证**：以真实 QEMU 运行结果和 parser 输出作为功能、性能与回归判断依据。
- **可扩展**：按子系统拆分 syscall 与内核模块，便于继续扩展 LTP case、文件系统语义和调度能力。

当前实现围绕以下能力展开：

- 双架构内核构建：`riscv64gc-unknown-none-elf` 与 `loongarch64-unknown-none`。
- 运行时根文件系统：MemFS 预载与 VirtIO-blk 上的 ext4 叠加，运行时可从 ext4 发现并执行测试脚本。
- 进程、线程组与调度：`clone`、`execve`、`exit`、`exit_group`、`wait4`、`sched_yield` 等提供面向 libc/BusyBox/benchmark 的真实子集。
- FD/VFS 与 IPC：常用文件、目录、pipe、poll/select、pseudo device、部分 socket loopback 与 ext4 写入路径。
- 内存与同步：`brk`、匿名/文件 `mmap`、`munmap`、`mprotect`、SysV shm、futex、robust list 的可用子集。
- 信号与身份：`rt_sig*`、`kill/tkill/tgkill`、UID/GID、supplementary groups、部分权限检查。
- 评测路径：`basic`、`busybox`、`lua`、`libc-test`、`iozone`、`libcbench`、`lmbench`，以及 `all` profile 中的 bounded LTP slice。

更细的 syscall 状态、剩余风险和 suite 边界见 [docs/syscall-matrix.md](docs/syscall-matrix.md)。

## BuildStorm 决赛状态

2026-08-15，当前 production 内核在远端 `47.110.253.40` 使用 QEMU 11.0.3、官方 `final-2026` glibc 镜像、`-snapshot -m 8G -smp 8` 配置下完成双架构 clean build，证据等级为 `official-pass`。最快 Stage B 基线与本轮稳定性候选分开记录：

| 架构 | 官方成功标记 | guest 编译时间 | 官方 judge 自动项 |
| --- | --- | ---: | ---: |
| RISC-V64 Stage B 基线 | `BUILDSTORM_COMPILE mode=multi ok=true` | 800.55 s | 180/180 |
| LoongArch64 Stage B 基线 | `BUILDSTORM_COMPILE mode=multi ok=true` | 660.71 s | 180/180 |
| RISC-V64 VFS 稳定性候选 | `BUILDSTORM_COMPILE mode=multi ok=true` | 860.15--867.16 s | 180/180 |
| LoongArch64 VFS 稳定性候选 | `BUILDSTORM_COMPILE mode=multi ok=true` | 674.49--687.23 s | 180/180 |

自动项包括 toolchain 8 分、minibuild 12 分、完整编译 40 分和本次 judge 基线下的时间分 120 分。内核设计优化文档 20 分由人工评审，不在上述自动结果中自行计分。正式评测机会重新测量同机 Linux 基线，因此最终时间分以评测机输出为准。

评测和设计入口：

- [BuildStorm 2.3 中文累计设计与优化记录](docs/buildstorm-2.3-design-optimization-cn.md)
- [当前内核架构总设计](docs/kernel-architecture-overview-cn.md)
- [历史累计设计记录](docs/buildstorm-kernel-design-optimization.md)
- [双架构 official-pass 证据结论](docs/evidence/buildstorm-stage2/20260814-stage2-buildstorm-official-completion.md)
- [Stage B per-CPU heap cache 结论与证据索引](docs/evidence/buildstorm-stage2/20260815-stageb-percpu-heap-cache-conclusion-cn.md)
- [VFS 稀疏页缓存与 clustered writeback 结论](docs/buildstorm-2.3-design-optimization-cn.md#11-2026-08-15-评测超时复核与-vfs-稳定性候选)

本轮候选的目标是消除评测机在 `ax-mm` 边界出现的超长尾，而不是把较快的
Stage B 基线误写成已被本轮改动提升。大于 8 MiB 的 regular file 不再整文件
驻留在连续 `Vec` 中，dirty data 按 256 KiB cluster 回写，连续 ext4 块批量提交；
双架构 sparse/truncate/fsync/rename/open-unlink 回归均通过。工作树中额外的
`os/src/fs/vfs.rs` parent/readlink cache 改动没有包含在上述官方成功 diff，
仍为 `unverified`，不得与稳定性候选混合归因。

完整运行命令如下，两个架构必须顺序执行，不能并发 QEMU：

```bash
python3 scripts/run_buildstorm.py --arch riscv64 \
  --image /srv/buildstorm/images/sdcard-rv-pub.img \
  --stage complete --timeout 15000 --memory 8G --smp 8

python3 scripts/run_buildstorm.py --arch loongarch64 \
  --image /srv/buildstorm/images/sdcard-la-pub.img \
  --stage complete --timeout 15000 --memory 8G --smp 8
```

项目未修改官方镜像、suite、guest script、judge、marker 或 guest `/proc/uptime`，production 行为也不按 crate、测试路径、命令或输出分支。诊断聚合仅在 `buildstorm-diagnostics` feature 下启用，默认 production 关闭。

## 当前评测边界

### 默认构建特性

根目录 `Makefile` 默认使用仓库固定工具链，并默认打开 iozone 与 lmbench：

| 变量 | 默认值 | 含义 |
| --- | --- | --- |
| `ARCH` | `riscv64` | `riscv64` 或 `loongarch64` |
| `IOZONE` | `1` | 打开 iozone/libcbench/lmbench 相关默认尾部路径 |
| `LMBENCH` | `1` | 打开 lmbench feature |
| `LTP` | `0` | 普通 `make build/check` 不默认打开 LTP |
| `LIBCTEST` | `0` | 完整 libc-test 收集器为 opt-in |
| `DEV_PRELOAD` | `0` | 默认不把测试脚本预载进 kernel |

`scripts/perf_baseline_runner.py --suite all` 会打开 `LTP=1`，在 iozone/libcbench/lmbench 后追加当前 bounded LTP slice。`--suite ltp` 仍可作为只跑 LTP slice 的聚焦诊断入口。

### Bounded LTP slice

当前 LTP 不是完整 suite 启用，而是一个明确收口的 case 列表：

```text
writev01,setegid02,getgroups01,setgroups01,setgroups02,setgroups03,setgroups04,access01,open02,setfsuid01,setfsgid01,faccessat01,access02,open03,symlink01,readlink01,lstat01,lstat02,symlink02,symlink03,symlink04,symlinkat01,readlinkat01,setitimer02,setitimer01,getitimer01,getitimer02
```

该 slice 覆盖真实 `writev`、UID/GID 与 supplementary groups、fsuid/fsgid 状态、`access(2)` 权限检查、`open(O_NOATIME)` 的所有者/权限路径、基础 symlink/readlink/lstat 路径元数据语义、`readlink(2)` 与 `lstat(2)` 的父路径搜索权限和前缀 symlink 解析语义、symlink 创建错误路径、`symlinkat(2)` 的目录 fd 语义、`readlinkat(2)` 的 `O_PATH|O_NOFOLLOW` 空路径 symlink 读取语义，以及 `setitimer(2)` / `getitimer(2)` 的基础错误路径、旧值写回、当前值读取和 interval signal 投递检查。后续扩展应继续按子系统逐步增加，不应把 full LTP script 当作已经可用的整体能力。

## 快速构建

仓库使用固定 Rust 工具链：

```toml
channel = "nightly-2025-01-18"
targets = ["riscv64gc-unknown-none-elf", "loongarch64-unknown-none"]
```

常用命令：

```bash
# 构建两种架构，并生成 kernel-rv / kernel-la
make all

# 仅构建一个架构
make ARCH=riscv64 build
make ARCH=loongarch64 build

# 开发阶段快速检查
make ARCH=riscv64 check
make ARCH=loongarch64 check

# 窄 baseline，不打开 iozone/lmbench/LTP
make ARCH=riscv64 check IOZONE=0 LMBENCH=0 LTP=0

# 外部诊断：聚焦 LTP slice
python scripts/perf_baseline_runner.py --arch riscv64 --suite ltp --runs 1 --delete-sdcard-copy
```

默认构建不会因为缺少 `sdcard-rv.img` / `sdcard-la.img` 失败。若存在对应 `.img.xz` 且环境有 `xz`，可执行：

```bash
make unpack-sdcard
```

本地调试若需要把镜像中的测试脚本预载进 MemFS，可使用：

```bash
make ARCH=riscv64 build DEV_PRELOAD=1
```

默认评测路径仍依赖运行时 VirtIO ext4，而不是把测试内容塞进 kernel。

## QEMU 运行示例

RISC-V64：

```bash
qemu-system-riscv64 -machine virt -kernel kernel-rv -m 128M \
  -nographic -smp 1 -bios default \
  -drive file=sdcard-rv.img,if=none,format=raw,id=x0 \
  -device virtio-blk-device,drive=x0,bus=virtio-mmio-bus.0 \
  -no-reboot -device virtio-net-device,netdev=net \
  -netdev user,id=net -rtc base=utc
```

LoongArch64：

```bash
qemu-system-loongarch64 -kernel kernel-la -m 128M \
  -nographic -smp 1 \
  -drive file=sdcard-la.img,if=none,format=raw,id=x0 \
  -device virtio-blk-pci,drive=x0 -no-reboot \
  -device virtio-net-pci,netdev=net0 -netdev user,id=net0 \
  -rtc base=utc
```

如果没有附加包含 `/init` 或测试脚本的 ext4 镜像，内核可能在启动后报告找不到 init 或 harness 输入。评测和性能结论应以带镜像的 QEMU 串口日志为准。

## 回归与性能验证

推荐使用仓库脚本统一构建、运行和保存证据：

```powershell
$env:PYTHONUTF8 = "1"
python scripts/perf_baseline_runner.py --arch riscv64 --suite all --runs 1 --delete-sdcard-copy
python scripts/perf_baseline_runner.py --arch loongarch64 --suite all --runs 1 --delete-sdcard-copy
```

可用 suite profile：

| Profile | 用途 |
| --- | --- |
| `all` | iozone + libcbench + lmbench + 当前 bounded LTP case list |
| `all-lmbench` | iozone + libcbench + lmbench 的兼容别名/聚焦路径，不含 LTP |
| `iozone` | 仅跑 iozone group |
| `libcbench` | 仅跑 libcbench group |
| `lmbench` | 仅跑 lmbench group |
| `ltp` | 仅跑当前 bounded LTP case list |

每次运行会在 `results/` 下保存关键证据，包括：

- `serial.log`
- `judge-summary.json`
- `runs.json`
- `command.txt`
- `docker-command.txt`
- `build-command.txt`
- `git-rev.txt`
- `kernel.sha256`
- `sdcard.sha256`
- `timing.json`

判断进展时优先看 `judge-summary.json` 与 `serial.log`，不要只看 wrapper 退出码或超时状态。

## 设计概要

### 分层结构

- **HAL 层**：通过 polyhal 抽象 RISC-V64 与 LoongArch64 差异。
- **内存管理**：页表、物理页分配、VMA、匿名/文件映射、SysV shm。
- **任务系统**：任务控制块、线程组、等待队列、定时器、信号和 futex wait path。
- **文件系统**：MemFS、ext4 运行时卷、VFS 叠加、fd-table、pipe/socket/pseudo device。
- **系统调用层**：按 Linux/asm-generic ABI 提供 libc 与评测所需的真实子集。
- **Harness 层**：从运行时文件系统发现测试脚本，按 feature 和 env 控制 suite 顺序。

### 项目结构

```text
wll_os/
├── os/                         # 内核源码
│   ├── src/
│   │   ├── fs/                 # VFS、MemFS、ext4、fd/pipe/socket 对象
│   │   ├── mm/                 # 地址空间、页表、mmap/shm 支撑
│   │   ├── syscall/            # syscall 分发与各类 syscall 实现
│   │   ├── task/               # 任务、调度、wait queue、harness
│   │   ├── trap/               # 中断与异常
│   │   └── config/             # 架构配置
│   └── Cargo.toml
├── docs/                       # 设计文档与 syscall matrix
├── scripts/                    # 回归、性能与辅助脚本
├── testdata/                   # judge parser 与测试相关脚本
├── vendor/                     # 离线依赖
├── Makefile                    # 构建入口
└── rust-toolchain.toml          # 固定工具链
```


## AI 工具使用声明

本项目开发过程中允许并实际使用了 AI 工具辅助工程开发、调试和文档整理。根据赛事要求，本队在此披露 AI 工具、相关大模型及使用场景。

| 工具 / 大模型 | 使用场景 |
| --- | --- |
| OpenAI Codex / GPT-5 | 辅助阅读内核代码、定位 QEMU 交互输入与进程回收问题、生成调试思路、整理中文注释与说明文字、辅助 Git 提交与分支合并操作。 |
| VSCode 及相关 AI Agent 工作流 | 辅助代码编辑、命令整理、问题复现步骤记录、开发文档草稿整理。 |

AI 工具输出仅作为辅助建议使用。关键实现、调试验证、提交推送和最终设计取舍由参赛队成员确认完成。与 AI 工具相关的成果、交互记录和使用边界将在开发相关文档、项目设计文档及答辩 PPT 中单独章节继续说明；若后续新增 AI 工具或使用场景，将同步补充披露。

本次 LTP/socket 边界扩展提交中，OpenAI Codex / GPT-5 主要用于辅助阅读现有 Makefile、评测脚本和内核 socket syscall 代码，整理 bounded LTP case 列表与文档说明，执行并汇总 scripts/perf_baseline_runner.py 的双架构 ltp / all 验证结果，以及辅助生成 Git 提交说明。AI 未替代最终工程判断；代码改动、验证结果、提交与推送由参赛队成员确认。

BuildStorm 优化阶段中，OpenAI Codex / GPT-5 还用于交叉核对 dirty worktree、源码和官方 `final-2026` 脚本，建立 MM/调度/VFS/namespace 的可证伪根因模型，辅助实现通用内核修复与独立 SMP 回归，并组织单实例双架构 QEMU、原始日志和 provenance 留存。AI 没有生成或注入评测 marker，也没有修改官方镜像、judge 和 guest 时间。详细披露、人工可核验内容和完整复现步骤见 [BuildStorm 2.3 中文文档](docs/buildstorm-2.3-design-optimization-cn.md)。

## 非本队来源说明

### 第三方库

| 库 | 用途 | 许可证 |
| --- | --- | --- |
| polyhal / polyhal-boot / polyhal-trap | 硬件抽象与启动/陷入支持 | MIT/Apache-2.0 |
| buddy_system_allocator | 物理页帧/堆分配支撑 | MIT |
| spin | 自旋锁 | MIT |
| lazy_static | 延迟初始化全局变量 | MIT/Apache-2.0 |
| bitflags | 位标志宏 | MIT/Apache-2.0 |
| log | 日志接口 | MIT/Apache-2.0 |
| ext4_rs | ext4 镜像读取和写入基础 | 见 vendored crate |

### 参考资源

- 《操作系统导论》(Operating Systems: Three Easy Pieces)
- rCore-Tutorial: <https://github.com/rcore-os/rCore-Tutorial-v3>
- Linux kernel 文档和 man-pages
- OSKernel2025-Nonix 的架构划分与 VirtIO/ext4 集成路径仅作为对照参考。本项目未拷贝其源码；当前 ext4 路径使用 `ext4_rs`，并保留 MemFS 与运行时 ext4 叠加模型。

## 已知边界

- 不是完整 Linux 内核。pthread、process group、namespace、ptrace、完整 signal queue、完整 TCP/IP 与部分权限/能力语义仍是风险区。
- ext4 写入、同步和 mmap writeback 以评测用例所需语义为优先，尚不是完整 journal/persistence 实现。
- 当前 LTP 只声明 bounded slice，不声明 full LTP 可用。
- 性能优化以真实 benchmark 串口日志和 parser summary 为准，单次低分或零分需要复跑确认。

## 未来计划

- **扩大 LTP 覆盖面**：继续按文件系统、进程/线程、信号、时间、权限和内存管理等子系统分批增加 case，避免一次性宣称 full LTP 支持。
- **完善 POSIX/Linux 兼容语义**：补齐更多 pthread、process group、session、权限检查、signal queue、robust futex 与 `/proc` 相关能力。
- **增强文件系统可靠性**：完善 ext4 写入、同步、truncate、rename、mmap writeback 等路径，逐步减少只面向评测用例的特殊处理。
- **改进调度与性能表现**：优化 futex wait path、定时器、上下文切换、文件 I/O 和 benchmark 热路径，形成更稳定的性能基线。
- **补充网络与设备支持**：在现有 loopback/socket 子集基础上，逐步扩展 TCP/UDP、virtio-net 和常用 pseudo device。
- **提升工程化质量**：持续整理 syscall matrix、设计文档和回归脚本，完善自动化证据归档，使功能变化、性能波动和风险边界更容易追踪。

## 许可证

本项目代码采用 MIT 许可证。
