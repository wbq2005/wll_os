# 系统调用

## 概述

wll_OS 实现了 POSIX 风格的系统调用接口，支持用户程序通过 `ecall` (RISC-V) 或 `syscall` (LoongArch) 指令陷入内核。

## 系统调用流程

```
用户程序
    |
    v
ecall / syscall 指令
    |
    v
CPU 陷入内核态
    |
    v
保存用户上下文到 TrapFrame
    |
    v
trap::user_interrupt()
    |
    v
handle_syscall()
    |
    v
syscall() 分发函数
    |
    v
具体系统调用处理函数
    |
    v
设置返回值到 TrapFrame
    |
    v
run_user_task() 返回用户态
```

## 系统调用号

| 调用号 | 名称 | 功能 |
|--------|------|------|
| 17 | getcwd | 获取当前工作目录 |
| 23 | dup | 复制文件描述符 |
| 24 | dup3 | 复制文件描述符到指定位置 |
| 25 | fcntl | 文件控制 |
| 29 | ioctl | IO控制 |
| 34 | mkdirat | 创建目录 |
| 35 | unlinkat | 删除文件 |
| 36 | symlinkat | 创建符号链接 |
| 37 | linkat | 创建链接 |
| 38 | renameat | 重命名路径 |
| 39 | umount2 | 卸载文件系统 |
| 40 | mount | 挂载文件系统 |
| 49 | chdir | 改变当前目录 |
| 56 | openat | 打开文件 |
| 57 | close | 关闭文件 |
| 59 | pipe2 | 创建管道 |
| 61 | getdents64 | 读取目录 |
| 62 | lseek | 设置文件偏移 |
| 63 | read | 读取数据 |
| 64 | write | 写入数据 |
| 65 | readv | 向量读取 |
| 66 | writev | 向量写入 |
| 71 | sendfile | 发送文件 |
| 72 | pselect6 | 多路复用 |
| 73 | ppoll | 多路复用 |
| 78 | readlinkat | 读取符号链接 |
| 79 | newfstatat | 获取文件状态 |
| 80 | fstat | 获取文件状态 |
| 82 | fsync | 同步文件 |
| 88 | utimensat | 设置文件时间 |
| 93 | exit | 进程退出 |
| 94 | exit_group | 退出进程组 |
| 96 | set_tid_address | 设置 TID 地址 |
| 98 | futex | 快速用户态互斥 |
| 101 | nanosleep | 纳秒级睡眠 |
| 103 | setitimer | 设置定时器 |
| 112 | clock_settime | 设置时钟 |
| 113 | clock_gettime | 获取时钟 |
| 114 | clock_getres | 获取时钟精度 |
| 115 | clock_nanosleep | 时钟睡眠 |
| 116 | syslog | 系统日志 |
| 118 | sched_setparam | 设置调度参数 |
| 119 | sched_setscheduler | 设置调度器 |
| 121 | sched_getparam | 获取调度参数 |
| 122 | sched_getscheduler | 获取调度器 |
| 124 | sched_yield | 主动让出 CPU |
| 125 | sched_get_priority_max | 获取最大优先级 |
| 126 | sched_get_priority_min | 获取最小优先级 |
| 127 | sched_rr_get_interval | 获取时间片 |
| 128 | sched_setaffinity | 设置 CPU 亲和性 |
| 129 | sched_getaffinity | 获取 CPU 亲和性 |
| 129 | kill | 发送信号 |
| 130 | tkill | 向线程发送信号 |
| 131 | tgkill | 向线程组发送信号 |
| 133 | sigsuspend | 挂起等待信号 |
| 134 | sigaction | 设置信号处理 |
| 135 | sigprocmask | 设置信号掩码 |
| 137 | sigtimedwait | 定时等待信号 |
| 139 | sigreturn | 信号返回 |
| 153 | times | 获取进程时间 |
| 154 | setpgid | 设置进程组 ID |
| 155 | getpgid | 获取进程组 ID |
| 157 | setsid | 创建会话 |
| 158 | getgroups | 获取组列表 |
| 160 | uname | 获取系统信息 |
| 163 | getrlimit | 获取资源限制 |
| 164 | setrlimit | 设置资源限制 |
| 165 | getrusage | 获取资源使用 |
| 166 | umask | 设置文件权限掩码 |
| 169 | gettimeofday | 获取时间 |
| 172 | getpid | 获取进程 ID |
| 173 | getppid | 获取父进程 ID |
| 174 | getuid | 获取用户 ID |
| 175 | geteuid | 获取有效用户 ID |
| 176 | getgid | 获取组 ID |
| 177 | getegid | 获取有效组 ID |
| 178 | gettid | 获取线程 ID |
| 179 | sysinfo | 获取系统信息 |
| 198 | socket | 创建套接字 |
| 199 | socketpair | 创建套接字对 |
| 200 | bind | 绑定地址 |
| 201 | listen | 监听连接 |
| 202 | accept | 接受连接 |
| 203 | connect | 连接 |
| 204 | getsockname | 获取套接字名称 |
| 205 | getpeername | 获取对端名称 |
| 206 | sendto | 发送数据 |
| 207 | recvfrom | 接收数据 |
| 208 | setsockopt | 设置套接字选项 |
| 209 | getsockopt | 获取套接字选项 |
| 210 | shutdown | 关闭套接字 |
| 214 | brk | 设置堆结束 |
| 215 | munmap | 解除内存映射 |
| 220 | clone | 创建进程/线程 |
| 221 | execve | 执行程序 |
| 222 | mmap | 内存映射 |
| 226 | mprotect | 设置内存保护 |
| 233 | madvise | 内存建议 |
| 242 | accept4 | 接受连接 |
| 260 | wait4 | 等待进程 |
| 261 | prlimit64 | 获取/设置资源限制 |
| 276 | renameat2 | 重命名文件 |
| 283 | membarrier | 内存屏障 |
| 326 | copy_file_range | 复制文件范围 |
| 318 | getrandom | 获取随机数 |

## 已实现系统调用

### 文件操作

| 系统调用 | 状态 | 说明 |
|----------|------|------|
| sys_openat | 已实现 | 从内存文件系统打开文件 |
| sys_close | 已实现 | 关闭文件描述符 |
| sys_read | 已实现 | 支持 stdin 和内存文件 |
| sys_write | 已实现 | 支持 stdout/stderr |
| sys_lseek | 已实现 | 支持内存文件定位 |
| sys_dup | 已实现 | 复制文件描述符 |
| sys_dup3 | 已实现 | 复制到指定位置 |
| sys_fstat | 已实现 | 基础实现 |

### 进程管理

| 系统调用 | 状态 | 说明 |
|----------|------|------|
| sys_exit | 已实现 | 进程退出 |
| sys_exit_group | 已实现 | 进程组退出 |
| sys_getpid | 已实现 | 获取进程 ID |
| sys_getppid | 已实现 | 获取父进程 ID |
| sys_sched_yield | 已实现 | 主动让出 CPU |
| sys_execve | 已实现 | 执行新程序 |

### 待实现系统调用

- sys_clone: 进程/线程创建
- sys_wait4: 等待子进程
- sys_brk: 堆管理
- sys_mmap/sys_munmap: 内存映射
- sys_nanosleep: 睡眠
- sys_gettimeofday: 获取时间
- sys_uname: 系统信息
- 信号相关: sys_sigaction, sys_kill 等
- 网络相关: sys_socket, sys_connect 等

## 系统调用返回值

```rust
pub type SyscallRet = Result<usize, SysErrNo>;
```

- 成功: `Ok(result)` - 返回值写入 a0/x[10]
- 失败: `Err(errno)` - 负的错误码写入 a0/x[10]

## 错误码

| 错误码 | 值 | 说明 |
|--------|-----|------|
| EPERM | 1 | 操作不允许 |
| ENOENT | 2 | 文件不存在 |
| ESRCH | 3 | 进程不存在 |
| EBADF | 9 | 文件描述符无效 |
| ENOMEM | 12 | 内存不足 |
| EACCES | 13 | 权限不足 |
| EFAULT | 14 | 地址错误 |
| EINVAL | 22 | 参数无效 |
| ENOSYS | 38 | 系统调用未实现 |
| EAGAIN | 11 | 资源暂时不可用 |
| EMFILE | 24 | 文件描述符过多 |
| ESPIPE | 29 | 不可寻址 |
