# BuildStorm：逻辑 VMA 与 resident-page 状态分离设计门槛

日期：2026-08-07
分支：`wbq_final_buildstorm_compile`
状态：兼容 `ResidentSet` API 的第一阶段已在 `50c9bd3` 完成并通过 capability gate；
稀疏 resident-run 候选发生启动前功能回归，**未保留为 production 性能补丁**。
关联归因：`20260807-global-build-path-root-cause-audit.md` 第 15 节

> BuildStorm full compile: **unverified / not completed**。本文件不是性能宣称，
> 也不授权在缺少回归的情况下改写 MM。

## 1. 仅验证一个假设

将逻辑 VMA（地址区间、权限、backing、共享语义）和页级 resident/frame/COW 状态拆分后，
匿名 demand fault 不再为“某页是否已驻留”而 split/insert/sort/coalesce 整个 VMA 向量。
如果该结构性改动使同一 300 秒、8 vCPU 窗口的编译进度提高至少 15%，或提高 5%–15% 且
`pf_anonymous_coalesce` 与 MemorySet 锁等待显著下降并且无正确性回归，才保留候选；低于
5% 必须回滚。

不得把本设计与调度器、VFS、块缓存、TLB 策略重构打包，也不得按 crate、命令、路径、
测试或输出选择行为。

## 2. 当前耦合及设计目标

当前 `MapArea` 同时拥有 VMA 元数据和 `Vec<FrameTracker>`。该 vec 只支持两种表示：空，
或从 `start_va` 开始每页连续且完整的 resident 序列。匿名 8 页 fault window 落在未驻留
逻辑 VMA 内时，内核必须把 VMA 分成 left/window/right，才能让 dense `frames` 仍和 VMA
起点对应；随后为了恢复正常拓扑又进行全量合并。

目标表示（名称待实现时确定）：

```text
MemorySet
  logical_areas: ordered, non-overlapping Vec<MapArea>
      interval + permissions + backing + sharing/COW policy
  resident_pages: per-address-space page-indexed mapping
      VPN -> FrameTracker + page state (private/shared/COW/clean-file)
  page_table: hardware PTE view derived from resident_pages + logical area policy
```

`resident_pages` 可以是排序稀疏向量、页号树或其他无歧义的页级索引；实现选择必须先用
小型基准证明 lookup、insert 和 range delete 不会重新引入全 VMA 扫描。它不能依赖
BuildStorm 特有字符串，也不能以“已编译 crate 数”决定策略。

## 3. 职责边界与接口契约（逻辑模块，不要求立即拆文件）

第一阶段可继续放在 `map_area.rs` 和 `memory_set.rs`；边界由类型、可见性和接口保证，
不以文件数量作为完成条件。纯文件整理必须另起提交和测量，不能与性能候选混在一起。

| 逻辑模块 | 唯一职责 | 只允许暴露的操作 | 禁止事项 |
| --- | --- | --- | --- |
| `VmaMap` | 有序非重叠区间、权限、backing、`mmap`/`mprotect`/`munmap`/`brk` 引起的 split/merge | `find(vpn)`、`split_at`、`replace_range`、`merge_adjacent`，返回不可变 policy 快照或显式 VMA 变更 | 不拥有 frame，不读写 resident entry，不直接 map/unmap PTE |
| `ResidentSet` | 唯一拥有用户 data frame 与 `Private`/`Cow`/`Shared`/`FileClean`/`FileDirty` 页状态 | `lookup`、`insert(_run)`、`extract_range`、`split_off`、`merge_from`、`iter_range`、`drain_all`；所有会移动所有权的 API 返回旧 owner 或未消费的输入 | 不修改 VMA 边界，不直接发布 PTE，不自行做 TLB 操作 |
| `PageTableOps` | 从 VMA policy 与 `ResidentPage` state 推导、发布、替换或撤销 leaf PTE | `publish_page`、`reprotect_range`、`unpublish_range`，返回实际变化 VPN 集与待延迟释放的旧 frame | 不持有 frame owner，不修改 VMA/resident 内部字段，不决定 COW/shared 语义 |
| `TlbProtocol` | 对已提交 PTE 变化执行本地失效、目标 CPU shootdown，并在确认失效后释放旧 frame | `invalidate_after_publish`、`revoke_then_retire`；输入为 root、变化 VPN 集、retire list | 不写 PTE，不分配或转移 resident ownership，不改变 VMA |

`MemorySet` 仅作为编排者和既有地址空间锁边界：它先取得 `VmaMap` policy，随后通过
`ResidentSet` 完成唯一所有权变更，再调用 `PageTableOps` 发布 PTE，最后调用
`TlbProtocol`。任何跨模块状态变化都必须经过该顺序；模块之间不得直接写彼此内部字段。

每个接口必须在注释和测试中说明四项契约：前置条件、frame 所有权转移、PTE/分配失败的
回滚点、以及“PTE 撤销 → local/remote shootdown → old frame retire”的 TLB 顺序。普通
demand fault 仅调用 `ResidentSet` 与 `PageTableOps`，不得调用 `VmaMap::split_at`。

### 3.1 变更事务：接口、提交点与回滚

以下是后续实现必须采用的接口级事务。名称是逻辑 API；第一阶段可以仍实现于
`map_area.rs` 与 `memory_set.rs`，但字段必须收紧为私有，任何调用者不得绕过 API。

| 变更 | 允许的接口顺序 | 前置条件与所有权 | 失败回滚与 TLB 顺序 |
| --- | --- | --- | --- |
| anonymous/file demand fault | `VmaMap::find` 取得不可变 policy → `ResidentSet::lookup/insert` → `PageTableOps::publish_page` → `TlbProtocol::invalidate_after_publish`（仅覆盖旧 leaf 时） | VMA 覆盖 VPN、权限允许访问；新 frame 在 `insert` 成功前由调用者临时持有，成功后唯一归 `ResidentSet` | 分配或读 backing 失败时 VMA/PTE 不变；PTE 发布失败时 `extract_range` 取回刚插入页并释放。普通缺页不得调用 VMA split/merge。 |
| `mprotect` | `VmaMap::replace_range` 生成 policy 变更集 → `PageTableOps::reprotect_range` → `TlbProtocol::invalidate_after_publish` | range 已完整覆盖；resident 页不移动，COW 的有效写权限由 VMA policy 与页状态共同计算 | PTE 写入前保留旧 policy；任一 PTE 失败即恢复已改 PTE 与 policy。若撤销 leaf，先写 PTE、再 local/remote invalidate，最后才允许 retire。 |
| `munmap` / `brk` 缩小 / exit | `VmaMap` 先生成 detach plan → `ResidentSet::extract_range` → `PageTableOps::unpublish_range` → `TlbProtocol::revoke_then_retire` → 提交 `VmaMap` | 所有受影响 VPN 有可枚举的 resident entry；`extract_range` 的返回值是尚未释放的唯一 owner | 在 PTE 撤销失败前恢复 resident/VMA；撤销成功后的旧 frame 只能进入 retire list，必须等本地和远端 shootdown 完成才 drop。 |
| fork / private COW | `VmaMap::clone_policy` → `ResidentSet::clone_for_fork` → `PageTableOps::reprotect_range`（父、子）→ `TlbProtocol::invalidate_after_publish` | `CLONE_VM` 不走本路径而共享同一 `Arc<Mutex<MemorySet>>`；private writable resident page 产生父子各一项 Cow 状态，共享 frame ref | 新子地址空间/PTE 创建失败时丢弃子 `ResidentSet`，父 PTE/COW 状态保持原值；提交后父侧只读 PTE 必须先完成 shootdown，才允许用户继续写。 |
| COW store fault | `ResidentSet::replace_cow_page` → `PageTableOps::publish_page` → `TlbProtocol::invalidate_after_publish` | VPN 归属 private、页状态为 Cow；新 frame 在 replace 成功前由调用者拥有 | 复制或 PTE 发布失败时保留旧 Cow entry/PTE，释放候选 frame；成功后旧共享 frame 的引用由取代操作返回，按普通 refcount 释放。 |
| shared/file writeback 或失效 | `ResidentSet::iter_range/extract_range` 提供页身份与 dirty 状态 → backing I/O → `PageTableOps::unpublish_range`（需要撤销时）→ `TlbProtocol` | shared 页的 identity 来自 backing object + page offset，不来自 VMA 片段；file I/O 不在 MemorySet 锁内等待 | I/O 失败不丢失 `FileDirty` 状态；若需要撤销映射，旧 frame 一律在 shootdown 后释放。 |

`PageTableOps` 返回的不是裸 PTE 修改结果，而是 `PteChangeSet { changed_vpns,
retire_after_tlb }`；只有 `TlbProtocol` 可以消费其中的 retire list。这样既避免
“PTE 已撤销但 frame 已提前释放”，也使调用点无法把 TLB 维护遗漏在错误分支。

## 4. 必须保持的不变量

1. `logical_areas` 按 start address 排序、两两不重叠；每个用户虚拟页至多属于一个 area。
2. VMA 的 split/merge 仅由 `mmap`、`munmap`、`mprotect`、`brk`、ELF 装载等**权限或
   backing 语义**改变触发；单页 demand fault、预取、COW copy 不得改变逻辑 VMA 拓扑。
3. resident entry 必须落在一个覆盖它的 logical VMA 内；其 PTE 权限是 area policy 与
   page state（尤其 COW）的交集。
4. 一个有效 leaf PTE 必须有且只有一个相容的 resident entry；unmap 后 PTE、resident
   entry 和 frame ownership 必须一起消失，不能留悬挂映射或 double free。
5. private anonymous/file COW 的 frame refcount、write bit 和 COW bit 必须在 parent 与
   child page state 中一致；首次写入只复制该页或既有通用窗口策略规定的页，绝不把共享
   写权限错误地授予另一地址空间。
6. `MAP_SHARED`、共享内存、clean file cache 的共享 frame 不得因拆分表示而变成私有页；
   `munmap`/writeback 仍按原有 shared-file 语义完成。
7. fork、exec、exit、失败回滚和地址空间 drop 均释放每个 frame 一次，并归还 ASID。

## 5. 锁、PTE 与 SMP 顺序

`SharedMemorySet = Arc<Mutex<MemorySet>>` 继续是单一地址空间的一致性边界。一次页故障、
VMA 更新或 fork 在持有该锁时建立可以发布的 resident/PTE 状态；不新增全局 MM 锁，也不在
持锁时取得会反向等待 MemorySet 的锁。frame allocator 的既有锁次序必须记录并保持，
避免 MemorySet ↔ frame allocator 反转。

为控制长临界区，允许在锁外准备不发布的 frame/元数据；重新持锁后必须重新验证 area、
页状态和 PTE 仍匹配，才原子提交。验证失败时丢弃暂存资源并重试或返回已有错误，不能覆盖
并发 COW/unmap 的结果。

PTE 写入后沿用现有本地/远端 TLB 失效协议：先完成 resident metadata 与 PTE 的一致更新，
再对实际运行该 root 的 CPU 做必要 shootdown，最后允许旧 frame 释放。不得因“逻辑 VMA
未拆分”省略 mprotect、munmap、COW 写保护或 fork 后的 TLB 失效。root activation 的 active-
root 快路径保持原语义，不能将其与本候选混改。

## 6. 分阶段迁移和可恢复性

1. 先增加内部 resident-page 抽象与 feature-off 单元/ABI 回归，不改变 `MapArea` 的外部
   syscall 语义。
2. 只迁移 anonymous demand fault；file fault、COW、fork 先保持兼容桥接。每个地址空间
   禁止同时存在两份可写的 owner 表；桥接层必须有明确唯一所有者。
3. 验证 anonymous `mmap`、`mprotect`、`munmap`、`brk`、多线程 `CLONE_VM`、fork COW 后，
   再迁移 private file/COW 和 shared/file mapping。
4. 每步失败（ENOMEM、非法地址、PTE map 失败）以事务方式回滚新分配 frame、resident entry
   和已经写入的 PTE；恢复原来可见状态。错误码保持当前 syscall 契约。
5. 不保留旧的“fault 后全量 coalesce”作为隐藏 fallback；如果兼容桥接需要它，必须单独测量
   并视为该阶段尚未解决，不得宣称性能完成。

## 7. 必过验证门

在 production 候选前后都保存 commit、dirty diff、kernel/image SHA-256、suite commit、
QEMU 完整参数、serial、judge 和 host 指标。顺序为：

1. 新的独立 MM 回归：匿名稀疏页 fault、`mprotect` 跨范围、partial `munmap`、fork+COW、
   shared mapping、并发 `CLONE_VM`；测试名和路径不得引用官方 BuildStorm。
2. RISC-V64 与 LoongArch64 release build，以及双架构 SMP/TLB/ASID 回归。
3. RISC-V64 官方镜像、production release、feature 全关、`-snapshot -m 8G -smp 8` 的
   单一 300 秒窗口，与已保存同配置基线比较。
4. 仅当达到保留门槛，运行双架构 toolchain/minibuild 和至少一个完整 15,000 秒 production
   BuildStorm；只有 serial 出现精确 `BUILDSTORM_COMPILE mode=multi ok=true` 才能称 full
   compile 成功，随后运行官方 judge。

诊断计数只通过 `buildstorm-diagnostics` feature 提供，默认 release 关闭；每十秒聚合，
不逐 syscall、page fault 或 block I/O 打印。

## 8. 2026-08-07 稀疏 ResidentSet 候选结果：回滚

本次实现保持上述逻辑模块边界在现有 `map_area.rs` / `memory_set.rs` 内，而没有进行单纯的
物理文件重排：匿名 VMA 使用相对 VPN 的 sparse runs，`ResidentSet` 作为 frame 唯一 owner，
并以页级 `Private` / `Cow` 状态处理匿名 fork、COW store 和“fork 后未驻留页首次 store”。
`PageTableOps` 仅从 VMA policy 与页状态发布 PTE，`TlbProtocol` 在 PTE 更新后执行现有
shootdown，再 drop 被替换的旧页。

已验证的 capability gate：RISC-V64 与 LoongArch64 feature-off release build 均通过；两架构
SMP user-memory lifecycle 回归均通过，包含 sparse anonymous fault、跨范围 `mprotect`、partial
`munmap`、fork+COW、shared mapping、`CLONE_VM` identity，以及此前失败的 COW-hole store-first。

RISC-V64 diagnostics 窗口（官方镜像、`-snapshot -m 8G -smp 8`）出现
`BUILDSTORM_TOOLCHAIN ok`、`BUILDSTORM_MINIBUILD ok` 和 `BUILDSTORM_BEGIN mode=multi`，无实际
panic/OOM；诊断计数的 `anonymous_coalesce calls=0`。这是“实现确实避开旧 coalesce 路径”的
证据，不是性能结论。

随后同配置 feature-off production 300 秒窗口的原始证据保存在远端
`/srv/buildstorm/evidence/20260807-riscv64-resident-state-production-window300/`：23 条
`Compiling`、2 条 `Finished`、最后 crate 为 `ax-posix-api`、marker window
`300.0432353469987 s`、无 panic/OOM、无 `BUILDSTORM_COMPILE` 结果。保存的同配置基线也是
23 条 `Compiling`、2 条 `Finished`、最后 crate `ax-posix-api`、marker window
`300.05089544099974 s`。因此可见进度提升为 **0%**，低于 5% 的保留门槛；候选的四个
production 源码修改已在本地及远端隔离 worktree 回滚，未提交、未推送，也不会进入长跑或 judge。

窗口时间线进一步排除了“相同 crate 数但更早到达”的解释：候选第一个 `Compiling` 位于 marker 后
`150.60147683699688 s`，而基线位于 `102.94391007099966 s`，候选晚了约 `47.66 s`；最后一个
可见 crate 也从基线的 `115.76524958699974 s` 推迟到候选的 `159.21617303099993 s`。因此仅凭
`anonymous_coalesce calls=0` 不能宣称端到端收益；该实现的稀疏索引、frame/PTE 发布或其它
未分解成本造成的影响仍是**待审计推测**，不得当作已经证实的单一根因。

完整 BuildStorm 仍为 **unverified / not completed**；缺少精确
`BUILDSTORM_COMPILE mode=multi ok=true`。

### 8.1 同配置比较更正

上面的“0%”不能作为最终候选判定：该窗口的候选 provenance 是 `50c9bd3` 加 sparse
ResidentSet dirty diff，而引用的 23-crate control 来自 `4a6bee2`，并非同一 source commit。
候选源码已暂时回滚，但候选 kernel SHA-256 `127c80966cb3e27f0b964f02fccf96007845c5d6381f7d2135251099de4b327e`
和原始日志仍保存在远端。必须先运行 clean `50c9bd3` control，再决定该候选是否满足保留
门槛；跨 commit 的 47.66 秒时间差只能作为线索，不能作为回归结论。

### 8.2 clean `50c9bd3` control：正式拒绝

已补跑同一 commit、同一官方镜像、默认 production feature、`-snapshot -m 8G -smp 8` 的
clean control：`/srv/buildstorm/evidence/20260807-riscv64-50c9-clean-control-window300-default/`。
它和候选均在 300 秒内到达 23 条 `Compiling`、2 条 `Finished` 和相同最后 crate，均无
panic/OOM/compile-success marker；因而 event count 单独不足以判定收益。

时间线给出了有效的保留判定：clean control 第一条 `Compiling` 为 marker 后
`122.56186046500079 s`，最后一条为 `131.77706680900155 s`；候选对应为
`150.60147683699688 s` 和 `159.21617303099993 s`，分别晚约 `28.04 s` 与 `27.44 s`
（约 23% / 21% 时序回归）。候选未达到 5% 的进度提升门槛，且明显更慢，故 production
源码保持回滚；不运行候选的长跑或 judge。

### 8.3 回滚 sparse 候选的补充源码审计（不重新启用候选）

本地保留的候选快照 `_candidate_memory_set.rs` 能证明两个实现层面的事实；它不是该次
远端 kernel 的完整 dirty diff，因此**不能单独证明**启动前退出或 21--23% 时序回归的唯一
运行时根因。

1. `map_area_page_window()` 在候选快照中接收 `start_idx/page_count`，却调用无范围参数的
   `area.resident.iter_pages()`，随后才在循环内过滤 VPN。anonymous demand fault 在插入最多
   8 页后每次调用该函数。因此该版本的范围 PTE 发布必然遍历该 VMA 的全部 resident page，
   而不是只遍历刚安装的 1--8 页；resident page 随运行增长时，这形成了额外的线性扫描。
   这是源码已确认的复杂度缺陷；它很可能解释部分端到端回归，但没有该版本的分项 profile，
   故“它就是全部回归原因”仍是推测。
2. 候选的 clean-file-cache fault 仍调用 `split_area_at(page_start)`、
   `split_area_at(window_end)` 和 `coalesce_areas()`。这与本设计的目标不一致：anonymous 和
   clean-file demand fault 都不得改变 VMA 拓扑。该路径也必须在重新实现前纳入 lifecycle
   回归；保存的启动前退出 serial 没有给出精确的 fault-site，不能把它断言为已证实的退出原因。

因此重新评审的实现门槛新增如下：

- `PageTableOps::publish_page/publish_range` 只能消费 `ResidentSet::lookup` 或
  `ResidentSet::iter_range(start_vpn, end_vpn)`；范围 PTE 发布禁止以 `iter_pages()` 后过滤的
  方式实现。
- `ResidentSet` 的 run lookup、插入相邻 run、range iteration 必须以小型 no_std 基准证明只与
  被请求 run/页数相关，而不是随整个 VMA 的 resident page 总数线性增长。
- 匿名和 clean-file fault 的独立回归都必须断言 VMA count 不变；只有 mmap/mprotect/munmap/
  brk/ELF 等语义操作才可调用 `VmaMap` split/merge。
- 在新的、完整保存的 dirty diff、双架构 lifecycle 回归和 feature-off 300 秒 clean control
  之前，不得把该审计结论变成新的 production 候选。
