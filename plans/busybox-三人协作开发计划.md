# BusyBox 与后续测试组三人协作开发计划

> 目标：在 basic 已全部通过的基础上，优先吃下 busybox 稳定分，同时提前铺路 lua、libc-test、iozone、UnixBench、iperf、libc-bench、lmbench、netperf、rt-tests、LTP 等后续测试组，避免三个人长期挤在同一个问题上。

参考测试仓库：

- 官方测试套件：[testsuits-for-oskernel pre-2025](https://github.com/oscomp/testsuits-for-oskernel/tree/pre-2025)
- 本仓库 busybox 评分脚本：`testdata/judge_busybox-musl.py`、`testdata/judge_busybox-glibc.py`

---

## 1. 核心结论

三个人不要长期一起开发 busybox。

推荐策略：

1. 第 1 天三个人一起复现 busybox，建立失败表和日志归档。
2. 第 2-5 天两个人主攻 busybox，一个人提前铺后续测试。
3. busybox 通过率达到约 70% 后，改成一个人收尾 busybox，一个人推进 lua/libc/iozone，一个人推进 network/perf/LTP。

原因：

- busybox 是后续测试的基础设施关卡，尤其依赖 `execve`、`clone/fork`、`wait4`、`pipe2`、`openat`、`read/write`、`stat`、`getdents64`、`/proc`、`/dev` 等。
- 但 busybox 不是终点，后面还有大量独立分数点。
- 三个人都改进程/文件系统核心代码容易互相冲突，GitLab MR 也会难 review。
- 后续测试可以提前定位 syscall 缺口，等 busybox 稳定后能快速接力。

---

## 2. 总体分工

| 成员 | 主责方向 | busybox 阶段任务 | 后续阶段任务 |
|---|---|---|---|
| A | 进程、执行、shell | `execve`、`clone/fork`、`wait4`、`exit_group`、`pipe2`、重定向、`ash/sh -c` | LTP 进程类、信号、线程、调度 |
| B | 文件系统、路径、伪文件系统 | `openat/read/write/lseek/stat/getdents/mkdir/unlink/rename`，`/proc`、`/dev`、`/tmp` | iozone、UnixBench、libc-bench 文件和性能问题 |
| C | 后续测试、CI、日志、内存网络预研 | 建 busybox 计分表、自动跑分、失败日志归类；辅助复现关键失败 | lua、libc-test、`mmap/brk/getrandom`、iperf/netperf 预研 |

每个人都可以修 busybox，但必须按模块边界拆 MR：

- A 不随意改文件系统内部实现。
- B 不随意改 trap/process 调度核心。
- C 不在 busybox 未稳定前大改网络或调度。

---

## 3. BusyBox 分数优先级

本地评分脚本中 busybox 主要命令如下：

```text
ash -c exit
sh -c exit
basename /aaa/bbb
cal
clear
date
df
dirname /aaa/bbb
dmesg
du
expr 1 + 1
false
true
which ls
uname
uptime
ps
pwd
free
hwclock
kill 10
ls
sleep 1
touch test.txt
echo "hello world" > test.txt
cat test.txt
cut -c 3 test.txt
od test.txt
head test.txt
tail test.txt
hexdump -C test.txt
md5sum test.txt
echo "..." >> test.txt
sort test.txt | ./busybox uniq
stat test.txt
strings test.txt
wc test.txt
[ -f test.txt ]
more test.txt
rm test.txt
mkdir test_dir
mv test_dir test
rmdir test
grep hello busybox_cmd.txt
cp busybox_cmd.txt busybox_cmd.bak
rm busybox_cmd.bak
find -name "busybox_cmd.txt"
```

建议按以下顺序吃分。

### P0：先让 shell 和执行链路稳定

目标命令：

- `ash -c exit`
- `sh -c exit`
- `echo "hello world" > test.txt`
- `echo "..." >> test.txt`
- `sort test.txt | ./busybox uniq`

重点能力：

- `execve` 的 `argv/envp/auxv` 栈布局正确。
- 动态链接程序可以正确加载。
- `clone/fork` 子进程返回值正确。
- `wait4` 能回收子进程。
- `pipe2` 能让父子进程或两个子进程通信。
- `dup/dup3/fcntl` 支持 shell 重定向。
- `exit/exit_group` 不污染父进程状态。

负责人：A 主责，B 辅助文件描述符，C 负责日志归档。

### P0：文件读写和 FD 表

目标命令：

- `touch`
- `cat`
- `cp`
- `rm`
- `mv`
- `grep`
- `head`
- `tail`
- `cut`
- `od`
- `hexdump`
- `md5sum`
- `strings`
- `wc`
- `more`

重点能力：

- `openat` 支持相对路径、绝对路径、`AT_FDCWD`。
- `O_CREAT`、`O_TRUNC`、`O_APPEND`、读写权限处理正确。
- `read/write/lseek/close` 行为接近 Linux。
- FD 表在 `fork/exec/dup` 后语义正确。
- 文件内容写回 ext4 或内存文件系统后能被后续命令读到。

负责人：B 主责，A 负责 FD 继承，C 负责归档通过数变化。

### P1：目录、stat、路径和元数据

目标命令：

- `ls`
- `pwd`
- `mkdir`
- `rmdir`
- `find`
- `stat`
- `[ -f test.txt ]`
- `dirname`
- `basename`

重点能力：

- `getdents64` 返回格式正确。
- `newfstatat/fstat/statx` 结构体字段大小和对齐正确。
- `mkdirat/unlinkat/renameat2` 基本语义正确。
- 当前工作目录 `cwd` 和进程 root 路径正确维护。
- 相对路径解析、`.`、`..`、多余 `/` 都不出错。

负责人：B 主责。

### P1：伪文件系统和系统信息

目标命令：

- `ps`
- `free`
- `uptime`
- `df`
- `dmesg`
- `uname`
- `which`
- `hwclock`

重点能力：

- `/proc/meminfo`
- `/proc/uptime`
- `/proc/stat`
- `/proc/self`
- `/proc/<pid>/stat`
- `/proc/<pid>/cmdline`
- `/dev/null`
- `/dev/zero`
- `/dev/urandom`
- `uname`
- `sysinfo`
- `gettimeofday/clock_gettime`

注意：很多命令不需要完整 Linux 行为，只要输出格式足够让 busybox applet 成功即可。

负责人：B 主责，C 辅助确认后续 libc/lua 是否也依赖这些路径。

### P2：时间、信号和调度 stub

目标命令：

- `date`
- `cal`
- `sleep 1`
- `kill 10`
- `true`
- `false`
- `expr 1 + 1`

重点能力：

- `nanosleep/clock_nanosleep` 至少能正确阻塞或让出。
- `clock_gettime/gettimeofday` 返回递增时间。
- `kill` 对不存在 pid 返回合理 errno，对可忽略信号不崩溃。
- `sched_yield` 可用。

负责人：A 主责。

---

## 4. 后续测试组提前规划

### C 可以提前做什么

C 不应该等 busybox 全通过后才开始后续测试。推荐提前做：

1. 跑 lua 组，定位是否卡在 `mmap/brk/getrandom/file_io/time`。
2. 跑 libc-test 的小样本，整理 syscall 缺口表。
3. 跑 iozone 最小命令，确认文件写入、读回、并发进程是否稳定。
4. 预研 iperf/netperf 的 socket syscall，但不要过早大改网络栈。
5. 建立每日分数 dashboard。

### 后续测试优先级

| 优先级 | 测试组 | 建议原因 |
|---|---|---|
| P1 | lua | syscall 面小，常依赖文件、时间、随机数、内存映射，适合 busybox 后接上 |
| P1 | libc-test | 能暴露 ABI/syscall 细节，长期收益高 |
| P1 | iozone | 文件系统性能和正确性，和 busybox 文件能力强相关 |
| P2 | libc-bench | 依赖内存、线程、时间，适合 mmap/brk 稳定后做 |
| P2 | UnixBench/lmbench | 对进程、pipe、上下文切换、文件性能敏感 |
| P3 | iperf/netperf | socket 栈工作量大，收益高但风险高 |
| P3 | cyclictest/rt-tests | 调度和 timer 精度要求高，后期优化 |
| P3 | LTP 全量 | 范围大，适合按 syscall 选点，不建议一开始硬啃全量 |

---

## 5. GitLab 分支策略

建议固定以下分支：

```text
main
develop
feat/busybox-exec-pipe
feat/busybox-fs-stat
feat/procfs-ps-free
feat/mm-mmap-lua
feat/net-loopback
fix/basic-regression-xxx
```

规则：

1. `main` 永远保持可提交评测。
2. `develop` 是每日集成分支。
3. 每个功能从 `develop` 拉分支。
4. 一个 MR 只解决一个明确问题。
5. 涉及 `task/process/fd/mm` 的 MR 必须至少两人 review。
6. 修复 busybox 时不能破坏 basic，所有 MR 必须附 basic 回归结果。
7. 每天晚上从 `develop` 选稳定版本合入 `main`，并打 tag。

Tag 格式：

```text
score-2026xxxx-basic-full-busybox-35-lua-0
score-2026xxxx-basic-full-busybox-48-lua-6
score-2026xxxx-basic-full-busybox-full-lua-10
```

---

## 6. Issue 模板

每个失败点建一个 issue。

````md
## 失败命令

busybox sort test.txt | ./busybox uniq

## 当前现象

- 是否 panic：
- 是否卡死：
- 是否输出错误：
- 是否缺少 testcase success：

## 复现方式

```bash
# 写清楚本地运行命令或评测命令
```

## 相关日志

```text
粘贴关键 syscall trace 或 serial log
```

## 初步判断

- 可能 syscall：
- 可能模块：
- 可能负责人：

## 完成标准

- [ ] 单项命令通过
- [ ] busybox 总通过数增加
- [ ] basic 不回归
- [ ] RISC-V 通过
- [ ] LoongArch 通过
````

---

## 7. MR 模板

````md
## 修改内容

- 修复：
- 影响模块：

## 修复前

```text
失败日志
```

## 修复后

```text
成功日志
```

## 验证结果

- [ ] basic-musl
- [ ] basic-glibc
- [ ] busybox-musl
- [ ] busybox-glibc
- [ ] RISC-V
- [ ] LoongArch

## 风险

- 是否改动调度：
- 是否改动内存映射：
- 是否改动 FD 表：
- 是否可能影响后续测试：
````

---

## 8. 每日开发节奏

### 上午

1. 10 分钟同步昨天分数变化。
2. 确认当天每个人只领 1-2 个主要 issue。
3. 明确当天可合并目标。

### 下午

1. 每个人在自己分支推进。
2. 遇到卡死先补日志，不要盲改。
3. 影响公共模块时先发 draft MR，让队友看方向。

### 晚上

1. 合并当天稳定 MR 到 `develop`。
2. 跑 basic + busybox。
3. 记录通过数和失败列表。
4. 如果稳定，把 `develop` 合入 `main` 并打 tag。

---

## 9. 两周里程碑

### 第 1 天：基线和表格

目标：

- busybox 失败项全部列入 scoreboard。
- 建 GitLab issue。
- 建 CI 或半自动脚本。
- 确认 basic 不回归。

产出：

- `docs/scoreboard-busybox.md`
- 第一份 full log
- 第一组 issue

### 第 2-3 天：shell、exec、FD、pipe

目标：

- `ash -c exit`、`sh -c exit` 通过。
- 重定向 `>`、`>>` 可用。
- `sort | uniq` 至少不崩溃，最好通过。
- `cp/cat/grep/head/tail/wc` 这类基础文件命令大量通过。

负责人：

- A：exec/process/pipe
- B：FD/file read-write
- C：lua 初跑 + 日志工具

### 第 4-5 天：目录、stat、procfs

目标：

- `ls/find/stat/pwd/mkdir/rmdir/mv/rm` 通过。
- `ps/free/uptime/df/uname` 尽量通过。
- busybox 总通过数达到 70% 左右。

负责人：

- A：wait/kill/sleep/time
- B：getdents/stat/procfs
- C：lua/libc-test 缺口表

### 第 6-7 天：busybox 收尾和双架构回归

目标：

- busybox 尽量全通过。
- RISC-V 和 LoongArch 都跑通。
- 合并稳定版本到 `main`。
- 提交一次平台评测，记录真实分数。

负责人：

- A：卡死类问题
- B：文件系统边界问题
- C：后续测试接入和 dashboard

### 第 2 周：后续组扩展

目标：

- lua 优先全通过。
- libc-test 按 syscall 分类推进。
- iozone 至少跑出有效结果。
- 如果文件/进程稳定，再考虑 UnixBench/lmbench。
- 网络只在有明确 socket 最小闭环后推进。

---

## 10. 技术风险清单

### 风险 1：busybox 通过但 basic 回归

处理：

- basic 是红线。
- 每个 MR 必须跑 basic。
- 修 busybox 时避免为了某个 applet 写特殊 hack，优先修通用 syscall 语义。

### 风险 2：三个人同时改 FD 表或 task 结构

处理：

- FD 表、进程生命周期、地址空间结构属于高冲突区。
- 这类 MR 必须提前沟通设计。
- 一次只合一个核心结构变更。

### 风险 3：后续测试被长期搁置

处理：

- C 从第 1 周就开始跑 lua/libc/iozone。
- 每天同步后续测试缺口。
- busybox 到 70% 后必须分兵。

### 风险 4：网络栈投入过早

处理：

- iperf/netperf 分数诱人，但工作量大。
- 没有稳定 `fork/pipe/fd/select/poll/time` 前，不要重投入网络。
- 先实现 loopback 最小闭环，再推进 TCP/UDP 完整语义。

### 风险 5：LTP 范围过大

处理：

- 不要一开始全量 LTP。
- 按 syscall 分类选容易拿分的 case。
- 优先 `read/write/open/stat/wait/clone/mmap/time` 类。

---

## 11. 建议的 scoreboard 格式

```md
# BusyBox Scoreboard

| 命令 | 状态 | 负责人 | 疑似模块 | Issue | 最后日志 | 备注 |
|---|---|---|---|---|---|---|
| ash -c exit | fail | A | exec/wait | #1 | logs/xxx.log | 子进程未退出 |
| cat test.txt | pass | B | fs/read | #2 | logs/xxx.log | 通过 |
| sort test.txt \| ./busybox uniq | fail | A/B | pipe/dup | #3 | logs/xxx.log | 管道无数据 |
```

状态建议：

- `todo`
- `reproduced`
- `in-progress`
- `fixed`
- `regressed`
- `blocked`

---

## 12. 最终建议

这阶段最重要的是节奏：

- busybox 必须打，但不要三个人全堵在 busybox。
- 先把 shell、exec、pipe、FD、FS 打稳。
- 一个人从第一周就负责后续测试和自动化。
- 每天只合稳定 MR，`main` 永远可评测。
- 分数推进以 scoreboard 为准，不靠感觉。

推荐当前人员配置：

```text
第 1 天：A+B+C 一起复现 busybox，建表。
第 2-5 天：A+B 主攻 busybox，C 跑 lua/libc/iozone。
第 6-7 天：A 或 B 收尾 busybox，另两人开始后续组。
第 2 周：1 人维护 busybox 回归，2 人拿后续分。
```
