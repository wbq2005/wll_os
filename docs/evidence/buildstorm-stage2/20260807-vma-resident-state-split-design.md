# BuildStorm：逻辑 VMA 与 resident-page 状态分离设计门槛

日期：2026-08-07
分支：`wbq_final_buildstorm_compile`
状态：设计审计完成；**未实现 production 代码，未启动新 QEMU**
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

## 3. 必须保持的不变量

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

## 4. 锁、PTE 与 SMP 顺序

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

## 5. 分阶段迁移和可恢复性

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

## 6. 必过验证门

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
