# BuildStorm 内核设计与优化累计记录

## 1. 文档目的与评分映射

本文是 2026 年全国大学生操作系统比赛内核赛道决赛 BuildStorm 题目 2.3 的人工评审材料。内容按官方 `final-2026` README 的四项要求组织：

| 官方要求 | 分值 | 本文对应章节 |
| --- | ---: | --- |
| 问题定位与根因分析 | 6 | 第 3、4 节 |
| 修复或优化的设计与实现 | 6 | 第 5、6 节 |
| 修改前后实验分析 | 4 | 第 7 节 |
| AI 使用说明与可复现步骤 | 4 | 第 8、9 节 |

提交 `6045dd2534668ea4d1cff94725c41afb0a855af4` 的结论为双架构
`official-pass`。RISC-V64 与 LoongArch64 均使用未修改的官方镜像、镜像内原始
脚本、官方 judge、QEMU `-snapshot -m 8G -smp 8` 和 production 内核完成从零
编译，并输出精确成功标记：

```text
BUILDSTORM_COMPILE mode=multi ok=true
```

官方脚本自动项在本次环境中均为 180/180。设计文档 20 分由人工评审，本文不
自行宣称该 20 分已经获得。第 13 节所述本轮 candidate 必须使用自己的
验证结果，不能直接继承这一历史 `official-pass`。

## 2. 权威输入与结果身份

- 内核基线提交：`055df99ff554441f7699c3518b1a4b204bd9c265`。
- 官方测例仓库：`https://github.com/oscomp/testsuits-for-oskernel`。
- 验证时 `final-2026` 提交：`b5ec6ef8497e1818cbdec3b54bb722f036e57972`。
- 官方 guest 脚本 SHA-256：`2f656a668076803fb465409374b6bcbb1fcbbc4f5c17a72b8ea4695668b9b33e`。
- 官方 judge SHA-256：`f9bc3c5c640217947775759b5b02aa4ceedfa76728d25d4f06d94ef5bc9d64dd`。
- RISC-V64 镜像 SHA-256：`d74e436522f5946ca17280a7a25f17dbb6604b71fe675bb8a021ce8e849b334c`。
- LoongArch64 镜像 SHA-256：`d1410544e677e11efb1c240be6ffb201c89d6de58c9675e73314a696e4cefdc5`。
- QEMU：11.0.3。
- Stage B 完整运行时源码 dirty diff SHA-256：`af84c6980faa9b3aef986a1a0c14f382d4f0c4f756de6641d24532eb5ce611ca`。

原始串口、runner JSON、构建日志、镜像/内核/源码哈希和 judge 输出保存在：

- `docs/evidence/buildstorm-stage2/20260815-riscv64-official-complete-stageb-percpu-heap-cache/`
- `docs/evidence/buildstorm-stage2/20260815-loongarch64-official-complete-stageb-percpu-heap-cache/`
- `docs/evidence/buildstorm-stage2/20260815-stageb-percpu-heap-cache-conclusion-cn.md`

## 3. 问题定位方法

### 3.1 先判定能力边界，再讨论性能

BuildStorm 不是单一编译器基准。它同时放大动态链接、进程生命周期、SMP 调度、虚拟内存、页表与 TLB、文件系统命名空间、文件缓存和同步原语的问题。开发过程始终寻找最早的真实失败边界：

1. 保存原始串口和完整 QEMU 参数。
2. 用官方 marker 区分 toolchain、minibuild 和 full build。
3. 将失败翻译为通用内核能力，而不是按 crate 名或测试路径分支。
4. 在 `buildstorm-diagnostics` 下加入有限聚合诊断。
5. 用独立生命周期回归证伪假设。
6. production 对比只改变一个内核假设。

### 3.2 证据分级

- `official-pass`：未修改官方镜像和脚本、官方 judge、规定资源、原始串口和成功 marker 全部满足。
- `capability-pass`：独立 ABI、SMP 或生命周期回归通过，但不能折算官方得分。
- `unverified`：短窗口、诊断版本、不同配置或没有精确成功 marker。

这一规则避免把“启动到某个 crate”或“进度更快”误报为编译成功。

## 4. 累计根因分析

### 4.1 syscall 期间过早恢复用户页表

早期运行在启动脚本后没有执行第一条命令。根因并非 shell，而是 syscall 包装层在 VFS/VirtIO 内核操作结束前恢复用户页表。低地址用户映射可能覆盖 QEMU virt MMIO，随后内核块设备访问在错误页表下执行。

修复后的不变量是：syscall、VFS、块设备和调度器内部始终使用内核地址空间；只有真正返回用户态前才激活目标 `MemorySet`。

### 4.2 cargo/rustc 所需 Linux ABI 不完整

工具链能启动后，cargo 子进程在 pipe、Unix socket、文件锁和子进程等待路径不能收敛。根因包括：

- pipe/socket/eventfd 缺少 `FIONBIO`，pipe 缺少 `FIONREAD`；
- Unix `SOCK_SEQPACKET` 缺少消息边界和 peer 关闭后的 EOF；
- `flock` 没有 open-file-description 所有权语义；
- child exit 与 wait 存在发布/观察竞态；
- `vfork` 与普通 `fork` 的地址空间生命周期没有完全分离。

这些能力按 Linux ABI 实现，不检查 cargo、rustc、crate 名或命令字符串。

### 4.3 LoongArch64 大型 PIE 后部读取损坏

LoongArch64 的 rustup/rustc 大型 PIE 在后部动态表附近出现错误 relocation。只读提取显示 ELF 头部正确而后部内容错误。根因是旧 ext4 `read_at` 对多层 extent tree 的后部逻辑块解析不完整。

修复采用 extent-aware 读取路径，适用于任意大文件，而不是识别 rustc 路径。

### 4.4 用户内存所有权与 COW 生命周期

长跑暴露出 `fork`、`exec`、退出、匿名/文件映射和共享内存之间的所有权压力。旧设计容易把 VMA 策略、已驻留页和页表条目混为一体，析构或 COW 时可能重复释放或扫描无关空洞。

当前设计将职责分开：

- `MapArea` 描述虚拟区间、权限和 backing；
- `ResidentSet` 以稀疏连续 run 持有实际驻留页；
- `FrameTracker` 的原子引用计数表达物理页共享；
- 页表只表达硬件映射，不拥有用户数据页；
- 架构代码负责激活、ASID 和 TLB 语义。

### 4.5 mmap 低地址空间耗尽

诊断长跑显示 mmap 失败集中于 VA 选择而不是物理内存耗尽。低 arena 的历史布局无法容纳编译器大规模、长生命周期、碎片化映射。解决方案是在两个架构都可用的正 canonical 用户地址范围增加独立高 mmap arena；低 arena 保留兼容性，高 arena 只在低区无法满足时使用。

### 4.6 调度器 idle-vCPU 唤醒风暴

host thread 采样发现一个有用 vCPU 接近满载时，其余 idle vCPU 也因每次 ready-queue 发布的广播 IPI 占用约 88% CPU，并产生每秒数万次自愿切换。guest 计数器看不到这一 TCG 放大效应。

根因是本地时间片重排队与外部 runnable 发布没有区分。当前调度器只对真正的外部发布，从 idle mask 中原子领取一个满足 affinity 的 CPU 并发送一次 IPI；本地重排队不唤醒其他 CPU。队列使用 PID 集合去重，重复 wakeup 不产生 IPI。

### 4.7 interval timer 全任务扫描

QMP 返回地址把周期性内核热点定位到 `live_tasks()`：每个 timer tick 都扫描历史 weak task registry 来寻找活动 interval timer。修复后维护只包含活动 timer owner 的弱引用索引，tick 只检查该小集合；投递信号前释放索引锁。

### 4.8 ext4 命名空间发布竞态

900 秒 production 运行曾在 `bitmaps`/`flatten_objects` 附近报 rustc metadata `ENOENT`。诊断记录到 lookup 在 writer 修改期间跨越 namespace generation：

- positive crossing：146；
- negative crossing：170。

静态审计确认 unlink/rename 在完整 mutation lock 外解析父目录和目标，reader 还可能在 writer 失效缓存后重新发布旧的 positive/negative cache。

根因不是单个 ext4 API，而是缺少从解析到修改再到缓存失效的统一命名空间事务。

### 4.9 kernel heap 中央锁与小对象 buddy 合并

Stage A 完成后，900 秒 diagnostics 已能走到 `flatten_objects`，但仍记录到 33,741,372 次 heap 锁获取和 5,004 次竞争。QMP 寄存器样本中反复出现中央 `buddy_system_allocator::Heap::dealloc` 合并与自旋簇。BuildStorm 的 cargo/rustc 生命周期产生大量 8 B 至 4 KiB 短命内核对象；旧 `LockedHeap` 让每次分配和释放都进入同一个中央 buddy 锁，并在释放时执行可能跨多个 order 的合并。

该热点不是 guest 某个 crate 的特例，而是通用 kernel heap 架构问题。因果模型是：小对象高频分配 -> 全局锁串行化 -> buddy split/merge 放大临界区 -> 多 vCPU 在 TCG 下反复自旋。若模型成立，批量化中央交互应显著降低锁获取和 QMP 自旋，并同时改善两个架构的完整 production 时间。

## 5. 当前修复与优化设计

### 5.1 地址空间与页帧所有权

- `UserVaLayout` 把低用户区、低 mmap arena、高 mmap arena 和用户上限集中定义。
- `ResidentSet` 用 `BTreeMap<run_start, ResidentRun>` 表示稀疏驻留页，split/extract 不扫描整个虚拟区间。
- 每个受管页帧有分块 `AtomicU32` 引用计数；`FrameTracker::clone/drop` 负责共享和最终释放。
- 页表页从唯一 `FrameTracker` 显式转交 raw page-table ownership；共享 frame 禁止转 raw。
- diagnostics 版本额外记录 free/tracked/page-table/contiguous 所有权状态，production 不包含该影子表。

### 5.2 fork、COW、exec 与 exit

- private writable 匿名/文件映射在 fork 时转为 COW；只读映射直接共享。
- shared/file-shared 映射保持共享语义，脏页状态由 resident page 记录。
- exec 先构建新 `MemorySet`，成功后原子替换，再按固定次序退休旧 address-space identity、页表和 resident owner。
- exit 先从可调度/等待结构发布终止状态，再释放进程资源；wait 使用统一事件观察，避免丢失子进程退出。

### 5.3 页表与 TLB 平台边界

共享 MM 层调用 PolyHAL 的 `PageTableWrapper`。RISC-V64 与 LoongArch64 分别实现 PTE 格式、root 激活和局部/全局 TLB 操作。跨 CPU 修改通过平台层 shootdown；MM 层不直接写架构寄存器。

### 5.4 SMP 调度与等待

- 用户和内核 runnable 队列分离，`VecDeque` 保持顺序，`BTreeSet` 保证 PID 唯一。
- dequeue 时根据当前 affinity 和有效优先级选择任务。
- idle CPU 在复查队列前发布 idle bit；producer 先发布任务再领取 bit，保证不会丢失唤醒。
- `WaitQueue` 统一阻塞、事件计数和唤醒；futex、pipe、socket、child wait 复用相同调度边界。

### 5.5 ext4 命名空间事务

`NAMESPACE_LOCK` 是命名空间一致性的外层事务：

- lookup 持 shared lock，覆盖 cache 接受、ext4 遍历和 positive/negative 发布；
- create/mkdir/link/symlink/unlink/rmdir/rename/exchange 持 exclusive lock，从首次解析覆盖到 inode/dirent 更新和缓存失效；
- writer 通过 upgradeable slot 进入，阻止新 reader，避免 `spin::RwLock` writer 饥饿；
- `*_locked` helper 避免递归获取同一锁；
- regular file data/writeback 不持 namespace lock；
- generation 保留为防御性 publication invariant 和诊断证据。

rename 的目标替换和 exchange 的三步重命名位于一个 outer transaction 内，reader 不会看到临时名字。open-unlink 仍立即移除目录项，同时由 open inode 引用维持数据寿命。

### 5.6 文件缓存和大文件 I/O

- inode metadata cache 容量覆盖 BuildStorm 超过 50K inode 的 working set；
- clean page cache、physical-block run cache 和 executable image cache 分层，分别服务普通读取、extent 连续性和 exec；
- namespace/inode 写入执行明确失效；
- 大文件读取完整遍历 extent tree；
- ext4 bitmap/inode mutation 仍由独立 transaction lock 串行化。

### 5.7 诊断与 production 隔离

锁等待、阶段计数、frame owner shadow、namespace crossing 和 QMP 归因只在 `buildstorm-diagnostics` feature 下启用。production 默认关闭，不按测试名、crate、路径、命令、输出或 marker 改变内核行为。

### 5.8 IRQ-safe per-CPU kernel heap cache

kernel heap 在中央 buddy 之上增加 8 B 至 4 KiB 的 canonical size class cache：

- 每 CPU、每 class 最多缓存 64 个 block；miss 时最多批量 refill 16 个，满时批量 flush 16 个；
- cache 命中只获取当前 CPU 的小锁，中央 buddy 仍唯一拥有底层 heap range、大对象、动态扩展和最终 OOM 判断；
- 所有回中央的 block 都使用 `Layout(block_size, block_size)`，避免以原请求 layout 释放 canonical buddy block；
- cache 锁与中央锁不同时持有，跨 CPU free 进入执行 dealloc 的当前 CPU cache；
- 中央分配失败时 drain 全部有界 cache 后重试，避免可用内存被永久困在 CPU-local 层。

第一次实现扩大了中央批处理临界区，但没有约束本地中断，在 ext4 mount 前暴露同 CPU 中断递归进入 `spin::Mutex` 的风险。最终实现采用等价于 Linux `local_irq_save/restore` 的 `InterruptGuard`：进入全局 allocator 前保存本地 IRQ 状态并关中断，退出时按原状态恢复。它既固定 CPU 身份，也阻止 timer/VirtIO 中断在同一 CPU 递归获取 cache 或中央锁。

## 6. 被证伪或降级的候选

以下候选没有达到独立性能门槛，因此没有作为“性能成功”陈述：

- ready-queue 线性去重替换：改善复杂度，但 300 秒 crate 边界无变化；
- PTE/TLB batching：早期有局部变化，300 秒边界为 0%；
- mmap VMA 局部合并和多种 vacant-slot 策略：没有稳定跨越边界；
- 单独 interval-timer owner index：消除采样热点，但 300 秒进度为 0%；
- namespace transaction：900 秒 129 到 131 个编译事件，约 1.6%，定位为正确性修复；
- raw-frame owner shadow：两次诊断累计数百万合法转换，没有复现 owner violation，只作为能力证据。

这些结果保留在累计文档和 evidence 中，避免重复投入已经被数据削弱的方向。

## 7. 实验分析

### 7.1 完整官方结果

| 架构 | guest 编译时间 | 产物大小 | kernel SHA-256 | 官方 judge 自动项 |
| --- | ---: | ---: | --- | ---: |
| RISC-V64 | 800.55 s | 1,683,456 B | `ad49e2d...603210` | 180/180 |
| LoongArch64 | 660.71 s | 1,716,224 B | `a8395d2...09592a` | 180/180 |

两次串口都包含 toolchain、minibuild 和完整 compile 成功 marker，无 panic、OOM、filesystem error。RISC-V64 host runner elapsed 为 841.52 秒，LoongArch64 为 695.39 秒。LoongArch64 judge 仍输出其旧的“expected 12”警告，但官方 2026 BuildStorm 计分配置规定为 8 vCPU；本次 QEMU 参数、marker 和 runner JSON 均为 `-smp 8`，judge 自动项仍为 180/180。

### 7.2 与本次 judge 基线比较

本地官方 judge 使用的 Linux 基线分别为 RISC-V64 1616 秒、LoongArch64 1985 秒。只对本次保存的 judge 配置作比较：

| 架构 | judge 基线 B | wll_OS 时间 t | 时间降低 | B/t |
| --- | ---: | ---: | ---: | ---: |
| RISC-V64 | 1616 s | 800.55 s | 50.46% | 2.02x |
| LoongArch64 | 1985 s | 660.71 s | 66.71% | 3.00x |

正式评测机会在同机重测 Linux 基线，因此以上时间分只表示本次 `official-pass` 环境，不能替代最终评测机成绩。

### 7.3 修改前后进展

早期完整运行在 3000 秒或更长窗口内没有成功 marker，不能计算严格的“成功样本对成功样本”加速比。可复核的状态变化是：

- 修改前：仅 toolchain/minibuild 通过，full clean build 超时或在后期出现 ENOENT/生命周期错误；
- namespace 修复前 900 秒：129 个 compile 事件，`bitmaps` metadata ENOENT；
- namespace 修复后 900 秒：131 个 compile 事件，越过 `bitmaps` 到 `flatten_objects`，无 filesystem error；
- Stage A RISC-V64 完整成功：1322.58 秒；Stage B 为 800.55 秒，降低 522.03 秒、39.47%；
- LoongArch64 可比完整成功样本：1103.14 秒；Stage B 为 660.71 秒，降低 442.43 秒、40.11%；
- 最终：两架构均在 15000 秒官方窗口内完成并通过 judge。

调度器单 idle wakeup 优化把早期 QEMU aggregate CPU 从约 718%-720% 降至 105%-108%，idle vCPU 自愿切换从约 52K-70K/s 降至约 95-131/s；晚期有用并行工作时 aggregate CPU 约 215%-217%。它显著减少宿主浪费，但 1800 秒 crate 边界只改善约 0.05%，所以不把它夸大为编译吞吐主因。

### 7.4 Stage B allocator 归因

同为 RISC-V64 diagnostics、SMP8、900 秒窗口：

| 指标 | Stage A | Stage B | 变化 |
| --- | ---: | ---: | ---: |
| compile marker 数 | 129 | 136 并完整成功 | +7，且跨越终点 |
| 中央 heap 锁获取 | 33,741,372 | 929,604 | -97.24% |
| 中央 heap 锁竞争 | 5,004 | 7 | -99.86% |
| 小对象 cache hit/miss | 不适用 | 20,032,390 / 53,030 | 99.74% hit |
| QMP 中央 allocator 自旋簇 | 35/192 samples | 0/192 samples | 消失 |

Stage B diagnostics 在 864.74 秒输出完整成功 marker。`drained_blocks=0` 表明该次运行没有依赖 OOM drain 才完成；它只证明本次压力范围，不表示 drain 路径可以删除。完整数据和首次无 IRQ 约束的失败证据见 Stage B 结论文档。

## 8. AI 使用说明

AI 参与了以下工作：

- 阅读源码、dirty diff、官方脚本/judge 和历史证据；
- 生成可证伪的根因模型、验证矩阵和候选排序；
- 辅助实现通用内核修复、diagnostics 聚合和独立 SMP 回归；
- 组织双架构构建、单实例 QEMU、日志解析和哈希留存；
- 汇总设计、实验和复现文档。

人工/开发者可独立核验的部分包括：

- 所有 production 源码 diff；
- 四种 release cfg 构建日志；
- 双架构 SMP/用户内存/namespace lifecycle 串口；
- 官方镜像、suite、kernel、source diff 哈希；
- 两份原始官方串口和官方 judge stdout/stderr；
- 本文列出的完整复现命令。

AI 没有修改官方镜像、guest 脚本、judge、marker 或 guest 时间，也没有按测试名、crate、路径、命令和输出来选择 production 行为。

## 9. 可复现步骤

### 9.1 更新并核验官方测例

```bash
git -C /srv/buildstorm/src/testsuits-for-oskernel fetch origin final-2026
git -C /srv/buildstorm/src/testsuits-for-oskernel checkout final-2026
git -C /srv/buildstorm/src/testsuits-for-oskernel rev-parse HEAD
git -C /srv/buildstorm/src/testsuits-for-oskernel status --short --branch
sha256sum /srv/buildstorm/src/testsuits-for-oskernel/scripts/buildstorm_testcode.sh
sha256sum /srv/buildstorm/src/testsuits-for-oskernel/judge/judge_buildstorm-glibc.py
```

### 9.2 静态与双架构构建

```bash
git diff --check
cargo metadata --no-deps --format-version 1

cd os
WLL_HARNESS_GROUPS=buildstorm WLL_HARNESS_LIBC=glibc \
  cargo +nightly-2025-01-18 build --locked --offline --release \
  --target riscv64gc-unknown-none-elf
WLL_HARNESS_GROUPS=buildstorm WLL_HARNESS_LIBC=glibc \
  cargo +nightly-2025-01-18 build --locked --offline --release \
  --target riscv64gc-unknown-none-elf --features buildstorm-diagnostics
WLL_HARNESS_GROUPS=buildstorm WLL_HARNESS_LIBC=glibc \
  cargo +nightly-2025-01-18 build --locked --offline --release \
  --target loongarch64-unknown-none --no-default-features --features loongarch
WLL_HARNESS_GROUPS=buildstorm WLL_HARNESS_LIBC=glibc \
  cargo +nightly-2025-01-18 build --locked --offline --release \
  --target loongarch64-unknown-none --no-default-features \
  --features loongarch,buildstorm-diagnostics
```

### 9.3 独立 SMP 回归

```bash
python3 scripts/run_smp_regression.py --arch riscv64 --timeout 180
python3 scripts/run_smp_regression.py --arch loongarch64 --timeout 180
```

回归应包含 namespace lifecycle、resident/high-arena/user-memory、COW/shared/file、ASID/TLB、interval timer、heap stress 和八 CPU 执行成功行。

### 9.4 官方完整运行

启动前确认没有其他 QEMU：

```bash
pgrep -af 'qemu-system-(riscv64|loongarch64)|run_buildstorm.py' || true
```

然后顺序执行，禁止并发：

```bash
python3 scripts/run_buildstorm.py --arch riscv64 \
  --image /srv/buildstorm/images/sdcard-rv-pub.img \
  --stage complete --timeout 15000 --memory 8G --smp 8

python3 scripts/run_buildstorm.py --arch loongarch64 \
  --image /srv/buildstorm/images/sdcard-la-pub.img \
  --stage complete --timeout 15000 --memory 8G --smp 8
```

### 9.5 官方 judge

```bash
python3 /srv/buildstorm/src/testsuits-for-oskernel/judge/judge_buildstorm-glibc.py \
  _tmp/buildstorm-riscv64-complete.log
python3 /srv/buildstorm/src/testsuits-for-oskernel/judge/judge_buildstorm-glibc.py \
  _tmp/buildstorm-loongarch64-complete.log
```

完成条件不是进度或前缀，而是精确 `BUILDSTORM_COMPILE mode=multi ok=true`、judge compile pass、双架构构建与回归通过、provenance 完整。

## 10. 剩余限制

- 最终评测机 Linux 基线会重测，时间分可能变化。
- 当前页表更新仍以 4 KiB leaf 为主，尚未形成通用 batched edit API；历史 batching 候选没有通过性能门槛。
- VMA 元数据仍是 `MemorySet.areas: Vec<MapArea>`，高 arena 解决容量问题，但超大 VMA 集合的索引复杂度仍可继续演进。
- VisionFive 2 与 Loongson 2K1000LA 的平台识别边界已建立，但本文结果是 QEMU `official-pass`，不等同于实板完成。

当前内核分层、所有权和锁协议详见 `docs/kernel-architecture-overview-cn.md`。

## 11. 2026-08-15 评测超时复核与 VFS 稳定性候选

### 11.1 现象与证据等级

评测机提交日志曾在 `ax-mm v0.5.28` 后超过 3000 秒，没有
`BUILDSTORM_COMPILE mode=multi ok=true`，也没有 panic、OOM 或 VirtIO 错误。
这只能证明该次运行 `unverified`，不能证明内核必然死锁。远端
`47.110.253.40` 使用官方 `final-2026` 提交
`b5ec6ef8497e1818cbdec3b54bb722f036e57972`、原始镜像和 QEMU 11.0.3 复核后，
同一生产候选连续越过该边界并完成：

本轮官方成功 dirty diff 的 SHA-256 为
`6ab666171bfb50f3d2c704807a23d9567e442eda64da4c44edd74de384d48a73`，稳定
patch-id 为 `8701497fa79e31b4266772f7695149dafcd52c93`。

| 架构 | guest marker | judge | 镜像 SHA-256 | 内核 SHA-256 |
| --- | --- | --- | --- | --- |
| RISC-V64 | `ok=true elapsed_s=862.17`、`860.15`、`867.16` | 180/180 | `d74e4365...b334c` | `ff426fed...aad41` / `eb1518f5...43fb8` |
| LoongArch64 | `ok=true elapsed_s=674.49`、`687.23` | 180/180 | `d1410544...fdc5` | `2c1c51be...31ee` / `a5c6dc1f...7ba8` |

这些运行均为 `official-pass`；四种 release/diagnostics cfg 构建和双架构
SMP 生命周期回归为 `capability-pass`。完整 provenance 保存在：

- `docs/evidence/buildstorm-stage2/20260815-riscv64-vfs-clustered-sparsefix-official-complete/`
- `docs/evidence/buildstorm-stage2/20260815-riscv64-vfs-hybrid-official-complete/`
- `docs/evidence/buildstorm-stage2/20260815-loongarch64-vfs-hybrid-official-complete/`
- `docs/evidence/buildstorm-stage2/20260815-vfs-hybrid-release-gates-retry1/`
- `docs/evidence/buildstorm-stage2/20260815-vfs-hybrid-smp-gates/`

### 11.2 根因模型与实现边界

评测日志和源码交叉检查后，最强的通用模型是“大文件编译工作集触发的
整文件缓存、逐块回写和 ext4 extent 查找放大”，它可以使 guest 在高 CPU
下长时间没有新的 crate marker。候选把数据平面拆成四层：

1. 小于 8 MiB 的 regular file 保留 dense cache，保持常规读取的低开销。
2. 大文件改用按页的 sparse cache；缺页从 ext4 读取，写入只分配被修改页，
   不再为整文件保留连续 `Vec<u8>`。
3. dirty range 以 256 KiB cluster 分批回写，连续物理 extent 的完整块最多
   以 64 块一次提交；尾部 partial block 仍执行 read-modify-write。
4. truncate 清理新 EOF 后的 partial block，shared-file 写回按文件 identity
   去重同步，保证 POSIX 读取零填充、fsync、rename 和 open-unlink 语义。

该设计不改变 `VmaMap/MapArea`、`ResidentSet`、`PageTableOps` 或
`TlbProtocol` 的所有权边界，也不按 crate、路径、命令、输出或评测 marker
分支。`smp-regression` 的 regular-file lifecycle 在两架构均通过。

### 11.3 性能解释与限制

此前 Stage B per-CPU heap cache 的单次最快基线是 RISC-V64 `800.55s`、
LoongArch64 `660.71s`；本候选分别为约 `860s`、`687s`，因此它不是吞吐加速，
而是针对评测机长尾的稳定性候选。RISC-V64 多次 sparse/hybrid 运行都在
约 900 秒内结束，不能把单次 800 秒基线外推为 3000 秒必现成功，也不能把
本候选宣称为时间分提升。正式评测仍以同机 Linux 基线为准。

当前工作树另有 `os/src/fs/vfs.rs` 的 parent/readlink cache 改动；它没有
包含在上述官方成功 dirty diff（patch-id `8701497fa79e31b4266772f7695149dafcd52c93`）
中，状态仍为 `unverified`，不得与本候选一起提交或归因。

### 11.4 AI 使用与复现

AI 用于交叉核对 dirty worktree、官方脚本、源码调用关系、日志和 hash，
并辅助设计通用页缓存/回写结构及独立生命周期回归。人工可复核的证据包括
dirty diff、kernel/image/suite hash、四 cfg 构建日志、双架构 SMP 串口、
官方 serial 和 judge 输出。复现时先确认无 QEMU，再按顺序运行：

```bash
export BUILDSTORM_SUITE_DIR=/srv/buildstorm/src/testsuits-for-oskernel
bash scripts/run_buildstorm_long_baseline.sh riscv64 \
  /srv/buildstorm/images/sdcard-rv-pub.img /srv/buildstorm/evidence/<run>
bash scripts/run_buildstorm_long_baseline.sh loongarch64 \
  /srv/buildstorm/images/sdcard-la-pub.img /srv/buildstorm/evidence/<run>
```

脚本使用 `-snapshot -m 8G -smp 8`，不修改镜像、guest `/proc/uptime`、
官方 suite、judge、marker 或 guest timeout。当前完整 BuildStorm 结论为
`official-pass`；时间优化收益仍以正式评测机复测为准。

## 12. 2026-08-15 VFS lookup 复用与有界缓存

### 12.1 热点证据与根因

基于 `c80c638594216c6e3dda109d85bb492c0c7b8195` 的完整 diagnostics 对照为
`867.01s`。末态计数显示 `open_path()` 在同一路径上分别执行 directory、
regular-file 和最终 inode-kind 查询；parent-symlink 与 readlink cache 的 16K
上限到达后还会清空整张表。前者重复经过相同 namespace generation 下的 ext4
解析，后者把大于 16K 的编译工作集周期性退化为冷缓存。

候选只修改 `os/src/fs/vfs.rs`，dirty diff SHA-256 为
`94feb228a41f07ee09ac10e4688343333a5056f7113bd998be8a5648a2cc819d`：

1. `open_path()` 对非 tmpfs 路径只调用一次 `lookup_kind()`，复用可复制的
   `(ino, Ext4NodeKind)`。
2. parent-symlink 与 readlink cache 上限从 16K 提升到 64K。
3. 满载时只淘汰一个确定性条目，不再清空整个工作集。

该设计不改变 tmpfs 路由、最终 symlink 语义、rename/unlink 失效、ext4 inode
所有权、VMA/Resident/PTE/TLB 边界或 diagnostics 开关。namespace writer 仍负责
失效，lookup 结果只在原有调用生命周期内复用。

### 12.2 同配置性能结果

官方 suite 为 `final-2026` 提交
`b5ec6ef8497e1818cbdec3b54bb722f036e57972`，QEMU 11.0.3，官方原始镜像以
snapshot 模式运行，production diagnostics 关闭，配置为 `-m 8G -smp 8`：

| 配置 | 对照 | 候选 | 提升 | 证据等级 |
| --- | ---: | ---: | ---: | --- |
| RISC-V64 diagnostics | 867.01 s | 793.68 s | 8.46% | `unverified` 性能归因 |
| RISC-V64 production | 860.15--867.16 s | 774.62 s | 9.94%--10.67% | `official-pass` |
| LoongArch64 production | 674.49--687.23 s | 620.70 s | 7.97%--9.68% | `official-pass` |

两次 production 都包含精确
`BUILDSTORM_COMPILE mode=multi ok=true`，官方 judge 自动项为 180/180；人工
设计文档 20 分不在自动结果中自行计分。RISC-V64/LoongArch64 production
kernel SHA-256 分别为 `8821c921...f5c31` 与 `3241a943...c30d`。

diagnostics 中首热点按因果预测下降：

- `vfs_open_parent_symlink`：`49,802,733us -> 2,637,242us`；
- `vfs_open_final_symlink`：`6,714,761us -> 3,809,435us`；
- `vfs_open_ext4_regular_lookup`：`313,455us -> 7,945us`。

### 12.3 回归、libc 边界与剩余风险

四种 RISC-V64/LoongArch64 production/diagnostics release cfg 均通过。两架构
SMP8 独立回归均通过 namespace、regular-file、resident/high-arena、
user-memory、TLB/ASID、interval timer、160 MiB heap stress 与 320 MiB 大内核
分配阶段，证据等级为 `capability-pass`。

`mode=multi` 仅表示多核模式。官方决赛性能运行选择 glibc。RISC-V64 额外运行
官方镜像中的 `/musl/buildstorm_testcode.sh`，得到
`BUILDSTORM_RESULT mode=multi status=OK rc=0 elapsed_s=783.15`、group end、
`ALL TESTS DONE`、QEMU exit 0 且无 panic/OOM，记为 `capability-pass`。
LoongArch 官方镜像经只读 `debugfs` 核验不含该 musl 脚本，所以该组合保持
`unverified`，没有修改镜像补齐 workload。

当前淘汰策略是确定性有界策略而非 LRU。它消除了整表清空的性能悬崖，但后续
若重构为统一 VFS cache，应保留 namespace generation、精确失效和锁顺序，不能
让 cache 取得 inode、VMA 或页帧所有权。

### 12.4 AI 使用、人工核验与复现

OpenAI Codex / GPT-5 用于交叉检查 dirty worktree、源码、官方脚本和日志，建立
可证伪因果模型，执行实现、单实例双架构 QEMU 与证据整理。人工可核验内容包括
源码 diff、四 cfg 构建日志、SMP JSON/串口、kernel/image/suite hash、完整 QEMU
参数、官方 serial 与 judge 输出。AI 未修改官方镜像、suite、guest script、
judge、marker 或 guest 时间，也未按 crate、测试路径、命令或输出选择 production
行为。

复现命令沿用第 9.4 节；运行前必须确认没有其他 QEMU。证据索引见
`docs/evidence/buildstorm-stage2/20260815-vfs-lookup-reuse-validation.md`，原始目录
包括两架构 production、RISC-V64 diagnostics、四 cfg gate、双架构 SMP gate 和
RISC-V64 musl capability run。

## 13. 2026-08-15 大内存、12 核启动与嵌套 QEMU 修复

### 13.1 评测失败边界

LoongArch64 在 `-m 36G -smp 12` 启动时出现约 71 MiB 的 heap allocation
panic，并伴随高编号 CPU 非法取指。RISC-V64 则已经完成 timed build 和 ELF-to-BIN，
但评测的 nested-QEMU 阶段返回 `run=FAIL`，运行输出为空。两者都不是编译 crate
名称对应的特殊问题，而是更大资源拓扑和 Linux 同步异常 ABI 暴露的通用内核边界。

### 13.2 LoongArch64 根因与架构修复

36 GiB 对应约 943 万个 4 KiB frame。旧引用表把每个计数存为
`AtomicUsize`，并用单个 `Vec` 一次分配约 72 MiB；buddy allocator 必须寻找
128 MiB 阶，而该初始化发生在动态 heap 扩展前。当前实现采用：

1. 每个计数收紧为 `AtomicU32`，其上限远高于可实现的单页 owner 数；
2. 每块最多 `2^18` 个 frame，production 引用块约 1 MiB；
3. diagnostics owner shadow 同步分块，每块约 256 KiB；
4. `FrameTracker` clone/drop、最后引用释放、page-table raw ownership 和
   contiguous allocation domain 不变。

另一个独立根因是启动栈容量。`MAX_CPUS=12`、每槽 128 KiB，需要 1.5 MiB，
旧汇编只保留 1 MiB。第 9 至 12 个槽会越过 `_smp_boot_stacks` 并破坏相邻 BSS。
RISC-V64 与 LoongArch64 入口现在均预留 12 个槽；Rust 在启动 secondary CPU 前
用 `_smp_boot_stacks_end` 断言链接区间不小于配置需求，避免配置再次漂移。

### 13.3 RISC-V64 同步 SIGILL 根因与修复

派生开发镜像中的独立 probe 在 QEMU `--version` 阶段复现了首个用户异常：
QEMU `cpuinfo_init` 先调用 `sigaction` 安装 SIGILL handler，再执行候选扩展指令。
Linux 会把故障 PC 和 `ucontext` 交给 handler，handler 跳过不支持的指令后经
`rt_sigreturn` 继续；旧内核直接终止线程组，因此 nested-QEMU 的 run log 为空。

trap 层现在把可捕获的同步 `SIGILL` 交给共享 signal 层。已安装且未屏蔽的
handler 使用既有 Linux signal frame；默认 disposition 仍终止，blocked/ignored
同步异常也终止，避免返回同一 PC 形成 livelock。该路径不识别 QEMU、cargo、
crate、测试路径、命令、输出或 marker。独立 probe 已从动态装载继续到 OpenSBI，
并加载真实 wll_OS 内核至 `[kernel] Hello, OS!`。

### 13.4 验证矩阵与证据分级

| 验证 | 配置 | 结果 | 等级 |
| --- | --- | --- | --- |
| release build | RV/LA production + diagnostics | 四项通过 | `capability-pass` |
| SMP lifecycle | RV/LA 8G/8 | 两架构完整通过 | `capability-pass` |
| LA 大内存启动/minibuild | production 36G/12 | toolchain、minibuild 通过 | `capability-pass` |
| RV public minibuild | production 16G/8 | toolchain、minibuild 通过 | `capability-pass` |
| RV nested QEMU | 派生镜像，production | QEMU/OpenSBI/真实内核加载通过 | `capability-pass` |
| RV public full build | production 16G/8、3000 s harness | `ok=true elapsed_s=786.62`，judge 180/180 | `capability-pass` |
| LA public full build | production 36G/12、3000 s harness | `ok=true elapsed_s=650.22`，judge 180/180 | `capability-pass` |

旧提交 `6045dd2534668ea4d1cff94725c41afb0a855af4` 的 8G/8 双架构
`official-pass` 仍是历史有效证据，但没有被当前 candidate 借用。本轮重新使用
未修改的官方镜像、原始 public script、当前官方 judge 和评测日志中的实际资源
配置完成双架构运行；由于不是公开规则指定的 8G/8，这两次 public suite 运行
单独记为 `capability-pass`。

资源描述存在上游冲突：当前 public README 仍写 8G/8，当前可执行 judge 的
`EXPECTED_CORES` 却是 RISC-V64 8、LoongArch64 12，而评测机原始命令明确为
RISC-V64 16G/8 与 LoongArch64 36G/12。本文记录冲突并优先采用可执行 judge 与
实际评测配置，不把它静默改写成一致。隐藏评测额外的 nested-QEMU 启动门不在
public 镜像中；其独立回归是 `capability-pass`，最终隐藏评测状态仍是
`unverified`，必须由新提交的评测机结果确认。

RISC-V64 完整运行的 host wall time 为 `13:47.93`，最大 resident set 为
`3,679,820 KiB`，runner exit status 为 0。production kernel SHA-256 为
`8e2ffd8473b0359b178c3fc73dd0639b81bf924637ee26155f5ce939fae423e0`，
完整 source dirty diff SHA-256 为
`2830ac0c2953e90674ffabeb838f381c8f1346f5eb22afd1064b1b627c615bb8`。
LoongArch64 完整运行的 host wall time 为 `11:25.51`，最大 resident set 为
`4,292,876 KiB`，runner exit status 为 0。production kernel SHA-256 为
`61e523cbb2049e3430faf0a35718b425b679001b6d409a580642b661d82c5000`；
source dirty diff SHA-256 与 RISC-V64 相同。

原始远端证据目录与复现命令见
`docs/evidence/buildstorm-stage2/20260815-large-memory-smp-sigill-validation-cn.md`。
AI 用于交叉检查评测日志、反汇编、dirty diff、源码所有权和单实例 QEMU 结果；
人工可复核内容包括所有原始串口、kernel/image/source hash、完整命令和结果 marker。

## 14. 2026-08-16 评测路由、LA mkdir ABI 与 clean-page cache

### 14.1 评测日志结论

本轮得分 568.1，其中 RV BuildStorm 149.9，LA BuildStorm 20.0。评分表和可执行
judge 只包含 glibc。RV 在 `BUILDSTORM_RESULT ... elapsed_s=2020.80` 后又启动
`/musl/buildstorm_testcode.sh`，这是第二个不计分 workload；而 glibc 流程内部的
`*-unknown-linux-musl.json` 是被测 ArceOS 的 Rust target，属于正式编译，不能关闭。
默认 `HARNESS_LIBC=glibc` 只移除前者，显式 `musl/both` capability 入口仍保留。

LA 的 release 编译已在 2020.04 秒结束，首错是 EFI 目录创建调用 legacy
`mkdir(1030)` 返回 ENOSYS。修复复用 `sys_mkdirat(AT_FDCWD, ...)`，没有增加第二套
VFS 语义。2026-08-17 的新评测证明 `/work/buildstorm.esp` 已能创建，但独立的
`/work/buildstorm.vars.fd/vars.fd` 仍返回 `ENOTDIR`；因此撤回“全部 `vars.fd` 信息
都是 mkdir 派生错误”的旧判断。未修改 public LA 脚本完整输出 `ok=true`，只能证明
公共流程，没有覆盖该隐藏 fixture。

### 14.2 热点因果模型与实现

完整 diagnostics 显示 anonymous demand fault 累计 77.891 秒，clean-file cache
fault 71.482 秒，COW 5.176 秒；MemorySet 等待、frame/heap 锁、extent、VirtIO、
TLB 与 activation 均不是同量级首热点。clean-file fault 平均安装约 16 页，旧实现
在一个 fault 内反复进入同一 clean-page cache 锁。

最终实现只在 `CleanPageCache` 责任单元内增加两个批处理接口：一次持锁检查连续
未缓存前缀，一次持锁提取连续 cache hit run。精确 LRU、容量、read-ahead 16、
并发插入二次确认和 inode/range 失效不变；VmaMap、ResidentSet、PageTableOps 和
TlbProtocol 没有被合并进 VFS。

### 14.3 A/B、验证与诚实边界

| 候选 | RV 时间 | 处理 |
| --- | ---: | --- |
| 基线 | 786.62 s | 对照 |
| read-ahead 32 | 787.31 s | 未保留 |
| cache batch | 766.54 / 789.87 s | 保留通用锁粒度优化 |
| second-chance | 836.88 s | 未保留 |
| clock + range invalidation | 768.04 s | 未优于 batch |
| 精确 LRU + range invalidation | 767.32 s | 未证明独立收益 |

两次 batch 平均 778.21 秒，相对基线约快 1.07%，未稳定越过宿主噪声，因此不把
它宣称为确定性时间分提升。LA 同结构为 642.70 秒，相对 646.32 秒快 0.56%。

最终 RV 16G/8 与 LA 36G/12 public serial 都包含精确
`BUILDSTORM_COMPILE mode=multi ok=true`，judge 均解析为 180/180；由于不是
公开规则指定的 8G/8，这两次运行属于 `capability-pass`。四种
production/diagnostics cfg 和双架构 SMP8（含新增
`directory-abi`）通过，属于 `capability-pass`。新提交在评测机隐藏门上的状态仍为
`unverified`。完整哈希、原始日志路径、被证伪候选和 AI 披露见
`docs/evidence/buildstorm-stage2/20260816-evaluator-la-mkdir-clean-cache-validation-cn/README.md`。

## 15. 2026-08-16 LA 隐藏 UEFI 阶段的目录 FD ABI 修复

### 15.1 首个失败边界与根因

评测提交 `2953af6a725c015f89e64db9eab8f60ee27f6fe1` 的 LoongArch64 日志并未
卡在编译器。Cargo 在 2439.63 秒完成 release artifact，随后评测脚本进入不计时的
UEFI 启动准备，最早错误为：

```text
mkdir: cannot create directory '/work/buildstorm.esp': Function not implemented
```

后续 ESP 目标的 `No such file or directory` 是父目录未创建的派生错误；当时同时
出现的 `buildstorm.vars.fd/vars.fd` 证据不足以归类。GNU coreutils 的递归目录创建会
保存目录 FD、切换工作目录，再以
asm-generic syscall 50 `fchdir` 恢复；旧分发表没有 syscall 50，因而返回
`ENOSYS`。这解释了为什么 toolchain、minibuild 和完整 Rust 编译都成功，LA 却没有
最终 `BUILDSTORM_RESULT`，只能得到环境 20 分。

### 15.2 通用设计与不变量

共享 syscall 层新增 `fchdir(50)` 与 `fchmodat2(452)`。`fchdir` 只接受现有
`MemDir`、`Ext4Dir` 或目录型 `O_PATH` descriptor，通过同一 `resolve_base_dir`
取得 logical path，叠加进程 root 后验证目录仍存在，再同步 task 的共享 fs cwd 与
兼容 cwd 字段。无效 FD 返回 `EBADF`，非目录 FD 返回 `ENOTDIR`，不存在的目标不
制造目录或伪造成功。

`fchmodat2` 复用已有 `set_mode_fd`/`set_mode_path`，支持 Linux
`AT_EMPTY_PATH` 与 `AT_SYMLINK_NOFOLLOW` 边界，并拒绝未知 flags。实现不拥有 inode、
open-file description 或目录生命周期，不改变 ext4 transaction、namespace cache
失效、VmaMap、ResidentSet、PageTableOps 或 TlbProtocol。两种架构共用同一 ABI；
LoongArch 平台层没有评测专用分支。

### 15.3 验证矩阵与证据等级

| 验证 | 配置 | 结果 | 等级 |
| --- | --- | --- | --- |
| release cfg | RV/LA production + diagnostics | 四项通过 | `capability-pass` |
| 独立目录元数据 ABI | RV SMP8 | `directory-metadata-abi` 与最终 `pass cpus=8` | `capability-pass` |
| 独立目录元数据 ABI | LA SMP8 | `directory-metadata-abi` 与最终 `pass cpus=8` | `capability-pass` |
| public BuildStorm | LA production 8G/8 | `ok=true elapsed_s=553.62`，judge 180/180 | `official-pass` |
| 评测机隐藏 UEFI 门 | 提交修复后 | 等待新评测 | `unverified` |

public 运行使用未修改的官方镜像、原始脚本、`-snapshot -m 8G -smp 8` 和
production kernel。它证明完整公共编译没有回归，但 553.62 秒与评测机 2439.63 秒
宿主条件不同，不能直接换算新时间分；本轮修复的性能收益记为 `not measured`。
评测机隐藏 UEFI 步骤不在 public script 内，因此不能把 public 结果提升为隐藏门
已经通过的声明。

原始日志、source/image/kernel hash、runner 参数、judge 输出和复现命令保存在：

- `docs/evidence/buildstorm-stage2/20260816-la-fchdir-capability-gates/`
- `docs/evidence/buildstorm-stage2/20260816-la-fchdir-official-public-complete/`

AI 用于评测日志与源码/dirty diff 的交叉核对、ABI 因果模型、实现和串行 QEMU
验证编排；开发者可复核 syscall 分发、目录 FD 类型约束、独立 coreutils 回归、四种
cfg 日志、双架构 SMP 串口和完整 public serial。未修改官方镜像、suite、guest
script、judge、marker、guest 时间或 `/proc/uptime`，production 行为不检查 crate、
测试名、路径、命令或输出。

## 16. 2026-08-16 MADV_DONTNEED resident retirement

### 16.1 证据与根因

评测 LA 日志在编译阶段出现 135 次 jemalloc
`MADV_DONTNEED does not work (memset will be used instead)`。源码交叉核对发现
asm-generic syscall 233 仅返回 `Ok(0)`，没有改变 resident、PTE 或 frame owner。
jemalloc 的自检因此读回旧字节并改用同步 memset。该现象同时解释了为什么早期 crate
仍有进展，但大量编译进程在内存回收边界重复消耗 CPU。

### 16.2 架构设计与不变量

`MapAreaBacking` 区分 `Anonymous` 与 `AnonymousShared`。`MADV_DONTNEED` 只从完整
覆盖范围内的 private anonymous VMA 提取 resident owner，不拆分或合并 VMA；shared
anonymous、SysV shm、shared/private file mapping 均不误丢弃。`ResidentSet` 只负责
转移指定 VMA-relative VPN 范围的 owner，不直接操作页表。

`PageTableOps::unmap_pages_without_flush` 按 2 MiB leaf-table 边界复用 walk，同时适配
三层与四层页表。`MemorySet` 负责协议顺序：先撤销 leaf，执行发布 fence、本地 full
flush 和以 address-space root 为键的远端 shootdown，最后 drop 暂存的 owner。
若 resident 存在但 leaf 已因先前 `mprotect(PROT_NONE)` 撤销，则旧保护操作已经完成
retirement，当前无需重复 flush。refault 重新分配 zeroed frame，VMA policy 保持不变。

### 16.3 验证与性能

四种 release cfg 和双架构 SMP8 均通过。独立 resident 回归写入非零页、discard、
确认 translation 消失、refault 后逐字节为零，并检查 VMA 数不变；同一 parent/child
COW 地址空间还验证 anonymous shared 页面不被 discard 且 fork 后保持同一物理页。

LA diagnostics 300 秒最终快照为 `calls=1481 requested_pages=190954
discarded_pages=32904`，syscall 233 count 为 1523，jemalloc fallback 警告为 0。
同机隔离 A/B 为 556.35 秒到 547.73 秒，提升 1.55%。随后从最终树排除了仅有
0.41% 收益的 Stage16 syscall-return/CPU-index 快路径，但保留 lazy user stack 和
Zicboz 清零后端；该精确最终树的 LA 与 RV 分别在 558.27 秒、686.50 秒产生
`ok=true` marker，官方 judge 均解析为 180/180，属于 `official-pass`。LA 相对纯净
`12f110ae` 的 623.15 秒累计提升 10.41%。Stage17 隔离收益低于旧 5% 门，但按后续
“正向优化无需回滚”规则保留；最终评测机分数仍为 `unverified`，不能跨宿主换算。

证据索引：
`docs/evidence/buildstorm-stage2/stage17-madvise-dontneed-official-20260816/README.md`。
AI 辅助日志/源码审计、协议设计、实现和串行 A/B；开发者可从最终增量补丁、原始
serial、runner JSON、SMP/cfg 日志、judge 与 hash 独立复核。

## 17. 2026-08-17 LoongArch64 TLB generation 验证协议

### 17.1 Stage22 归因与因果模型

LoongArch64 diagnostics 330 秒最终快照记录 user-root activation 501,352 次、
kernel-root restore 501,351 次及 `local_flush=1,008,959`。同时
`table_misses=0`、`missing_trap=8`；syscall 的同 root/ASID/stable-generation
比例为 237,860 / 295,101（约 80.6%），timer 为 10,177 / 11,053（约 92.1%）。
store/load page fault 则几乎总伴随 generation 变化。

据此建立的模型不是“LoongArch 不需要 TLB flush”，而是“只有精确证明 root、
非零 ASID 和页表代际未变的 CPU，才可跨 kernel-root 区间保留用户 translation”。
页表编辑、shootdown、ASID recycle 或 ASID 0 仍必须回到保守失效。

### 17.2 实现边界与不变量

`PageTableWrapper` 在所有 wrapper 级单页/批量 map/unmap 后推进 translation
generation；generation 原子操作只在 LoongArch64 生效。平台层为每个 CPU 缓存
已验证的 `{root, ASID, generation}`。激活路径先发布 active root，再处理 deferred
shootdown，随后才决定是否复用 translation；远端 IPI、全 CPU flush、PTE 代际变化
和 ASID recycle 都会使验证失效。kernel ASID 0 与用户 ASID 0 可能别名，因此保持
全 flush，非零用户 ASID 则可安全保留。

VmaMap 继续只拥有区间策略，ResidentSet 继续拥有 resident frame，PageTableOps
继续拥有 leaf 操作，TlbProtocol 继续拥有本地/远端失效。该候选没有改动 COW、
shared/file mapping、exec/exit/fork、VFS、scheduler 或 allocator 的所有权边界。

### 17.3 A/B、反证与性能边界

同机、同镜像、同 8G/8 的 LoongArch64 production 对照从 555.02 秒降到
542.02 秒，改善 13.00 秒（2.34%）。短窗第 23 crate 从 35.84 秒降到 33.64 秒，
约 6.1%，但 330 秒时两者同为 34 crates，后续边界基本持平。候选 diagnostics
记录 `local_flush=254071`；由于诊断计数口径随实现改变，只能表述为直接热点约减少
四分之三，不能当成精确硬件事件差。

Stage24 尝试在普通 syscall/fault 中始终保留 user root。它虽通过四 cfg 和双架构
SMP8，却在 LA production 600 秒仅输出 toolchain，没有 minibuild/BEGIN，也没有
panic/OOM。这一结果强反证了取消 kernel-root trap boundary 的模型，因此 Stage24
保持隔离。历史 `3418beb6` 直接删除 activation flush 曾触发
`InstructionNotExist`/SIGILL；Stage23 依靠完整 generation 与 shootdown invalidation，
不能与该失败方案混同。

2.34% 的 wall-time 收益远小于失效次数降幅，说明 TLB 不是剩余主根因。下一轮应
回到 rustc/LLVM 长空档，对用户运行、fault、I/O、ext4、allocator、MemorySet 和
VFS 等待做统一时间轴归因，不以继续删除 TLB 边界作为默认方向。

### 17.4 验证矩阵、官方证据与 AI 披露

| 验证 | RISC-V64 | LoongArch64 | 等级 |
| --- | --- | --- | --- |
| production/diagnostics release | 通过/通过 | 通过/通过 | `capability-pass` |
| SMP8 MM/VFS/ABI/heap 回归 | 通过 | 通过 | `capability-pass` |
| public BuildStorm 8G/8 | `ok=true 773.45s` | `ok=true 542.02s` | `official-pass` |
| public judge 自动项 | 180/180 | 180/180 | `official-pass` |

官方 suite commit 为 `b5ec6ef8497e1818cbdec3b54bb722f036e57972`，QEMU 11.0.3，
运行参数为 `-snapshot -m 8G -smp 8`。镜像、script、judge、marker、guest 时间和
`/proc/uptime` 均未修改；production 不按 crate、测试、路径、命令或输出分支。
自动 180 分不包含人工设计文档 20 分，也不能跨宿主换算正式评测机时间分。

AI 用于 diagnostics 与源码/dirty diff 交叉核对、协议设计、实现和串行 QEMU 编排；
开发者可从原始 serial、runner JSON、source diff/status、patch-id、kernel/image hash、
judge 输出及四 cfg/SMP 日志独立复核。完整记录见
`docs/evidence/buildstorm-stage2/20260817-stage23-la-tlb-generation-conclusion-cn.md`。

## 18. 2026-08-17 脚本全局根命名空间修复

### 18.1 首错与交叉证据

评测 LA 在 3008.46 秒完成 release artifact 后，隐藏 nested-QEMU 准备阶段首次
失败于 `/work/buildstorm.vars.fd/vars.fd` 的 `ENOTDIR`。目录、symlink、递归复制与
file-to-directory 重建的独立回归均通过，不支持放宽 VFS 路径类型语义。

进一步审计发现，harness 原来会从脚本目录向上选择“最近的 shebang 解释器”。当
全局 root 和 suite root 同时提供解释器时，绝对脚本 `/glibc/...` 会被隐式改成
root `/glibc`，使其子进程的绝对 `/work/...` 映射到 `/glibc/work/...`。独立负向
回归在旧逻辑上稳定得到 `exit=1 root=/.../suite`。

Cosmos `mirror-upload-b2488999da07` 的可迁移线索与此一致：其 bootstrap 使用
`pivot_root` 将评测盘变为唯一全局 `/`，并卸载旧根。Cosmos 没有修补 `cp` 或
`vars.fd`，其 QEMU 资源路径也不同，因此只作为架构交叉证据，没有复制实现。

### 18.2 通用设计

脚本启动现在先检查当前全局 root 是否提供 shebang 解释器。若提供，则保留 `/`
及脚本原始绝对路径；只有全局解释器不可用时，才回退到真正自包含的 suite root。
这与 Linux mount namespace 中绝对路径从进程 root 解析的语义一致。

production 不检查架构、测试、crate、命令、输出或资源路径。VFS 对
`regular/child` 的 `ENOTDIR` 语义不变；syscall、MM、调度、TLB、marker 与时间均未
修改。新增 SMP-only 双向回归同时证明全局 root 优先和兼容 root fallback，避免修复
公共镜像时破坏旧的独立 userspace 布局。

### 18.3 验证与边界

四种 release cfg、RV/LA SMP8 均通过；最终 SMP 串口同时包含
`script-root-namespace root=/`、`script-compat-root` 和 `pass cpus=8`。LA public
8G/8 完整编译为 543.97 秒；与最新评测配置对齐的 36G/12 完整编译为 547.47 秒，
最新 public judge 解析为 180/180。两次均有精确
`BUILDSTORM_COMPILE mode=multi ok=true`。

该修复发生在 workload 启动边界，不宣称编译速度收益；相对 Stage23 同机 542.02
秒的差异仅 0.36%。公共镜像不含隐藏 nested-QEMU 资源布局，因此隐藏
`vars.fd` 门仍为 `unverified`，需要新提交评测确认。完整身份、哈希、原始日志、
Cosmos 分支历史和复现命令见
`docs/evidence/buildstorm-stage2/20260817-stage25-script-root-namespace/README.md`。

## 19. 2026-08-17 zip 评测入口与 LA 隐藏门观测

### 19.1 两个通道的首错边界

最新官方评测总分为 `596.3`：RISC-V64 BuildStorm 已得 `178.1`，LoongArch64
仍只有 toolchain/minibuild 的 `20.0`。LA 原始串口显示 Rust clean build 在
`1500.31s` 完成产物生成，随后隐藏准备步骤首错为：

```text
cp: cannot stat '/work/buildstorm.vars.fd/vars.fd': Not a directory
```

这不是公开 `final-2026` 脚本中的路径；公开脚本只负责编译并输出
`BUILDSTORM_COMPILE`，因此该隐藏后处理仍是 `unverified`。VFS 对普通文件作为目录
前缀返回 `ENOTDIR` 的 Linux 语义不能按 `vars.fd` 路径放宽，下一步需要评测通道
提供隐藏 fixture 或通过通用错误观测 marker 暴露其真实类型。

另一条 zip 通道在任何内核启动前报：

```text
make: *** No rule to make target 'all'. Stop.
```

交叉检查 `2e698077` 的 Git 树确认顶层 `Makefile` 实际存在；根因是上传包未把
Makefile 放在解压根（最可能是 GitHub 下载包的外层仓库目录）。该错误不能归因于
LA 内核或 BuildStorm workload。

### 19.2 通用打包设计与观测 marker

`create_kernel_zip.py` 现在从当前提交执行 `git archive --format=zip HEAD`，不添加
外层目录，并在生成前校验四个根入口和 ZIP CRC。`Makefile` 的 `all` 目标首先执行
`submission-preflight`，输出：

```text
WLL_SUBMISSION_PREFLIGHT status=START cwd=...
WLL_SUBMISSION_PREFLIGHT status=OK makefile=... cargo=... os=...
```

该 marker 只报告构建入口布局，不选择架构、测试、命令、输出或路径，不改变内核
运行时行为；若日志没有它，应先判定为 zip 解包/工作目录问题，而不是立即修改
MM/VFS。打包脚本输出 commit、entry count、SHA-256，便于将评测 zip 与源码身份绑定。

### 19.3 验证与证据分级

| 项目 | 结果 | 等级 |
| --- | --- | --- |
| RV 官方 BuildStorm | `178.1`，含 `BUILDSTORM_COMPILE` | `official-pass` |
| LA 官方 BuildStorm | artifact 后隐藏 `vars.fd` 首错 | `unverified` |
| `2e698077` 原始 Git 树 | 顶层 Makefile 存在 | `capability-pass` |
| 根相对 zip 结构 | 由新脚本本地 CRC/入口检查 | `capability-pass` |

本轮 zip 修复只处理提交包的通用入口，不宣称已修复 LA 隐藏 UEFI/nested-QEMU
阶段。AI 用于交叉核对两份原始评测日志、官方脚本、提交树和成功队伍 Makefile 结构；
开发者可用 `python3 create_kernel_zip.py --output ../wll_os-submission.zip`、
`unzip -l` 和压缩包内 `make all` 复核。未修改官方 image、suite、judge、guest
marker 或时间源。
