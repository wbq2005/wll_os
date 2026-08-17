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

### 2026-08-17 LoongArch64 TLB generation 优化

Stage22 diagnostics 证明 LoongArch64 在约 50.1 万次用户陷入中，user-root activation
与 kernel-root restore 各执行一次全 TLB 失效；但普通 syscall 和 timer 返回中，
root、非零 ASID 与 PTE generation 分别有约 80.6% 和 92.1% 保持稳定。Stage23
因此由 `PageTableWrapper` 统一拥有 translation generation，并在平台层按 CPU
验证 `{root, ASID, generation}`。PTE 修改、远端 IPI、deferred shootdown 和 ASID
recycle 都会撤销验证；ASID 0 仍走保守 flush。RISC-V64 路径不执行 generation
原子操作。

同一 LoongArch64 8G/8 官方 production A/B 从 555.02 秒降到 542.02 秒，改善
13.00 秒（2.34%）。直接 TLB 失效热点明显下降，但时间收益表明它不是剩余主根因。
更激进的 Stage24 user-root retention 在 600 秒只到 toolchain，已经隔离且未合入。
最终候选在未修改官方 `final-2026` suite `b5ec6ef` 上完成双架构 8G/8：

| 架构 | 成功标记 | guest 编译时间 | judge 自动项 |
| --- | --- | ---: | ---: |
| RISC-V64 | `BUILDSTORM_COMPILE mode=multi ok=true` | 773.45 s | 180/180 |
| LoongArch64 | `BUILDSTORM_COMPILE mode=multi ok=true` | 542.02 s | 180/180 |

两项均为 `official-pass`；设计文档 20 分由人工评审，未自行计分。完整因果模型、
不变量、A/B、反证和证据索引见
[Stage23 中文结论](docs/evidence/buildstorm-stage2/20260817-stage23-la-tlb-generation-conclusion-cn.md)。

### 2026-08-16 评测修复

最新评测提交 `2953af6a725c015f89e64db9eab8f60ee27f6fe1` 总分为 542.1；RV
BuildStorm 已通过并得到 123.9，LA 仍只有 toolchain 与 minibuild 的 20 分。日志
复核表明 LA 的 Rust clean build 已在 2439.63 秒完成，0 分发生在随后启动产物的
隐藏 UEFI 准备阶段：GNU coreutils `mkdir -p` 调用 `fchdir(50)`，旧内核返回
`ENOSYS`，导致 `/work/buildstorm.esp` 未创建并且最终成功 marker 缺失。

修复在共享 Linux ABI 层实现 `fchdir(50)` 和 coreutils 同路径需要的
`fchmodat2(452)`，复用现有目录 FD、cwd、root 和 VFS 元数据语义，不包含 LA、
BuildStorm、路径或命令特判。双架构 production/diagnostics release 与 SMP8
`directory-metadata-abi` 回归均通过；未修改 public LA 8G/8 完整流程得到
`BUILDSTORM_COMPILE mode=multi ok=true elapsed_s=553.62`，官方 judge 自动项
180/180。该 public 流程为 `official-pass`；评测机独有的 UEFI 后处理门仍为
`unverified`，需下一轮评测确认。

2026-08-17 的下一轮评测已证明 mkdir 修复生效：ESP 目录错误消失，LA clean build
在 3008.46 秒完成。新的唯一首错是
`cp: cannot stat '/work/buildstorm.vars.fd/vars.fd': Not a directory`。扩展后的独立
LA SMP8 回归已覆盖 `.fd` 目录、短/长目录 symlink、GNU `stat/cp/cmp`，以及文件删除
后同名目录重建，均通过。POSIX `ENOTDIR` 因而指向 hidden fixture 前缀本身不是目录；
在无法取得 hidden 镜像前，该门保持 `unverified`，内核不为特定路径放宽
`regular/child` 语义。审计记录见
[评测输出 42/26 边界](docs/evidence/buildstorm-stage2/20260817-evaluator-output42-26-la-vars-prefix-audit-cn.md)。

### 2026-08-16 MADV_DONTNEED 通用优化

同一轮 LA 日志还出现 135 次 jemalloc
`MADV_DONTNEED does not work (memset will be used instead)`。旧内核对 syscall 233
只返回成功，没有撤销 private anonymous resident page，jemalloc 因而退化为主动
memset。当前实现保持 VMA 拓扑不变，批量撤销 PTE，完成本地/远端 TLB retirement
后再释放 ResidentSet frame owner；anonymous shared、SysV shm 和 file mapping 保持
原所有权语义。

LA diagnostics 300 秒记录 1481 次调用、190954 个请求页和 32904 个实际丢弃页，
fallback 警告降为 0。隔离 A/B 中 LA production 从 556.35 秒降至 547.73 秒，
提升 1.55%。排除未采纳的 Stage16 syscall-return 快路径后，最终提交树的 LA/RV
完整时间分别为 558.27 秒和 686.50 秒；其中 LA 相对纯净 `12f110ae` 的 623.15 秒
累计提升 10.41%。两架构均包含精确 `BUILDSTORM_COMPILE mode=multi ok=true`，
官方 judge 自动项均为 180/180。完整证据见
[Stage17 MADV_DONTNEED 验证](docs/evidence/buildstorm-stage2/stage17-madvise-dontneed-official-20260816/README.md)。

上一轮日志复核还得到两个确定性问题：

- RV 在已完成计分 glibc BuildStorm 后，又启动了不计分的
  `/musl/buildstorm_testcode.sh`。默认 harness 现在只运行 glibc；显式
  `HARNESS_LIBC=musl` 或 `both` 仍保留为 capability 验证入口。glibc 流程内部的
  `*-unknown-linux-musl.json` 是 ArceOS Rust target，属于正式编译，未被关闭。
- LA clean build 已在 2020.04 秒完成，0 分首错是 EFI 准备阶段 legacy
  `mkdir(1030)` 返回 ENOSYS。分发现在复用 `mkdirat(AT_FDCWD, ...)`；双架构独立
  `directory-abi` 回归均通过。

当前候选在未修改 public 镜像和 judge 上以远端复现资源配置完成；这些运行因资源
不等于公开规则的 8G/8，证据等级为 `capability-pass`：

| 架构 | 配置 | 成功标记 | public judge |
| --- | --- | --- | ---: |
| RISC-V64 | 16G/8 | `ok=true elapsed_s=789.87` | 180/180 |
| LoongArch64 | 36G/12 | `ok=true elapsed_s=642.70` | 180/180 |

diagnostics 将剩余首热点定位为 anonymous demand fault（77.891 秒）和 clean-file
fault（71.482 秒）。保留的 clean-page cache batch 把 16 页预读检查和连续命中
分别合并到一次锁临界区；两次 RV 同结构运行平均 778.21 秒，相对 786.62 秒基线
约快 1.07%，未稳定越过宿主噪声，因此只声明通用锁粒度优化，不宣称确定性时间分
提升。完整候选矩阵、被证伪的 second-chance/range 方案和证据分级见
[2026-08-16 验证记录](docs/evidence/buildstorm-stage2/20260816-evaluator-la-mkdir-clean-cache-validation-cn/README.md)。

2026-08-15，提交 `6045dd2534668ea4d1cff94725c41afb0a855af4` 在远端
`47.110.249.0` 使用 QEMU 11.0.3、官方 `final-2026` glibc 镜像、
`-snapshot -m 8G -smp 8` 配置下完成双架构 clean build，证据等级为
`official-pass`。VFS lookup 复用候选已在同配置完成双架构验证：

| 架构 | 官方成功标记 | guest 编译时间 | 官方 judge 自动项 |
| --- | --- | ---: | ---: |
| RISC-V64 VFS lookup 复用 | `BUILDSTORM_COMPILE mode=multi ok=true` | 774.62 s | 180/180 |
| LoongArch64 VFS lookup 复用 | `BUILDSTORM_COMPILE mode=multi ok=true` | 620.70 s | 180/180 |

自动项包括 toolchain 8 分、minibuild 12 分、完整编译 40 分和本次 judge 基线下的时间分 120 分。内核设计优化文档 20 分由人工评审，不在上述自动结果中自行计分。正式评测机会重新测量同机 Linux 基线，因此最终时间分以评测机输出为准。

评测和设计入口：

- [BuildStorm 2.3 中文累计设计与优化记录](docs/buildstorm-2.3-design-optimization-cn.md)
- [当前内核架构总设计](docs/kernel-architecture-overview-cn.md)
- [历史累计设计记录](docs/buildstorm-kernel-design-optimization.md)
- [双架构 official-pass 证据结论](docs/evidence/buildstorm-stage2/20260814-stage2-buildstorm-official-completion.md)
- [Stage B per-CPU heap cache 结论与证据索引](docs/evidence/buildstorm-stage2/20260815-stageb-percpu-heap-cache-conclusion-cn.md)
- [Stage17 MADV_DONTNEED 双架构 official-pass](docs/evidence/buildstorm-stage2/stage17-madvise-dontneed-official-20260816/README.md)
- [VFS 稀疏页缓存与 clustered writeback 结论](docs/buildstorm-2.3-design-optimization-cn.md#11-2026-08-15-评测超时复核与-vfs-稳定性候选)
- [VFS lookup 复用与有界缓存验证记录](docs/evidence/buildstorm-stage2/20260815-vfs-lookup-reuse-validation.md)

本轮候选把 `open_path()` 对同一 ext4 路径的目录、regular-file 与 inode-kind
重复查询合并为一次 generation-validated lookup，并把 parent/readlink cache 从
16K 扩到 64K，满载时逐项淘汰而不是清空整个工作集。相对同配置 production
对照，RISC-V64 提升 `9.94%--10.67%`，LoongArch64 提升
`7.97%--9.68%`。四种 release/diagnostics cfg 与双架构 SMP8 namespace、
regular-file、resident/high-arena、TLB/ASID、320 MiB 大分配和独立用户内存
lifecycle 均通过。

本轮 candidate 另外修复了评测机更大资源配置暴露的三项通用边界：
页帧引用计数采用分块 `AtomicU32`，避免 36 GiB 内存启动时的高阶连续分配；
两架构 secondary boot stack 按 12 核、每核 128 KiB 完整预留并在 Rust 启动
路径断言容量；同步用户 `SIGILL` 通过 Linux signal frame 投递，使 QEMU 的
CPU 扩展探测 handler 能修改 `ucontext.pc` 后恢复。四 cfg、双架构 SMP、
LoongArch64 36G/12 minibuild、RISC-V64 16G/8 minibuild 和独立 nested-QEMU
真实内核加载均为 `capability-pass`。当前 candidate 已在评测日志对应的
RISC-V64 16G/8 与 LoongArch64 36G/12 配置完成 public clean build，分别得到
`BUILDSTORM_COMPILE mode=multi ok=true elapsed_s=786.62` 与 `650.22 s`，当前
官方 judge 均解析为 180/180。由于资源配置不等于公开规则的 8G/8，这两次运行
只记为 `capability-pass`，不作为正式得分声明。public README 仍写 8G/8，而
可执行 judge 期望 RV 8 核、LA 12 核；该上游冲突已在设计文档中保留。评测机
额外的 hidden nested-QEMU 最终门仍为 `unverified`，不能由独立
probe 的 `capability-pass` 代替。详见
[本轮验证索引](docs/evidence/buildstorm-stage2/20260815-large-memory-smp-sigill-validation-cn.md)。

`mode=multi` 表示多核编译，不表示双 libc。官方决赛证据是 glibc；RISC-V64
另以 musl 脚本完成 `BUILDSTORM_RESULT ... status=OK rc=0 elapsed_s=783.15`，
记为 `capability-pass`。官方 LoongArch 镜像不含
`/musl/buildstorm_testcode.sh`，因此 LoongArch musl 为 `unverified`，不伪造
镜像内容，也不把缺少 workload 报告为内核通过或失败。

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
| `HARNESS_LIBC` | `glibc` | 默认只运行计分 libc；`musl`/`both` 需显式选择 |

`scripts/perf_baseline_runner.py --suite all` 会打开 `LTP=1`，在 iozone/libcbench/lmbench 后追加当前 bounded LTP slice。`--suite ltp` 仍可作为只跑 LTP slice 的聚焦诊断入口。
该开发 runner 与 `scripts/run_basic_judge.py` 显式使用 `HARNESS_LIBC=both`，因此默认
提交行为的收紧不会降低本地双 libc 回归覆盖。

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

# 生成课程 zip：条目直接位于压缩包根目录
python3 create_kernel_zip.py --output ../wll_os-submission.zip
```

zip 测评通道要求 `Makefile`、`Cargo.toml`、`os/Cargo.toml` 和
`rust-toolchain.toml` 直接位于压缩包根目录。不要直接上传 GitHub 的
`<repo>-<commit>.zip`，那种下载包会额外嵌套一层目录，评测器在解压目录执行
`make all` 时会得到 `No rule to make target 'all'`。`create_kernel_zip.py` 使用
当前提交的 `git archive` 生成无外层目录的包，并在本地校验根入口与 ZIP CRC。
评测日志中的 `WLL_SUBMISSION_PREFLIGHT status=OK` 表示已进入正确源码根目录；它
不能替代 QEMU 内核或官方 BuildStorm 成功标记。

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

Harness 启动绝对路径脚本时，以当前全局 root 为首要命名空间：只要全局 root 提供
shebang 解释器，脚本及其 `/work`、`/tmp`、`/opt` 等绝对路径都从 `/` 解析；仅当
全局解释器不可用时，才回退到自包含的 suite root。该规则不按测试名、libc、架构、
命令或输出分支，并保留旧兼容镜像的独立 userspace 能力。对应因果模型和双架构
证据见 `docs/evidence/buildstorm-stage2/20260817-stage25-script-root-namespace/README.md`。

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
