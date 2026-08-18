# wll_OS 当前内核架构总设计

## 1. 目标与范围

wll_OS 是面向 RISC-V64 与 LoongArch64 的 Rust `no_std` 内核。当前架构以“共享策略、平台机制分离”为主线：进程、调度、MM、VFS 和 Linux ABI 由架构无关层实现；页表格式、trap、TLB、CPU 启动、时钟、UART 和块设备接入位于 PolyHAL/平台边界。

本文描述 BuildStorm `official-pass` 对应源码的实际结构，不把未实现的目标架构写成现状。

## 2. 总体分层

```text
用户程序 / glibc / rustc / cargo
              |
        trap 与 syscall ABI
              |
+-------------+----------------------------------+
| 进程与任务 | 调度/等待 | MM | VFS/FD | IPC/时间 |
+-------------+----------------------------------+
|      架构无关所有权、锁协议和 Linux 语义       |
+------------------------------------------------+
| PolyHAL: 页表/PTE/TLB/trap/启动/设备抽象        |
+------------------------------------------------+
| RISC-V64 QEMU/VisionFive2 | LoongArch64/2K1000 |
+------------------------------------------------+
```

关键源码入口：

- 启动与 feature 组合：`os/src/main.rs`、`os/src/config/`。
- 平台与 SMP：`os/src/platform.rs`、`os/src/task/manager.rs`。
- trap/syscall：`os/src/trap/`、`os/src/syscall/`。
- 进程与任务：`os/src/task/`。
- 内存：`os/src/mm/`。
- 文件系统：`os/src/fs/`、`vendor/ext4_rs/`。
- 架构页表：`patch/polyhal/src/pagetable/`。

## 3. 启动与平台模型

### 3.1 设备树驱动的不可变事实

启动 CPU 初始化 DTB 后，平台层提取并保存：

- platform kind；
- physical CPU 数量与 hardware ID；
- 物理内存总量；
- 架构相关 RTC 和设备信息。

`PlatformKind` 区分 QEMU virt、VisionFive 2/JH7110、Loongson 2K1000LA 和 unknown。上层只查询稳定事实，不按 BuildStorm 或 guest 命令选择平台行为。

### 3.2 CPU 生命周期

每个 CPU 经过 `Absent -> Present -> Starting -> Online/Failed`。只有完成 trap、timer、调度和跨核通知初始化的 CPU 才标为 Online。online mask 是调度 affinity、idle publication 和 TLB shootdown 的共同依据。

### 3.3 架构边界

- RISC-V64：OpenSBI/DTB、Sv39、SBI secondary start、`sfence.vma`。
- LoongArch64：PGDL/ASID、PLV trap、LoongArch TLB 指令和对应中断控制。
- 共享层不直接编码 PTE 位布局和 CSR/寄存器细节。

## 4. Trap、用户态与 syscall

### 4.1 TrapFrame 所有权

每个用户任务保存可恢复的 `TrapFrame`。进入 syscall 时，CPU-local 指针只在当前 trap 生命周期内有效；execve 和 `rt_sigreturn` 用 CPU-local 完成标志告诉统一返回路径不要机械增加 PC。

### 4.2 内核/用户页表边界

系统调用执行期间保持内核地址空间。VFS、VirtIO、调度和内存管理不在用户页表下运行。真正准备返回用户态时：

1. 取得目标任务和 `MemorySet`；
2. 激活目标 root/ASID；
3. 恢复完整用户寄存器；
4. 由架构指令返回用户态。

RISC-V 返回路径在寄存器完全恢复前保持 SIE 关闭，避免 IPI 把半恢复现场误判为用户 trap；`sret` 后才恢复中断。LoongArch 保存并恢复用户需要的浮点/向量状态。

### 4.3 syscall 组织

syscall 层负责 Linux ABI 校验和用户指针复制，具体资源语义下沉到 task/MM/VFS/IPC。用户内存访问统一通过 `MemorySet` translate/copy helper，越界返回 `EFAULT`，不直接解引用用户虚拟地址。

## 5. 任务、进程与调度

### 5.1 TaskControlBlock

`TaskControlBlock` 保存稳定身份和共享对象：PID/TGID、`SharedMemorySet`、文件描述符表、trap frame、调度属性、信号和父子关系。频繁变化的状态位于受锁 inner state 中。

Linux clone flags 决定共享边界：

- `CLONE_VM` 共享同一个 `Arc<Mutex<MemorySet>>`；
- 普通 fork 创建独立地址空间并建立 COW；
- `CLONE_FILES` 共享 FD table，否则复制表项并共享 open-file-description；
- vfork 显式等待 child exec/exit，不把普通 fork 生命周期混入。

### 5.2 ReadyQueue

用户任务和内核任务各有一个 `ReadyQueue`：

- `VecDeque<Arc<TCB>>` 保存 runnable 顺序；
- `BTreeSet<PID>` 镜像成员关系，避免重复入队；
- dequeue 时实时检查 status、CPU affinity 和有效优先级；
- RT 任务保留有界公平策略。

### 5.3 runnable 发布协议

本地时间片/syscall 重排队调用 local 路径；只有 affinity 不允许当前 CPU 时才转成外部通知。外部 producer：

1. 在队列锁内发布任务并去重；
2. 释放队列锁；
3. 从满足 affinity 的 idle mask 原子领取一个 CPU；
4. 只向该 CPU 发送 IPI。

idle CPU 先发布 idle bit，再复查队列。于是 producer 发布与 idle-side 复查之间不存在丢失唤醒窗口。

### 5.4 WaitQueue 与退出

`WaitQueue` 把阻塞、事件 generation 和唤醒统一起来。pipe/socket/futex/child wait 不依赖轮询。任务退出先发布 Zombie/exit record 并唤醒 waiter，再进入资源回收；open file、地址空间和共享对象按引用生命周期释放。

## 6. 虚拟内存总体设计

### 6.1 职责拆分

```text
UserVaLayout  -> 哪些 VA 合法、mmap 在哪里找空洞
MapArea       -> VMA 区间、权限、backing、策略
ResidentSet   -> 哪些页实际驻留、每页状态与 frame 所有权
MemorySet     -> address-space identity、areas、page-table root
PolyHAL PT    -> PTE 编码、walk、map/unmap、root 激活
TlbProtocol   -> 本地 flush 与跨 CPU shootdown
```

这五层不能互相替代：VMA 不拥有页表页，PTE 不拥有用户数据 frame，TLB 失效不改变 VMA 策略。

### 6.2 UserVaLayout

`os/src/config/user_va.rs` 集中定义：

- low user range：`PAGE_SIZE..USER_STACK_TOP`；
- low mmap arena：`0x2000_0000..USER_STACK_TOP`；
- high mmap arena：`0x20_0000_0000..0x40_0000_0000`；
- `USER_ADDRESS_LIMIT` 覆盖两个不连续 arena。

高 arena 位于正 canonical Sv39 用户空间，也可由 LoongArch PGDL 接受；内核 DMW 位于独立高地址。固定映射必须完全位于合法用户区，普通 mmap 先低后高，避免与历史 ABI 冲突。

### 6.3 MapArea 与 VMA 元数据

`MemorySet.areas` 当前是 `Vec<MapArea>`。每个 `MapArea` 保存 `[start,end)`、PTE 权限、backing 和一个 `ResidentSet`。backing 分为匿名、共享内存、private file 和 shared file。

insert、split、mprotect、munmap 和 coalesce 都先维护 VMA 元数据，再更新受影响的 resident/PTE。当前没有单独的 interval tree；高 arena 解决了最紧迫的容量问题，超大 VMA 数的检索复杂度是后续可演进点。

### 6.4 ResidentSet

`ResidentSet` 是一个 VMA 的唯一 resident data-frame owner。内部以 VMA-relative VPN 为 key 的 `BTreeMap` 保存连续 run，只为实际驻留页分配元数据。

它支持：

- page lookup/insert；
- contiguous run 插入；
- range extract/split；
- run merge；
- 保留 `PageState` 的共享与脏页语义。

`PageState` 区分 Private、Cow、Shared、FileClean、FileDirty。VMA 权限表达策略，page state 表达当前 resident leaf 的所有权状态。

### 6.5 FrameTracker 与分配域

物理 frame 分为三个域：

1. tracked user/file-cache frame：由 `FrameTracker` 持有；
2. page-table frame：唯一 tracker 通过 `into_raw_ppn()` 转交 PolyHAL；
3. contiguous frame：kernel heap 扩展、VirtIO DMA、kernel stack 等显式范围。

每个受管 PPN 有一个 `AtomicU32` 引用计数。引用表按最多 `2^18` 个 frame
分块，每个 production 块最多 1 MiB，避免大内存配置在 heap 尚未动态扩展前申请
一个数十 MiB 的连续数组。clone 增加引用，drop 的最后一个 owner 释放 frame。
共享 frame 禁止转交 raw ownership，从类型边界避免 COW/shared 页被页表析构误释放。

diagnostics feature 额外维护 free/tracked/page-table/contiguous 原子状态机；production 只保留实际引用计数。

### 6.6 Kernel heap 分层分配器

kernel heap 使用两层结构：中央 `buddy_system_allocator::Heap` 唯一拥有静态/动态扩展的 heap range 和大对象；每个 CPU 在 8 B 至 4 KiB 的 canonical class 上维护有界 cache。cache 容量为每 class 64 个 block，中央 refill/flush 各以最多 16 个 block 批处理。

cache 只改变分配路径，不改变底层所有权：所有回中央的 block 使用 canonical `Layout(block_size, block_size)`；中央 OOM 前 drain 全部 CPU cache 后重试；cache 锁与中央锁不同时持有。全局 allocator 入口使用本地 IRQ save/disable/restore，保证当前 CPU 身份稳定，并避免 timer/VirtIO 中断在同 CPU 递归获取 `spin::Mutex`。

### 6.7 Demand fault 与预取

匿名和文件 lazy area 在首次访问时分配/读取页。匿名 fault 可按小窗口批量建立连续 resident，clean file fault 使用受限 read-ahead；窗口始终截断到 VMA 边界，失败时保持所有权可回收。

### 6.8 fork/COW/shared/file

- private writable mapping：parent/child 共享 `FrameTracker`，PTE 清 W，page state 设 Cow；写 fault 分配并复制私有页。
- readonly mapping：共享 frame，无需写时复制。
- shared memory 与 shared file：保持 shared state，写入对共享 owner 可见。
- file dirty page 在 msync/unmap/exit 的 writeback 责任路径处理；clean page 可直接丢弃并在下次 fault 重读。

### 6.9 exec 与析构顺序

exec 先在独立 `MemorySet` 中加载 ELF、解释器、stack 和 auxv。全部成功后替换 TCB 的 shared memory set；旧空间不再可运行后才退休 address-space identity，释放页表树，最后释放 VMA resident frames。

普通 exit 和 exec replacement 使用同一所有权次序，不通过测试路径特殊处理。

## 7. 页表与 TLB 协议

### 7.1 PageTableOps

PolyHAL `PageTable` 提供 map/unmap/translate/release，架构文件实现 PTE 位、页表级数和 root restore。`KernelPageAlloc` 是 PolyHAL 与内核 frame allocator 的适配器：分配页表页时完成 tracked -> raw ownership 转移，释放时归还 frame allocator。

### 7.2 address-space identity

每个 `MemorySet` 有不可变 identity，用于区分 root 重用和 TLB 代际。identity 与 root 生命周期绑定，不能因 exec 或 clone 原地改写成另一个地址空间。

### 7.3 TLB 规则

- 新 leaf 映射或权限改变执行目标 VA 本地失效；
- unmap/range rewrite 完成 PTE 更新后执行 shootdown；
- 跨 CPU shootdown 只针对 online CPU；
- address-space 析构前保证不会再有 CPU 使用旧 root；
- RISC-V64 与 LoongArch64 的具体指令留在各自实现。

LoongArch64 的 `PageTableWrapper` 额外拥有单调 translation generation。所有 wrapper
级 map/unmap 都在 leaf 写入后推进 generation；平台层按 CPU 保存已验证的
`{root, ASID, generation}`。用户返回只有在非零 ASID 的三项身份精确匹配时才复用
translation。PTE 编辑、remote IPI、deferred shootdown、全局 flush 和 ASID recycle
都会撤销验证，ASID 0 始终走保守全 flush。激活协议先发布 active root 再读取
deferred request，使并发编辑者要么同步 shootdown 当前 CPU，要么由新的 generation
迫使该 CPU 在返回用户态前失效。

RISC-V64 不执行 generation 原子更新，继续使用其既有 ASID/sfence.vma 协议。
当前实现仍以逐页操作为主。历史批处理候选没有通过 300 秒性能门槛，因此没有引入
未证实的复杂事务 API。

## 8. VFS、FD 与 ext4

### 8.1 VFS 与后端

VFS 提供路径规范化、inode/file 抽象和 FD table。MemFS 与挂载后的 ext4 是并列后端；regular file、directory、symlink、pipe、socket、eventfd 等通过 `FileDescriptor` 统一暴露 read/write/poll/ioctl/close 能力。

### 8.2 open-file-description 语义

dup/fork 共享 OFD，因此共享文件偏移、状态标志和 `flock` owner。只有最后一个描述符引用释放时才解锁并唤醒 waiter。POSIX record lock 与 flock 使用独立表，避免混淆 owner 生命周期。

### 8.3 ext4 mutation 与 namespace 两层锁

- `EXT4_MUTATION_LOCK`：保护 ext4_rs 缺少内部串行化的 bitmap/inode transaction。
- `NAMESPACE_LOCK`：保护路径解析、dirent/inode 更新和 cache publication 的原子可见性。

lookup 持 shared namespace transaction；namespace writer 从第一次 parent/name resolution 到缓存失效持 exclusive transaction。regular data I/O 和 writeback 不持 namespace lock，避免把大文件写入串行化。

### 8.4 namespace cache

缓存包括 positive path、negative path、directory snapshot 和 inode metadata。writer 精确失效受影响 parent/path，并增加 generation。reader 只在同一 shared transaction/generation 中发布结果。

VFS 的 parent-symlink 与 readlink result cache 使用 64K 有界容量，满载时逐项
淘汰，避免把大型编译工作集整表清空。`open_path()` 在完成 tmpfs/ext4 后端选择
后，对同一 ext4 路径只取得一次 generation-validated `(ino, kind)`，并在该次
open 生命周期内复用。缓存只保存解析结果，不拥有 inode、file description、VMA
或 frame；rename/unlink/symlink mutation 仍通过既有 invalidation protocol
撤销相关结果。

### 8.5 文件数据路径

ext4 数据路径包含：

- extent-aware `read_at`；
- physical-block run cache；
- clean page cache；
- bounded regular file cache；
- executable image cache；
- dirty range 与 writeback queue。

缓存容量有明确上限，namespace 或 inode mutation 触发相应失效。官方镜像始终用 QEMU snapshot，不修改基准镜像本体。

## 9. IPC、时间与信号

- pipe/socket/eventfd 支持 blocking/nonblocking 与 poll 唤醒。
- Unix seqpacket 保存消息边界和 EOF。
- futex key 由地址空间/共享 backing 语义确定，阻塞通过 WaitQueue。
- interval timer state 仍在 TCB inner 中，active-owner index 只加速扫描，不取得 timer state 所有权。
- 信号投递修改 task pending state；实际用户 handler frame 在安全用户返回边界构建。
- 同步用户异常保留故障 PC 和完整 `ucontext`。已安装且未屏蔽的 handler 通过
  Linux signal frame 接收异常，`rt_sigreturn` 可恢复 handler 修改后的 PC；默认、
  忽略或屏蔽同步异常时终止线程组，避免重复执行同一故障指令。
- monotonic/uptime 来自真实 timer，不为 BuildStorm 改写或缩放。

## 10. 锁顺序与析构不变量

主要规则：

1. 不持 TCB inner 锁进入全局 timer owner registry；syscall/teardown 先释放 inner。
2. namespace outer lock 先于 ext4 mutation 和 namespace cache lock。
3. regular data/writeback 不持 namespace outer lock。
4. ready-queue 锁释放后才发送 IPI。
5. page-table mutation 在 `MemorySet` 锁内，shootdown 不重新获取同一 `MemorySet`。
6. exec/exit 先取消可运行性，再退休 identity/page table/resident owner。
7. `FrameTracker` 最后引用才归还 allocator；raw/contiguous 域必须走各自释放接口。
8. kernel heap cache 锁与中央 buddy 锁不同时持有；allocator 临界区内本地中断保持关闭并在退出时恢复原状态。

这些规则是 correctness 边界，不因 production/diagnostics cfg 改变。

## 11. Diagnostics 与回归架构

### 11.1 cfg 隔离

`buildstorm-diagnostics` 提供：

- lock wait/hold 聚合；
- MM、exec、fault、mmap 和 scheduler 阶段计数；
- namespace generation crossing；
- frame owner shadow；
- per-CPU heap cache hit/miss、refill/flush 与中央交互计数；
- 周期性固定大小 snapshot。

所有诊断状态都由 feature gate 包围，production 默认关闭。诊断只观测，不修改 syscall 结果、调度策略、时间和 marker。

### 11.2 独立 SMP 回归

`smp-regression` 不包含官方任务名和预期输出逻辑，覆盖：

- 八 CPU 启动、运行和跨核唤醒；
- resident/high-arena/user-memory lifecycle；
- fork/COW/shared/file mapping；
- ASID/root/TLB isolation；
- interval timer；
- heap stress；
- concurrent namespace create/unlink/rename/open-unlink/cache invalidation。

该回归是 `capability-pass`，官方完整镜像运行才是 `official-pass`。

## 12. 最近已提交基线的 BuildStorm 完成状态

RISC-V64：

```text
BUILDSTORM_COMPILE mode=multi ok=true elapsed_s=774.62 cores=8 bytes=1683456 arch=riscv64
```

LoongArch64：

```text
BUILDSTORM_COMPILE mode=multi ok=true elapsed_s=620.70 cores=8 bytes=1716224 arch=loongarch64
```

两架构官方 judge 自动项均为 180/180。完整实验、AI 披露和复现步骤见 `docs/buildstorm-2.3-design-optimization-cn.md`。
这些 marker 对应提交 `6045dd2534668ea4d1cff94725c41afb0a855af4`；第 15 节
新增架构在本轮 candidate 中单独验证，不借用这里的 `official-pass`。

## 13. 当前限制与演进方向

- VMA metadata 当前是 Vec，超大集合可演进为带 gap augmentation 的有序索引，但必须保持 ResidentSet 所有权边界。
- 页表缺少已被实测证明有收益的通用 edit transaction；在没有新证据前不增加复杂度。
- kernel stack 的完整显式 owner/回收审计仍值得继续。
- per-CPU heap cache 当前按固定上限和批量参数工作；后续若调整参数，必须保持 canonical layout、OOM drain、IRQ 状态和锁序不变量，并重新执行双架构压力与完整官方运行。
- 实板支持必须分别验证 boot、interrupt、timer、SMP、DMA、storage、cache maintenance；QEMU 通过不能代替 VisionFive 2 或 Loongson 2K1000LA gate。
- 初赛回归仍是提交要求，BuildStorm 完成不替代其他测例。

## 14. 2026-08-15 regular-file data-plane update

当前 VFS/ext4 数据平面采用“dense 小文件 + sparse 大文件页缓存 + clustered
writeback”的分层结构。`CachedRegularFile` 只在文件不超过 8 MiB 时保留连续
字节数组；更大的文件以 `BTreeMap<page_index, FrameTracker>` 保存实际修改页，
读取缺页直接走 extent-aware ext4 路径。回写先按 dirty range 切成 256 KiB
cluster，再对连续物理块做批量写入；partial block 仍由 ext4 完成
read-modify-write。truncate 会清理 EOF 所在块的尾部字节，避免后续扩展暴露旧数据。

这层只拥有文件数据缓存和持久化状态，不拥有 VMA、PTE 或用户页表 frame；
`MemorySet -> MapArea -> ResidentSet -> FrameTracker` 的所有权链保持不变。
namespace transaction lock 只保护名字解析和目录 mutation，regular-file
writeback 不持有 namespace lock；RISC-V64 和 LoongArch64 的平台差异仍在
VirtIO、页表和 TLB 边界内，数据面逻辑共用。

VFS lookup 复用层的最新 production 结果为 `774.62s/620.70s`；相对前一版
同配置 VFS production 对照分别提升 `9.94%--10.67%` 与 `7.97%--9.68%`。
官方成功 marker、镜像/内核 hash 和 SMP 生命周期证据见
`docs/buildstorm-2.3-design-optimization-cn.md` 第 12 节。该层已纳入当前架构，
但其确定性淘汰仍不是完整 LRU；未来统一 cache replacement 时必须保持
namespace generation 与精确失效边界。

## 15. 2026-08-15 大内存、多核启动与同步异常边界

LoongArch64 评测配置暴露了两个启动期容量不变量。第一，36 GiB 内存约有
943 万个 4 KiB frame；若引用表用单个 `Vec<AtomicUsize>`，有效数据约 72 MiB，
buddy allocator 会为它寻找 128 MiB 阶，而初始化又早于动态 heap 扩展。当前
`FrameRefRegion` 因此采用分块 `AtomicU32` 表。分块只改变索引和分配形状，不改变
`FrameTracker`、page-table raw frame 或 contiguous frame 的所有权域。

第二，secondary boot stack 的容量必须满足
`MAX_CPUS * SMP_BOOT_STACK_SIZE`。两种架构的汇编均预留 12 个 128 KiB 槽，
Rust 启动路径使用 `_smp_boot_stacks_end` 在启动 secondary CPU 前断言容量。汇编
仍只负责架构入口和栈区，CPU 的 Present/Starting/Online 状态机、调度、IPI 和
TLB 协议保持在共享平台层。

RISC-V64 的嵌套 QEMU 能力还要求同步 `SIGILL` 遵循 Linux signal ABI。TCG 的
host CPU 能力探测会先安装 handler，再故意执行候选指令，并由 handler 修改
`ucontext.pc` 后继续。trap 层现在把可捕获的同步 `SIGILL` 交给 signal 层构造
frame；默认处置和不可安全返回的 blocked/ignored 情况仍终止。该机制不识别
QEMU、cargo、路径或输出，平台特定指令解码仍留在架构 trap 边界。

当前证据、配置与分级见
`docs/evidence/buildstorm-stage2/20260815-large-memory-smp-sigill-validation-cn.md`。
当前 candidate 已在未修改的 public suite 上以评测日志对应的 16G/8 与 36G/12
配置完成双架构 clean build，官方 judge 均解析为 180/180。评测机不可取得的
hidden nested-QEMU 门仍需新提交复测；独立 probe 只能证明通用能力，不能代替
隐藏评测结果。

## 16. 2026-08-16 harness、目录 ABI 与 clean-page cache 边界

### 16.1 harness 只负责选择 workload

默认 `HARNESS_LIBC=glibc`，因此正式 artifact 不再在计分 glibc 结束后自动运行
第二轮 musl workload。`musl` 与 `both` 仍是显式构建选项。该选择只发生在 harness
发现脚本阶段，不改变 syscall、VFS、MM、调度或时间语义。glibc BuildStorm 内部
使用的 `*-unknown-linux-musl.json` 是 ArceOS build target，不是第二个 harness
suite，仍完整执行。

### 16.2 Linux 目录 ABI 统一入口

asm-generic legacy `mkdir(1030)` 与 `mkdirat(34)` 统一进入
`sys_mkdirat`。legacy wrapper 只补充 `AT_FDCWD`，不复制路径解析、权限、umask、
ext4 mutation 或 namespace invalidation。独立 `directory-abi` 回归位于
`smp-regression` feature 下，不进入 production harness。

### 16.3 clean-page cache 批处理

文件缺页的 read-ahead 上限仍为 16 页。`uncached_prefix_len()` 在一个 cache 锁
临界区检查连续未缓存前缀，`get_run()` 在一个临界区取得连续命中并更新既有精确
LRU。缓存仍只拥有 clean `FrameTracker` 引用；VMA 的 resident owner、页表 leaf
与 TLB shootdown 完全留在 MM 层。insert 时的二次查询继续解决并发填充竞态，
inode/range mutation 继续负责精确失效。

second-chance 和 BTree range invalidation 均做过独立完整 A/B，但没有优于 batch，
因此未进入最终架构。最终 public 结果为 RV 16G/8 `789.87s`、LA 36G/12
`642.70s`，两者 judge 均解析为 180/180；由于资源不等于公开规则的 8G/8，证据
等级为 `capability-pass`。性能收益未稳定越过噪声。详细证据见
`docs/evidence/buildstorm-stage2/20260816-evaluator-la-mkdir-clean-cache-validation-cn/README.md`。

### 16.4 目录 FD 与 cwd 的共享 ABI 边界

`fchdir(50)` 复用 syscall 层的目录 descriptor 解析，只接受 `MemDir`、`Ext4Dir`
与目录型 `Path`，取得 logical path 后再按进程 root 验证节点并更新共享 cwd。
`fchmodat2(452)` 复用 VFS 的 FD/path metadata 修改入口。两者不把 inode、目录项或
open-file description 所有权搬进 syscall 层，也不绕过 namespace transaction 与
cache invalidation。

该能力由 RISC-V64 与 LoongArch64 共用，不属于 LA UEFI 平台代码。评测日志中的
LA clean build 已完成，失败来自随后 coreutils 目录恢复操作缺少 syscall 50；通用
修复后的 LA public 8G/8 完整流程为 `official-pass`，而评测机独有 UEFI 后处理仍为
`unverified`。证据见
`docs/evidence/buildstorm-stage2/20260816-la-fchdir-official-public-complete/`。

## 17. MADV_DONTNEED 与匿名 resident retirement

`MapAreaBacking::AnonymousShared` 将 `MAP_SHARED|MAP_ANONYMOUS` 从 private anonymous
policy 中显式分离。VMA 只描述地址范围、权限和 backing policy；实际物理页继续由
`ResidentSet` 唯一持有，fork/clone 对 shared resident 增加 frame owner 引用，COW
只作用于 private mapping。

`MADV_DONTNEED` 的职责分布如下：syscall 层验证页对齐、长度与 advice；`MemorySet`
验证请求范围完整覆盖并只选择 private anonymous VMA；`ResidentSet` 提取重叠 VPN
的 owner；`PageTableOps` 批量清除 4 KiB leaf。`TlbProtocol` 的顺序固定为 PTE revoke、
发布 fence、本地 flush、远端 root-keyed shootdown、最后释放 frame owner。该顺序
避免任何 CPU 在 stale TLB 尚存时复用物理页。

discard 不改变 VMA 拓扑，因此下一次访问按原权限重新 fault，并从 frame allocator
获得清零页。shared anonymous、SysV shm 与 file mapping 不进入该路径。诊断计数只在
`buildstorm-diagnostics` 下编译，production 默认关闭。双架构 SMP 生命周期和完整
8G/8 BuildStorm 均已通过，证据见
`docs/evidence/buildstorm-stage2/stage17-madvise-dontneed-official-20260816/`。

## 18. LoongArch64 trap-root 与 TLB generation 边界

LoongArch64 trap 入口继续切换到 kernel root/ASID 0。非零用户 ASID 的 translation
与 kernel ASID 0 不别名，因此可跨该区间保留；返回用户态时由第 7 节的精确三元组
验证决定是否复用。用户 ASID 0 仍可能与 kernel root 别名，必须全量失效。

这一设计不把 trap-root 生命周期交给 `MemorySet` 之外的模块，也不让
`PageTableWrapper` 管理 CPU-local 状态：页表 wrapper 只拥有 generation，平台层只
拥有 CPU-local 验证与 shootdown，`MemorySet` 负责把两者按地址空间锁序组合。
Stage24 的“普通 syscall/fault 始终保留 user root”方案在 public LA 600 秒窗口只到
toolchain，已经被隔离；当前架构不取消 kernel-root 安全边界。

同源 LoongArch64 8G/8 A/B 为 555.02 秒到 542.02 秒，提升 2.34%；双架构最终
8G/8 public BuildStorm 分别为 RV 773.45 秒、LA 542.02 秒，均有精确 `ok=true`
marker 和 judge 180/180。该结果证明协议正确并消除了真实高频失效，但收益也说明
TLB 不是剩余主热点。证据与诚实边界见
`docs/evidence/buildstorm-stage2/20260817-stage23-la-tlb-generation-conclusion-cn.md`。

## 19. VFS 路径类型与隐藏 fixture 边界

VFS 必须保持 Linux 的组件类型不变量：除最终分量外，每个路径分量都必须解析为
目录；普通文件不能因为调用者随后附加子路径而变成隐式目录。`metadata`、`statx`、
`openat`、`chdir` 与 `mkdirat` 必须对同一 inode 类型给出一致结果，namespace cache
只能缓存该事实，不能改变它。

2026-08-17 评测中，LA artifact 已构建完成，随后
`/work/buildstorm.vars.fd/vars.fd` 返回 `ENOTDIR`。独立双层目录、短/长 symlink、
GNU copy 与文件到目录重建序列在当前 LA SMP8 内核通过；因此不能用 VFS 容错把
`regular/child` 接受为合法路径。hidden fixture 在可取得原始 inode/mount 身份前
保持 `unverified`。这条边界防止把评测数据布局错误伪装成内核兼容性修复。

## 20. 脚本启动的 root authority

Harness 的 `UserProgramSpec.root` 是进程文件系统命名空间的权威边界，不是根据脚本
所在目录任意选择的搜索前缀。对绝对脚本路径，root 选择遵循以下顺序：

1. 当前全局 root 提供 shebang 解释器时，绝对路径全部从 `/` 解析。
2. 当前全局 root 不提供解释器时，才允许选择包含完整解释器的独立 suite root。
3. 进程创建后，ELF、解释器、cwd、exec 和所有 syscall 路径共享同一 root，不在
   VFS 或 syscall 层再次猜测。

该边界等价于 Linux 在既定 mount namespace/chroot 中解析绝对路径。脚本附近存在
另一个同名解释器不能隐式改变 `/work`、`/tmp` 或 `/opt` 的归属。兼容回退仍用于
没有全局 userspace 的旧镜像，但由“全局解释器不可用”这一能力条件触发，不按 libc、
测试名、架构或资源路径触发。

VFS 继续只负责路径分量与 inode 类型，普通文件作为中间分量必须返回 `ENOTDIR`；
harness 不通过 VFS 容错修复命名空间错误。该职责划分由双向 SMP 回归覆盖，详见
`docs/evidence/buildstorm-stage2/20260817-stage25-script-root-namespace/README.md`。

## 21. Stage23 evaluator 回归后的 TLB 安全边界

官方 SMP12/36G LoongArch clean build 曾在 `ax-hal` 编译期间触发 glibc
`corrupted double-linked list`/`SIGABRT`。该结果与 generation-based TLB reuse
的 stale translation 风险一致，且成功 v16 树使用的是保守失效路径。因此当前架构
恢复用户根激活和 user-to-kernel root 切换的本地全量 `TLB::flush_all()`，不再以
CPU-local generation 验证替代硬件失效。该回退不改变 VmaMap、ResidentSet、PTE、
TlbProtocol 的所有权边界，也不影响已验证的 UAL、脚本 root 和目录 ABI 修复。

四种 release/cfg 编译检查已通过；新的官方 `BUILDSTORM_COMPILE ... ok=true` 尚未
取得，当前证据等级为 `unverified`。Stage23 的历史 public 8G/8 结果保留为历史
证据，不作为修复树的稳定性承诺。
