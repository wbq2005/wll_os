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
