# 2026-08-16 评测复核、LoongArch mkdir ABI 与 clean-page cache 验证

## 1. 身份与证据分级

- 候选基线 HEAD：`f8e95319ce16ee2c5ac5e440cd5669a851d24459`。
- 官方 `final-2026`：`b5ec6ef8497e1818cbdec3b54bb722f036e57972`。
- 远端：`47.110.249.0`，QEMU 11.0.3；单实例顺序运行。
- RISC-V64 评测日志 SHA-256：
  `d6ce45e7d62e65b772a116167a5c89113a179f2337daa1400cd2e64621f13d7a`。
- LoongArch64 评测日志 SHA-256：
  `87fc48643e3faf0d8740c22344a71970f938ffc9d3ad9e7486bed704ad4074ae`。
- 官方原始 RV 镜像 SHA-256：
  `d74e436522f5946ca17280a7a25f17dbb6604b71fe675bb8a021ce8e849b334c`。

本文把未修改 public 镜像、脚本和 judge、但使用非计分资源配置的完整成功运行
记为 `capability-pass`；四 cfg build/check、SMP 与独立目录 ABI 回归同样记为
`capability-pass`。只有满足规定 8G/8 资源的运行才可记为 `official-pass`；新提交
在评测机额外隐藏门复测前保持 `unverified`。

## 2. 为什么 glibc 计分时出现 musl

必须区分两个同名但职责不同的层次：

1. glibc BuildStorm 脚本内部的
   `riscv64gc-unknown-linux-musl.json` / `loongarch64-unknown-linux-musl.json`
   是 ArceOS 被构建程序的 Rust target。它属于正式 glibc BuildStorm 编译，不能关闭。
2. RV 日志在首轮成功后又出现
   `[harness] SCRIPT /musl/buildstorm_testcode.sh`，这是第二个完整 suite。当前 judge
   只读取 glibc BuildStorm marker，不给该轮分数；它会额外消耗约一次完整编译时间。

评测日志中，首轮已经在 `elapsed_s=2020.80` 成功并结束
`buildstorm-glibc`，随后才启动 `/musl/buildstorm_testcode.sh`。本轮新增
`HARNESS_LIBC=glibc|musl|both`：默认及未设置时只运行 glibc；musl/both 仍可显式
用于兼容性验证。本地 basic/performance runner 显式选择 `both`，保持原有双 libc
回归覆盖；production 不检查测试名、crate、命令或输出。

## 3. LoongArch64 0 分首错

LA timed clean build 实际已经完成：

```text
Finished `release` profile [optimized] target(s) in 33m 08s
... done (2020.04s)
```

首错发生在随后的 EFI 启动准备：

```text
mkdir: cannot create directory '/work/buildstorm.esp': Function not implemented
cp: cannot create regular file '/work/buildstorm.esp/EFI/BOOT/BOOTLOONGARCH64.EFI': No such file or directory
cp: cannot stat '/work/buildstorm.vars.fd/vars.fd': Not a directory
```

源码已有 `open=1024`、`link=1025`、`unlink=1026`、`rmdir=1031`，但缺少
asm-generic legacy `mkdir=1030` 分发。修复把 1030 统一转发到既有
`sys_mkdirat(AT_FDCWD, path, mode)`；权限、umask、路径解析、ext4 transaction 与
namespace invalidation 仍只有一套实现。`vars.fd` 是 mkdir 失败后的派生错误：加入
ABI 后，未修改 public LA 流程已完整输出 `ok=true`。

独立 `smp-regression` 新增 `directory-abi`，使用 BusyBox 覆盖多级 mkdir、重复
`mkdir -p`、`test -d` 和递归清理。RV/LA 均通过。

## 4. 热点审计与候选

完整 RV diagnostics 末态：

| 路径 | 次数 | 页数 | 累计时间 |
| --- | ---: | ---: | ---: |
| anonymous demand fault | 915,657 | 1,649,182 | 77.891 s |
| clean-file cache fault | 255,959 | 4,101,743 | 71.482 s |
| COW | 83,244 | 83,244 | 5.176 s |

MemorySet 各站点总等待约 80 ms，frame/heap 锁、extent、VirtIO、TLB 与 activation
采样均不足以解释当前总时长。clean-file fault 平均一次安装约 16 页，正好等于
read-ahead 上限；旧路径对每页重复获取 `CLEAN_PAGE_CACHE` 锁并重复查询 LRU 树。

保留候选增加 `uncached_prefix_len()` 和 `get_run()`：一次锁临界区检查连续未缓存
前缀，一次锁临界区取得连续命中 run。预读页数仍为 16，I/O、缓存容量、精确 LRU、
失效、FrameTracker/ResidentSet/PTE/TLB 所有权均不变。

受控候选结果：

| 候选 | RV elapsed | 结论 |
| --- | ---: | --- |
| 同提交基线 | 786.62 s | 对照 |
| read-ahead 16 -> 32 | 787.31 s | 被证伪，未保留 |
| cache batch 首轮 | 766.54 s | 完整成功 |
| second-chance 单独加入 | 836.88 s | 回退 9.17%，未保留 |
| second-chance + range invalidation | 768.04 s | 追回回退，但不优于 batch |
| 精确 LRU + range invalidation | 767.32 s | 与 batch 相差 0.10%，未证明独立收益 |
| 最终 cache batch 复跑 | 789.87 s | 完整成功，显示宿主噪声不可忽略 |

两次最终结构相同的 RV batch 平均为 778.21 秒，相对 786.62 秒约快 1.07%；该收益
未稳定越过噪声，不能宣称 5% 加速。LoongArch64 同结构为 642.70 秒，相对
646.32 秒快 0.56%。本轮把它记录为通用锁粒度优化，不夸大为确定性时间分提升。

## 5. 最终验证矩阵

| 验证 | 结果 | 等级 |
| --- | --- | --- |
| RV/LA production release check | 通过 | `capability-pass` |
| RV/LA diagnostics release check | 通过 | `capability-pass` |
| RV SMP8 lifecycle + directory ABI | 通过 | `capability-pass` |
| LA SMP8 lifecycle + directory ABI | 通过 | `capability-pass` |
| RV public 16G/8 | `ok=true elapsed_s=789.87`，judge 180/180 | `capability-pass` |
| LA public 36G/12 | `ok=true elapsed_s=642.70`，judge 180/180 | `capability-pass` |
| 新提交隐藏评测 | 尚未运行 | `unverified` |

最终 RV serial SHA-256 为
`5989da3bfbc0b7af757c3560262730d540f8111637216db0b9f7fe7b1bc02003`，
LA serial 为
`d9f8b11af16fd5b01c5c623f6c5adbd9a8668373f45e229157b521ed56bf2655`。
最终 production release rebuild 的 RV/LA kernel SHA-256 分别为
`a1171d2563ca61f7003906fcfcbfd926dcc84684f7093dc2d03257f034047650` 和
`01e011d4d6eb273c53f353d44027d5505e23d9ce57be9388646c96f801f3332e`。
远端原始证据保存在
`/srv/buildstorm/evidence/20260816-final-cache-batch/`。

## 6. AI 使用与合规

OpenAI Codex / GPT-5 用于交叉核对 dirty worktree、评测日志、官方脚本/judge、
源码调用关系和 diagnostics，设计候选并执行单实例 A/B。人工可复核内容包括源码
diff、四 cfg 日志、双架构 SMP 串口、原始 BuildStorm serial、kernel/image/source
hash 和 judge 输出。没有修改官方镜像、suite、guest script、judge、marker、guest
时间或 timeout；production 不按 crate、路径、命令或输出选择行为。
