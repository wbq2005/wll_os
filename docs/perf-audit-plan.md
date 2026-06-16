# libcbench / lmbench / iozone 架构审查与优化计划

## 目标

围绕 `libcbench`、`lmbench`、`iozone` 做证据驱动的性能审查，先建立稳定基线，再按风险分阶段优化。

## 原则

- 不做 benchmark 特判。
- 不删必要正确性检查。
- 不牺牲文件系统一致性换分。
- 不在没有前后对比数据时声称提升。
- 每项优化都要能回归到具体子项。

## 当前判断

现有源码和历史日志指向这些热点：

- `sys_read` / `sys_write` 的整块临时缓冲和双拷贝。
- pipe 使用 `VecDeque<u8>` 的逐字节读写。
- ext4 regular file 的整文件缓存和整文件 flush。
- `readv/writev` 的逐 iovec 递归调用。
- fork/clone 没有 COW，`MemorySet::clone` 会复制整个地址空间。
- `mmap/brk` 与 frame allocator 的 eager 分配和清零。
- fd table、task registry、path lookup 的线性扫描与全局锁。

## benchmark 对应路径

| benchmark | 主要压测路径 |
|---|---|
| libcbench | libc malloc/free、memcpy/memset、stdio、string、pthread；同时会压到 `brk/mmap/clone/futex/write` |
| lmbench | syscall/trap、open/close/stat/read/write、pipe、fork/exec/wait、mmap/pagefault、scheduler/context switch |
| iozone | `read/write/pread/pwrite/readv/writev/preadv/pwritev`、ext4 cache、block I/O、fd/table、同步写回 |

## 阶段 0：测量与基线建设

### 目标

建立可重复、可比较、可回滚的 benchmark 流程，不改核心逻辑。

### 产出

- 统一结果目录：`results/YYYYMMDD-HHMM-{arch}-{suite}-summary/` 和 `results/YYYYMMDD-HHMM-{arch}-{suite}-NN/`
- 每轮保存：
  - `serial.log`
  - `judge-summary.json`
  - `env.json`
  - `git-rev.txt`
  - `command.txt`
  - `docker-command.txt`
  - `exit-code.txt`
  - `timing.json`
  - `sdcard.sha256`
- 汇总保存：
  - `judge-summary.json`
  - `env.json`
  - `git-rev.txt`
  - `runs.json`

### 运行规则

- 每个 arch/libc/suite 至少 5 轮。
- 噪声大的 lmbench 子项至少 7 轮。
- 记录 mean / median / stddev / CV / min / max。
- 用 MAD 或 2-sigma 标记异常值。
- 冷缓存：每轮使用新复制的 sdcard 镜像。
- 热缓存：同一 boot 内连续跑第二轮，第一轮只做 warmup。

### 基线命令

```powershell
python scripts/perf_baseline_runner.py --arch riscv64 --suite all
python scripts/perf_baseline_runner.py --arch loongarch64 --suite all
python scripts/perf_baseline_runner.py --arch riscv64 --suite lmbench
python scripts/perf_baseline_runner.py --arch loongarch64 --suite lmbench
```

### 回滚

- 删除新增的结果目录、`scripts/perf_baseline_runner.py` 的阶段 0 扩展即可。

## 阶段 1：低风险局部优化

### 目标

优先消除明显的局部热路径开销。

### 候选修改

- `os/src/syscall/fs.rs`
  - 减少 `sys_read/sys_write` 的每次整块分配。
  - 让常见小 I/O 走更短的缓冲路径。
- `os/src/fs/fd.rs`
  - pipe 的读写改为批量拷贝，而不是逐字节 `VecDeque<u8>` 操作。
  - 优化 fd 分配和 close-on-exec 扫描。
- `os/src/syscall/user.rs`
  - `read_cstr` 按页或按块扫描，减少逐字节翻译。

### 影响 benchmark

- lmbench：Simple read/write/fstat、Pipe bandwidth、open/close。
- iozone：小 record 顺序/随机 I/O。
- libcbench：stdio、部分 malloc/stdio 相关子项。

### 验证

- 前后同一镜像、同一命令、同一轮数对比。
- 必须保留 basic / busybox / libc-test / lmbench / iozone 全回归。

### 风险

- partial read/write 语义错误。
- pipe 的 nonblock / EOF / EAGAIN 行为变化。

## 阶段 2：中等风险结构优化

### 目标

在不破坏语义的前提下，改进数据结构和缓存结构。

### 候选修改

- `os/src/fs/ext4_vol.rs`
  - 从整文件缓存转向按脏区跟踪的 regular file cache。
  - 避免 flush 时 clone 全文件再从 0 写回。
- `os/src/mm/memory_set.rs`
  - 把 VMA 查找从线性结构逐步改进为更适合 page fault/mmap 的结构。
  - 减少 `range_covered/find_free_area/handle_page_fault` 的扫描成本。
- `os/src/syscall/fs.rs`
  - 让 `readv/writev` 走真正的聚合路径，而不是递归调用普通 read/write。
- `os/src/fs/vfs.rs` / `os/src/fs/mod.rs`
  - 改进 path lookup / directory lookup 的缓存与索引。

### 影响 benchmark

- iozone：random-read、stride-read、read-backwards、pwritev/preadv。
- lmbench：bw_file_rd、lat_mmap、pagefault、open/stat。
- libcbench：malloc、pthread、stdio 的间接受益。

### 验证

- 增加 fsync / sync / truncate / unlink / rename / mmap 回归。
- 比较冷缓存与热缓存差值，不只看总分。

### 风险

- 文件内容一致性、mtime/ctime、写回时序。
- mmap 与 read/write 语义不一致。

## 阶段 3：高风险架构优化

### 目标

处理真正的大瓶颈，但要承担更高实现复杂度。

### 候选修改

- 引入 page cache，并与 file-backed mmap 统一。
- fork/clone 引入 COW。
- 异步写回和更清晰的 fsync 语义。
- per-CPU / per-thread allocator 或更细粒度锁。
- scheduler / wait queue / fd / FS 的锁粒度重构。

### 影响 benchmark

- lmbench：fork/exec/wait/context switch/pagefault。
- iozone：大吞吐、随机写、同步写回。
- libcbench：pthread、malloc、stdio 的整体表现。

### 风险

- 脏页、写回、崩溃一致性。
- COW 和共享映射边界。
- 多锁并发下的竞态。

## 优先级

1. 阶段 0 基线
2. `sys_read/sys_write` 与 pipe 的低风险热路径
3. ext4 regular cache 的脏区化 / 分块化评审
4. fd / path / VMA 的结构优化
5. COW、page cache、异步写回

## 建议优先做的 3 件事

1. 先做可重复 baseline runner。
2. 再做 `sys_read/sys_write` 和 pipe 的局部优化。
3. 然后评审 ext4 regular cache 的脏区化设计。

## 下一步数据收集

- 先固定一个 `riscv64` 和一个 `loongarch64` 的干净镜像。
- 每个 suite 连续跑 5 轮。
- 记录原始 serial log 和 judge-summary JSON。
- 单独保留冷缓存与热缓存结果。
- 只在同一套数据上比较优化前后差异。
