# BuildStorm 内核设计与优化记录

## 1. 阶段范围与结论

本文是 BuildStorm 的累计设计与优化文档。阶段一先在 RISC-V64 和 LoongArch64 上跑通未修改官方 glibc 镜像中的工具链与 `cargo new/build/run` minibuild；后续阶段完成真实 SMP、内存与文件系统生命周期修复、热点归因、双架构回归和完整 tgoskits clean build。

当前最高证据等级为 `official-pass`。2026-08-14 的最终正式运行使用 QEMU 11.0.3、未修改的官方镜像与脚本、官方 `final-2026` judge，以及 `-snapshot -m 8G -smp 8`。两个架构都输出完整成功 marker：

```text
BUILDSTORM_TOOLCHAIN ok
BUILDSTORM_MINIBUILD ok
BUILDSTORM_COMPILE mode=multi ok=true
```

RISC-V64 guest 编译耗时 1322.99 秒，LoongArch64 为 1103.14 秒。官方 `judge_buildstorm-glibc.py` 对两个架构均通过全部脚本项，给出 180/180；设计文档 20 分仍由人工评审。完整身份、原始日志、哈希和复现命令见第 39 节及 `docs/evidence/buildstorm-stage2/20260814-stage2-buildstorm-official-completion.md`。

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

## 20. Rejected soft last-CPU scheduling candidate (2026-08-03)

The corrected `102a055` RISC-V candidate was rerun with QEMU 11.0.3, the
unmodified public image and official script, `-m 8G -smp 8`, and the
3000-second harness limit. All eight TCG vCPU threads remained active and the
guest reached the standard-library dependency build, but the run ended after
3000.092 seconds without `BUILDSTORM_COMPILE mode=multi ok=true`. The unchanged
official judge therefore reports 20.0/180 scripted points. The raw serial,
runner JSON, kernel hash, source diff, build log, host data, input hashes, and
judge output are retained under the server evidence directory
`/srv/buildstorm/evidence/102a055-official-rv-current/`. This is `unverified`,
not an official complete pass.

Scheduler inspection found that a foreground task is requeued globally after
each 50 ms timer boundary and can resume on any eligible CPU. The TCB recorded
only current ownership and explicit affinity, so it could not prefer the CPU
whose local ASID/TLB and QEMU translation cache already contained the task's
working set. The candidate architecture-neutral scheduler recorded the CPU after a
successful Ready-to-Running transition and prefers that CPU among equal-
priority runnable tasks. This is a soft hint: explicit affinity is checked
first, realtime priorities retain their ordering and bounded fairness, and a
CPU immediately falls back to the global FIFO when no local candidate exists.

Both release builds pass (RISC-V64 11.03 seconds, LoongArch64 21.79 seconds),
and both independent 8-CPU SMP regressions pass with `dispatch=0xff`, ASID
isolation, and 160 MiB heap stress. Raw local logs are in `_tmp/release-rv-last-
cpu.log`, `_tmp/release-la-last-cpu.log`, `_tmp/smp-regression-riscv64.log`,
and `_tmp/smp-regression-loongarch64.log`.

A directly comparable 360-second diagnostic reached the same final crate,
`ax-posix-api`, as the previous candidate. Its second snapshot counted 418,234
user activations versus 422,719 previously, a 1.1% reduction, but it did not
advance the official build boundary. The optimization was therefore rejected
and its code reverted; no 3000-second run or speedup is claimed. The experiment did not
inspect test names, paths, commands, markers, elapsed time, or expected output;
it does not alter the image, official script, judge, clock, CPU count, or build
artifacts. AI assistance identified the migration hypothesis and prepared the
implementation; release, SMP, raw serial, provenance, and official judge
outputs are the developer-verifiable evidence. The raw A/B diagnostic is
retained at `/srv/buildstorm/evidence/97fbf20-rv-diag-360/`.

## 21. Rejected lockless page-table identity candidate (2026-08-03)

The 360-second diagnostics count about 420,000 user address-space activations.
Compiler threads share one `Arc<Mutex<MemorySet>>`, and the return-to-user path
previously acquired that process-wide lock on every syscall even though it
only read the stable page-table root and ASID. Mapping edits, faults, and COW
operations legitimately need the lock; user-root activation does not change
either identity field between clone and exec.

The candidate TCB snapshots the root and ASID at construction. The exec path updates
the current task's snapshot while replacing its MemorySet after terminating
thread-group peers. User return activates a non-owning page-table handle from
that snapshot, while mapping mutation remains serialized and retains the
existing active-root publication, shootdown generation, ASID reuse, and
architecture-specific invalidation rules. RISC-V still retains ASID-tagged
translations; LoongArch still performs its conservative local activation
flush.

Both release builds pass, and both independent `-smp 8` regressions pass with
`dispatch=0xff`, ASID reuse/isolation, and 160 MiB heap stress. The raw logs are
`_tmp/release-rv-lockless-activate.log`, `_tmp/release-la-lockless-activate.log`,
`_tmp/smp-regression-riscv64.log`, and `_tmp/smp-regression-loongarch64.log`.
Official compile time and speedup are `not measured`; this remains `unverified`
until a comparable diagnostic advances the build and an unchanged official
run emits the complete success marker.

The comparable 360-second run reached the same final `ax-posix-api` crate. Its
second snapshot counted 427,502 user activations versus 422,719 in the
baseline, so it showed no throughput improvement. The candidate was reverted
without a 3000-second run. The experiment did not inspect workload
names, paths, commands, output, time, or CPU count and does not modify the
image, official script, judge, clock, or build artifacts. AI assistance was
used for lock-path analysis and implementation; the saved builds, SMP logs,
raw serial, hashes, and official judge are the developer-verifiable checks.
The raw diagnostic is retained at
`/srv/buildstorm/evidence/cc7d0f7-rv-diag-360/`.

## 22. RISC-V syscall user-root retention (2026-08-03)

The previous RISC-V trap path restored the shared kernel root before every
syscall, then reactivated the same user root and ASID on return. A 360-second
diagnostic counted roughly 420,000 pairs of these transitions. RISC-V user
page tables already share the kernel RAM root entries, so ordinary syscall,
VFS, scheduler, and memory-management code does not require the root change.
The actual conflict is low physical MMIO, which overlaps the user heap range.

RISC-V now retains the current user root across non-scheduling syscall and
handled-fault boundaries. Virtio MMIO block operations and goldfish RTC reads
explicitly restore the kernel root before dereferencing low device addresses.
Scheduling, blocking, task exit, exec replacement, and kernel tasks retain
their existing kernel-root boundary. LoongArch is unchanged and continues to
restore the kernel root and perform its conservative activation flush. User
buffers are still accessed through translated physical frames, and mapping
edits retain the existing lock and cross-CPU shootdown generation protocol.

Both release builds and both independent `-smp 8` regressions pass, including
ASID reuse/isolation and 160 MiB heap stress. An additional RISC-V run with the
unmodified public image reached both official toolchain and minibuild markers
in 32.1 host seconds, exercising cargo, ext4/virtio I/O, process creation,
futexes, time, and console output. Raw logs are `_tmp/release-rv-retain-user-
root.log`, `_tmp/release-la-retain-user-root.log`, `_tmp/smp-regression-
riscv64.log`, `_tmp/smp-regression-loongarch64.log`, and
`_tmp/buildstorm-riscv64-minibuild.log`.

Complete-build timing and speedup remain `not measured`, and no official score
is claimed until the unchanged complete script emits its success marker. The
implementation does not inspect workload names, paths, commands, output,
timing, or expected results and does not alter the official image, script,
judge, CPU count, clock, or artifacts. AI assistance identified the root-
switch boundary and prepared the patch; the release, SMP, official minibuild,
raw serial, hashes, and judge outputs are developer-verifiable evidence.

## 23. Rejected active-root fast-path (2026-08-03)

A follow-up candidate skipped user-root activation whenever the CPU-local
active-address-space slot was merely nonzero. Both release builds and the
RISC-V 8-CPU SMP regression passed, but the unmodified official-image workload
did not: its `ax-posix-api` build script exited with `SIGILL`, and the minibuild
gate reported failure. A nonzero slot is insufficient because it does not
prove that the recorded root and ASID belong to the task about to resume.

The candidate was reverted before ECS deployment. The retained `70f3813`
mechanism still performs the exact root/ASID check on every user return and
therefore does not have this stale-identity failure. Raw negative evidence is
`_tmp/buildstorm-riscv64-minibuild.log`. No timing or score is claimed. This
experiment did not modify the official image, script, judge, clock, CPU count,
or artifacts and did not branch on workload names, paths, commands, or output.

## 24. Exact active-root activation fast-path (2026-08-03)

The corrected follow-up keeps the MemorySet lock and reads the task's actual
page-table root. RISC-V skips `MemorySet::activate()` only when the CPU-local
active root exactly equals that root. A device access, scheduling boundary,
exec, or any different task leaves zero or a different root and therefore
takes the full activation path. LoongArch remains unconditional.

Both release builds, both independent 8-CPU SMP regressions, and an additional
unmodified-image RISC-V official minibuild pass; the latter reached both
environment markers in 31.7 host seconds without the rejected candidate's
`SIGILL`. Raw logs are `_tmp/release-rv-exact-active-root.log`,
`_tmp/release-la-exact-active-root.log`, `_tmp/smp-regression-riscv64.log`,
`_tmp/smp-regression-loongarch64.log`, and
`_tmp/buildstorm-riscv64-minibuild.log`.

Complete timing and speedup are `not measured`; no official score is claimed.
The fast-path only removes redundant synchronization after exact identity
verification. It does not change mapping edits, shootdowns, ASID reuse, MMIO
boundaries, official inputs, clock, CPU count, artifacts, or workload-visible
semantics, and it contains no test-name/path/command/output condition. AI
assistance prepared and audited the identity condition; the saved release,
SMP, official-image serial, hashes, and judge results are developer-verifiable.

## 25. Evidence-driven BuildStorm throughput campaign (2026-08-06 to 2026-08-07)

### Verified boundary and retained production change

The outer complete-run timeout is now 15,000 seconds, exceeding the official
guest compile allowance of 14,400 seconds. The unmodified RISC-V64 glibc
image, production release kernel, diagnostics-disabled build, QEMU
`-snapshot -m 8G -smp 8`, and official script completed the full outer window
without panic or OOM. It emitted the toolchain and minibuild success markers
but no `BUILDSTORM_COMPILE mode=multi ok=true`; the official judge therefore
still reports only 20.0 scripted points. The run reached 33 `Compiling`
events and ended at `rustc-literal-escaper`, proving sustained slow progress
rather than a completed compile or deterministic kernel crash.

The short-window runner starts its 300-second clock only after
`BUILDSTORM_BEGIN mode=multi`, records Cargo progress and host samplers, and
terminates only its own QEMU process. Comparable production windows observed
2 `Compiling` events with one vCPU and 23 with eight vCPUs, an 11.5x event-
count ratio. This is a progress indicator, not normalized compiler throughput
or an official score.

The only new production optimization retained by the campaign is range-local
`mprotect` processing. The previous implementation scanned and globally
sorted/rebuilt every VMA while holding the process MemorySet lock for a change
that can affect only one requested range. The retained implementation starts
at the first overlapping VMA, stops at the range end, and coalesces only the
changed interval and its immediate boundaries. Operations that can add,
remove, or reorder arbitrary mappings retain the global coalescer.

In comparable feature-gated diagnostics, aggregate `mprotect` time fell from
31,569,748 to 2,050,299 microseconds, approximately 93.5%. In feature-off
production windows the first timed crate moved from 130.157906 to 113.134767
seconds, and the 23rd crate moved from 139.770461 to 123.749086 seconds,
11.46% earlier. Both architecture release builds, both independent eight-CPU
SMP/TLB/ASID regressions, and both official toolchain/minibuild checks passed.
The subsequent 15,000-second run still did not finish, so complete-build time
and score remain unverified.

### Diagnostic and experiment infrastructure

`buildstorm-diagnostics` remains an explicit, default-off feature. Its hot
paths use fixed relaxed atomics and per-CPU/fixed-size state; one CPU emits an
aggregate report every ten seconds. Production does not emit these reports or
branch on process names. The counters cover CPU user/kernel/idle ticks,
context switches, migrations, run queues, live/runnable/blocked processes,
blocking categories, page faults, root activation/TLB activity, selected VFS
and virtio work, and ranked lock wait/hold totals. The window runner preserves
raw serial output, complete QEMU arguments, source/image/kernel identity,
timeline progress, and host pidstat/iostat/vmstat data.

### Rejected and blocked candidates

Every rejected production candidate changed one hypothesis only and was
reverted immediately when it failed the 5% floor. The concise table and
evidence index are in
`docs/evidence/buildstorm-stage2/20260807-rejected-candidates-summary.md`.
The rejected set includes a larger anonymous-fault window, local anonymous
coalescing, scheduler empty-ready waiting and blocked-owner boundaries, shared
pipe OFD nonblocking state, adjacent VMA extension, a zero-move subset,
`VecDeque`, bounded batching, and encapsulated vacant slots. The final vacant-
slot production window was 10.44% slower at the first timed crate and 9.22%
slower at the 23rd crate even though diagnostic coalescer time fell 94.73%.

The current blocker is not a missing syscall and not a host swap/device
saturation condition. After the retained `mprotect` fix, anonymous demand-
fault metadata maintenance is measurable, but every tested contiguous-
`Vec<MapArea>` shortcut failed production throughput. No further threshold
tuning in that representation is authorized. A future candidate needs a
separate ownership/COW/file-mapping audit for a representation that avoids
both middle VMA insertion and whole-vector reconstruction.

### Reproduction, compliance, and AI disclosure

The authoritative attribution report is
`docs/evidence/buildstorm-stage2/20260806-attribution-report.md`; the VMA-wide
audit is `docs/evidence/buildstorm-stage2/20260807-global-vma-audit.md`.
Evidence was captured with official suite commit
`b5ec6ef8497e1818cbdec3b54bb722f036e57972`, official image SHA-256
`d74e436522f5946ca17280a7a25f17dbb6604b71fe675bb8a021ce8e849b334c`,
and QEMU 11.0.3. Exact commands and artifact hashes are in each `runner.json`
or `launch.json`; large host sampler logs remain in the named local/ECS
evidence directories and are intentionally not duplicated in this commit.

No retained path inspects an official crate, test, command, output marker,
artifact name, elapsed time, or expected score. The official image, guest
script, judge, markers, CPU count, and guest time are unchanged. AI assisted
with trace classification, code audits, candidate implementation, and report
drafting. The developer-verifiable checks are the source diff, dual release
builds, dual SMP regressions, official-image marker windows, 15,000-second raw
serial and host metrics, hashes, and official judge output. There is still no
successful compile marker, so no complete-build or timing-score claim is made.

## 28. vfork lifetime and nested-COW correctness gate (2026-08-12)

Feature-gated exec diagnostics isolated a general process-lifetime failure:
for `CLONE_VM | CLONE_VFORK`, the parent resumed and reused its stack before
the child completed exec or exit. The failing envp slot had a valid VMA, PTE,
and resident entry, but its pointer value had already been overwritten. The
fix introduces a deferred block/wake handshake around vfork publication and
releases the parent from both child exec-success and exit paths. It does not
inspect command names, paths, crates, test names, markers, or expected output.

The same audit found a second general MM correctness issue. A private page
that had completed one COW fault could be writable in the parent while its VMA
already carried the COW flag. A later fork skipped the parent remap because
the VMA flag did not change, changed the page-level state back to COW, and
left the shared parent PTE writable. `fork_cow` now remaps the parent whenever
the page state transitions into COW. The independent SMP regression performs
a nested fork and verifies read-only parent/child PTEs and frame separation on
the next parent write.

RISC-V64 and LoongArch64 production/SMP release checks passed, as did the
RISC-V64 diagnostic lifecycle run. That run passed resident-memory lifecycle,
user-memory lifecycle, and minibuild without the prior exec failure or stack
smashing. The feature-off RISC-V64 production window used the unmodified
official image with `-snapshot -m 8G -smp 8` and reached 23 `Compiling` lines
in 300 seconds, exactly the same coarse progress as the comparable baseline.
Before/after progress is therefore 23/23 (0% measured improvement); this is a
correctness gate, not a retained performance optimization. No successful
`BUILDSTORM_COMPILE mode=multi ok=true` marker has yet been observed.

Raw production-window evidence, launch arguments, hashes, and host samplers
are in
`docs/evidence/buildstorm-stage2/20260812-riscv64-production-smp8-nested-cow-remap-window300/`.
The complete reproduction is `python3 scripts/run_buildstorm.py --arch
riscv64 --image /srv/buildstorm/images/sdcard-rv-pub.img --stage complete
--timeout 15000 --memory 8G --smp 8`, with diagnostics disabled. AI assisted
with semantic auditing, diagnostic design, implementation review, and report
drafting. Developer-verifiable evidence consists of the source diff, dual-
architecture builds/regressions, raw serial logs, hashes, exact QEMU arguments,
and host metrics; official compile and judge success remain unverified until
the exact successful marker is present.

### Complete-run outcome

The corresponding production complete run used the unmodified official image
and suite, diagnostics disabled, `-snapshot -m 8G -smp 8`, and the full
15,000-second outer timeout. It again stopped at 33 `Compiling` events and
produced no `BUILDSTORM_COMPILE` result, panic, or OOM. Serial output remained
unchanged for most of the 4:10:07 run while QEMU consumed 773% aggregate host
CPU; all eight TCG threads were observed busy, with zero swap and no storage
saturation. Because this repeats the prior 33-event long-run boundary, the
result is classified as a deterministic high-CPU stall/livelock boundary.
The exact internal mechanism is not proven by the feature-off run.

The official judge awarded 20/180 scripted points (toolchain 8, minibuild 12,
compile 0, compile-time 0). Raw evidence is in
`docs/evidence/buildstorm-stage2/20260812-riscv64-production-complete-15000-official/`.
The next diagnostic must isolate this stable late boundary using existing
feature-gated aggregate counters; it must not combine scheduler, MM, VFS, or
block-cache production changes. The long-run `pidstat -C` filter also needs a
host-runner-only correction because Linux truncated the QEMU comm and the log
contains headers only.

## 29. Late-boundary MemorySet attribution (2026-08-12)

An 1,800-second RISC-V64 diagnostic window reached the repeated late compile
phase with up to ten runnable rustc tasks and user execution on all eight
vCPUs. Over the final 835.85 seconds, `memory_set_activation` accumulated
1,327.84 seconds of cross-task lock wait, followed by 1,120.44 seconds for the
aggregate MemorySet class. Direct address-space activation and TLB shootdown
work totaled only about 11.6 seconds. Host swap, host storage saturation,
virtio lock wait, and block-cache lock wait were absent. The first measured
kernel bottleneck is therefore shared MemorySet lock contention at user entry
and mmap/munmap, not scheduling, raw TLB instructions, or block I/O.

The historical lockless candidate `cc7d0f7` is not reusable as written. It
copied raw root and ASID values into every TCB, while exec replaces the shared
MemorySet after merely marking peer threads Zombie. The current peer-exit path
does not synchronously wait for each peer's `running_cpu` ownership to drain;
therefore a peer may still retain the old execution identity while the old
page table and resident frames are destroyed. Independent root and ASID
atomics also do not define a transactional identity pair. These are
source-derived lifetime hazards, not a demonstrated explanation for the
BuildStorm stall.

The next candidate gate is an independent lifecycle regression covering
concurrent `CLONE_VM` mapping edits and user return, exec peer quiescence,
address-space replacement/drop, ASID reuse, and remote shootdown publication.
Only after that gate passes may a RISC-V lockless activation snapshot be
considered. The preferred boundary is one immutable address-space identity
value, published transactionally, whose owning MemorySet and resident frames
remain alive until no CPU can execute the old identity. LoongArch keeps its
existing conservative activation/flush path unless separate measurements
justify an architecture-specific change. No production optimization or
complete-build timing is claimed from this diagnostic run.

Raw and derived evidence is under
`docs/evidence/buildstorm-stage2/20260812-riscv64-diagnostics-smp8-late-boundary-window1800/`.
The run used source commit `50c9bd348f13cd845880bdcf1ccf3a1e84eac0ea`
plus diagnostic diff SHA-256
`3b070754d619d33381ad991ec099b293d8ddb65bfd76c48905dfacdc70841c8f`,
kernel SHA-256
`6697a8971432d34a801c51be0a95ae5a1c7b782081a560bd59fe6d284489be7f`,
the official image SHA-256
`d74e436522f5946ca17280a7a25f17dbb6604b71fe675bb8a021ce8e849b334c`,
and QEMU 11.0.3 with `-snapshot -m 8G -smp 8`. AI assisted with aggregate
delta calculation, source cross-checking, and lifecycle review. All numbers
are reproducible from the saved raw JSON and serial/host logs. There is still
no exact `BUILDSTORM_COMPILE mode=multi ok=true` marker.

## 30. Rejected RISC-V activation-token candidate (2026-08-12)

The late diagnostic justified testing one architecture-specific hypothesis:
remove the shared `MemorySet` lock from the RISC-V user-entry identity read.
The candidate published root and ASID as one SATP token, retained the existing
mapping mutex, kept LoongArch on its conservative locked activation path, and
added address-space retirement and activation-identity regressions. Four cfg
builds, both production release builds, and both eight-CPU SMP regressions
passed before the performance window.

The official-image production window rejected the candidate. Baseline and
candidate both reached 23 `Compiling` lines, two `Finished` lines, and the
same last crate, `ax-posix-api`; coarse progress improvement was 0%. The first
timed crate moved from 177.413 to 193.633 seconds (9.14% slower), and the 23rd
crate moved from 187.227 to 205.649 seconds (9.84% slower). Both hosts had zero
swap, negligible storage utilization, and activity on all eight TCG threads.
The production candidate and its dedicated kernel regression were therefore
reverted. The corrected host SMP runner remains: it now waits for the final
`[smp-regression] pass cpus=` marker instead of accepting an intermediate
phase pass.

The lifecycle audit exposed but did not solve a broader ownership issue:
`CLONE_VM` without `CLONE_THREAD` shares the current address-space owner, so
exec detachment cannot safely be modeled as an in-place replacement visible
to all sharing processes. That source-derived risk prevents reusing the token
design without a per-process view/backing ownership model. It is not a proven
cause of the BuildStorm boundary.

Raw evidence and the exact comparison are in
`docs/evidence/buildstorm-stage2/20260812-riscv64-production-smp8-activation-token-window300/`.
The candidate kernel SHA-256 was
`f7349c21c1421b89c6cdc3655b9395daa240e9416f159a0dc9f2dd3d9e4dc4ea`.
No official compile success is claimed; the exact successful marker remains
absent. AI assisted with semantic lifetime auditing, regression design,
measurement comparison, and documentation. Developer-verifiable artifacts
are the source diff, dual-architecture build/SMP logs, raw serial, launch
metadata, and host samplers.

## 31. Rejected munmap range-drain candidate (2026-08-12)

The next isolated hypothesis targeted the measured 414 seconds of `munmap`
MemorySet hold time. `unmap_range` was changed to drain only the contiguous
VMA interval, withdraw every affected PTE, issue one remote shootdown, and
drop removed resident owners after shootdown completion. This eliminated the
whole-vector rebuild, global sort/coalesce, and per-VMA remote shootdown while
preserving the existing MemorySet lock and architecture-neutral PTE ordering.

Four cfg checks, dual production release builds, and dual eight-CPU SMP
regressions passed. The official-image production window nevertheless reached
the same 23 `Compiling` lines and final `ax-posix-api` crate. The 23rd crate
improved only from 187.227 to 185.047 seconds (1.16%), below the mandatory 5%
floor, so the candidate and its dedicated regression were immediately
reverted.

Raw evidence is under
`docs/evidence/buildstorm-stage2/20260812-riscv64-production-smp8-unmap-range-drain-window300-run2/`.
The host did not swap or saturate storage and all eight TCG threads were
active. Candidate kernel SHA-256 was
`0dc6c3e5469a7e28ef439c01058141d8418a6893701cc1092496cc3867a5d914`.
This result narrows the hotspot: local VMA removal mechanics alone do not
explain the late high-CPU boundary. Complete BuildStorm remains unverified.
AI assisted with lifecycle auditing, regression design, controlled remote
measurement, and evidence comparison; all claims are reproducible from the
saved launch, summary, serial, and host sampler files.

## 32. Rejected eager large-mmap segregation candidate (2026-08-12)

Late diagnostics showed repeated 128-MiB mapping failures after the low user
range fragmented below an 85.37-MiB maximum sampled gap. A new RISC-V-only
address-layout candidate therefore differed from the previously rejected
fallback: no-hint mappings of at least 128 MiB were sent to a separate
positive-Sv39 window before low-range fragmentation formed. Low mappings and
LoongArch behavior were unchanged. Architecture-defined root ownership kept
the kernel identity range shared and made only the high user root entries
process-owned. No TLB policy, VFS, scheduler, official script, image, marker,
or guest time behavior changed.

Four cfg checks, dual production release builds, and dual eight-CPU SMP
regressions passed. The independent regression exercised address selection,
high-range demand fault, fork/COW, partial unmap, drop, and new-root isolation.
Both unchanged official images also reached the exact toolchain and minibuild
success markers with production diagnostics disabled.

The comparable RISC-V production window reached 23 `Compiling` lines, two
`Finished` lines, and the same final `ax-posix-api` crate as the control: 0%
coarse progress improvement. Against the current-commit high-window control,
the first crate regressed from 171.810 to 194.230 seconds (13.05%) and the
23rd crate from 182.424 to 204.443 seconds (12.07%). Host swap and I/O wait
were zero, storage was not saturated, and all eight TCG threads were active.
The candidate and its dedicated regression were therefore reverted under the
mandatory below-5% rule.

Raw evidence is in
`docs/evidence/buildstorm-stage2/20260812-riscv64-production-smp8-mmap-large-segregation-window300/`.
The candidate kernel SHA-256 was
`e8280e6e504586199c85bd2e994bc4567a072f7bc42c7df9990b8bbc7ea9ddf9`;
the unchanged official image SHA-256 was
`d74e436522f5946ca17280a7a25f17dbb6604b71fe675bb8a021ce8e849b334c`.
No exact compile-success marker was emitted, so complete BuildStorm remains
unverified. AI assisted with semantic auditing, regression design, execution,
and evidence comparison; developer-verifiable artifacts include the source
diff, dual-architecture logs, raw serial, launch JSON, and host samplers.

## 33. Per-frame ownership table (2026-08-12)

Three feature-gated runs refined the first synchronous exec hotspot. Publishing
the new address space cost 64 us across 79 marker-window execs, while dropping
the old `MemorySet` cost 27.584 seconds. A second run attributed 30.414 of
30.448 seconds (99.89%) to dropping VMA/resident owners; ASID retirement cost
56 us and page-table owner destruction 32.876 ms. A third, attribution-only
heap-instrumented run recorded 68.49 million heap acquisitions, 4.37 million
contended acquisitions, 126.89 seconds wait, and 185.90 seconds hold during
the marker window. These runs identify per-resident-frame ownership teardown
as the first actionable representation cost. The global heap counters do not
prove that every heap event came from resident destruction.

The architecture-neutral change replaces one heap-allocated
`Arc<FrameTrackerInner>` per tracked frame with fixed refcount metadata. After
all platform memory regions are registered, each managed region receives an
immutable array containing one `AtomicUsize` per frame. `FrameTracker` holds
only a physical page number. Clone increments that frame's count; drop
decrements it and returns the frame to the existing buddy allocator only for
the last owner. This keeps page-level COW, shared mapping, and clean file-cache
ownership unchanged.

The ownership boundary is explicit. Only the frame allocator constructs
trackers. Page-table allocation transfers an exclusively tracked frame into
raw `PageAlloc` ownership through `into_raw_ppn`; contiguous DMA frames retain
their existing raw allocation/free API. Overflow, underflow, non-exclusive
raw transfer, and deallocation while tracked are asserted. Initialization is
shared by RISC-V64 and LoongArch64 after DTB memory discovery; no
architecture-specific fast path was added. Exec replacement, VMA topology,
PTE publication, TLB/shootdown ordering, scheduler, VFS, and block-cache
behavior are unchanged.

Both targets passed production and diagnostic cfg checks. Both eight-vCPU SMP
regressions passed resident and user-memory lifecycles plus the final all-CPU
gate, and both unchanged official images passed exact toolchain/minibuild
markers. In comparable RISC-V64 production windows, the first `Compiling`
event moved from 171.810 to 49.057 seconds (71.45% earlier) and the 23rd from
182.424 to 57.669 seconds (68.39% earlier). Both runs still had 23 compiling
events after 300 seconds, so coarse crate-count improvement was 0%. The
candidate is retained under the declared >=15% latency threshold, but final
compile-time improvement is not measured.

Evidence is in
`docs/evidence/buildstorm-stage2/20260812-riscv64-diagnostics-smp8-exec-replace-phase-window300/`,
`20260812-riscv64-diagnostics-smp8-exec-drop-owner-phase-window300/`,
`20260812-riscv64-diagnostics-smp8-heap-lock-phase-window300/`, and
`20260812-riscv64-production-smp8-frame-refcount-table-window300/`.
Candidate kernel SHA-256 is
`399a39a25a67b3b6edf1733f294ba552004e317c8fcb263a35704393fdad842d`;
the unchanged official image SHA-256 is
`d74e436522f5946ca17280a7a25f17dbb6604b71fe675bb8a021ce8e849b334c`.
The re-fetched official suite commit is
`b5ec6ef8497e1818cbdec3b54bb722f036e57972`.

Reproduce the short candidate window with the command in its evidence
`README.md`. Reproduce the final gate with diagnostics disabled and:

```text
BUILDSTORM_SUITE_DIR=/srv/buildstorm/src/testsuits-for-oskernel \
  scripts/run_buildstorm_long_baseline.sh riscv64 \
  /srv/buildstorm/images/sdcard-rv-pub.img \
  docs/evidence/buildstorm-stage2/20260812-riscv64-production-frame-refcount-complete-15000-official
```

The long-run host harness takes a nonblocking host-wide `flock` and rejects
launch when another kernel QEMU process is already active. This prevents a
restarted supervisor or overlapping monitor from contaminating the single-run
evidence. Its `pidstat -t` sampler records QEMU thread-level CPU and context
switch activity. These controls change neither the official guest script nor
kernel behavior.

AI assisted with diagnostic design, source and ownership auditing, candidate
implementation, regression construction, controlled execution, and evidence
comparison. Developer-verifiable artifacts are the source diff, dual-target
logs, official marker logs, raw serial, hashes, exact QEMU arguments, and host
samplers. Complete BuildStorm remains `unverified / not completed`; there is
no exact `BUILDSTORM_COMPILE mode=multi ok=true` marker yet.

## 34. Stage 1 retained high mmap arena (2026-08-14)

The independent global audit corrected the late-boundary model. The per-frame
ownership table removed a startup destructor cost, but a 1,800-second
diagnostic interval later recorded up to 2,305 VMAs and a maximum free mmap gap
of only 85.37 MiB while userspace requested lazy reservations as large as
128 MiB. In one 70.088-second late interval, 56,625 of 88,498 mmap selections
returned `ENOMEM`. mmap/munmap topology changes, PTE/TLB commits and the shared
`MemorySet` lock were downstream multipliers of this address-capacity failure.

Stage 1 introduced an architecture-neutral user-VA policy with a historical
low arena and a disjoint high arena at
`0x20_0000_0000..0x40_0000_0000`. `MmContext` owns independent cursors for the
two arenas. Reservations remain lazy. RISC-V restore and release share one
non-contiguous root-ownership predicate: roots `0..1` and `128..255` are owned
by the process, while the kernel identity root and other global roots remain
shared. LoongArch uses the same logical user arena while keeping PLV0 DMW
state platform-owned. Fixed mappings, shared memory and shared-file writeback
use the common legal-range policy.

The high-arena diagnostic recorded 5,542 successful selections and zero
selection `ENOMEM` by snapshot 67. RISC-V production still had 23 compile
events at 300 seconds, but its 1,800-second run crossed the old 33-event
`rustc-literal-escaper` boundary and reached event 34, `hashbrown`, at
430.325 seconds. A separate diagnostics run reached 65 events and
`rdif-reset`; that count is attribution evidence, not comparable production
throughput. Four release cfg builds and dual-architecture eight-CPU resident,
high-arena, user-memory, COW/shared/file, ASID/TLB and heap-stress regressions
passed as `capability-pass`.

The retained production kernel SHA-256 is
`56870ac6589aaa62a6e5ce8cbbd4071aee5faa44c7af61d9615da742256d4fb0`.
Raw evidence and the full causal decision are under
`docs/evidence/buildstorm-stage2/20260814-riscv64-production-stage1-high-arena-window300/`,
`20260814-riscv64-production-stage1-high-arena-window1800/`,
`20260814-riscv64-diagnostics-stage1-high-arena-window1800/`, and
`20260814-stage1-high-mmap-arena-conclusion.md`. AI assisted with evidence
reconciliation, cfg ownership auditing, implementation, regression design and
comparison. Complete BuildStorm remains `unverified / not completed`.

## 35. Stage 2 single-idle-CPU runnable publication (2026-08-14)

Host thread sampling exposed a scheduler/virtualization interaction hidden by
guest counters. Before Stage 2, one useful QEMU vCPU was near 100% host CPU,
while seven nominally idle vCPU threads each consumed about 88% and generated
roughly 52,000-70,000 voluntary context switches per second. The cause was a
broadcast reschedule IPI on every ready-queue publication, including ordinary
local time-slice and syscall requeues. Under QEMU TCG this formed a persistent
idle-vCPU wakeup herd.

The scheduler now distinguishes external publication from local requeue.
External insertion samples task affinity, inserts and deduplicates while
holding the ready-queue lock, then atomically claims one eligible bit from the
published idle mask and sends one IPI. Local time-slice, syscall and kernel
task requeues do not wake another CPU when the current CPU is eligible; an
affinity mismatch automatically falls back to external notification. A
duplicate PID insertion sends no IPI. Control events that may change several
tasks, including thread-group state and affinity changes, retain broadcast
notification.

The lost-wakeup invariant is preserved: an idle CPU publishes its bit before
rechecking the affinity-filtered ready queue. A producer publishes the queue
entry before claiming that bit. Therefore either the idle-side recheck sees
the task, or the producer claims the bit and sends the interrupt. Blocked-task
completion, fork/vfork and new-task paths remain external publications. MM,
VFS, PTE/TLB, frame ownership and official runner semantics are unchanged.

At 300 seconds, production still reached 23 compile events and `ax-posix-api`;
the last-event timestamp was 56.871 seconds versus Stage 1's 56.870 seconds.
This is no measured crate-boundary gain. The direct hotspot nevertheless
collapsed: final QEMU use fell from about 718-720% to 105-108%, idle vCPU use
fell to 0-2%, and their voluntary context switches fell to about 95-131 per
second. Diagnostics later used two useful vCPUs near 100% each and recorded
user execution on all eight CPUs. Under the retained-positive-optimization
rule this resource-efficiency correction stays.

The completed 1,800-second production comparison reached the same 34 compile
events and the same final `hashbrown` crate as Stage 1. The final event moved
from 430.325 to 430.123 seconds, only about 0.05%. In the late phase, two useful
TCG threads ran near 100% and the other six stayed near 1-4%, for about 215-217%
aggregate QEMU CPU instead of the previous 718-720%. Thus the stage preserved
real parallelism and eliminated the host wakeup herd, but it did not move the
compile dependency boundary. The next hotspot must be re-attributed from the
post-`hashbrown` useful-vCPU work rather than inferred from scheduler wakeups.

Four release cfg builds and both eight-CPU architecture regression matrices
passed as `capability-pass`. The RISC-V production, RISC-V diagnostics,
LoongArch production and LoongArch diagnostics kernel SHA-256 values were
`404040dbaae5b0e2f22c72a3eaa7e110cfe45cd57089b55a7d6bf5562a7823ad`,
`438f23a83eb99338b7c4bcaeed45a2f9eecd97559167376e5721e517495d20b0`,
`3228003211a6f06f4eef9a5ce796bf176a5070ac62e4bf18718656487e29030b`,
and `69b3cdab51c687c5e8c5711abc2efb1086d7649215ebbf3e51fdaa3d265b5235`.
AI assisted with host/guest timeline correlation, wakeup-protocol auditing,
implementation, regression design and measurement. No official pass or full
compile is claimed without `BUILDSTORM_COMPILE mode=multi ok=true`.

## 36. Stage 2 raw-frame ownership invariant (2026-08-14)

After the ext4 directory insertion lockup was fixed, a delayed-QMP production
window reached 48 compile events and then glibc aborted with
`malloc_consolidate(): unaligned fastbin chunk detected`. An Arc frame-owner
control did not reproduce that signature in 600 seconds but only reached the
old 23-event boundary. This changes the attribution weight of the per-frame
`AtomicUsize` candidate, but does not prove it is the unique corruption source.

The ownership audit separates three allocation domains. `FrameTracker` owns
resident user and file-cache pages. PolyHAL's page allocator transfers a sole
tracker into raw ownership for page-table roots and intermediate tables.
Kernel heap extension, VirtIO DMA and kernel stacks use raw contiguous ranges.
The production refcount table protects only the first domain, so its existing
assertions cannot detect a duplicate raw free or an overlap between domains.

A diagnostics-only byte shadow now records each managed page as free, tracked,
page-table, or contiguous. Every allocation/transfer/free performs an atomic
expected-state transition and panics with the PPN, expected/actual states and
tracked count on a mismatch. Periodic diagnostics report active totals and
transition count. Production cfg contains none of this shadow state or branch.
The experiment is falsifiable: the raw-owner hypothesis requires an invariant
failure before the userspace abort; reproducing the abort with all transitions
valid rejects it and redirects the audit to heap metadata writes or another
subsystem.

Static review also found that kernel stacks are allocated as 16-page
contiguous ranges but their ownership is not stored in `TaskControlBlock` and
no matching release path exists. This is a real lifetime leak and can cause
long-run pressure, but it is not evidence of a duplicate free and is not mixed
into the current diagnostic experiment.

All four remote release configurations passed: RISC-V64 and LoongArch64,
production and diagnostics. Both architectures also passed the SMP8 resident,
high-arena and user-memory lifecycle regressions, 64 isolation iterations,
ASID/root checks and 160-MiB heap stress (`capability-pass`). `git diff
--check` passed.

Two diagnostics windows exercised the owner shadow. The 700-second window
recorded 1,623,609 legal transitions and eight compile events; after about 430
seconds it entered a cargo-only userspace stall. A separate 600-second delayed
QMP window recorded 2,887,086 legal transitions and 54 compile events. Its
sampled user PCs changed and later snapshots contained eight or more runnable
rustc threads, so the cargo-only stall did not reproduce and is not a stable
hotspot. Neither run reported an ownership violation, panic, OOM, fastbin
error, or successful result marker.

A production control using the same source identity reached 112 compile events
and `uguid` in 700 seconds, with its last event at about 699 seconds. It did not
reproduce the historical SIGABRT, but it also did not emit
`BUILDSTORM_COMPILE mode=multi ok=true`. The one fastbin abort is therefore
downgraded to a non-stable anomaly; raw allocator overlap is weakened but not
formally disproved. A longer production delayed-QMP window is the next
attribution gate, and no production MM/TLB candidate follows from the shadow
alone. Full evidence and hashes are in
`docs/evidence/buildstorm-stage2/20260814-stage2-raw-frame-ownership-audit.md`.
BuildStorm remains `unverified / not completed`.

## 37. Stage 2 interval-timer owner index candidate (2026-08-14)

The 1,800-second refcount control reached 131 compile events and
`flatten_objects` without the historical fastbin abort. Delayed QMP sampled
the same `task::manager::live_tasks()` PC on most CPUs. Return addresses
resolved to `next_interval_timer_deadline_us` and
`wake_expired_interval_timers`, which were scanning the entire weak task
registry on every timer tick. `pidstat` showed roughly 81--93% CPU on each of
the eight TCG vCPU threads during this shape, with no fixed user PC. The
stable residual hotspot is therefore a timer bookkeeping architecture issue,
not raw frame ownership.

The candidate adds an active interval-timer owner index containing only weak
TCB references. `TaskInner.interval_timers` remains the sole timer state. A
`setitimer` update registers or removes the TCB after releasing its inner lock;
process teardown clears timer state and then removes the owner. Timer deadline
and expiry scans hold the owner index lock, inspect only active owners, remove
stale or disarmed entries, and release the lock before delivering signals.
The lock order is registry then TCB inner on timer paths; syscall and teardown
paths release TCB inner before acquiring the registry, so no reverse lock edge
is introduced. With no active interval timers, the tick path is a short empty
scan instead of an allocation plus a full historical-task walk.

This is a single production hypothesis. It does not modify scheduling policy,
VmaMap/ResidentSet/PageTableOps/TlbProtocol, VFS, block cache, or guest time.
The four release cfg builds passed (`capability-pass`), and RISC-V64 SMP8
resident/high-arena/user-memory lifecycle passed. The LoongArch64 lifecycle
gate remained `unverified` after 120 seconds at the pre-existing high-arena
phase with no failure marker. A same-image RISC-V64 production 300-second
window is the next gate; success requires a measurable progress improvement,
no correctness regression, and still does not claim BuildStorm completion
without `BUILDSTORM_COMPILE mode=multi ok=true`.

The 300-second RISC-V64 candidate window completed without panic/OOM and
reached 23 compile events, exactly the prior early boundary. Measured progress
gain is therefore 0%, below the optimization threshold. QMP still gives a
useful falsification result: `live_tasks()` disappeared from sampled PCs, and
the residual samples moved to the scheduler's `wfi`/idle-mask publication
path. The interval-timer index remains as a structural, semantics-preserving
cleanup, but it is not a measured performance win. A regression-only timer
state-machine probe passed one-shot disable and periodic catch-up semantics on
RISC-V64 together with the SMP8 lifecycle suite (`capability-pass`). The next
candidate must isolate idle/wakeup behavior and must not mix scheduler changes
into this timer/MM experiment.

## 38. Stage 2 ext4 namespace transaction architecture (2026-08-14)

A same-image 900-second production window crossed the old `hashbrown` boundary
but rustc failed `bitmaps` with `failed to create file encoder: No such file or
directory`. The failure was intermittent and QMP did not show a stable MM,
allocator, scheduler, or VFS PC. Static audit instead found that unlink and
rename resolved names before serializing the complete namespace mutation, and
that a reader could retain an old directory snapshot, cross writer cache
invalidation, then republish a stale positive or negative path result.

Diagnostics confirmed the race window. The pre-fix 900-second run recorded 146
positive and 170 negative namespace-generation crossings before an encoded
metadata ENOENT. The fix introduces one architecture-neutral namespace
transaction boundary in `ext4_vol`: concurrent lookups hold a shared `RwLock`
from cache acceptance through traversal and publication; create, mkdir,
symlink, hardlink, unlink, rmdir, rename and exchange hold exclusive ownership
from first resolution through ext4 mutation, invalidation and generation
increment. Writer entry uses the `spin` upgradeable slot so new readers stop
while existing readers drain. Already-locked helpers prevent recursive lock
acquisition. Rename destination removal and three-step exchange remain inside
one outer write transaction. Regular-file data/writeback and the MM,
scheduler, PTE/TLB, frame and guest-time protocols are unchanged.

The independent `smp-regression` probe runs only under its explicit feature.
Seven pinned readers stress one lookup while a writer performs 64
negative-to-positive/create-unlink transitions with epoch acknowledgements,
then verifies same-parent and cross-parent rename, open-unlink inode lifetime,
and final cache invalidation. RISC-V64 and LoongArch64 both passed this probe
and the existing interval-timer, resident/high-arena/user-memory,
COW/shared/file, ASID/TLB, heap-stress and SMP8 gates (`capability-pass`). All
four production/diagnostics release cfg builds passed. A post-fix 300-second
diagnostics run produced 34 complete snapshots with zero positive and zero
negative generation crossings.

The comparable RISC-V64 production counts were 23/23 compile events at 300
seconds, 90/91 at 600 seconds and 129/131 at 900 seconds. The candidate crossed
`bitmaps`, reached `flatten_objects`, and did not reproduce ENOENT, panic or
OOM. The 900-second count change is about 1.6%, so no performance success is
claimed and this is not the next throughput candidate. The correction is
retained under the user-directed positive roll-forward rule because it closes
a general namespace consistency defect without a measured regression or new
QMP lock hotspot.

Evidence is under
`docs/evidence/buildstorm-stage2/20260814-stage2-post-hashbrown-namespace-audit.md`,
`20260814-stage2-namespace-transaction-gates/`,
`20260814-riscv64-diagnostics-stage2-namespace-transaction-window300/`, and
`20260814-riscv64-production-stage2-namespace-transaction-window900/`. The
production serial SHA-256 is
`1256c1ad4911f5e8d9fbf1e6e8b2ffaaa65a618977bf2900a4dac1a409a1d83d`;
the unchanged RISC-V image SHA-256 is
`d74e436522f5946ca17280a7a25f17dbb6604b71fe675bb8a021ce8e849b334c`.
The rechecked official `final-2026` ref is
`b5ec6ef8497e1818cbdec3b54bb722f036e57972`.

AI assisted with evidence correlation, namespace/cache race modeling, lock
architecture, implementation, regression design, controlled dual-architecture
execution, and timeline comparison. Developer-verifiable artifacts include
the exact dirty diff, source/kernel/image hashes, four build logs, dual-arch
SMP logs and JSON, raw serial logs, QMP samples, and runner arguments.
Complete BuildStorm remains `unverified / not completed`; there is no exact
`BUILDSTORM_COMPILE mode=multi ok=true` marker.

## 39. Dual-architecture official full-build completion (2026-08-14)

The Stage 38 namespace transaction architecture crossed the prior intermittent
rustc metadata ENOENT boundary in complete production runs. Both runs used the
clean official `final-2026` suite at
`b5ec6ef8497e1818cbdec3b54bb722f036e57972`, QEMU 11.0.3, the original public
images in snapshot mode, production kernels with diagnostics disabled, and
the required `-m 8G -smp 8` configuration. No QEMU instances overlapped.

RISC-V64 completed with the exact marker
`BUILDSTORM_COMPILE mode=multi ok=true elapsed_s=1322.99 cores=8
bytes=1683456 arch=riscv64`. Its kernel SHA-256 is
`071116d8e795e2340ea2de65d515508f26fb4aecc91ec41248d88a007a9311fe`,
and the official judge passed all scripted entries for 180/180 using its
1616-second baseline. LoongArch64 completed with
`BUILDSTORM_COMPILE mode=multi ok=true elapsed_s=1103.14 cores=8
bytes=1716224 arch=loongarch64`. Its kernel SHA-256 is
`deaa5ce7b492783e6cdabeaae8ddba767ad791170a049d6f7a29f79eb14adbf9`,
and the official judge passed all scripted entries for 180/180 using its
1985-second baseline.

The LoongArch judge emitted a non-failing warning that it expected 12 cores.
The warning is retained verbatim. The published BuildStorm gate and this
campaign require eight vCPUs and 8 GiB, and the runner JSON records exactly
that resource identity; a future evaluator rule change would require a new
run rather than reinterpretation of this evidence.

The RISC-V and LoongArch public image SHA-256 values are respectively
`d74e436522f5946ca17280a7a25f17dbb6604b71fe675bb8a021ce8e849b334c`
and `d1410544e677e11efb1c240be6ffb201c89d6de58c9675e73314a696e4cefdc5`.
The detached dirty source remains based on
`055df99ff554441f7699c3518b1a4b204bd9c265`; its retained raw binary diff
SHA-256 is
`c22bc0a79e78a72a0c3c8dd13bf23c33692c5fe0991cc57ef0b1ce91c3c2a22d`.

All four production/diagnostics release cfg builds and both architecture SMP8
namespace, resident/high-arena/user-memory, COW/shared/file, ASID/TLB, timer,
heap-stress and CPU-execution regressions had already passed as
`capability-pass`. The two complete official image/script/judge runs now raise
full BuildStorm clean compilation to `official-pass`. The manual 20-point
design-document review is not asserted by the automated result.

Raw evidence is under
`docs/evidence/buildstorm-stage2/20260814-riscv64-official-complete-stage2/`,
`20260814-loongarch64-official-complete-stage2/`, and the consolidated report
`20260814-stage2-buildstorm-official-completion.md`. AI assisted with audit,
implementation, regression design, controlled execution and evidence capture;
the source diff, serial logs, kernel/image/suite hashes, runner arguments and
official judge outputs are retained for developer verification.

## 40. 2026-08-15 VFS sparse-cache and clustered writeback stability candidate

The evaluator-side RISC-V run that remained at `ax-mm` beyond 3000 seconds was
`unverified`: it had no panic/OOM/device error and no successful compile marker.
On remote host `47.110.253.40`, the official `final-2026` suite at
`b5ec6ef8497e1818cbdec3b54bb722f036e57972` was rerun with the public images,
QEMU 11.0.3, `-snapshot -m 8G -smp 8`, and production diagnostics disabled.
The validated dirty patch has stable patch-id
`8701497fa79e31b4266772f7695149dafcd52c93` and source commit
`2acfed66b145ee0c48b55cc1046513ab20290a17`.

The patch changes only the general ext4 regular-file data path, shared-file
sync deduplication, and an independent lifecycle regression. Files larger than
8 MiB use page-granular sparse cache entries instead of a whole-file `Vec`; dirty
data is written in 256 KiB ranges, contiguous complete blocks are clustered,
and truncate/partial-block handling preserves zero-fill semantics. No VMA,
resident ownership, PTE, TLB, scheduler, or guest-time policy is changed.

The official results are:

| arch | exact marker | judge | image SHA-256 | kernel SHA-256 |
| --- | --- | --- | --- | --- |
| RISC-V64 | `ok=true elapsed_s=862.17`, `860.15`, and `867.16` | 180/180 | `d74e4365...b334c` | `ff426fed...aad41` / `eb1518f5...43fb8` |
| LoongArch64 | `ok=true elapsed_s=674.49` and `687.23` | 180/180 | `d1410544...fdc5` | `2c1c51be...31ee` / `a5c6dc1f...7ba8` |

The earlier Stage B heap-cache run was faster (`800.55s` / `660.71s`) but the
submitted evaluator later exhibited a >3000-second long tail. The present
candidate is therefore recorded as a reliability/stability candidate, not as a
throughput improvement. Both architectures passed the regular-file sparse,
truncate, fsync, rename and open-unlink lifecycle under `smp-regression`; all
four release/diagnostics cfg builds passed. Evidence directories are the three
20260815 `vfs-*` official runs plus `20260815-vfs-hybrid-release-gates-retry1`
and `20260815-vfs-hybrid-smp-gates`.

The local `os/src/fs/vfs.rs` cache-size/eviction experiment is intentionally not
part of this patch or its attribution; it remains `unverified` and must be
reviewed as a separate experiment. AI assisted source/evidence correlation,
design, implementation and test orchestration; all hashes, raw logs, commands
and judge output are retained for manual verification. The official score is
locked only by the exact markers above; the manual document score is not
claimed here.

## 41. VFS lookup reuse and bounded working-set retention (2026-08-15)

The previously isolated `os/src/fs/vfs.rs` experiment was validated separately
on base `c80c638594216c6e3dda109d85bb492c0c7b8195`. Its diff SHA-256 is
`94feb228a41f07ee09ac10e4688343333a5056f7113bd998be8a5648a2cc819d`.
The causal model was repeated ext4 kind lookup inside `open_path()` plus complete
parent/readlink cache clearing when a compiler working set crossed 16K entries.
The retained design reuses one generation-validated `(inode, kind)` value,
raises those two bounds to 64K, and evicts one entry at capacity. Namespace
invalidation, tmpfs routing, symlink policy, inode lifetime and MM ownership are
unchanged.

Comparable production results improved from `860.15-867.16s` to `774.62s` on
RISC-V64 (9.94-10.67%) and from `674.49-687.23s` to `620.70s` on LoongArch64
(7.97-9.68%). Both runs used the unmodified public images, QEMU 11.0.3,
`-snapshot -m 8G -smp 8`, production diagnostics disabled, official suite
`b5ec6ef8497e1818cbdec3b54bb722f036e57972`, and contain the exact
`BUILDSTORM_COMPILE mode=multi ok=true` marker. The public judge reports 180/180
scriptable points; the manual document score is not asserted.

All four production/diagnostics release cfg builds passed. Both architecture
SMP8 regressions passed namespace, regular-file, resident/high-arena,
user-memory, TLB/ASID, timer, heap-stress and 320 MiB large-allocation phases.
RISC-V64 musl separately completed with `BUILDSTORM_RESULT status=OK rc=0` at
`783.15s` and is `capability-pass`. The public LoongArch image has no
`/musl/buildstorm_testcode.sh`, verified read-only with `debugfs`, so that
combination remains `unverified` rather than being fabricated or misreported.

AI assisted evidence correlation, implementation, controlled QEMU execution
and documentation. Developer-verifiable artifacts include source and binary
hashes, raw serial logs, runner JSON, official judge output and lifecycle logs.
No official image, suite, guest script, marker, judge, guest time, or
workload-dependent production branch was changed. The compact evidence index
is `docs/evidence/buildstorm-stage2/20260815-vfs-lookup-reuse-validation.md`.

## 42. Large-memory boot and synchronous SIGILL compatibility (2026-08-15)

The LoongArch64 evaluation log exposed two independent startup capacity bugs.
At 36 GiB, a flat per-frame `AtomicUsize` table requested about 72 MiB as one
allocation; the buddy allocator rounded that request to a 128 MiB order before
dynamic heap expansion was available. The replacement stores `AtomicU32`
counters in chunks of at most `2^18` frames, keeping each production allocation
at about 1 MiB. `FrameTracker` clone/drop/raw-transfer semantics and allocator
domains are unchanged. Separately, twelve 128 KiB secondary stacks require
1.5 MiB, while both assembly entries reserved only 1 MiB. Both entries now
reserve twelve slots, and Rust checks the linker-symbol span before starting
secondary CPUs.

The RISC-V64 evaluator then completed compilation but failed its nested-QEMU
run gate with an empty run log. A derived, non-official capability image and
diagnostic kernel localized the first fault to QEMU's `cpuinfo_init`: QEMU had
installed a `SIGILL` handler and intentionally executed an extension probe.
The kernel used to terminate the process instead of delivering the synchronous
fault. The shared signal layer now constructs the existing Linux signal frame
for an installed, unblocked handler, preserving the faulting context so
`rt_sigreturn` can resume at the PC selected by userspace. Default, blocked,
and ignored synchronous faults terminate instead of livelocking.

The independent probe now dynamically launches QEMU, executes OpenSBI, and
loads a real wll_OS kernel through `[kernel] Hello, OS!`. This is
`capability-pass`, not an official score claim: the probe rootfs/image is
derived and contains no official marker logic. Four production/diagnostics
release configurations, dual-architecture SMP lifecycle tests, LoongArch64
36G/12 startup and minibuild, and RISC-V64 public minibuild have passed. The
RISC-V64 16G/8 and LoongArch64 36G/12 public clean builds completed with
`BUILDSTORM_COMPILE mode=multi ok=true` at `786.62s` and `650.22s`; the current
official judge parsed each raw serial log as 180/180. These unmodified public
suite runs are `capability-pass`, because their resources differ from the
published 8G/8 scoring configuration. The public README still says 8G/8, while the
executable judge expects 8 RISC-V CPUs and 12 LoongArch CPUs and the evaluator
logs use 16G/8 and 36G/12. This upstream resource-description conflict is
recorded rather than hidden. The unavailable evaluator-only nested-QEMU gate
remains `unverified`; the independent nested-QEMU regression is only
`capability-pass`.

No official image, suite, guest script, judge, marker, or guest clock was
modified. No production branch inspects a crate name, path, command, QEMU name,
or expected output. The evidence index is
`docs/evidence/buildstorm-stage2/20260815-large-memory-smp-sigill-validation-cn.md`.

## 43. Evaluator routing, LoongArch mkdir ABI, and clean-page batching (2026-08-16)

The scored tables contain only glibc, but the RISC-V evaluator log launched
`/musl/buildstorm_testcode.sh` after the successful glibc group. This was an
unscored second workload, not the `*-unknown-linux-musl.json` Rust target used
inside the required ArceOS build. The default harness now selects glibc only;
`musl` and `both` remain explicit capability modes.

The LoongArch timed build itself completed in 2020.04 seconds. Its first real
failure was legacy asm-generic `mkdir(1030)` returning ENOSYS while preparing
the EFI directory. The new dispatch delegates to the existing
`mkdirat(AT_FDCWD, ...)` implementation, preserving one permission, umask,
pathname, ext4-transaction, and namespace-invalidation path. A feature-gated,
independent directory ABI regression passes on both architectures.

The latest complete diagnostics rank anonymous demand faults at 77.891 seconds
and clean-file cache fault resolution at 71.482 seconds; COW is 5.176 seconds
and aggregate MemorySet wait is about 80 milliseconds. The retained VFS change
batches the sixteen-page clean-cache prefix check and contiguous hit collection
under one lock acquisition each. Read-ahead size, exact LRU, cache capacity,
invalidation, I/O, FrameTracker ownership, ResidentSet, PTE, and TLB protocols
are unchanged.

RISC-V batch runs completed at 766.54 and 789.87 seconds versus a 786.62-second
same-host baseline; the mean improvement is 1.07% and is not stable beyond host
noise. LoongArch completed at 642.70 seconds versus 646.32 seconds. A
second-chance replacement regressed to 836.88 seconds and was removed; range
invalidation variants at 768.04/767.32 seconds did not beat batch. The final
public runs use 16G/8 and 36G/12 respectively and both parse as 180/180. Four
production/diagnostics cfg checks and both SMP lifecycle regressions pass. The
resource-mismatched public runs are `capability-pass`; the new evaluator-hidden
result remains unverified until resubmission.

AI assisted evidence correlation, causal-model design, implementation, and
single-QEMU A/B orchestration. Developer-verifiable artifacts include raw
serial, judge output, cfg logs, SMP logs, source/image/kernel hashes, and the
production diff. No official image, suite, script, marker, judge, guest time,
or workload-dependent production branch was changed. The evidence index is
`docs/evidence/buildstorm-stage2/20260816-evaluator-la-mkdir-clean-cache-validation-cn/README.md`.

## 44. LoongArch64 directory-fd ABI after the evaluator build (2026-08-16)

Evaluator commit `2953af6a725c015f89e64db9eab8f60ee27f6fe1` finished the
LoongArch64 Rust build in 2439.63 seconds. The earliest later failure was GNU
coreutils `mkdir -p` returning `ENOSYS` while creating `/work/buildstorm.esp`;
the missing directory caused the following `cp` errors and prevented the final
BuildStorm result marker. Source and ABI correlation identified asm-generic
`fchdir(50)` as the missing general capability, rather than a compiler, MM, or
LoongArch platform failure.

The shared syscall layer now implements `fchdir(50)` using the existing
directory-descriptor resolver and process root/cwd state. It also implements
`fchmodat2(452)` through the existing FD/path metadata operations, including
bounded `AT_EMPTY_PATH` and `AT_SYMLINK_NOFOLLOW` handling. The change does not
own directory objects, bypass namespace invalidation, alter architecture MM,
or inspect workload names, paths, commands, or output.

All four RISC-V64/LoongArch64 production/diagnostics release checks passed.
Both SMP8 regressions passed a feature-gated glibc/coreutils directory metadata
probe and their final CPU/lifecycle marker. An unmodified public LoongArch64
8G/8 run completed with
`BUILDSTORM_COMPILE mode=multi ok=true elapsed_s=553.62`; the official judge
parsed 180/180 automated points. This is `official-pass` for the public flow.
The evaluator-only UEFI preparation remains `unverified` until resubmission,
and the cross-host elapsed times are not used to claim a timing improvement.

AI assisted log/source correlation, causal modeling, implementation, and
serial-QEMU evidence collection. Raw serial, runner metadata, hashes, judge
output, build logs, and the independent regression are indexed under
`docs/evidence/buildstorm-stage2/20260816-la-fchdir-capability-gates/` and
`docs/evidence/buildstorm-stage2/20260816-la-fchdir-official-public-complete/`.

## 45. Anonymous resident retirement for MADV_DONTNEED (2026-08-16)

The evaluator emitted 135 jemalloc fallback warnings because syscall 233
returned success without discarding memory. The retained implementation gives
`MADV_DONTNEED` Linux-compatible behavior for private anonymous residents while
leaving VMA topology, shared mappings, SysV memory, and file mappings intact.

Ownership remains layered. `ResidentSet` extracts VMA-relative owners but does
not edit page tables. `PageTableOps` batches leaf revocation and reuses one walk
per 2 MiB leaf table. `MemorySet` publishes the PTE writes, performs the local
flush and remote root-keyed shootdown, and only then releases retired frame
owners. A later fault allocates a zeroed page under the existing VMA policy.
`AnonymousShared` is classified separately so fork/clone sharing and discard
policy cannot be confused with private anonymous memory.

The LA diagnostic window observed 1,481 calls, 190,954 requested pages, and
32,904 discarded pages; the fallback warning count became zero. The isolated
same-host Stage17 A/B improved from 556.35 to 547.73 seconds (1.55%). The exact
submission tree excludes the rejected Stage16 syscall-return/CPU-index fast
path. Its complete unmodified 8G/8 runs finished at 558.27 seconds on
LoongArch64 and 686.50 seconds on RISC-V64; both official judges parsed
180/180. Relative to clean `12f110ae` at 623.15 seconds, the final LoongArch64
tree is 10.41% faster. Four release cfg checks and both SMP8 lifecycle suites
passed. The complete evidence, seven-file Stage17 incremental patch, and
19-file final cumulative patch are under
`docs/evidence/buildstorm-stage2/stage17-madvise-dontneed-official-20260816/`.

AI assisted with evidence correlation, invariant review, implementation, and
single-QEMU A/B orchestration. The official image, suite, guest script, judge,
markers, guest clock, and workload-independent production behavior were not
modified.
