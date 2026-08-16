# Stage17 MADV_DONTNEED 双架构验证

日期：2026-08-16

## 结论

评测提交 `2953af6a725c015f89e64db9eab8f60ee27f6fe1` 的 LA 0 分首先由
后续提交 `12f110ae9a739728ee987aa69355f2ec3d7c364a` 补齐
`fchdir(50)`/`fchmodat2(452)` 解决。纯净 `12f110ae` 的远端日志包含：

```text
BUILDSTORM_COMPILE mode=multi ok=true elapsed_s=623.15 cores=8 bytes=1716224 arch=loongarch64
```

Stage17 继续处理评测日志中的 135 条
`MADV_DONTNEED does not work (memset will be used instead)`。旧 syscall 233
无条件返回成功却不丢弃页面，jemalloc 读回旧内容后退化为同步 memset。新实现只撤销
private anonymous resident page；VMA 拓扑保持不变，PTE 撤销后完成本地 TLB flush 和
远端 shootdown，最后才释放 ResidentSet 中的 frame owner。anonymous shared、SysV
shared memory 和 file mapping 不被误丢弃。

## 身份

- 最终提交基线 HEAD：`12f110ae9a739728ee987aa69355f2ec3d7c364a`。
- 最终生产树：该 HEAD 加 `source-final-status.txt` 所列 19 文件变更；累计补丁
  `final-cumulative-from-12f.patch`，SHA-256
  `c62dbbff7494814fc1e6de0e28ef5d5f06df2da6c2f27e0e447e527bcb2d31c8`。
- Stage17 独立增量：`stage17-final-incremental.patch`，SHA-256
  `112bfa092f2f66228e735ed1f783c2c66120efc538719a56d636af0db191b7ff`。
- 仅有 0.41% 隔离收益的 Stage16 syscall-return、signal pending hint 和 CPU-index
  快路径已排除；最终树保留独立验证必需的 lazy user stack 与 Zicboz 清零后端。
- 官方 suite：`b5ec6ef8497e1818cbdec3b54bb722f036e57972`。
- RV 镜像：`d74e436522f5946ca17280a7a25f17dbb6604b71fe675bb8a021ce8e849b334c`。
- LA 镜像：`d1410544e677e11efb1c240be6ffb201c89d6de58c9675e73314a696e4cefdc5`。
- QEMU：11.0.3；正式运行均为 `-snapshot -m 8G -smp 8`，且串行执行。

## 验证结果

| 验证 | 结果 | 分级 |
| --- | --- | --- |
| RV/LA production + diagnostics release | 四项 `rc=0` | `capability-pass` |
| RV SMP8 生命周期 | resident、namespace、VFS、directory 与最终 `pass cpus=8` | `capability-pass` |
| LA SMP8 生命周期 | resident、namespace、VFS、directory 与最终 `pass cpus=8` | `capability-pass` |
| LA diagnostics 300 s | calls=1481，requested=190954，discarded=32904，fallback 警告=0 | `capability-pass` |
| Stage17 隔离 LA A/B | 556.35 s -> 547.73 s，提升 1.55% | `official-pass` |
| 最终 LA production 8G/8 | `ok=true elapsed_s=558.27`，judge 180/180 | `official-pass` |
| 最终 RV production 8G/8 | `ok=true elapsed_s=686.50`，judge 180/180 | `official-pass` |

Stage17 隔离改善低于旧 5% 门，但按后续“正向优化无需回滚”规则保留；它同时补齐
标准内存回收语义并消除 jemalloc fallback。最终 LA 相对纯净 `12f110ae` 的
623.15 秒快 64.88 秒，即累计提升 10.41%。不能用这台宿主的时间直接换算评测机分数。

第一次 LA SMP 回归额外创建三个测试 MemorySet，改变 ASID 复用节奏并使后续
namespace 阶段超时。控制树通过后，测试被改为复用原有 parent/child COW 地址空间；
生产实现未因此放宽。调整后双架构均得到最终 SMP marker。

## 复现

```bash
cargo check -p wll_OS --release --target riscv64gc-unknown-none-elf \
  --no-default-features --features riscv
cargo check -p wll_OS --release --target loongarch64-unknown-none \
  --no-default-features --features loongarch

python3 scripts/run_smp_regression.py --arch riscv64 \
  --image /srv/buildstorm/images/sdcard-rv-pub.img --timeout 90
python3 scripts/run_smp_regression.py --arch loongarch64 \
  --image /srv/buildstorm/images/sdcard-la-pub.img --timeout 120

python3 scripts/run_buildstorm.py --arch loongarch64 \
  --image /srv/buildstorm/images/sdcard-la-pub.img \
  --stage complete --timeout 15000 --memory 8G --smp 8
python3 scripts/run_buildstorm.py --arch riscv64 \
  --image /srv/buildstorm/images/sdcard-rv-pub.img \
  --stage complete --timeout 15000 --memory 8G --smp 8
```

`candidate-*` 与 `control-*` 是 Stage17 隔离 A/B；`final-*` 是排除 Stage16 后的精确
最终树。原始 serial、runner JSON、四 cfg 日志、SMP 日志、judge 输出、源码状态、
补丁和 hash 均在本目录。没有修改官方镜像、suite、guest script、judge、marker、
guest 时间或 `/proc/uptime`；production 不检查 crate、测试名、路径、命令或输出。
