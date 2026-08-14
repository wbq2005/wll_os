# Stage B：IRQ-safe per-CPU kernel heap cache 结论

## 1. 结论

Stage B 在 RISC-V64 与 LoongArch64 的未修改官方 `final-2026` glibc 镜像上均完成 BuildStorm clean build，证据等级为 `official-pass`：

```text
BUILDSTORM_COMPILE mode=multi ok=true elapsed_s=800.55 cores=8 bytes=1683456 arch=riscv64
BUILDSTORM_COMPILE mode=multi ok=true elapsed_s=660.71 cores=8 bytes=1716224 arch=loongarch64
```

两架构官方 judge 自动项均为 180/180。设计文档 20 分由人工评审，本文不自行计分。正式评测机会重测同机 Linux 基线，因此这里不锁定最终时间分。

## 2. 候选与因果模型

Stage A 的完整编译已经证明 ABI、MM、VFS 和调度路径能够收敛，但 900 秒 diagnostics 仍记录 33,741,372 次中央 heap 锁获取、5,004 次竞争，以及 QMP 样本中的 buddy `Heap::dealloc` 合并和自旋簇。

BuildStorm 会同时运行 cargo、rustc 和链接器，产生大量 8 B 至 4 KiB 的短命内核对象。旧全局 `LockedHeap` 使每次小对象 alloc/free 都串行进入中央 buddy，并在 free 时执行 split/merge。因果预测是：如果中央 heap 是剩余复合热点，减少小对象的中央交互应同时降低锁计数、QMP 自旋和两架构 production 完整时间，而不需要识别 crate、路径或命令。

## 3. 架构设计

实现位于 `os/src/mm/heap_allocator.rs`：

- 8 B 至 4 KiB 共 10 个 canonical size class；
- 每 CPU、每 class 容量 64；
- miss 时最多从中央 refill 16 个 block，cache 满时 flush 16 个；
- 大对象和底层 heap range 仍由唯一中央 buddy 管理；
- 所有回中央 block 使用 `Layout(block_size, block_size)`；
- cache 锁与中央锁不同时持有；
- 跨 CPU free 迁入执行 dealloc 的当前 CPU cache；
- 中央 OOM 前 drain 全部有界 cache 后重试。

这是一层所有权不变的流量聚合：CPU cache 暂存中央 buddy 已分配出的 block，不拥有新的物理 frame 域，也不改变 `FrameTracker`、`ResidentSet`、页表或 TLB 职责。

## 4. IRQ-safe 修订

第一次实现扩大了批量中央临界区，但没有关闭本地中断。diagnostics 内核在 `[VIRTIO] mounting ext4..` 附近停止，符合 timer/VirtIO 中断在同一 CPU 递归进入 allocator、再次获取非可重入 `spin::Mutex` 的模型。

最终实现增加 `InterruptGuard`：

1. 读取并保存本地 IRQ enable 状态；
2. 原状态为 enabled 时关闭中断；
3. 完成 cache/central allocator 操作；
4. guard drop 时仅在原状态为 enabled 的情况下恢复中断。

这等价于 kernel allocator 入口的 `local_irq_save/restore`。它同时固定 `current_cpu_index()` 的使用窗口，并阻止同 CPU 中断递归获取 cache 或中央锁。失败运行保存在 `20260815-riscv64-diagnostics-stageb-percpu-heap-cache-window900/`，修订后的 60 秒与 900 秒运行分别保存在对应 `irqguard-window60/`、`irqguard-window900/` 目录。

## 5. 性能证据

### 5.1 完整 production

| 架构 | 可比基线 | Stage B | 降低时间 | 提升 |
| --- | ---: | ---: | ---: | ---: |
| RISC-V64 | 1322.58 s | 800.55 s | 522.03 s | 39.47% |
| LoongArch64 | 1103.14 s | 660.71 s | 442.43 s | 40.11% |

两个结果都超过 15% 性能候选门槛，并且包含精确 `mode=multi ok=true`，不是短窗口进度推断。

### 5.2 RISC-V64 diagnostics 900 秒

| 指标 | Stage A | Stage B |
| --- | ---: | ---: |
| compiling marker | 129，停在 `flatten_objects` | 136，完成 `arceos-helloworld` |
| 完整编译 | 未完成 | 864.74 s 成功 |
| 中央 heap 锁获取 | 33,741,372 | 929,604 |
| 中央 heap 锁竞争 | 5,004 | 7 |
| cache hit/miss | 不适用 | 20,032,390 / 53,030 |
| cache hit rate | 不适用 | 约 99.74% |
| `drained_blocks` | 不适用 | 0 |
| QMP 中央 allocator 自旋簇 | 35 / 192 samples | 0 / 192 samples |

中央锁获取下降 97.24%，竞争下降 99.86%。`drained_blocks=0` 只说明该次工作集没有触发 OOM drain，不能据此删除恢复路径。

## 6. 回归与证据分级

`capability-pass` 门禁：

- RISC-V64/LoongArch64 production release 构建；
- RISC-V64/LoongArch64 diagnostics release 构建；
- 双架构 SMP8 的 160 MiB heap stress；
- ASID/TLB、resident/high-arena、namespace；
- 独立 user-memory lifecycle、fork/COW/shared/file mapping；
- 两架构八 CPU 完整 marker。

`official-pass` 门禁：

- QEMU 11.0.3，`-snapshot -m 8G -smp 8`；
- 未修改官方镜像、guest script 和 judge；
- production 内核；
- toolchain、minibuild、完整 compile 成功 marker；
- 原始串口、runner JSON、source/kernel/image/suite 哈希和 judge 输出。

LoongArch64 judge stderr 仍包含旧的“expected 12”提示，但 2026 BuildStorm 官方计分配置要求 8 vCPU，本次参数和 marker 均为 8，judge 自动项为 180/180。该上游提示原样保留。

## 7. 证据索引

- RV64 official：`20260815-riscv64-official-complete-stageb-percpu-heap-cache/`
- LA64 official：`20260815-loongarch64-official-complete-stageb-percpu-heap-cache/`
- RV64 diagnostics 900 s：`20260815-riscv64-diagnostics-stageb-percpu-heap-cache-irqguard-window900/`
- RV64 Stage A diagnostics 900 s 对照：`20260814-riscv64-diagnostics-stagea-blocked-owner-late-window900/`
- RV64 diagnostics 60 s：`20260815-riscv64-diagnostics-stageb-percpu-heap-cache-irqguard-window60/`
- 首次无 IRQ 约束的失败运行：`20260815-riscv64-diagnostics-stageb-percpu-heap-cache-window900/`
- 四 cfg 构建与双架构 SMP：`20260815-stageb-percpu-heap-cache-gates/`

官方 suite 提交为 `b5ec6ef8497e1818cbdec3b54bb722f036e57972`。RV64 kernel SHA-256 为 `ad49e2df7438fa54291905c014db5df0d81381d816e87b881162594d61603210`，LA64 为 `a8395d2fc53846316fcfa50a034315d08d2b0dd3b2171b2459b95a8e2e09592a`。

## 8. 合规与 AI 披露

production 行为没有按 crate、测试、路径、命令、输出或 marker 分支。diagnostics 聚合只在 `buildstorm-diagnostics` feature 下启用，默认 production 关闭。没有修改官方镜像、suite、guest script、judge、marker、QEMU 资源或 guest `/proc/uptime`。

OpenAI Codex / GPT-5 用于审计 dirty worktree、建立和证伪根因模型、实现 allocator 架构、组织双架构构建/SMP/QEMU、解析日志并整理文档。可由开发者独立核验的材料包括 production diff、四 cfg 构建日志、双架构回归、两份官方串口、runner 参数、全部哈希和 judge 输出。

## 9. 复现命令

```bash
python3 scripts/run_buildstorm.py --arch riscv64 \
  --image /srv/buildstorm/images/sdcard-rv-pub.img \
  --stage complete --timeout 15000 --memory 8G --smp 8

python3 scripts/run_buildstorm.py --arch loongarch64 \
  --image /srv/buildstorm/images/sdcard-la-pub.img \
  --stage complete --timeout 15000 --memory 8G --smp 8

python3 /srv/buildstorm/src/testsuits-for-oskernel/judge/judge_buildstorm-glibc.py \
  _tmp/buildstorm-riscv64-complete.log
python3 /srv/buildstorm/src/testsuits-for-oskernel/judge/judge_buildstorm-glibc.py \
  _tmp/buildstorm-loongarch64-complete.log
```

两次 QEMU 必须顺序执行。启动前和结束后均应确认没有其他 `run_buildstorm.py` 或 `qemu-system-*` 实例。
