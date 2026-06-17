# Kernel worker lane / foreground harness audit 5D.5

## Conclusion

5D.5 的目标已经从“不要开启 worker”推进到“系统具备安全的后台 kernel work 基建”。

本阶段没有实现或默认开启 writeback worker。完成点是调度边界：user foreground ready lane 和 kernel task lane 已分离，`run_user_task_foreground()` 永远只取用户任务；用户任务阻塞等待中的 `run_ready_task_once()` 也只泵用户任务，不会误跑 `trap_frame=None` 的 kernel task。

新的 go/no-go 边界是：

- Go：可以把 kernel-only work 放进独立 kernel ready lane，由非 foreground 的调度点显式 drain。
- No-go：仍不能把 writeback 或其他后台 worker 纳入用户 foreground 完成条件，也不能让它影响 START / END marker、foreground timeout 或用户任务清理边界。

## Implemented Boundary

### Ready lanes

`os/src/task/manager.rs` 现在有两个就绪队列：

- `USER_READY_QUEUE`：只承载 `!task.is_kernel` 的用户任务。
- `KERNEL_READY_QUEUE`：只承载 `task.is_kernel` 的 kernel-only task。

`manager::add_task()` 保留为兼容入口，但内部按 `TaskControlBlock::is_kernel` 分流到 `add_user_task()` 或 `add_kernel_task()`。`add_task_front()`、`remove_task_instances()`、`retain_tasks()`、`queue_len()` 等兼容 API 也覆盖两个 lane。

新增/明确的语义入口：

- `fetch_user_task_for_foreground()`：只从 user lane 取任务。
- `fetch_kernel_task()`：只从 kernel lane 取任务。
- `has_user_task()` / `has_kernel_task()`：分别检查两个 lane。
- `fetch_task()`：保留为兼容 wrapper，先 user 后 kernel，但 foreground 路径不再调用它。

### Foreground harness

`os/src/task/harness.rs::run_user_task_foreground()` 已改为：

- 初始用户 task 仍通过 `manager::add_task()` 入队，但会被分流到 user lane。
- 完成条件使用 `manager::has_user_task()`，kernel lane 非空不会拖住或污染 foreground run。
- 调度循环使用 `manager::fetch_user_task_for_foreground()`，不会消费 kernel lane。
- 如果用户 lane 中的任务缺失 trap frame，只记录 `foreground user task ... missing trap frame`，不把它当 kernel task 执行。

这保证后台 kernel task 不会造成 foreground loop 提前退出、误 timeout、漏 marker 或误清理用户任务树。

### Scheduler and blocking wait

`os/src/task/mod.rs` 现在区分三类入口：

- `fetch_dispatchable_user_task()`：只取 user lane，并保留跳过 foreground-active 用户任务的逻辑。
- `fetch_dispatchable_task()`：普通 scheduler 入口，先取 user lane；只有非 foreground 时才退到 `fetch_kernel_task()`。
- `drain_kernel_ready_once()`：显式 kernel lane pump，foreground active 时直接返回 `false`。

`run_next_task()` 改为用 `task.is_kernel` 判定是否走 `TaskContext` kernel switch，不再用 `trap_frame=None` 推断 kernel task。若一个用户任务没有 trap frame，只记录错误并退出当前调度片。

`run_ready_task_once()` 改为只调用 `fetch_dispatchable_user_task()`。因此 foreground 下用户阻塞等待时不会运行 kernel task；非 foreground 阻塞等待如需推进 kernel work，会在独立的 `drain_kernel_ready_once()` 调用点执行。

`yield_current_once()` 在 foreground active 时使用 `has_user_task()`，避免 kernel lane 中的 ready task 让用户路径误判“还有 foreground 用户任务可跑”。

## Why `is_kernel` Is The Boundary

`trap_frame=None` 不是可靠的 kernel/user 判据。用户任务在运行中会临时 `take()` trap frame；kernel task 则由 `TaskControlBlock::new_kernel_task()` 显式设置 `is_kernel=true`。因此 5D.5 后的分类规则是：

- user/kernel 分类只看 `TaskControlBlock::is_kernel`。
- `trap_frame` 只表示用户上下文是否当前可恢复。
- `trap_frame=None` 对用户任务是错误状态，不是进入 kernel task 分支的理由。

## Non-goals

- 不实现 writeback worker。
- 不默认开启 writeback worker。
- 不把后台 worker 放入 user foreground lane。
- 不让后台 worker 参与单个用户 program 的 START / END marker、timeout、清理或完成条件。
- 不用 benchmark 特判、marker 伪造或 testcase shortcut 掩盖调度问题。

## Verification

已完成的静态/编译验证：

1. `cargo fmt`
2. `cargo +nightly-2025-01-18 check --locked --offline --release --target riscv64gc-unknown-none-elf`
3. `cargo +nightly-2025-01-18 check --locked --offline --release --target loongarch64-unknown-none --no-default-features --features loongarch`

已完成的完整验证闭环：

1. Filtered suites, LoongArch:
   - `PYTHONUTF8=1 python scripts/perf_baseline_runner.py --arch loongarch64 --suite iozone --runs 1 --timeout 1800 --delete-sdcard-copy`
   - `PYTHONUTF8=1 python scripts/perf_baseline_runner.py --arch loongarch64 --suite libcbench --runs 1 --timeout 1800 --delete-sdcard-copy`
   - `PYTHONUTF8=1 python scripts/perf_baseline_runner.py --arch loongarch64 --suite lmbench --runs 1 --timeout 2400 --delete-sdcard-copy`
   - Evidence:
     - `results/20260618-0203-loongarch64-iozone-summary`
     - `results/20260618-0207-loongarch64-libcbench-summary`
     - `results/20260618-0209-loongarch64-lmbench-summary`
   - All three runs had `run_exit_code=0` and `judge_exit_code=0`.
2. Filtered suites, RISC-V:
   - `PYTHONUTF8=1 python scripts/perf_baseline_runner.py --arch riscv64 --suite iozone --runs 1 --timeout 1800 --delete-sdcard-copy`
   - `PYTHONUTF8=1 python scripts/perf_baseline_runner.py --arch riscv64 --suite libcbench --runs 1 --timeout 1800 --delete-sdcard-copy`
   - `PYTHONUTF8=1 python scripts/perf_baseline_runner.py --arch riscv64 --suite lmbench --runs 1 --timeout 2400 --delete-sdcard-copy`
   - Evidence:
     - `results/20260618-0213-riscv64-iozone-summary`
     - `results/20260618-0218-riscv64-libcbench-summary`
     - `results/20260618-0220-riscv64-lmbench-summary`
   - All three runs had `run_exit_code=0` and `judge_exit_code=0`.
3. Full default `all`, both architectures:
   - `PYTHONUTF8=1 python scripts/perf_baseline_runner.py --arch loongarch64 --suite all --runs 1 --timeout 3600 --delete-sdcard-copy`
   - `PYTHONUTF8=1 python scripts/perf_baseline_runner.py --arch riscv64 --suite all --runs 1 --timeout 3600 --delete-sdcard-copy`
   - Evidence:
     - `results/20260618-0224-loongarch64-all-summary`
     - `results/20260618-0234-riscv64-all-summary`
   - Both runs had `run_exit_code=0` and `judge_exit_code=0`.

All required parser groups in the filtered and `all` runs were `status=ok` and `closed=true`. Serial grep for every run reported zero hits for:

- `TIMEOUT`
- `panic`
- `User page fault`
- `kernel page fault`
- `trap_frame`
- `foreground user task`
- `missing scheduler context for kernel task`

Known non-blocking score/output caveat: `lmbench-glibc` reported one zero item in LoongArch filtered/all and RISC-V all, while the group remained closed and `judge_exit_code=0`. This is not a lane split regression signal.

## Rollback Conditions

如果后续改动出现任一现象，应撤回对应 scheduler/kernel-lane 改动：

- `run_user_task_foreground()` 取到 kernel task。
- foreground 下的 `run_ready_task_once()` 执行 kernel task。
- foreground marker 缺失、提前退出或 START/END 不配对。
- kernel lane ready task 导致用户 foreground timeout 或清理边界变化。
- `missing scheduler context for kernel task`。
- kernel page fault。
- 已稳定的 basic / busybox 路径回退。

## Current Go / No-go

Go：5D.5 kernel lane 基建可以作为后续后台 kernel work 的调度基础。

No-go：writeback worker 仍不能仅因为 lane 存在就默认开启；它还需要单独的 worker 生命周期、退出、等待、日志和双架构 marker 验证。
