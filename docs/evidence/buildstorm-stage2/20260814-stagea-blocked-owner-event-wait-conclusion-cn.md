# BuildStorm Stage A：blocked-owner 事件等待结论

日期：2026-08-14

## 证据分级

- RISC-V64 完整 BuildStorm：`official-pass`。
- RISC-V64、LoongArch64 SMP8/内存生命周期门禁：`capability-pass`。
- LoongArch64 当前候选完整 BuildStorm：`unverified`，本阶段未运行。
- “本候选提高完整编译速度”：被 A/B 反证，不作提升声明。

## 候选边界

基线提交为 `080f380b8d48ef5292ad227137c2de52e9fbb594`。生产代码只修改
`os/src/task/mod.rs`，二进制 diff SHA-256 为
`0898577db0c8bb40ae623aa08daf7aee207ce0a47f5a4a47137586e5f1871248`。
工作区原有的 `plans` 删除、候选快照和历史 evidence 均未清理、覆盖或并入
本候选。

对应实现：

- `block_current_and_run_next()`：删除固定次数空转。
- `wait_for_blocked_owner_event()`：发布 idle 位后再次检查 owner 状态和 ready queue。
- `idle_until_interrupt()`：统一 WFI/`idle 0` 入口，并保证返回时中断关闭、idle 位清除。
- `run_blocked_owner_task()`：复用原有单用户边界执行与 running CPU claim。

## 因果模型与不变量

旧路径在 timed blocked owner 没有可运行任务时反复扫描 ready queue。基线在
300 秒累计 138,856,916 次 empty iteration 和 147,480,518 次
`task_manager` acquire，但锁等待只有 2,815 us。这说明热点是空轮询制造的锁流量，
不是锁竞争。

新路径遵循以下闭环：

1. owner CPU 先发布 idle 位，再做第二次 owner/ready-queue 检查。
2. enqueue 早于第二次检查时，新任务由该 CPU 直接取走。
3. enqueue 晚于第二次检查时，生产者观察到 idle 位并发送 IPI。
4. blocked owner 自身被其他 CPU 唤醒时，`blocking_cpu` 触发定向 IPI。
5. owner 与另一个 ready task 同时醒来时，ready task 在取得 running claim 前放回本地队列。
6. timer 在进入等待前已经编程；WFI 返回后仍由原循环调用 `wake_expired_timers()`。
7. `CURRENT_TASK`、`running_cpu`、`blocking_cpu` 的原所有权边界不变，同一用户上下文和内核栈不得由两个 CPU 同时执行。

本阶段没有修改 timer、wait queue、VFS、MM、页表或 TLB 协议。

## 能力门禁

本地以下 release 构建通过：

- RISC-V64 production、diagnostics、`smp-regression`；
- LoongArch64 production、diagnostics、`smp-regression`；
- `cargo metadata --no-deps --format-version 1`；
- `git diff --check`。

远端使用 QEMU 11.0.3、8 GiB、8 vCPU 和未修改的 15,032,385,536 字节
官方镜像，两个架构串行运行。两者均出现：

- interval timer state machine；
- resident/high-arena memory lifecycle；
- namespace lifecycle；
- 独立 user-memory lifecycle；
- SMP8、ASID/TLB isolation、heap stress 最终 pass marker。

原始门禁在 `20260814-stagea-blocked-owner-gates/`。

## 300 秒诊断 A/B

对照为 `20260814-riscv64-diagnostics-stage2-namespace-transaction-window300/`，
候选为 `20260814-riscv64-diagnostics-stagea-blocked-owner-window300/`。
两者使用同一宿主、官方镜像、8 GiB、8 vCPU 和 300 秒 marker 窗口。

| 指标 | 对照 | 候选 | 结果 |
| --- | ---: | ---: | ---: |
| timed loops | 1,504 | 1,508 | 等价 |
| timed wall time | 251,462,615 us | 251,409,400 us | -0.021% |
| empty iterations | 138,856,916 | 31,402 | -99.977% |
| task-manager acquires | 147,480,518 | 6,134,581 | -95.840% |
| task-manager wait | 2,815 us | 2,927 us | 无竞争恶化 |
| 第二个忙 TCG 线程 | 73.1% | 6.6% | 空转 CPU 基本消除 |

候选没有 panic 或 OOM。diagnostics 进度为 22 个 compiling marker，对照为 23，
不能据此声称吞吐提升。

## 300 秒 production A/B

对照为
`20260814-riscv64-production-stage2-namespace-transaction-window900/` 的前
300 秒，候选为
`20260814-riscv64-production-stagea-blocked-owner-window300/`。

两者都到达第 23 个 `ax-posix-api` marker：对照 54.266 秒，候选 54.420 秒。
候选慢 0.154 秒（约 0.28%），属于轮询精度和运行抖动；两者在 300 秒内均未进入
下一 crate。因此 production 300 秒进度为 0% 提升。

QMP 每 30 秒采样一次。候选 60--271 秒期间 CPU0 的 8 个样本位于用户地址，
CPU1--CPU7 的 70 个样本全部位于内核等待路径。这个平台期只有一个可运行 rustc；
调度器重构不能把单个用户态 crate 自动变为并行工作。

## 完整 RISC-V64 官方结果

命令使用未修改的 production runner：

```text
python3 scripts/run_buildstorm.py --arch riscv64 \
  --image /srv/buildstorm/images/sdcard-rv-pub.img \
  --stage complete --timeout 15000 --memory 8G --smp 8
```

结果：

```text
BUILDSTORM_COMPILE mode=multi ok=true elapsed_s=1322.58 cores=8 bytes=1683456 arch=riscv64
```

官方 `final-2026@b5ec6ef8497e1818cbdec3b54bb722f036e57972` judge 四项均通过，
脚本分数 180/180，judge exit 为 0。当前 production kernel SHA-256 为
`cdf1ea41f029d4aa8bdb96c0f7a0660aa8266e392dba022730964ee6c92ac076`。

保存基线为 1322.99 秒，候选快 0.41 秒，即 0.031%。该差异远低于 5% 门槛，
不能归因为编译速度提升。原始证据在
`20260814-riscv64-official-complete-stagea-blocked-owner/`。

## 架构决策

Stage A 作为通用正向优化保留：它在不改变等待语义的前提下移除了约一个宿主核的
无效占用，并通过当前候选的完整 RISC-V64 官方流程。它不是 BuildStorm 编译速度
根因。

当前证据不支持立即实施 per-task kernel stack/per-CPU scheduler-context 大重构。
早期长平台期是单个 rustc 的用户态计算，完整耗时也没有因释放空转核而改变。
下一生产候选必须来自后半程可运行任务并行期的直接 kernel-time 证据；不得仅凭
调度架构更接近 Linux 就假设会提高 BuildStorm 吞吐。
