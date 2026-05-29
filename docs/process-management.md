# 进程管理

## 概述

wll_OS 的进程管理模块负责任务的创建、调度、切换和销毁。采用简单的轮转调度算法，支持用户态和内核态任务。

## 核心数据结构

### 任务控制块 (TaskControlBlock)

```rust
pub struct TaskControlBlock {
    pub pid: Pid,                          // 进程ID
    pub inner: Mutex<TaskControlBlockInner>, // 内部数据（需加锁）
}

pub struct TaskControlBlockInner {
    pub status: TaskStatus,                // 任务状态
    pub memory_set: MemorySet,             // 地址空间
    pub task_ctx: TaskContext,             // 任务上下文（内核态）
    pub trap_frame: Option<TrapFrame>,     // 陷阱帧（用户态）
    pub exit_code: i32,                    // 退出码
    pub parent: Option<Arc<TaskControlBlock>>, // 父进程
    pub children: Vec<Arc<TaskControlBlock>>,  // 子进程列表
    pub fd_table: FileDescriptorTable,     // 文件描述符表
}
```

### 任务状态

```rust
pub enum TaskStatus {
    Ready,      // 就绪状态
    Running,    // 运行状态
    Zombie,     // 僵尸状态
    Blocked,    // 阻塞状态
}
```

## 任务创建

### 1. 内核任务

```rust
pub fn new_kernel_task(entry: fn() -> !) -> Arc<TaskControlBlock>
```

用于创建内核线程，不拥有用户地址空间。

### 2. 用户任务

```rust
pub fn new_user(elf_data: &[u8]) -> Result<Arc<TaskControlBlock>, SysErrNo>
```

从 ELF 文件创建用户进程：

1. 解析 ELF 文件
2. 加载程序段到地址空间
3. 分配用户栈
4. 初始化 TrapFrame
5. 创建文件描述符表

### 3. Init 进程

```rust
pub fn add_initproc() {
    if let Some(elf_data) = crate::fs::read_file("init") {
        match TaskControlBlock::new_user(&elf_data) {
            Ok(task) => manager::add_task(task),
            Err(e) => log::error!("Failed to load init: {:?}", e),
        }
    }
}
```

## 调度器

### 调度流程

```
run_tasks() -> run_next_task()
    |
    v
fetch_task() <- 就绪队列
    |
    v
设置当前任务
    |
    v
恢复 TrapFrame（用户任务）或 上下文切换（内核任务）
    |
    v
运行直到中断/系统调用
    |
    v
保存状态 -> 放回就绪队列 或 标记为 Zombie
    |
    v
run_next_task() (循环)
```

### 调度策略

- **算法**: 简单轮转 (Round-Robin)
- **时间片**: 由定时器中断触发调度
- **触发条件**:
  - 定时器中断（时间片用完）
  - 进程主动 yield
  - 进程退出

## 上下文切换

### 用户态 ↔ 内核态

**用户态陷入内核**:
1. 用户程序执行 `ecall` (RISC-V) 或 `syscall` (LoongArch)
2. CPU 切换到内核态，保存用户上下文到 TrapFrame
3. 调用 `user_interrupt()` 处理
4. 如果是系统调用，调用 `handle_syscall()`
5. 处理完成后，通过 `run_user_task()` 返回用户态

**内核态返回用户态**:
```rust
let reason = unsafe { run_user_task(&mut ctx) };
match reason {
    EscapeReason::SysCall => { /* 系统调用已处理 */ }
    EscapeReason::Timer => { /* 重新调度 */ }
    _ => { /* 其他原因 */ }
}
```

### 内核态任务切换

```rust
unsafe {
    context::switch_to(
        &mut idle_ctx as *mut TaskContext,
        &task_ctx as *const TaskContext,
    );
}
```

使用汇编实现的 `switch_to` 函数切换内核栈和寄存器。

## 进程退出

```rust
pub fn exit_current_and_run_next(exit_code: i32) {
    if let Some(task) = current_task() {
        task.set_exit_code(exit_code);
        task.set_status(TaskStatus::Zombie);
        // 不放入就绪队列，等待父进程回收
    }
    run_next_task();
}
```

## 进程间关系

- **父子关系**: 通过 `parent` 和 `children` 字段维护
- **资源回收**: Zombie 进程的资源由父进程 `wait` 时释放（待实现）

## 关键文件

| 文件 | 功能 |
|------|------|
| task/task.rs | TCB 实现 |
| task/manager.rs | 就绪队列 |
| task/processor.rs | CPU 状态 |
| task/context.rs | 上下文切换（汇编） |
| task/pid.rs | PID 分配 |
| task/mod.rs | 调度器入口 |
