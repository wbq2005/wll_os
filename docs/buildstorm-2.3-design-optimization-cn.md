# BuildStorm 内核设计与优化累计记录

## 1. 文档目的与评分映射

本文是 2026 年全国大学生操作系统比赛内核赛道决赛 BuildStorm 题目 2.3 的人工评审材料。内容按官方 `final-2026` README 的四项要求组织：

| 官方要求 | 分值 | 本文对应章节 |
| --- | ---: | --- |
| 问题定位与根因分析 | 6 | 第 3、4 节 |
| 修复或优化的设计与实现 | 6 | 第 5、6 节 |
| 修改前后实验分析 | 4 | 第 7 节 |
| AI 使用说明与可复现步骤 | 4 | 第 8、9 节 |

当前结论为双架构 `official-pass`。RISC-V64 与 LoongArch64 均使用未修改的官方镜像、镜像内原始脚本、官方 judge、QEMU `-snapshot -m 8G -smp 8` 和 production 内核完成从零编译，并输出精确成功标记：

```text
BUILDSTORM_COMPILE mode=multi ok=true
```

官方脚本自动项在本次环境中均为 180/180。设计文档 20 分由人工评审，本文不自行宣称该 20 分已经获得。

## 2. 权威输入与结果身份

- 内核基线提交：`055df99ff554441f7699c3518b1a4b204bd9c265`。
- 官方测例仓库：`https://github.com/oscomp/testsuits-for-oskernel`。
- 验证时 `final-2026` 提交：`b5ec6ef8497e1818cbdec3b54bb722f036e57972`。
- 官方 guest 脚本 SHA-256：`2f656a668076803fb465409374b6bcbb1fcbbc4f5c17a72b8ea4695668b9b33e`。
- 官方 judge SHA-256：`f9bc3c5c640217947775759b5b02aa4ceedfa76728d25d4f06d94ef5bc9d64dd`。
- RISC-V64 镜像 SHA-256：`d74e436522f5946ca17280a7a25f17dbb6604b71fe675bb8a021ce8e849b334c`。
- LoongArch64 镜像 SHA-256：`d1410544e677e11efb1c240be6ffb201c89d6de58c9675e73314a696e4cefdc5`。
- QEMU：11.0.3。
- 完整运行时源码 dirty diff SHA-256：`c22bc0a79e78a72a0c3c8dd13bf23c33692c5fe0991cc57ef0b1ce91c3c2a22d`。

原始串口、runner JSON、构建日志、镜像/内核/源码哈希和 judge 输出保存在：

- `docs/evidence/buildstorm-stage2/20260814-riscv64-official-complete-stage2/`
- `docs/evidence/buildstorm-stage2/20260814-loongarch64-official-complete-stage2/`
- `docs/evidence/buildstorm-stage2/20260814-stage2-buildstorm-official-completion.md`

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

## 5. 当前修复与优化设计

### 5.1 地址空间与页帧所有权

- `UserVaLayout` 把低用户区、低 mmap arena、高 mmap arena 和用户上限集中定义。
- `ResidentSet` 用 `BTreeMap<run_start, ResidentRun>` 表示稀疏驻留页，split/extract 不扫描整个虚拟区间。
- 每个受管页帧有 `AtomicUsize` 引用计数；`FrameTracker::clone/drop` 负责共享和最终释放。
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
| RISC-V64 | 1322.99 s | 1,683,456 B | `071116d...9311fe` | 180/180 |
| LoongArch64 | 1103.14 s | 1,716,224 B | `deaa5ce...14adbf9` | 180/180 |

两次串口都包含 toolchain、minibuild 和完整 compile 成功 marker，无 panic、OOM、filesystem error。RISC-V64 host runner elapsed 为 1402.73 秒，LoongArch64 为 1156.19 秒。

### 7.2 与本次 judge 基线比较

本地官方 judge 使用的 Linux 基线分别为 RISC-V64 1616 秒、LoongArch64 1985 秒。只对本次保存的 judge 配置作比较：

| 架构 | judge 基线 B | wll_OS 时间 t | 时间降低 | B/t |
| --- | ---: | ---: | ---: | ---: |
| RISC-V64 | 1616 s | 1322.99 s | 18.13% | 1.22x |
| LoongArch64 | 1985 s | 1103.14 s | 44.43% | 1.80x |

正式评测机会在同机重测 Linux 基线，因此以上时间分只表示本次 `official-pass` 环境，不能替代最终评测机成绩。

### 7.3 修改前后进展

早期完整运行在 3000 秒或更长窗口内没有成功 marker，不能计算严格的“成功样本对成功样本”加速比。可复核的状态变化是：

- 修改前：仅 toolchain/minibuild 通过，full clean build 超时或在后期出现 ENOENT/生命周期错误；
- namespace 修复前 900 秒：129 个 compile 事件，`bitmaps` metadata ENOENT；
- namespace 修复后 900 秒：131 个 compile 事件，越过 `bitmaps` 到 `flatten_objects`，无 filesystem error；
- 最终：两架构均在 15000 秒官方窗口内完成并通过 judge。

调度器单 idle wakeup 优化把早期 QEMU aggregate CPU 从约 718%-720% 降至 105%-108%，idle vCPU 自愿切换从约 52K-70K/s 降至约 95-131/s；晚期有用并行工作时 aggregate CPU 约 215%-217%。它显著减少宿主浪费，但 1800 秒 crate 边界只改善约 0.05%，所以不把它夸大为编译吞吐主因。

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
