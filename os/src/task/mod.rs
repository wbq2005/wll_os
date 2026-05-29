pub mod task;
pub mod manager;
pub mod processor;
pub mod context;
pub mod pid;

use alloc::sync::Arc;
use alloc::string::{String, ToString};
use alloc::format;
use alloc::vec::Vec;
use lazy_static::lazy_static;
use spin::Mutex;
use polyhal_trap::trapframe::{TrapFrame, TrapFrameArgs};
use polyhal_trap::trap::run_user_task;

use crate::mm::memory_set::MemorySet;
use crate::task::context::TaskContext;
use crate::task::processor::Processor;
use crate::fs::fd::FileDescriptorTable;
use crate::console::putchar;

lazy_static! {
    /// 当前运行的任务
    pub static ref CURRENT_TASK: Mutex<Option<Arc<TaskControlBlock>>> = Mutex::new(None);
    
    /// CPU 处理器状态
    pub static ref PROCESSOR: Mutex<Processor> = Mutex::new(Processor::new());

    /// 孤儿进程收养者：`/init` 或预载入 harness（无 init 时），供父退出时移交子进程
    pub static ref ORPHAN_REAPER: Mutex<Option<Arc<TaskControlBlock>>> = Mutex::new(None);
}

/// `MemorySet` / `FdTable` 在 `CLONE_VM` / `CLONE_FILES` 下跨任务共享（`fork` 时各自深拷贝）。
pub type SharedMemorySet = Arc<Mutex<MemorySet>>;
pub type SharedFdTable = Arc<Mutex<FileDescriptorTable>>;

#[inline]
pub fn new_shared_memory_set(ms: MemorySet) -> SharedMemorySet {
    Arc::new(Mutex::new(ms))
}

#[inline]
pub fn new_shared_fd_table() -> SharedFdTable {
    Arc::new(Mutex::new(FileDescriptorTable::new()))
}

/// 拷贝一份 fd 表（`fork` 未带 `CLONE_FILES` 时使用）
pub fn dup_fd_table(src: &FileDescriptorTable) -> SharedFdTable {
    Arc::new(Mutex::new(src.clone()))
}

/// 在内核中为当前环境登记收养者（在创建 init 进程或内核 harness 时调用一次）
pub fn set_orphan_reaper(task: Arc<TaskControlBlock>) {
    *ORPHAN_REAPER.lock() = Some(task);
}

fn orphan_reaper() -> Option<Arc<TaskControlBlock>> {
    ORPHAN_REAPER.lock().clone()
}

/// 初始化内核页表
pub fn init_kernel_page() {
    crate::mm::page_table::init_kernel_page_table();
}

/// 添加 init 进程
pub fn add_initproc() {
    // 尝试从文件系统加载 init 程序
    log::info!("[task] Loading init process...");

    let init_candidates = ["init", "/init"];
    let elf_data = init_candidates
        .iter()
        .find_map(|path| crate::fs::read_file(path));

    if let Some(elf_data) = elf_data {
        match TaskControlBlock::new_user(&elf_data) {
            Ok(task) => {
                log::info!("[task] Init process loaded, pid={}", task.pid.0);
                set_orphan_reaper(task.clone());
                manager::add_task(task);
            }
            Err(e) => {
                log::error!("[task] Failed to load init process: {:?}", e);
                if !try_start_preloaded_test_harness() {
                    report_no_init_and_maybe_shutdown("init ELF parse/load failed");
                }
            }
        }
    } else {
        log::warn!("[task] No init program found in filesystem");
        if !try_start_preloaded_test_harness() {
            report_no_init_and_maybe_shutdown("init not found in MemFS");
        }
    }
}

fn console_write(msg: &str) {
    for b in msg.bytes() {
        putchar(b);
    }
}

fn report_no_init_and_maybe_shutdown(reason: &str) {
    console_write("\n[boot-error] No runnable init task.\n");
    console_write("[boot-error] reason: ");
    console_write(reason);
    console_write("\n");
    console_write("[boot-error] hint: ensure ext4 root on virtio-blk is available at boot (e.g. /init on the disk).\n");
    console_write("[boot-error] hint: local dev without runtime disk: place sdcard-rv.img / sdcard-la.img (or run `make unpack-sdcard`) so build.rs preloads MemFS.\n");
    console_write("[boot-error] hint: with virtio disk, kernel mounts ext4 at boot; harness discovers scripts via fs::list_files() (MemFS + ext4).\n");
    

    // 默认开发模式下直接关机，避免无任务时长时间 idle 看起来像"卡死"。
    // 如需保留 idle，可使用 cargo feature: `--features no-init-idle`.
    #[cfg(not(feature = "no-init-idle"))]
    {
        log::error!("[task] shutdown due to missing init");
        polyhal::instruction::shutdown();
    }
}

/// 合并 MemFS + ext4 上的脚本路径（运行时扫描；编译期预载仍写入 MemFS）。
fn collect_script_paths() -> Vec<String> {
    let all_files = crate::fs::list_files();
    console_write("[harness] total files found: ");
    console_write(&alloc::format!("{}", all_files.len()));
    console_write("\n");
    
    let mut scripts: Vec<String> = all_files
        .into_iter()
        .filter(|path| {
            let name = path.rsplit('/').next().unwrap_or(path);
            let matches = name.ends_with("_testcode.sh") || name == "run-all.sh";
            if matches {
                console_write("[harness]   script found: ");
                console_write(path);
                console_write("\n");
            }
            matches
        })
        .collect();
    scripts.sort();
    console_write("[harness] scripts after filter: ");
    console_write(&alloc::format!("{}", scripts.len()));
    console_write("\n");
    scripts
}

fn try_start_preloaded_test_harness() -> bool {
    let scripts = collect_script_paths();

    if scripts.is_empty() {
        return false;
    }

    console_write("[task] no /init, start preloaded test harness task.\n");
    let task = TaskControlBlock::new_kernel_task(run_preloaded_test_harness);
    set_orphan_reaper(task.clone());
    manager::add_task(task);
    true
}

fn run_preloaded_test_harness() -> ! {
    console_write("[harness] HARNESS_ENTER\n");
    let scripts = collect_script_paths();

    for script in scripts {
        console_write("[harness] SCRIPT ");
        console_write(&script);
        console_write("\n");

        let group = group_name_from_script(&script);
        console_write("#### OS COMP TEST GROUP START ");
        console_write(&group);
        console_write(" ####\n");

        if let Some(bytes) = crate::fs::read_file(&script) {
            console_write("[harness] script read ok, len=");
            console_write(&alloc::format!("{}", bytes.len()));
            console_write("\n");
            if let Ok(text) = core::str::from_utf8(&bytes) {
                let cmds = extract_exec_names(text);
                console_write("[harness] extracted ");
                console_write(&alloc::format!("{}", cmds.len()));
                console_write(" commands\n");
                for cmd in cmds {
                    console_write("[harness] RUN_CMD ");
                    console_write(&cmd);
                    console_write("\n");
                    run_one_test_binary(&cmd);
                    console_write("[harness] CMD_DONE ");
                    console_write(&cmd);
                    console_write("\n");
                }
            } else {
                console_write("[harness] skip non-utf8 script: ");
                console_write(&script);
                console_write("\n");
            }
        } else {
            console_write("[harness] script missing: ");
            console_write(&script);
            console_write("\n");
        }

        console_write("#### OS COMP TEST GROUP END ");
        console_write(&group);
        console_write(" ####\n");
    }

    console_write("[harness] all preloaded scripts done, shutdown.\n");
    polyhal::instruction::shutdown();
}

fn group_name_from_script(script: &str) -> String {
    let base = script.rsplit('/').next().unwrap_or(script);
    if let Some(name) = base.strip_suffix("_testcode.sh") {
        return name.to_string();
    }
    if base == "run-all.sh" {
        return "basic".to_string();
    }
    base.to_string()
}

fn extract_exec_names(script: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut in_tests = false;

    for line in script.lines() {
        let t = line.trim();
        if t.is_empty() || t.starts_with('#') {
            continue;
        }
        if t.starts_with("tests=\"") {
            in_tests = true;
            let rest = t.trim_start_matches("tests=\"").trim();
            if !rest.is_empty() && rest != "\"" {
                names.push(rest.to_string());
            }
            continue;
        }
        if in_tests {
            if t == "\"" {
                in_tests = false;
                continue;
            }
            names.push(t.trim_matches('"').to_string());
            continue;
        }
        if let Some(cmd) = t.split_whitespace().next() {
            if let Some(name) = cmd.strip_prefix("./") {
                if !name.is_empty() {
                    // Keep "test_" prefix so it matches runner dict key (e.g., "test_brk")
                    names.push(name.to_string());
                }
            }
        }
    }

    names
}

fn run_one_test_binary(name: &str) {
    console_write("[harness] START ");
    console_write(name);
    console_write("\n");

    // Output format required by judge_basic-*.py: "========== START <name> =========="
    // Everything between START and END markers is parsed as test output.
    console_write("========== START ");
    console_write(name);
    console_write(" ==========\n");

    // Try multiple path variants for the test binary
    let candidates: [&str; 6] = [
        name,
        name.trim_start_matches('/'),
        &format!("/{}", name),
        &format!("/test_{}", name.strip_prefix("test_").unwrap_or(name)),
        &format!("/mnt/{}", name),
        &format!("/mnt/test_{}", name.strip_prefix("test_").unwrap_or(name)),
    ];
    let elf_data = candidates
        .iter()
        .filter_map(|path| crate::fs::read_file(path))
        .next();

    let Some(elf_data) = elf_data else {
        console_write("[harness] skip missing test binary: ");
        console_write(name);
        console_write("\n");
        console_write("========== END ");
        console_write(name);
        console_write(" ==========\n");
        return;
    };

    console_write("[harness] ELF loaded, creating task...\n");
    let task = match TaskControlBlock::new_user(&elf_data) {
        Ok(task) => task,
        Err(err) => {
            console_write("[harness] skip invalid ELF: ");
            console_write(name);
            console_write(" (");
            console_write(&alloc::format!("{:?}", err));
            console_write(")\n");
            console_write("========== END ");
            console_write(name);
            console_write(" ==========\n");
            return;
        }
    };
    console_write("[harness] task created\n");

    let pid = task.pid.0;
    console_write("[harness] task created pid=");
    console_write(&alloc::format!("{}", pid));
    console_write(", adding to scheduler...\n");
    manager::add_task(task.clone());

    console_write("[harness] waiting for task pid=");
    console_write(&alloc::format!("{}", pid));
    console_write(" to complete...\n");
    loop {
        if task.status() == TaskStatus::Zombie {
            break;
        }
        suspend_current_and_run_next();
    }
    console_write("[harness] test done: ");
    console_write(name);
    console_write(" pid=");
    console_write(&alloc::format!("{}", pid));
    console_write("\n");

    // Output format required by judge_basic-*.py
    console_write("========== END ");
    console_write(name);
    console_write(" ==========\n");
}

/// 开始运行任务
///
/// 这是调度器的入口函数，从内核 main 函数调用
/// 循环从就绪队列中获取任务并执行
pub fn run_tasks() -> ! {
    log::info!("[task] Starting task scheduler...");
    run_next_task();
}

/// 挂起当前任务并运行下一个
///
/// 将当前任务放回就绪队列，然后切换到下一个任务
pub fn suspend_current_and_run_next() {
    // 获取当前任务
    let current = current_task();

    if let Some(task) = current {
        // 将当前任务状态改为 Ready
        task.set_status(TaskStatus::Ready);

        // 将任务放回就绪队列
        manager::add_task(task);

        // 清除当前任务
        *CURRENT_TASK.lock() = None;
    }

    // 运行下一个任务
    run_next_task();
}

/// 退出当前任务并运行下一个
///
/// 将当前任务标记为 Zombie，然后切换到下一个任务
/// - exit_code: 退出码
pub fn exit_current_and_run_next(exit_code: i32) {
    // 获取当前任务
    let current = current_task();

    if let Some(task) = current {
        log::info!("[task] Task {} exiting with code {}", task.pid.0, exit_code);

        // 设置退出码
        task.set_exit_code(exit_code);

        // 将任务状态改为 Zombie
        task.set_status(TaskStatus::Zombie);

        // 父进程退出前将子进程过继给收养者，避免无法再被 wait/waitpid 关联
        let orphans = {
            let mut inn = task.inner.lock();
            core::mem::take(&mut inn.children)
        };
        if !orphans.is_empty() {
            if let Some(reaper) = orphan_reaper() {
                let mut rinner = reaper.inner.lock();
                for child in orphans {
                    {
                        let mut cin = child.inner.lock();
                        cin.parent = Some(reaper.clone());
                    }
                    rinner.children.push(child);
                }
            } else {
                for child in orphans {
                    let mut cin = child.inner.lock();
                    cin.parent = None;
                }
            }
        }

        // Zombie 任务不放回就绪队列，资源会在父进程 wait 时释放

        // 清除当前任务
        *CURRENT_TASK.lock() = None;
    }

    // 运行下一个任务
    run_next_task();
}

/// 运行下一个任务
///
/// 从就绪队列中获取下一个任务并切换到它
fn run_next_task() -> ! {
    // UART marker: 'S' = scheduler entry
    #[cfg(target_arch = "riscv64")]
    unsafe { core::arch::asm!("li t0, 0x10000000; li t1, 0x53; sb t1, 0(t0)") }

    if let Some(task) = manager::fetch_task() {
        // 设置当前任务
        *CURRENT_TASK.lock() = Some(task.clone());

        // 设置任务状态为运行中
        task.set_status(TaskStatus::Running);

        log::debug!("[task] Switching to task pid={}", task.pid.0);
        // UART marker: 'T' = about to get trap_frame
        #[cfg(target_arch = "riscv64")]
        unsafe { core::arch::asm!("li t0, 0x10000000; li t1, 0x54; sb t1, 0(t0)"); }

        // 获取任务的 TrapFrame（用户态上下文）
        // 如果任务有保存的 TrapFrame，从那里恢复
        // 否则这是一个新任务，需要初始化
        // 只有用户态任务才切换其地址空间。
        // `MemorySet::new_bare()` + RISC-V `PageTable::restore()` 会清零根页表「低半」条目；
        // 本项目内核链接在 `0x80200000`，落在该低半区——若对纯内核线程切换 SATP，会在用户页表里丢失内核代码映射而卡死。
        let trap_frame = {
            let mut inn = task.inner.lock();
            let has_user_ctx = inn.trap_frame.is_some();
            if has_user_ctx {
                inn.memory_set.lock().activate();
            }
            inn.trap_frame.take()
        };

        if let Some(mut ctx) = trap_frame {
            // 恢复任务的 TrapFrame 并返回用户态
            // 使用 polyhal_trap 提供的返回机制
            log::debug!("[task] Restoring TrapFrame for task {}", task.pid.0);
            // UART marker: 'U' = about to call run_user_task
            #[cfg(target_arch = "riscv64")]
            unsafe { core::arch::asm!("li t0, 0x10000000; li t1, 0x55; sb t1, 0(t0)"); }
            // 调用 polyhal_trap::run_user_task 从 TrapFrame 返回
            // 这会恢复用户态上下文并运行，直到中断发生才返回
            let reason = unsafe { run_user_task(&mut ctx) };
            // UART marker: 'R' = returned from run_user_task
            #[cfg(target_arch = "riscv64")]
            unsafe { core::arch::asm!("li t0, 0x10000000; li t1, 0x52; sb t1, 0(t0)"); }
            log::debug!("[task] User task returned with reason: {:?}", reason);
            // Check execve before putting ctx back into task inner
            let execve_done = crate::trap::take_execve_done();
            // If execve: activate new memory set and re-run immediately
            if execve_done {
                // Activate the new address space before re-running
                task.inner.lock().memory_set.lock().activate();
                // Extract sepc before moving ctx
                let sepc = ctx[TrapFrameArgs::SEPC];
                // Put trapframe back for re-run
                task.inner.lock().trap_frame = Some(ctx);
                log::info!(
                    "[task] execve done, re-running task {} with new program at sepc={:#x}",
                    task.pid.0,
                    sepc
                );
                task.set_status(TaskStatus::Running);
                // Re-run the task: it will jump to the new program entry point
                let _reason2 = unsafe { run_user_task(&mut *task.inner.lock().trap_frame.as_mut().unwrap()) };
                // Task returned from the re-run (probably another syscall)
                ctx = task.inner.lock().trap_frame.take().unwrap();
                task.set_status(TaskStatus::Ready);
                manager::add_task(task);
                *CURRENT_TASK.lock() = None;
                run_next_task();
            }
            // Normal case: put ctx back and requeue task
            task.inner.lock().trap_frame = Some(ctx);
            // 继续调度下一个任务
            run_next_task();
        } else {
            // 新任务或内核任务，使用 task_ctx 进行上下文切换
            let task_ctx = task.task_ctx();
            let mut idle_ctx = TaskContext::zero_init();
            // 设置 idle_ctx 的返回地址为 run_next_task 的继续点
            // 这样当任务让出 CPU 时，可以回到这里继续调度
            idle_ctx.set_ra(kernel_task_return as *const () as usize);

            log::debug!("[task] Starting new task {} via context switch", task.pid.0);

            // 切换到任务的上下文
            // 注意：switch_to 不会返回，而是直接跳转到任务的入口函数
            unsafe {
                context::switch_to(
                    &mut idle_ctx as *mut TaskContext,
                    &task_ctx as *const TaskContext,
                );
            }

            // 这行代码不会执行到，因为 switch_to 直接跳转
            unreachable!();
        }
    } else {
        // 没有可运行任务，进入 idle
        log::debug!("[task] No tasks available, idling");
        idle_loop();
    }
}

/// 内核任务返回点
///
/// 当内核任务通过 suspend_current_and_run_next 让出 CPU 时，
/// 最终会回到这里，然后继续调度下一个任务
#[no_mangle]
extern "C" fn kernel_task_return() -> ! {
    log::debug!("[task] Kernel task returned, scheduling next");
    // 清除当前任务
    *CURRENT_TASK.lock() = None;
    // 继续调度下一个任务
    run_next_task();
}

/// 从 TrapFrame 返回用户态
///
/// 使用 polyhal_trap 的机制从保存的上下文返回
///
/// # Safety
///
/// 此函数不再直接使用，改用 `polyhal_trap::trap::run_user_task`
unsafe fn return_to_user(_ctx: TrapFrame) {
    // 已改用 run_user_task，此函数保留为兼容
    unimplemented!("use polyhal_trap::trap::run_user_task instead")
}

/// Idle 循环
///
/// 当没有任务时执行，等待中断
fn idle_loop() -> ! {
    loop {
        // 等待中断
        #[cfg(target_arch = "riscv64")]
        unsafe { core::arch::asm!("wfi"); }

        #[cfg(target_arch = "loongarch64")]
        unsafe { core::arch::asm!("idle 0"); }

        // 检查是否有新任务
        if manager::has_task() {
            run_next_task();
        }
    }
}

/// 获取当前任务
pub fn current_task() -> Option<Arc<TaskControlBlock>> {
    CURRENT_TASK.lock().clone()
}

/// 任务控制块
/// 
/// 每个进程/线程对应一个 TaskControlBlock
pub struct TaskControlBlock {
    pub pid: pid::Pid,
    pub inner: Mutex<TaskControlBlockInner>,
}

/// 任务控制块内部数据
///
/// 需要加锁保护的可变数据
pub struct TaskControlBlockInner {
    pub status: TaskStatus,
    pub memory_set: SharedMemorySet,
    pub task_ctx: TaskContext,
    pub trap_frame: Option<TrapFrame>, // 用户态上下文（中断时保存）
    pub exit_code: i32,
    /// `clone(2)` 传入的 clone 位（已去掉 CSIGNAL），fork 形态为 0
    pub clone_flags: usize,
    pub parent: Option<Arc<TaskControlBlock>>,
    pub children: Vec<Arc<TaskControlBlock>>,
    pub fd_table: SharedFdTable, // 文件描述符表
    pub cwd: String,
    pub program_break: usize,
    pub mapped_break: usize,
    pub next_mmap: usize,
}

/// 任务状态
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskStatus {
    /// 就绪状态 - 可以运行
    Ready,
    /// 运行状态 - 正在运行
    Running,
    /// 僵尸状态 - 已退出但资源未释放
    Zombie,
    /// 阻塞状态 - 等待某个事件
    Blocked,
}
