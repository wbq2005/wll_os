# Stage23 LoongArch64 TLB generation 优化结论

日期：2026-08-17

## 1. 结论与证据等级

Stage23 在 LoongArch64 地址空间激活路径中引入页表 translation generation，
并以每 CPU 的 `{root, ASID, generation}` 验证记录代替“每次用户返回都全量失效”
的保守策略。该候选基于 `d676f7508bc08fb563ab451f184028158b942ed2`，生产
补丁 patch-id 为 `d7d03aeb5c30375b6b6b7b4a295715db67af0a49`。

双架构 production/diagnostics release 与 SMP8 独立回归均通过，属于
`capability-pass`。未修改的官方 `final-2026` 镜像、脚本和 judge 在
`-snapshot -m 8G -smp 8` 下得到：

| 架构 | 完整成功标记 | guest 编译时间 | judge 自动项 | 等级 |
| --- | --- | ---: | ---: | --- |
| LoongArch64 | `BUILDSTORM_COMPILE mode=multi ok=true` | 542.02 s | 180/180 | `official-pass` |
| RISC-V64 | `BUILDSTORM_COMPILE mode=multi ok=true` | 773.45 s | 180/180 | `official-pass` |

自动项不包含人工设计文档 20 分。正式评测机将重新测量 Linux baseline，本文不把
本地 judge 时间分换算为最终比赛得分。

## 2. Stage22 因果模型

Stage22 在 `buildstorm-diagnostics` 下分别记录 root 激活、失效原因和 trap 前后
的 root/ASID/generation。330 秒最终快照包含：

- user-root activation 501,352 次，kernel-root restore 501,351 次；
- `local_flush=1,008,959`，其中 user activation 与 kernel restore 各贡献约 50 万次；
- `table_misses=0`、`missing_trap=8`、`trap_asid_zero=0`；
- syscall 中 237,860 / 295,101 次保持同 root、同 ASID、同 generation，约 80.6%；
- timer 中 10,177 / 11,053 次满足同一稳定条件，约 92.1%；
- store/load page fault 的同地址空间返回几乎都伴随 generation 变化。

因此可证伪模型是：普通 syscall/timer 返回的绝大多数用户 translation 没有变化，
但旧路径仍在“用户 root -> kernel root -> 用户 root”两端各执行一次全 TLB 失效。
页故障、PTE rewrite、远端 shootdown 和 ASID recycle 则必须继续使旧 translation
失效。若精确验证协议成立，普通返回可复用带非零 ASID 的用户 TLB；若 generation
发生变化，仍必须走保守 flush。

## 3. Stage23 架构与不变量

### 3.1 所有权边界

`PageTableWrapper` 拥有单调 translation generation。wrapper 层的单页和批量
map/unmap 在实际 leaf 写入后推进 generation，使所有用户页表修改路径共用一个
事实来源。该原子递增只在 LoongArch64 编译；RISC-V64 的读取和更新均为 no-op，
不在其热路径引入原子操作。

平台层为每个 CPU 保存已验证的 `{root, ASID, generation}`。`MemorySet` 仍负责
地址空间锁、root/ASID 激活和页表生命周期，`PageTableOps` 仍只负责 PTE，
`TlbProtocol` 仍负责本地与远端失效。VmaMap、ResidentSet、frame、COW、shared/file
mapping、scheduler、VFS 和 allocator 的职责没有并入该缓存。

### 3.2 协议顺序

1. 持有地址空间锁的激活路径先发布 active root，再读取 deferred shootdown 请求。
2. deferred request 未确认时先全量失效，并使 CPU 的验证记录无效。
3. 仅当非零 ASID 的 root、ASID 与 generation 精确匹配时，用户返回可以保留条目。
4. PTE generation 变化、远端 IPI、全 CPU flush、deferred shootdown 和 ASID recycle
   都会阻止旧记录被复用。
5. ASID 0 与 kernel root 共用标签，继续执行保守全量失效；非零用户 ASID 切换到
   kernel ASID 0 时不会发生别名，可跨 kernel-root 区间保留。
6. 只有完成硬件 root/ASID 切换及必要失效后，才发布新的已验证三元组。

这些规则同时关闭两个竞态：编辑者要么在 active-root 集合中看到该 CPU 并同步
shootdown，要么 CPU 返回用户态时看到更高 generation 并本地失效。任何情况下都
不能把旧 PTE translation 当成已验证状态。

## 4. 性能 A/B 与解释

同一远端、同一官方镜像、同一 8G/8 配置的 LoongArch64 完整 production A/B：

| 版本 | guest 编译时间 | 结果 |
| --- | ---: | --- |
| clean control `d676f750` | 555.02 s | `ok=true` |
| Stage23 | 542.02 s | `ok=true` |

改善为 13.00 秒，即 2.34%。330 秒短窗中，第 23 个 crate 从 35.84 秒降到
33.64 秒，约 6.1%；两者都到达 34 个 crate，后续边界基本持平。Stage23 diagnostics
最终记录 `local_flush=254071`。由于 Stage22 与 Stage23 的计数口径随实现改变，不能
把差值当成严格硬件事件计数；它仍足以说明直接机制热点约下降四分之三。

时间收益远小于失效次数降幅，说明 TLB 不是当前剩余编译时间的主根因。Stage23
是正向且有完整正确性门禁的通用优化，可以保留；不能据此宣称 BuildStorm 已接近
性能上限或已经找到最终根因。

## 5. Stage24 反证

Stage24 进一步尝试在普通 LoongArch64 syscall/fault 期间始终保留 user root。
该方案通过四种 release cfg 和双架构 SMP8，但同一 public LA production 在 600 秒
只输出 `BUILDSTORM_TOOLCHAIN ok`，没有 minibuild、BEGIN、panic 或 OOM。因此
“只要 ASID 不变就可以取消 kernel-root 边界”的更激进模型被运行结果强反证。

Stage24 不进入生产候选。后续不得直接删除 `user_interrupt()` 的 LoongArch64
kernel-root boundary；若要改变该边界，必须先证明 trap entry、kernel direct-map、
异常嵌套和用户权限隔离在保留 user root 时仍成立。

历史提交 `3418beb6` 曾直接取消非零 ASID 的 activation flush，并在官方镜像触发
`InstructionNotExist`/SIGILL。Stage23 与它的关键差别是完整的 PTE generation
所有权、远端 shootdown invalidation 和每 CPU 精确验证，不能把两者视作同一方案。

## 6. 验证矩阵

| 门禁 | RISC-V64 | LoongArch64 | 证据等级 |
| --- | --- | --- | --- |
| production release | 通过 | 通过 | `capability-pass` |
| diagnostics release | 通过 | 通过 | `capability-pass` |
| SMP8 生命周期/内存/TLB/ABI 回归 | 通过 | 通过 | `capability-pass` |
| resident/high-arena/user-memory | 通过 | 通过 | `capability-pass` |
| fork/COW/shared/file mapping | 通过 | 通过 | `capability-pass` |
| ASID isolation/reuse/shootdown | 通过 | 通过 | `capability-pass` |
| 160 MiB heap 与 320 MiB allocation | 通过 | 通过 | `capability-pass` |
| official public BuildStorm 8G/8 | 773.45 s，180/180 | 542.02 s，180/180 | `official-pass` |

官方 suite 为 `b5ec6ef8497e1818cbdec3b54bb722f036e57972`，2026-08-17
重新查询远端 `final-2026` 后仍一致。QEMU 为 11.0.3，所有 QEMU 串行运行；镜像、
脚本、judge、marker 和 guest 时间均未修改。

## 7. 证据索引与下一步

- Stage22 归因：`20260817-loongarch64-diagnostics-stage22-tlb-attribution-window330/`
- Stage23 A/B 与 diagnostics：`20260817-stage23-la-tlb-generation/`
- 四 cfg 与 SMP：`20260817-stage23-la-tlb-generation-final-gates/`
- LA official-pass：`20260817-stage23-la-tlb-generation-final-complete/`
- RV official-pass：`20260817-stage23-riscv64-final-complete/`
- Stage24 反证：`20260817-stage24-la-user-root-retention/`

下一轮不应继续盲目删除 TLB 边界。应对 rustc/LLVM crate 间长空档做低扰动的
phase-aligned 归因，把用户运行、fault、block I/O、ext4 metadata/data、allocator
central interaction、MemorySet 等待和 VFS 锁等待按同一时间轴关联。只有某个等待链
能解释完整阶段 wall time，才进入 MM/VFS/allocator 的下一项生产改动。

AI 用于原始日志与源码/diff 的交叉核对、因果模型、实现和串行验证编排。开发者可从
生产 diff、四 cfg/SMP 日志、双架构 serial/runner/judge/hash 和 Stage24 失败串口
独立复核。production 不检查 crate、测试名、路径、命令或输出；diagnostics 仅在
`buildstorm-diagnostics` 下启用。
