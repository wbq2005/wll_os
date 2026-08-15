# 2026-08-15 大内存、SMP 启动与同步 SIGILL 验证索引

## 身份与证据边界

- 本地基线：`6045dd2534668ea4d1cff94725c41afb0a855af4`，dirty candidate。
- 官方 suite：`final-2026`，`b5ec6ef8497e1818cbdec3b54bb722f036e57972`。
- RISC-V64 官方镜像 SHA-256：
  `d74e436522f5946ca17280a7a25f17dbb6604b71fe675bb8a021ce8e849b334c`。
- LoongArch64 官方镜像 SHA-256：
  `d1410544e677e11efb1c240be6ffb201c89d6de58c9675e73314a696e4cefdc5`。
- 远端：`root@47.110.249.0`。
- 隔离 worktree：`/srv/buildstorm/worktrees/stage4-frame-ref-chunks-20260815`。

本轮没有修改官方镜像、suite、guest script、judge、marker 或 guest 时间。
派生 RISC-V64 镜像只用于独立 nested-QEMU 能力回归，因此其结果只能是
`capability-pass`。public README 写 8G/8，但当前可执行 judge 期望 RV 8 核、
LA 12 核，实际评测日志分别使用 16G/8 与 36G/12。public suite 的完整运行按
实际评测配置执行并由当前官方 judge 解析；该上游不一致不会被静默忽略。

## 根因证据

LoongArch64 原始评测日志在 36G/12 配置报告：

```text
Heap allocation error, layout = Layout { size: 74695256, align: 8 }
```

36 GiB 约有 943 万个 frame。旧的 flat `Vec<AtomicUsize>` 需要约 72 MiB，
buddy allocator 将其提升到 128 MiB 阶，且引用表初始化早于动态 heap 扩展。
此外，12 个 128 KiB secondary stack 共需 1.5 MiB，旧汇编只预留 1 MiB。

RISC-V64 原始评测日志已完成 release build 和 ELF-to-BIN，却在 untimed
nested-QEMU 阶段得到 `run=FAIL`。派生镜像 diagnostics 复现的首个异常为
QEMU `cpuinfo_init` 中的同步 SIGILL；反汇编确认故障指令位于安装 SIGILL
handler 之后的 CPU 扩展探测序列。

## 已保存证据

- LA 36G/12 frame table 与启动栈：
  `/srv/buildstorm/evidence/20260815-loongarch64-36g-frame-ref-chunks-stack12`
- LA public 36G/12 minibuild：
  `/srv/buildstorm/evidence/20260815-loongarch64-public-minibuild-frame-stack12-sigill`
- RV/LA 四 cfg 与 SMP gate：
  `/srv/buildstorm/evidence/20260815-sigill-frame-stack12-smp-gates`
- RV nested-QEMU 基线：
  `/srv/buildstorm/evidence/20260815-riscv64-nested-qemu-probe-baseline`
- RV nested-QEMU diagnostics 修复后：
  `/srv/buildstorm/evidence/20260815-riscv64-nested-qemu-probe-sigill`
- RV nested-QEMU production 动态启动：
  `/srv/buildstorm/evidence/20260815-riscv64-nested-qemu-probe-sigill-production`
- RV nested-QEMU production 加载真实内核：
  `/srv/buildstorm/evidence/20260815-riscv64-nested-qemu-kernel-production`
- RV public 16G/8 minibuild：
  `/srv/buildstorm/evidence/20260815-riscv64-public-minibuild-sigill`
- RV public 16G/8 完整运行：
  `/srv/buildstorm/evidence/20260815-riscv64-complete-sigill-frame-stack12`
- LA public 36G/12 完整运行：
  `/srv/buildstorm/evidence/20260815-loongarch64-complete-sigill-frame-stack12`

## 当前结果

| 项目 | 结果 | 分级 |
| --- | --- | --- |
| RV/LA production + diagnostics release | 四项通过 | `capability-pass` |
| RV/LA 8G/8 SMP 生命周期 | 两架构通过 | `capability-pass` |
| LA 36G/12 production 启动 | 调度器启动、toolchain 通过 | `capability-pass` |
| LA 36G/12 public minibuild | `BUILDSTORM_MINIBUILD ok` | `capability-pass` |
| RV 16G/8 public minibuild | `BUILDSTORM_MINIBUILD ok` | `capability-pass` |
| RV nested-QEMU production | QEMU/OpenSBI/真实内核加载通过 | `capability-pass` |
| RV 16G/8 public full build | `ok=true elapsed_s=786.62`，judge 180/180 | `official-pass` |
| LA 36G/12 public full build | `ok=true elapsed_s=650.22`，judge 180/180 | `official-pass` |
| hidden nested-QEMU evaluator gate | 尚未复测 | `unverified` |

RISC-V64 runner exit status 为 0，host wall time 为 `13:47.93`，最大 resident
set 为 `3,679,820 KiB`。归档身份为：

- production kernel：
  `8e2ffd8473b0359b178c3fc73dd0639b81bf924637ee26155f5ce939fae423e0`；
- source dirty diff：
  `2830ac0c2953e90674ffabeb838f381c8f1346f5eb22afd1064b1b627c615bb8`；
- RISC-V64 image：
  `d74e436522f5946ca17280a7a25f17dbb6604b71fe675bb8a021ce8e849b334c`；
- QEMU：11.0.3；suite：`b5ec6ef8497e1818cbdec3b54bb722f036e57972`。

LoongArch64 runner exit status 为 0，host wall time 为 `11:25.51`，最大 resident
set 为 `4,292,876 KiB`。归档身份为：

- production kernel：
  `61e523cbb2049e3430faf0a35718b425b679001b6d409a580642b661d82c5000`；
- source dirty diff：
  `2830ac0c2953e90674ffabeb838f381c8f1346f5eb22afd1064b1b627c615bb8`；
- LoongArch64 image：
  `d1410544e677e11efb1c240be6ffb201c89d6de58c9675e73314a696e4cefdc5`；
- QEMU：11.0.3；suite：`b5ec6ef8497e1818cbdec3b54bb722f036e57972`。

完整编译只能由原始日志中的精确
`BUILDSTORM_COMPILE mode=multi ok=true` 判定。完成后还需归档 runner JSON、
release build log、source dirty diff、kernel/image hash、QEMU 版本和完整参数。

## 复现命令

运行前先确认不存在其他 runner 或 QEMU；两个架构必须顺序执行：

```bash
python3 scripts/run_buildstorm.py --arch riscv64 \
  --image /srv/buildstorm/images/sdcard-rv-pub.img \
  --stage complete --timeout 3000 --memory 16G --smp 8

python3 scripts/run_buildstorm.py --arch loongarch64 \
  --image /srv/buildstorm/images/sdcard-la-pub.img \
  --stage complete --timeout 3000 --memory 36G --smp 12
```

独立 nested-QEMU probe 使用派生开发镜像中的 RISC-V64 动态链接器、依赖闭包、
OpenSBI 和真实 wll_OS kernel，运行仓库脚本 `scripts/qemu_nested_probe.sh`。
它不读取或生成官方 marker。
