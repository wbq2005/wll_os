# wll_OS - 操作系统大赛 2026

| 项目 | 内容 |
| --- | --- |
| 队伍 ID | T2026105749910208 |
| 队伍名称 | wll_OS |
| 学校 | 华南师范大学 |

## 项目简介

wll_OS 是一个面向全国大学生计算机系统能力大赛操作系统内核赛道的 Rust 内核项目，当前支持 RISC-V64 和 LoongArch64 两种架构。项目以真实评测语义为目标：构建、运行、性能与回归结论都以 QEMU 串口日志和 judge parser 输出为准，不使用假 marker 或仅靠脚本包装的成功状态替代内核运行结果。

当前实现围绕以下能力展开：

- 双架构内核构建：`riscv64gc-unknown-none-elf` 与 `loongarch64-unknown-none`。
- 运行时根文件系统：MemFS 预载与 VirtIO-blk 上的 ext4 叠加，运行时可从 ext4 发现并执行测试脚本。
- 进程、线程组与调度：`clone`、`execve`、`exit`、`exit_group`、`wait4`、`sched_yield` 等提供面向 libc/BusyBox/benchmark 的真实子集。
- FD/VFS 与 IPC：常用文件、目录、pipe、poll/select、pseudo device、部分 socket loopback 与 ext4 写入路径。
- 内存与同步：`brk`、匿名/文件 `mmap`、`munmap`、`mprotect`、SysV shm、futex、robust list 的可用子集。
- 信号与身份：`rt_sig*`、`kill/tkill/tgkill`、UID/GID、supplementary groups、部分权限检查。
- 评测路径：`basic`、`busybox`、`lua`、`libc-test`、`iozone`、`libcbench`、`lmbench`。bounded LTP slice 仅作为外部诊断探针，不进入默认提交路径。

更细的 syscall 状态、剩余风险和 suite 边界见 [docs/syscall-matrix.md](docs/syscall-matrix.md)。

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

`scripts/perf_baseline_runner.py --suite all` 不打开 `LTP=1`，只覆盖默认提交路径中的 iozone/libcbench/lmbench。当前 bounded LTP group 只通过 `--suite ltp` 作为外部诊断探针运行。

### Bounded LTP slice

当前 LTP 不是完整 suite 启用，而是一个明确收口的 case 列表：

```text
writev01,setegid02,getgroups01,setgroups01,setgroups02,setgroups03,setgroups04,access01,open02,setfsuid01,setfsgid01
```

该 slice 覆盖真实 `writev`、UID/GID 与 supplementary groups、fsuid/fsgid 状态、`access(2)` 权限检查、以及 `open(O_NOATIME)` 的所有者/权限路径。后续扩展应继续按子系统逐步增加，不应把 full LTP script 当作已经可用的整体能力。

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
| `all` | iozone + libcbench + lmbench，不含 LTP |
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

## 许可证

本项目代码采用 MIT 许可证。
