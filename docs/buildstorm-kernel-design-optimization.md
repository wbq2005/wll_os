# BuildStorm 内核设计与优化记录

## 1. 阶段范围与结论

本文是 BuildStorm 的累计设计与优化文档。本轮只验收阶段一环境能力：在 RISC-V64 和 LoongArch64 上，使用未修改的官方 glibc 镜像和镜像内原始脚本，跑通工具链检查以及 `cargo new/build/run` 的 minibuild。完整 tgoskits 编译、真实 SMP 调度和编译性能优化不属于本轮完成项。

本轮最高证据等级为 `official-pass`。2026-07-21 的最终正式运行均使用 QEMU `-snapshot -m 8G -smp 8`，两个架构都输出：

```text
BUILDSTORM_TOOLCHAIN ok
BUILDSTORM_MINIBUILD ok
```

官方 `judge_buildstorm-glibc.py` 对两个架构均给出环境项 20.0 分：toolchain 8.0，minibuild 12.0。`compile ok` 与 `compile time` 尚未执行完成，均为 0；不能据此主张后续 160 分。

## 2. 权威输入与发布差异

- 官方仓库：`https://github.com/oscomp/testsuits-for-oskernel`
- 分支：`final-2026`
- 正式验证前重新 fetch 的 commit：`d69becb811573aa789a788e2940fa5ed8f9388f3`
- commit 时间：`2026-07-21T05:32:33+08:00`
- 仓库源码 `scripts/buildstorm_testcode.sh` SHA-256：`446a3321f8d45f37637ac43b1b6bd66e09beb304969f8abac69ccd701252aa0b`
- 官方 judge SHA-256：`586a30e260f39cf425c0960a2965d8684bc3a93b77842a68dfd80fa6ff3ee87b`
- 两个官方镜像内脚本 SHA-256：`5bfbaa5bd99bec595ccd980fff0ebc002d42c0ce0a7e28af266f0ddad69b7189`

镜像内脚本与该 commit 的仓库源码存在一处上游发布差异：镜像把测试组包裹行写成 `START/END buildstorm-glibc`，仓库源码写成 `START/END buildstorm`。计分命令、两个环境 marker 和完整编译逻辑完全一致。本轮正式运行以官方镜像内未修改脚本为准，判分以该 commit 的官方 judge 为准，没有修改镜像或脚本来消除差异。

## 3. 问题定位、证据与根因

### 3.1 第一故障边界：脚本路径已找到，但第一条 echo 未执行

初始单核诊断日志停在：

```text
[harness] SCRIPT /glibc/buildstorm_testcode.sh
```

没有测试组 START 行，也没有任何 BuildStorm marker。原始日志保存在 `docs/evidence/buildstorm-stage1/baseline-riscv64-no-script-echo.log`。该运行只有一个 HART，只用于确定故障边界，不能作为计分证据。

限流诊断继续追踪用户态 trap、最近一次 syscall、页表切换和块设备访问。根因是 syscall 包装在内部 VFS/virtio 操作尚未结束时过早恢复用户页表。低地址用户映射可能覆盖 QEMU virt 平台的 MMIO 地址，导致内核随后访问 virtio MMIO 时使用了错误页表。故障表面看起来像 `/bin/sh` 没有执行第一条 echo，实际边界在内核态 syscall/VFS 生命周期。

### 3.2 工具链已启动，但 cargo 子进程不能完整收敛

修复页表边界后，shell、glibc 动态加载器、rustup、rustc 和 cargo 可以启动。RISC-V64 的中间日志进一步收敛为 `TOOLCHAIN ok`、`MINIBUILD fail`。诊断任务快照显示 cargo/rustc 线程分别阻塞在 futex、pipe I/O、子进程等待和 Unix socket 接收路径。

这里有四个相互独立但共同影响 cargo 的通用 ABI 缺口：

1. pipe、socket 和 eventfd 缺少 `FIONBIO`；pipe 缺少 `FIONREAD`，上层运行库无法按 Linux 语义切换非阻塞状态或查询可读字节数。
2. Unix `SOCK_SEQPACKET` 未实现。cargo 使用的本地进程通信要求保留消息边界，不能简单退化为 stream。
3. seqpacket 对端关闭后没有形成 EOF，`recvfrom` 会永久等待已经消失的 peer。
4. `flock(2)` 缺失。cargo 的缓存和包目录锁依赖 open-file-description 语义；把它错误地并入 POSIX record lock 会破坏 dup/fork/close 生命周期。

### 3.3 LoongArch64 大型 PIE 后部内容损坏

共享 ABI 修复后，LoongArch64 首次运行到 rustc 时出现：

```text
rustc: error while loading shared libraries: unexpected reloc type 0x00dd8170
```

只读提取官方镜像确认 `/root/.cargo/bin/rustc` 最终指向 16,547,096 字节的 LoongArch PIE `rustup`，动态表位于约 14 MiB 文件偏移。`0xdd8170` 实际是该 ELF 的 relocation target offset，不是 relocation type，说明 ELF 头部正确但文件后部读取错块。

根因是整文件 ELF 读取仍调用 ext4_rs 的 legacy `read_at`。大型文件的 extent tree 跨多个节点时，后部逻辑块可能被旧映射辅助函数解析为错误内容。动态加载器只是第一个能明确暴露损坏的位置。

### 3.4 运行器存在假阳性风险

旧运行器只搜索 marker 前缀，因此 `BUILDSTORM_MINIBUILD fail` 也会被当成“到达阶段”。这不会改变来宾输出，但会让本地自动化错误返回 0。根因是阶段完成条件没有编码成功状态和前置依赖。

## 4. 通用设计、平台边界与实现

### 4.1 syscall 页表生命周期

用户 trap 返回调度器后不会直接在 syscall 包装内恢复用户态。因此 syscall 全程保持内核页表；调度器只在真正调用 `run_user_task()` 前激活目标任务地址空间。这样 VFS、块设备、网络和内存管理的嵌套内核操作始终在内核映射下执行。

该规则不依赖 RISC-V 或 LoongArch 指令细节。架构代码仍只负责实际页表寄存器切换和 TLB 语义，调度器负责“何时进入用户地址空间”的共享策略。

### 4.2 ioctl 与非阻塞状态

`FIONBIO` 被收敛到文件描述符能力接口，pipe、socket、eventfd 各自更新已有的非阻塞状态；不支持该 ioctl 的描述符返回 `ENOTTY`。`FIONREAD` 对 pipe 返回当前缓冲区可读字节数。用户指针仍通过统一的跨页 copy helper 访问，空指针返回 `EFAULT`。

### 4.3 Unix SOCK_SEQPACKET

实现只在 `AF_UNIX` 接受 `SOCK_SEQPACKET`，协议必须为 0。数据进入包队列而不是 stream 字节队列，因此一次 send 对应一个消息；接收缓冲区较小时按 Linux 消息语义截断当前包。poll/read/recv 共享“消息可读或连接 EOF”的判定。对端关闭写端或最后一个 peer 引用消失后，空队列读取返回 0。

该设计复用现有 Unix socket 状态机，没有把 cargo、rustc、测试路径或输出文本写入网络代码。

### 4.4 flock 的 open-file-description 所有权

新增 Linux syscall 32，并支持 `LOCK_SH`、`LOCK_EX`、`LOCK_NB`、`LOCK_UN`。锁所有者使用现有 open-file-description ID：dup 和 fork 共享锁，只有最后一个描述引用释放时才解锁并唤醒等待者。flock 与 POSIX record lock 使用独立锁表，但复用文件身份键和 keyed wait queue。

平台层不参与 flock；ext4 与 MemFS 只提供稳定文件身份。

### 4.5 大 ELF 的 extent-aware 读取

`slurp_regular_file()` 继续以 inode size 为唯一文件边界，但每次读取改用内核现有的 `extent_aware_read_at()`。该路径显式遍历完整 extent tree，适用于多层 extent 的大型 PIE，也同样服务两个架构。修复没有按架构、文件名、路径或 ELF 内容分支。

### 4.6 runner 的成功条件

运行器按阶段使用完整成功条件：

- toolchain：必须出现 `BUILDSTORM_TOOLCHAIN ok`
- minibuild：必须同时出现 toolchain ok 和 minibuild ok
- complete：还必须出现 `BUILDSTORM_COMPILE mode=multi ok=true`

runner 只负责构建、启动、计时和记录，不向来宾注入 marker，也不修改 judge 语义。

## 5. 实验数据

### 5.1 阶段结果

| 运行 | 配置 | 观察结果 | host elapsed | 官方 judge |
| --- | --- | --- | ---: | ---: |
| RV 初始故障边界 | 1 HART，诊断用 | 未进入第一条 echo | not measured | 0/180；非计分配置 |
| RV 中间基线 | `-snapshot -m 8G -smp 8` | toolchain ok，minibuild fail | 18.695 s | 8.0/180 |
| RV 最终 | `-snapshot -m 8G -smp 8` | toolchain ok，minibuild ok | 150.539 s | 20.0/180 |
| LA 最终 | `-snapshot -m 8G -smp 8` | toolchain ok，minibuild ok | 67.052 s | 20.0/180 |

最终两次 host elapsed 包含 QEMU 启动以及来宾真实 minibuild。失败运行更早退出，不能与成功运行计算“加速比”。

### 5.2 编译时间与加速比

- 清理诊断残留后的最终内核 release 构建：RV 10.78 s，LA 10.31 s；这是主机端增量内核构建耗时，不是 BuildStorm 来宾编译成绩，也没有可比较的 clean-build 基线。
- tgoskits 完整 clean build：`not yet applicable`。
- BuildStorm guest compile elapsed：`not measured`。
- Linux 对照基线：`not measured`。
- 完整编译加速比：`not measured`，没有可比较的成功全量编译样本。
- 真实 8 核并行效率：`unverified`。QEMU 配置提供 8 vCPU，但本轮没有声称内核已实现真实 SMP 调度。

## 6. 验证与回归

### 6.1 官方 BuildStorm

两份原始串口日志、运行参数 JSON 和 judge 输出位于 `docs/evidence/buildstorm-stage1/`。官方 judge 结果均为：

```json
[
  {"name":"buildstorm env toolchain","pass":1,"total":1,"score":8.0},
  {"name":"buildstorm env minibuild","pass":1,"total":1,"score":12.0},
  {"name":"buildstorm compile ok","pass":0,"total":1,"score":0},
  {"name":"buildstorm compile time","pass":0,"total":1,"score":0.0}
]
```

### 6.2 独立能力回归

通用 compiler probe 在不依赖官方 marker 的独立诊断入口中真实执行了 cargo project 创建、debug build 和产物运行：

```text
PROBE_NEW=0
Finished `dev` profile [unoptimized + debuginfo] target(s) in 1m 44s
PROBE_BUILD=0
Hello, world!
PROBE_RUN=0
```

该证据等级为 `capability-pass`，不计官方分数。probe 使用的临时诊断 harness 已从生产源码删除；保留原始日志用于说明 IPC、进程回收和编译链路的独立验证。

### 6.3 历史能力回归

- CAgent：RV/LA 都重新得到 10/10 case pass；两份脚本仍没有 END marker，最终由 harness timeout 收尾。因此只表述为“10/10 case regression passed”，不重新宣称 CAgent 脚本完整结束。
- 初赛 basic smoke：RV glibc 与 musl 均为 92/102，仅 mount/umount 相关项失败。LA 镜像中 `./run-all.sh` 缺失，脚本打印 `No such file or directory`，所以 LA basic 记为测试资产不足、`unverified`，不能解释为内核 0/102 回归。
- 生产源码检索不到阶段诊断开关、task snapshot、last-syscall probe 或 `[diag]` 输出。

## 7. AI 使用说明与人工验证

AI 用于以下辅助工作：

1. 归纳串口日志和 task snapshot，把大量 syscall、futex、pipe 和子进程状态整理为可验证的阻塞假设。
2. 检索 syscall、VFS、页表、socket、文件锁和 ext4 extent 调用链，提出最小通用补丁草案。
3. 对照 Linux ABI 检查 `FIONBIO/FIONREAD`、seqpacket EOF、flock OFD 生命周期和 runner marker 条件。
4. 整理双架构证据、哈希、QEMU 参数、judge 输出和文档表格。

AI 没有修改官方镜像、脚本或 judge，没有生成成功 marker，也没有伪造 CPU、时间、文件或结果。所有关键结论均由开发者人工完成以下验证：

- 重新 fetch 并直接阅读官方脚本和 judge；
- 审查最终 kernel diff，排除测试名称、路径、命令和预期输出特判；
- 分别执行 RV/LA release 构建；
- 在未修改镜像上以 `-snapshot -m 8G -smp 8` 实际启动 QEMU；
- 查看原始串口输出并运行官方 judge；
- 计算整盘镜像、内核、脚本和 judge SHA-256；
- 复核 CAgent 与 basic smoke 日志，并明确记录未验证项。

## 8. 完整复现步骤

以下命令在仓库根目录执行。镜像路径按本机实际位置替换，但镜像哈希必须与证据一致。

```powershell
git -C _tmp/testsuits-for-oskernel-final-2026 fetch origin final-2026
git -C _tmp/testsuits-for-oskernel-final-2026 rev-parse origin/final-2026

python scripts/run_buildstorm.py `
  --arch riscv64 `
  --image 'D:\BaiduNetdiskDownload\2026OSImage-Pub\sdcard-rv-pub.img\sdcard-rv-pub.img' `
  --stage minibuild --timeout 900

python scripts/run_buildstorm.py `
  --arch loongarch64 `
  --image 'D:\BaiduNetdiskDownload\2026OSImage-Pub\sdcard-la-pub.img\sdcard-la-pub.img' `
  --stage minibuild --timeout 900

python _tmp/testsuits-for-oskernel-final-2026/judge/judge_buildstorm-glibc.py `
  _tmp/buildstorm-riscv64-minibuild.log
python _tmp/testsuits-for-oskernel-final-2026/judge/judge_buildstorm-glibc.py `
  _tmp/buildstorm-loongarch64-minibuild.log

Get-FileHash -Algorithm SHA256 `
  'D:\BaiduNetdiskDownload\2026OSImage-Pub\sdcard-rv-pub.img\sdcard-rv-pub.img', `
  'D:\BaiduNetdiskDownload\2026OSImage-Pub\sdcard-la-pub.img\sdcard-la-pub.img', `
  'target\riscv64gc-unknown-none-elf\release\wll_OS', `
  'target\loongarch64-unknown-none\release\wll_OS'
```

运行器生成的 JSON 保存完整 QEMU 参数和 host elapsed。最终实际参数另见 `provenance.json`。

## 9. 剩余风险与下一阶段

- 尚未完成 untimed `cargo build -p tg-xtask` 和 timed tgoskits clean build。
- 尚未获得 `BUILDSTORM_COMPILE ... ok=true`，完整编译成功分和性能分均未锁定。
- 尚未实现或证明真实 SMP；DTB/OpenSBI 显示 8 个 HART 不能替代调度、IPI、跨核唤醒和 TLB shootdown 验证。
- 后续优化必须继续保持共享 ABI/VFS/MM 逻辑与 RISC-V、LoongArch 平台层边界，避免为 QEMU 固化实现。

## 10. Stage 2 continuation (2026-07-26)

### Evidence and current gate

- Official suite checkout: `final-2026` commit
  `2f6ea561af35c36f4d1bf0dad5ab2eb312839ccc`.
- Official script SHA-256:
  `446A3321F8D45F37637AC43B1B6BD66E09BEB304969F8ABAC69CCD701252AA0B`.
- Official judge SHA-256:
  `586A30E260F39CF425C0960A2965D8684BC3A93B77842A68DFD80FA6FF3EE87B`.
- RISC-V64 and LoongArch64 release builds completed successfully. The
  unmodified official images were launched independently with
  `-snapshot -smp 8 -m 8G`; both produced `BUILDSTORM_TOOLCHAIN ok` and
  `BUILDSTORM_MINIBUILD ok`. The official judge reports 20.0/180 scripted
  points for these two environment gates. This is `official-pass` evidence
  only for toolchain and minibuild, not for full compile or timing.
- Independent eight-core SMP regressions passed on both architectures.
  RISC-V64 reported `asid_check=translation`; LoongArch64 reported
  `asid_check=root-csr`. Both exercised eight dispatching CPUs, TLB
  transport, separate address spaces, ASID reuse after a global flush, and
  160 MiB allocator stress. This is `capability-pass`, not contest score
  evidence.
- Raw serial logs, runner JSON, build logs, and official judge output are in
  `docs/evidence/buildstorm-stage2/20260726-asid-vfs-metadata/`. The runner
  JSON records exact QEMU arguments, kernel hashes, build commands, and host
  elapsed time.

### Bottleneck and design

The 600-second RISC-V64 diagnostic is `unverified` performance evidence,
but identifies the next general bottleneck. At the 480-second snapshot,
parent symlink resolution had fallen to 41.2 seconds after the resolved-path
cache, while `statx_vfs` consumed 92.9 seconds and `open_vfs_and_fd`
consumed 99.0 seconds.

`metadata_with_kind()` resolves an ext4 path once and returns the inode kind
and metadata together. `vfs::metadata()` uses the combined result, preserving
the existing symlink follow behavior and the `ENOTDIR` versus `ENOENT`
distinction. This removes a duplicate path resolution from the normal
non-symlink metadata path without adding a stale metadata cache or making
behavior depend on workload names, paths, command lines, output, CPU count,
or elapsed time.

The follow-up 600-second diagnostic is also `unverified`, but supports this
specific change: at its 480-second snapshot `statx_vfs` averaged 7.14 ms
(63.65 s / 8,910 calls), compared with 9.47 ms (92.87 s / 9,808 calls) in
the preceding parent-cache sample. `open_vfs_and_fd` did not improve, so it
remains the first performance blocker and no full-build speedup is claimed.

Comparable full compile time: not measured. Guest full-compile elapsed,
speedup, and score remain not measured until the original complete workload
ends with `BUILDSTORM_COMPILE mode=multi ok=true` under the official judge.

### Review, AI disclosure, and reproduction

AI assisted with log aggregation, call-path inspection, and the small
interface change. Developer verification consisted of reviewing the
path/error behavior, running both release builds, both official minibuild
runs, both official judge inputs, and both SMP regressions. The anti-cheat
audit searched kernel and runner sources for BuildStorm/tgoskits/output/time
conditionals; production behavior is not specialized for the workload.

Reproduce with `scripts/run_buildstorm.py --stage minibuild` and
`scripts/run_smp_regression.py` for each architecture, using the image paths
and `-smp 8 -m 8G` values recorded in the evidence JSON. Before claiming the
remaining 160 scripted points, run the original `complete` stage sequentially
for RISC-V64 and LoongArch64, preserve both serial logs, and apply the
unmodified official judge.

## 11. Stage 2 final campaign update (2026-07-26)

### Updated official authority and gate

- The official `final-2026` branch was fetched again immediately before the
  scoring campaign. The current commit is
  `1eac61d3becaa592c8ef12a7535f0ec6bb9e3e36` (2026-07-25), which updates the
  official BuildStorm judge baselines and expected core counts.
- The current candidate runner has been restored to the original shared
  configuration: RISC-V64 and LoongArch64 both use `-m 8G -smp 8`; the
  external complete-run timeout remains 6250 seconds.
- The official self-check baselines are 4655.23 seconds for RISC-V64 and
  6223.00 seconds for LoongArch64.
- Per the updated submission gate, one architecture completing one unmodified
  official run is sufficient to commit. No complete or timing score is claimed
  until the raw serial log contains
  `BUILDSTORM_COMPILE mode=multi ok=true` and the unmodified official judge
  accepts that log.

### First measured blocker and root cause

The page-table audit identified syscall 226 (`mprotect`) as the first current
performance blocker. In the pre-fix 240-second RISC-V64 diagnostic snapshot,
23,763 calls consumed 54,839,219 guest microseconds, or 2,307.8 us/call.
The code rewrote mapped PTEs and issued a remote RISC-V RFENCE to each of the
other seven harts individually, including harts that were not executing the
modified address space.

The root cause was not the required TLB invalidation itself. It was the
shootdown transport and target policy: seven SBI round trips were used for one
logical invalidation, and inactive CPUs were synchronously flushed even though
ASID-tagged translations cannot be consumed until that address space is
activated there.

### Architecture-neutral design and platform boundary

`MemorySet::protect_range()` now skips permission-identical areas, overwrites
existing leaf PTEs instead of unmapping and rebuilding them, and performs one
remote shootdown per syscall. The RISC-V platform transport combines all
representable hardware hart IDs into one SBI hart mask.

For non-global RISC-V invalidations, the platform first publishes a pending TLB
generation to every remote CPU and only then samples each CPU's active address
space. CPUs currently running the modified root are synchronously RFENCE'd.
Inactive CPUs defer the flush until `mark_current_address_space()` runs before
their next user entry. Publishing the request before sampling the active root
covers both directions of the task-migration race. ASID recycling continues to
use an immediate all-CPU flush.

The final source review found a narrower activation race in that first
implementation: the CPU sampled its pending generation before publishing its
active root. A shootdown between those two operations could classify the CPU
as inactive after it had already sampled the old generation. The corrected
path publishes the root inside `MemorySet::activate()` while the caller still
holds the shared memory-set lock, then changes the hardware page table before
releasing that lock. An editor of the same page table therefore either
finishes first and leaves a generation consumed during activation, or starts
after the new root is live and includes that CPU in the synchronous remote
flush. The later user-entry publication was removed. This is a general
page-table ownership fix and is not tied to BuildStorm.

The shared memory-management policy remains architecture neutral. RISC-V uses
SBI RFENCE and deferred generation checks; LoongArch keeps its IOCSR IPI and
acknowledgement path. No behavior depends on a test name, executable path,
crate, command line, expected output, elapsed time, or score marker.

### Measured effect and capability evidence

In the post-fix 240-second RISC-V64 diagnostic snapshot, 23,516 `mprotect`
calls consumed 44,536,947 guest microseconds, or 1,893.9 us/call. The measured
per-call reduction is 17.9%. This is diagnostic evidence, not a full-build
speedup claim; the crate mix and total call counts differ between runs.

The final RISC-V64 independent SMP regression passed at 8G/8 with all eight
CPUs dispatching work, remote TLB targets, ASID translation/isolation and
reuse, 64 isolation iterations, and 160 MiB heap stress. The LoongArch64 SMP
regression also uses the restored 8G/8 configuration.

Both default production release builds passed after the final code change:
RISC-V64 completed in 9.52 host seconds and LoongArch64 in 10.38 host seconds.
These are incremental kernel build times, not guest BuildStorm compile times.
The raw logs and JSON are under
`docs/evidence/buildstorm-stage2/20260726-final-release-and-smp/` and
`docs/evidence/buildstorm-stage2/20260726-riscv64-mprotect-deferred-shootdown/`.

### Anti-cheat audit and AI disclosure

The production audit confirmed that `/proc/uptime`, `clock_gettime`, and
`sysinfo.uptime` derive from the same monotonic kernel timer, and CPU reporting
derives from the actual online mask. The runner only builds, launches, scans
for official success markers, times, hashes, and records the run. It does not
modify the image, official script, judge, guest clock, artifacts, or output.
BuildStorm diagnostics are compile-time disabled in production builds.

The first frozen production launch exposed two same-stem candidates,
`/glibc/buildstorm_testcode.sh` and `/musl/buildstorm_testcode.sh`. The harness
group filter did not encode the requested userspace ABI, so an early return
from the first script allowed the stale musl script and its non-official
markers to run. The generic harness selector now accepts the compile-time
`WLL_HARNESS_LIBC` dimension; the official finals runners select `glibc`.
A follow-up 16G/8 minibuild log contains only the glibc script and both current
official environment markers. This selection does not inspect script contents
or expected output.

AI assisted with aggregating diagnostic counters, reviewing page-table and SBI
ordering, proposing the bounded shootdown change, and organizing evidence.
Developer verification consists of source/diff review, both release builds,
the independent SMP runs, unmodified-image execution, raw-log inspection, and
the official judge. The abandoned lazy-ELF and shared-readlink-cache
experiments were removed from production after they failed performance or
locking validation; their negative evidence remains archived.

### Exact reproduction

```powershell
python scripts/run_smp_regression.py --arch riscv64 `
  --image 'D:\BaiduNetdiskDownload\2026OSImage-Pub\sdcard-rv-pub.img\sdcard-rv-pub.img' `
  --timeout 45

powershell -NoProfile -ExecutionPolicy Bypass `
  -File scripts/supervise_buildstorm.ps1 `
  -Arch riscv64 `
  -Image 'D:\BaiduNetdiskDownload\2026OSImage-Pub\sdcard-rv-pub.img\sdcard-rv-pub.img' `
  -TimeoutSeconds 6250 `
  -Repo 'D:\wll_os-master1' `
  -PythonExe 'C:\Users\22478\.cache\codex-runtimes\codex-primary-runtime\dependencies\python\python.exe'

python _tmp/testsuits-for-oskernel-final-2026/judge/judge_buildstorm-glibc.py `
  _tmp/buildstorm-riscv64-complete.log
```

### Final complete-run result

The post-race-fix RISC-V64 production run used the unmodified official image
with `-m 16G -smp 8` and stopped at the 6250-second external limit. It passed
the toolchain and minibuild gates but never reached `BUILDSTORM_BEGIN` or
`BUILDSTORM_COMPILE`; the last serial line was in the untimed `tg-xtask`
pre-build at `Compiling hashbrown v0.17.1`. The runner recorded
`host_elapsed_seconds=6250.391`, kernel SHA-256
`1ca1dd2811acd36a11ed5fed39a2e6f5ccebb45a1dac84f4696ad81523db8857`,
and `reached_stage_marker=false`. The unmodified official judge reports
20.0/180 scripted points. This is `unverified`, not an official complete
pass, so the submission commit and push gate remains closed.

Raw evidence is in
`docs/evidence/buildstorm-stage2/20260726-riscv64-official-timeout-racefix/`.

## 12. Heap and contiguous-block campaign (2026-07-28)

### First real blocker and general fix

The first post-mprotect production run failed at 1332.75 host seconds while
compiling `typenum`. The kernel allocator reported a 9,548,636-byte request
failing with `heap_actual=382407824` and `heap_total=402653184`. The dynamic
heap policy reserved only 1/32 of guest RAM, capped at 256 MiB, so the 16G
RISC-V64 guest had 384 MiB total kernel heap including the fixed 128 MiB
early heap. The raw panic and runner metadata are in
`docs/evidence/buildstorm-stage2/20260727-riscv64-mprotect-presplit-production-heap-blocker/`.

The architecture-neutral policy now reserves 1/16 of detected guest RAM,
capped at 1 GiB, from the contiguous frame allocator. The existing
architecture boundary still converts the reserved physical range through
RISC-V identity RAM or the LoongArch DMW mapping. RISC-V64 16G and
LoongArch64 36G therefore both request a bounded 1 GiB extension, leaving
the remaining frames available to userspace. The independent RISC-V64 SMP
regression passed with eight online CPUs, ASID isolation/reuse, remote TLB
targets and 160 MiB simultaneous kernel-heap stress. A production probe ran
1800.36 seconds without reproducing the heap panic, and a later production
run remained stable for 4303.281 seconds. These are capability and stability
evidence, not an official complete pass.

The fixed per-CPU resource bound was also raised from eight to twelve, so the
scheduler, TLB, diagnostic and CPU-state arrays remain able to represent a
wider machine even though the candidate runner now uses `-smp 8`. Both
production release builds pass. The prior local LoongArch64 36G/12 QEMU
regression remains unverified because the
host had about 1.7 GiB available physical memory and QEMU failed before boot
with `cannot set up guest memory 'loongarch.ram'`.

### Contiguous reads and bounded cache

The clean-file fault path reads ahead up to sixteen 4 KiB pages. Previously
each cache miss issued an independent synchronous virtio request even when
ext4 mapped the pages to consecutive physical blocks. `BlockRange` now
accepts a list of root-device block offsets, serves existing cache entries,
and merges each consecutive miss run into one raw request under the existing
I/O lock. The ext4 clean-page path resolves physical blocks first and submits
the batch only for the standard 4 KiB block/page case. Non-standard block
sizes, sparse extents, short data and allocation errors retain the original
per-page path. The optimization is based on extents and offsets, not names,
commands, crate identities or expected output.

The block cache remains bounded but was increased from 4096 blocks (16 MiB)
to 32768 blocks (128 MiB). This fits within the new 1 GiB dynamic heap while
the separate clean-page and executable caches remain independently capped.
At the 480-second diagnostic snapshot, the 128 MiB candidate had completed
more open, readlink, statx and mprotect operations and reached
`pin-project-lite`; the 16 MiB batched-read candidate had reached `bytes`.
Raw reads were similar (54,587 versus 54,539), so a full-build speedup is not
claimed. Both results are `unverified` diagnostic evidence under
`docs/evidence/buildstorm-stage2/20260728-riscv64-batched-block-600s/` and
`docs/evidence/buildstorm-stage2/20260728-riscv64-block-cache-128m-600s/`.

### Final official run and judge

The final production candidate used the unmodified public image and glibc
script with `-m 16G -smp 8` and the 6250-second external timeout. QEMU was
`11.0.0 (v11.0.0-12122-ga4bb4b10c9)`. The runner recorded host elapsed
`6250.391` seconds and production kernel SHA-256
`c00b66613f76511abd7696b4d4bf08daf8d7a91bf467c1b31d254e6e61940526`.
The log contains `BUILDSTORM_TOOLCHAIN ok` and `BUILDSTORM_MINIBUILD ok`,
but remained in the untimed `tg-xtask` build and never emitted
`BUILDSTORM_COMPILE mode=multi ok=true`.

The unmodified official judge reports 20.0/180 scripted points:
toolchain 8, minibuild 12, compile success 0 and compile time 0. This is
`unverified`, not `official-pass`, and does not satisfy the commit/push gate.
Raw serial output, runner JSON, build log, SMP result, supervisor records and
judge output are in
`docs/evidence/buildstorm-stage2/20260728-riscv64-block-cache-128m-official-6250-timeout/`.

### Provenance, anti-cheat audit and AI disclosure

The saved official authority remains suite commit
`1eac61d3becaa592c8ef12a7535f0ec6bb9e3e36`, script SHA-256
`446A3321F8D45F37637AC43B1B6BD66E09BEB304969F8ABAC69CCD701252AA0B`,
judge SHA-256
`CE4F78D56FB1DB5FAC4588DE57DBA314B0EAA7CF8D3E3ED2FE4F60CF1329EB99`,
and RISC-V64 image SHA-256
`C03C6091EB1C400D4C1F400130D11810B00E18811D4E5C201E7F75802DFAFDE0`.
The remote branch could not be refreshed during this campaign because the
Windows TLS provider returned `SEC_E_NO_CREDENTIALS`; no local authority,
image, script or judge was replaced.

The production source audit found no branching on BuildStorm output, crate
names, compiler command lines, expected artifacts, score markers or elapsed
time. `/proc/uptime`, `clock_gettime` and `sysinfo.uptime` continue to use
the kernel monotonic timer, and online CPU reporting uses the actual platform
mask. The runner only builds, launches with `-snapshot`, scans official
markers, times, hashes and records. Diagnostic counters are behind explicit
`buildstorm-diagnostics,perf-counters` features and were absent from the
final production hash.

AI assisted with log aggregation, heap sizing analysis, block-I/O design,
diagnostic comparison and evidence organization. Developer verification
consisted of source/diff review, both production release builds, the RISC-V64
independent SMP regression, repeated unmodified-image runs, raw serial-log
inspection and execution of the unmodified official judge. No complete time
or speedup is reported because the required success marker is missing.

Reproduce the final run and judge with:

```powershell
powershell -NoProfile -ExecutionPolicy Bypass `
  -File scripts/supervise_buildstorm.ps1 `
  -Arch riscv64 `
  -Image 'D:\BaiduNetdiskDownload\2026OSImage-Pub\sdcard-rv-pub.img\sdcard-rv-pub.img' `
  -TimeoutSeconds 6250 `
  -Repo 'D:\wll_os-master1' `
  -PythonExe 'C:\Users\22478\.cache\codex-runtimes\codex-primary-runtime\dependencies\python\python.exe'

& 'C:\Users\22478\.cache\codex-runtimes\codex-primary-runtime\dependencies\python\python.exe' `
  '_tmp\testsuits-for-oskernel-final-2026\judge\judge_buildstorm-glibc.py' `
  '_tmp\buildstorm-riscv64-complete.log'
```

### SATP boundary and exec page-table lifetime candidate

The 2026-07-31 diagnostic run identified repeated address-space transitions as
the next general hot path after the VFS lookup cache: at approximately
360 seconds, user page-table activation and kernel page-table restoration were
each recorded about 47,000 times. The user roots already contain the shared
kernel mappings, so the scheduler can retain the active RISC-V user root across
ordinary syscall/page-fault boundaries and only restore the kernel root when a
task leaves the user-run loop. `MemorySet::activate` now also avoids rewriting
`satp` when the requested root and ASID are already active. These changes are
architecture-neutral at the scheduler boundary; the RISC-V ASID check is kept
inside the page-table implementation, while LoongArch retains its conservative
activation invalidation.

The same audit found that `execve` replaced a `MemorySet` while the outgoing
user root could still be current. `PageTableWrapper::drop` intentionally
protects the current root, so this leaked the old root frame and delayed ASID
reuse. `execve` now restores the stable kernel root before replacing the address
space, preserving the existing ownership and isolation rules without matching
test names or paths.

The candidate compiles for RISC-V64 and LoongArch64 in release mode. The
independent RISC-V SMP regression passed with eight CPUs and ASID/TLB checks;
the LoongArch regression passed with the same kernel and official image at the
restored `-m 8G -smp 8` configuration. The source and runtime evidence for this
candidate is under
`docs/evidence/buildstorm-stage2/20260731-203301-riscv64-official-pre-satp-hotpath/`.

Measured complete-build speedup is not yet applicable: the prior production
run was stopped before `BUILDSTORM_BEGIN`, and the official compile success
marker remains unverified. Host memory pressure was recorded separately (about
1.6 GiB free physical memory while a 16-GiB RV guest was running), so timing
comparisons from that run are not presented as kernel speedups.

## 13. Child-exit wait race and final-workload sequencing (2026-08-01)

### Root cause and general fix

The RISC-V BuildStorm pre-build repeatedly stalled at crate boundaries where
Cargo waits for short-lived child processes. Source review found a general
lost-wakeup race in both `wait4` and `waitid`: each checked the child state,
then registered a bare child-exit sleep. A child exiting in that interval could
wake an empty queue, leaving the parent blocked despite an already-reapable
zombie. `WaitQueue::sleep_until_if` already supports atomic
register-then-condition checking, so the child wait queue now exposes
`sleep_on_child_exit_if`. `wait4` rechecks non-consumingly for a zombie and
matching child; `waitid` also rechecks stopped/continued events without
consuming them. The following loop iteration remains responsible for the
normal Linux-visible reap or siginfo write.

This is a process-lifecycle fix with no branch on workload names, executable
paths, output text, elapsed time, or expected score. The matching update to
`docs/blocking-semantics.md` records the register-before-recheck invariant.

### Submission sequencing and verification

The default top-level build had `HARNESS_GROUPS=cagent`, so a plain submission
kernel correctly finished CAgent and then shut down without ever selecting
BuildStorm. The default is now `cagent,buildstorm`; focused CAgent and
BuildStorm runners continue to set a single group explicitly. A RISC-V 1G/1CPU
sequence smoke using the unchanged official image recorded all ten CAgent
cases as passing and then the next harness selection:

```text
[harness] SCRIPT /glibc/buildstorm_testcode.sh
#### OS COMP TEST GROUP START buildstorm-glibc ####
```

The smoke subsequently exhausted the intentionally small 1G heap while Rust
started; it is only evidence that selection proceeds beyond CAgent, not a
BuildStorm complete or performance result. Both RISC-V64 and LoongArch64
release builds with `WLL_HARNESS_GROUPS=cagent,buildstorm` completed. The
wait-race candidate also passed both unmodified-image toolchain/minibuild runs
and independent 8-vCPU SMP regressions; the latter are `capability-pass`.
The current official judge reports 20/180 scripted points from minibuild-only
logs, correctly withholding complete points without `BUILDSTORM_COMPILE
mode=multi ok=true`.

Raw source diffs, hashes, release logs, SMP logs, judge output, command JSON,
and the sequence smoke are under:

- `docs/evidence/buildstorm-stage2/20260801-waitid-race-crossarch-regressions/`
- `docs/evidence/buildstorm-stage2/20260801-submission-cagent-then-buildstorm-sequence/`

The official complete gate remains unverified. This workstation has about
15.7 GiB visible RAM and showed sustained paging with an 8G guest, so the
archived local complete attempts are not presented as score evidence. Re-run
both original-image complete commands at `-m 8G -smp 8` on the evaluation
machine, preserve raw serial and official judge output, then update the timing
table with only the successful markers. AI assisted with log aggregation and
race analysis; the developer-reviewed work consists of the source diff, two
release builds, two SMP regressions, official-image minibuilds, official judge
outputs, and the CAgent-to-BuildStorm sequence smoke.

## 14. New official image gate and current blocker (2026-08-02)

### General memory and execve fixes

The current candidate reserves a memory-scaled kernel heap range of up to 1 GiB
and uses one eighth of detected RAM before the cap. This is a general allocator
policy for transient kernel allocations; it does not inspect BuildStorm names,
paths, output, or elapsed time. Both architecture configurations now expose a
4 MiB lazy user-stack VMA. `execve` accepts up to 4096 vector entries and 1 MiB
of string data, returning `E2BIG` instead of silently truncating a vector. Stack
strings and pointer words are copied page-wise through the address-space
translation API, avoiding a byte-at-a-time translation loop for large Rust
compiler argument vectors. The existing shared kernel-root and ASID ownership
rules are unchanged.

The attempted RISC-V lazy user-root shortcut was reverted after the official
minibuild failed immediately: low user roots do not safely cover all paths that
touch kernel/virtio mappings. The safe design remains explicit kernel-root
coverage for kernel work and architecture-local page-table activation. This
negative experiment is preserved as evidence and is not part of the production
claim.

### Official complete runs on the new images

The unmodified official images, official runner, and official judge were used
with QEMU 11.0.3, `-m 8G -smp 8`, and the revised 3000-second timeout. Both
runs reached the environment gates and then timed out during the real multi-
crate compile:

| Architecture | Host elapsed | Markers | Judge | Evidence |
| --- | ---: | --- | ---: | --- |
| RISC-V64 | 3000.148 s | toolchain, minibuild, BEGIN; no compile success | 20.0/180 | `docs/evidence/buildstorm-stage2/20260802-riscv64-new-image-official-3000-timeout/` |
| LoongArch64 | 3000.086 s | toolchain, minibuild, BEGIN; no compile success | 20.0/180 | `docs/evidence/buildstorm-stage2/20260802-loongarch64-final-official-3000-timeout/` |

The official judge output is saved beside each raw serial log, runner JSON,
release-build log, command, QEMU/version data, image/source hashes, and timing
record. The valid 600-second RISC-V diagnostic preceding the official run
measured `user_pt=493072`, `kernel_pt=493072`, `mprotect=62968` (about 35.9 s),
`statx=54890` (about 28.0 s), `openat=23201` (about 28.9 s), and 30,605 block
reads totaling 1,924,387,330 bytes. These numbers identify translation and
metadata/block I/O pressure, but are diagnostic evidence only and do not imply
complete-build points.

### Gate status and audit

Both release builds and independent eight-vCPU SMP regressions remain passing;
the official complete gate is `unverified` on both architectures because the
required `BUILDSTORM_COMPILE mode=multi ok=true` marker is absent. The official
judge therefore correctly awards only the 20 environment points. The
anti-cheat audit found that this candidate diff adds no branch on test names,
paths, commands, output strings, CPU count, fake time, or expected score, and
does not modify an official image, script, or judge. The pre-existing harness
does select named test groups, and the pre-existing diagnostics are explicitly
feature-gated; neither is used by the candidate's allocator, stack, or execve
paths. Diagnostic-only counters remain excluded from the production artifact.
No complete-build speedup or timing score is claimed; comparable successful
compile time is `not measured`.

### LoongArch ASID activation refinement

The 600-second RISC-V diagnostic recorded 493,072 user page-table activations.
RISC-V already writes the ASID-tagged `satp` root without a full TLB flush.
The corresponding LoongArch activation path still performed `TLB::flush_all()`
for every nonzero ASID return. A candidate removed that activation-time flush
because each leaf map/unmap already executes LoongArch `invtlb 0x06` and the
existing IPI shootdown reaches other CPUs executing the same root.

Both local release builds and both independent 8-CPU SMP regressions completed
after this change, but the unmodified LoongArch official image exposed a
stronger correctness boundary: the harness entered the official script and
then shut down before the first toolchain marker. The 64-second run used
`-m 8G -smp 8`; its runner recorded no result markers. The candidate was
therefore reverted. Per-address invalidation under kernel ASID 0 is not by
itself proof that every user-ASID translation affected by intermediate page-
table state is retired before return. The conservative LoongArch activation
flush remains required until a complete generation/ownership design has direct
official-script regression coverage. Performance improvement is `not
applicable`, and no score claim is made.

## 15. Official 3000-second rerun on 2026-08-03

The new public images were run without modification using the unchanged
official runner and judge, QEMU 11.0.3, `-m 8G -smp 8`, and a 3000-second
timeout. Both architectures reproduced the same shared blocker: the script
successfully prebuilt `tg-xtask`, entered `BUILDSTORM_BEGIN mode=multi`, and
then spent the entire window compiling the ArceOS std target and Rust core
libraries inside the guest. No kernel panic, OOM, or test-specific branch was
observed.

| Architecture | Host elapsed | Result | Judge | Evidence |
| --- | ---: | --- | ---: | --- |
| RISC-V64 | 3000.044 s | timeout before compile marker | 20.0/180 | `docs/evidence/buildstorm-stage2/20260803-official-3000-timeout/` |
| LoongArch64 | 3000.247 s | timeout before compile marker | 20.0/180 | `docs/evidence/buildstorm-stage2/20260803-official-3000-timeout/` |

Runner JSON records the exact commands, `-m 8G -smp 8`, QEMU version, kernel
hashes (`f986ca60...cd8c8d` and `bbd2f231...ade41b5`), and elapsed times.
The official judge script hash was
`f9bc3c5c640217947775759b5b02aa4ceedfa76728d25d4f06d94ef5bc9d64dd`.
These are `unverified` complete-build attempts, not score claims. The first
remaining blocker is environmental/runtime throughput for the official
multi-crate guest build; kernel release and independent SMP gates remain
passing. No comparable successful compile time is available (`not measured`).

## 16. RISC-V ASID-0 scoped kernel-return invalidation (2026-08-03)

The previous diagnostics counted roughly 493,000 user and kernel page-table
activations in a short official workload. The kernel-return half was calling
global `sfence.vma`, which also discarded translations belonging to live user
ASIDs. The new transition rule keeps LoongArch's conservative full
invalidation, while RISC-V uses the ISA-defined `sfence.vma x0, asid=0` when
returning to the shared kernel root. Full flushes remain in explicit
shootdown and ASID-reuse paths.

This is a general ASID/TLB ownership optimization and does not inspect test
names, paths, commands, output, or timing. Both production release builds and
the independent 8-CPU SMP regression passed after the change. Regression
evidence is in `docs/evidence/buildstorm-stage2/20260803-asid0-scoped-flush/`:
RV reports `tlb_targets=0xfe, asid_check=translation`; LA reports
`tlb_targets=0x7f, asid_check=root-csr`. Build elapsed comparison is
`not measured`; the official complete run is still in progress, so no
complete-build speedup or score is claimed. The candidate source change is
committed separately and remains unpushed until both official gates pass.

## 17. Ext4 inode metadata cache working-set correction (2026-08-03)

The diagnostic snapshot observed 52,989 ext4 metadata lookups in 300 seconds.
The previous 32,768-entry inode metadata cache cleared the entire B-tree when
full, so a compiler working set larger than that limit repeatedly reread inode
blocks. The limit is now 131,072 entries. Existing namespace/inode mutation
invalidations remain authoritative, so this only changes retention capacity;
it does not return stale metadata or special-case an official workload.

Both architecture release builds passed with this change. Their raw build
logs are in `docs/evidence/buildstorm-stage2/20260803-metadata-cache-capacity/`.
The LoongArch official complete run using this candidate is active; timing and
score remain `unverified` until its `BUILDSTORM_COMPILE mode=multi ok=true`
marker and official judge result are recorded.

## 18. Corrected LoongArch flush boundary (2026-08-03)

The first ASID-scoped implementation accidentally changed the activation
condition from `address_space_id == 0 || loongarch64` to only
`address_space_id == 0`. That removed the conservative LoongArch flush for
nonzero user ASIDs and caused an `InstructionNotExist` trap during the first
official user program. The corrected implementation scopes invalidation only
on RISC-V; LoongArch retains full local invalidation for every user activation.

An independent 180-second official-script run with the corrected boundary
reached `BUILDSTORM_TOOLCHAIN`, `BUILDSTORM_MINIBUILD`, and
`BUILDSTORM_BEGIN mode=multi` without the trap; it then continued compiling
the ArceOS std dependencies. Evidence is in
`docs/evidence/buildstorm-stage2/20260803-loongarch-tlb-fix-180/`. This is
diagnostic progress, not an official complete pass or score claim.

## 19. Final corrected dual-architecture gate and remaining blocker

After correcting the LoongArch activation boundary, both production release
builds and both independent 8-CPU SMP regressions passed. RISC-V reported
`cpus=8`, `dispatch=0xff`, and `asid_check=translation`; LoongArch reported
`cpus=8`, `dispatch=0xff`, and `asid_check=root-csr`. The raw build, serial,
and JSON records are in `docs/evidence/buildstorm-stage2/20260803-final-gates/`.

The corrected candidate was then run sequentially with the unmodified public
images, official runner and judge, QEMU 11.0.3, `-m 8G -smp 8`, and a
3000-second timeout:

| Architecture | Host elapsed | Markers | Judge | Evidence |
| --- | ---: | --- | ---: | --- |
| RISC-V64 | 3000.090 s | toolchain, minibuild, BEGIN; no compile success | 20.0/180 | `docs/evidence/buildstorm-stage2/20260803-final-official-rv/` |
| LoongArch64 | 3000.272 s | toolchain, minibuild, BEGIN; no compile success | 20.0/180 | `docs/evidence/buildstorm-stage2/20260803-final-official-la/` |

Neither run produced a kernel panic, OOM, or architecture trap. Both remained
inside the official Rust/ArceOS standard-library dependency build when the
window expired. Consequently both complete gates remain `unverified`; no
complete-build score or speedup is claimed.

RISC-V now also avoids invalidating the unchanged shared kernel root on every
trap boundary. ASID reuse and explicit mapping shootdowns retain their required
invalidations, while LoongArch keeps full activation invalidation. Both release
and SMP gates pass after this refinement. A 300-second diagnostic still ended
in dependency compilation; evidence is in
`docs/evidence/buildstorm-stage2/20260803-rv-no-kernel-sfence-300/`.

The anti-cheating audit found no branch on official test names, paths, command
text, output markers, CPU count, elapsed time, or expected score. The candidate
does not modify an official image, test script, runner, or judge, and does not
preload a tested binary or fabricate time/CPU/filesystem state. AI assistance
was used to inspect traces and propose the ASID/cache changes; each retained
change was verified by release builds, independent SMP runs, raw official
serial output, and the unchanged judge. The remaining complete-build timing is
`not measured` because no successful complete marker exists.
