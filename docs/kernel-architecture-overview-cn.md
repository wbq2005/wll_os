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

每个受管 PPN 有一个 `AtomicUsize` 引用计数。clone 增加引用，drop 的最后一个 owner 释放 frame。共享 frame 禁止转交 raw ownership，从类型边界避免 COW/shared 页被页表析构误释放。

diagnostics feature 额外维护 free/tracked/page-table/contiguous 原子状态机；production 只保留实际引用计数。

### 6.6 Demand fault 与预取

匿名和文件 lazy area 在首次访问时分配/读取页。匿名 fault 可按小窗口批量建立连续 resident，clean file fault 使用受限 read-ahead；窗口始终截断到 VMA 边界，失败时保持所有权可回收。

### 6.7 fork/COW/shared/file

- private writable mapping：parent/child 共享 `FrameTracker`，PTE 清 W，page state 设 Cow；写 fault 分配并复制私有页。
- readonly mapping：共享 frame，无需写时复制。
- shared memory 与 shared file：保持 shared state，写入对共享 owner 可见。
- file dirty page 在 msync/unmap/exit 的 writeback 责任路径处理；clean page 可直接丢弃并在下次 fault 重读。

### 6.8 exec 与析构顺序

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

当前实现仍以逐页操作为主。历史批处理候选没有通过 300 秒性能门槛，因此没有引入未证实的复杂事务 API。

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

这些规则是 correctness 边界，不因 production/diagnostics cfg 改变。

## 11. Diagnostics 与回归架构

### 11.1 cfg 隔离

`buildstorm-diagnostics` 提供：

- lock wait/hold 聚合；
- MM、exec、fault、mmap 和 scheduler 阶段计数；
- namespace generation crossing；
- frame owner shadow；
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

## 12. BuildStorm 完成状态

RISC-V64：

```text
BUILDSTORM_COMPILE mode=multi ok=true elapsed_s=1322.99 cores=8 bytes=1683456 arch=riscv64
```

LoongArch64：

```text
BUILDSTORM_COMPILE mode=multi ok=true elapsed_s=1103.14 cores=8 bytes=1716224 arch=loongarch64
```

两架构官方 judge 自动项均为 180/180。完整实验、AI 披露和复现步骤见 `docs/buildstorm-2.3-design-optimization-cn.md`。

## 13. 当前限制与演进方向

- VMA metadata 当前是 Vec，超大集合可演进为带 gap augmentation 的有序索引，但必须保持 ResidentSet 所有权边界。
- 页表缺少已被实测证明有收益的通用 edit transaction；在没有新证据前不增加复杂度。
- kernel stack 的完整显式 owner/回收审计仍值得继续。
- 实板支持必须分别验证 boot、interrupt、timer、SMP、DMA、storage、cache maintenance；QEMU 通过不能代替 VisionFive 2 或 Loongson 2K1000LA gate。
- 初赛回归仍是提交要求，BuildStorm 完成不替代其他测例。
