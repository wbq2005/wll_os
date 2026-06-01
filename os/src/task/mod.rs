pub mod context;
pub mod manager;
pub mod pid;
pub mod processor;
pub mod task;

use alloc::format;
use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicUsize, Ordering};
use lazy_static::lazy_static;
use polyhal_trap::trap::run_user_task;
use polyhal_trap::trapframe::{TrapFrame, TrapFrameArgs};
use spin::Mutex;

use crate::console::putchar;
use crate::fs::fd::FileDescriptorTable;
use crate::mm::memory_set::MemorySet;
use crate::task::context::TaskContext;
use crate::task::processor::Processor;

static mut SCHEDULER_CONTEXT: TaskContext = TaskContext {
    ra: 0,
    sp: 0,
    s: [0; 12],
};
static SCHEDULER_CONTEXT_PTR: AtomicUsize = AtomicUsize::new(0);

/// Flag set when the scheduler is context-switching FROM a user task that called exit()
/// (via ECANCELED in the trap handler). When this flag is set, kernel_task_return()
/// should NOT call run_next_task() again, because we already switched to the harness
/// and the harness's loop will handle scheduling. This prevents double-scheduling.
pub(crate) static RUNNING_FROM_ECANCELED: AtomicUsize = AtomicUsize::new(0);

lazy_static! {
    /// 当前运行的任务
    pub static ref CURRENT_TASK: Mutex<Option<Arc<TaskControlBlock>>> = Mutex::new(None);

    /// CPU 处理器状态
    pub static ref PROCESSOR: Mutex<Processor> = Mutex::new(Processor::new());

    /// 孤儿进程收养者：`/init` 或预载入 harness（无 init 时），供父退出时移交子进程
    pub static ref ORPHAN_REAPER: Mutex<Option<Arc<TaskControlBlock>>> = Mutex::new(None);
}

/// Unified specification for launching user programs with full control over
/// argv, envp, cwd, and output marker name. Used by the test harness to run
/// basic test binaries with correct paths and working directories.
pub struct UserProgramSpec {
    /// Absolute ELF path, e.g. "/glibc/basic/test_brk"
    pub path: String,
    /// Argument vector (argv[0] should be the program name or path)
    pub argv: Vec<String>,
    /// Environment vector (e.g. "PATH=/bin:/glibc", "LD_LIBRARY_PATH=/lib")
    pub envp: Vec<String>,
    /// Current working directory for the new task
    pub cwd: String,
    /// Logical root directory for this task, e.g. "/glibc" or "/musl"
    pub root: String,
    /// Output marker name for judge, e.g. "test_brk" (None = use argv[0])
    pub marker_name: Option<String>,
}

impl Default for UserProgramSpec {
    fn default() -> Self {
        Self {
            path: String::new(),
            argv: vec![String::from("/init")],
            envp: vec![
                String::from("PATH=/:/bin:/usr/bin"),
                String::from("LD_LIBRARY_PATH=/"),
            ],
            cwd: String::from("/"),
            root: String::from("/"),
            marker_name: None,
        }
    }
}

impl UserProgramSpec {
    /// Returns the marker name: explicit marker_name or argv[0]
    pub fn marker(&self) -> String {
        self.marker_name
            .clone()
            .unwrap_or_else(|| self.argv.first().cloned().unwrap_or_default())
    }
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

pub fn orphan_reaper() -> Option<Arc<TaskControlBlock>> {
    ORPHAN_REAPER.lock().clone()
}

fn is_kernel_task(task: &Arc<TaskControlBlock>) -> bool {
    task.is_kernel
}

fn task_ctx_ptr(task: &Arc<TaskControlBlock>) -> *mut TaskContext {
    task.task_ctx_ptr()
}

fn switch_kernel_task_back_to_scheduler(task: &Arc<TaskControlBlock>) {
    let scheduler_ctx_ptr = SCHEDULER_CONTEXT_PTR.load(Ordering::SeqCst);
    if scheduler_ctx_ptr == 0 {
        log::error!(
            "[task] missing scheduler context for kernel task {}",
            task.pid.0
        );
        return;
    }
    let current_ctx_ptr = task_ctx_ptr(task);
    unsafe {
        context::switch_to(current_ctx_ptr, scheduler_ctx_ptr as *const TaskContext);
    }
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
        .find_map(|path| crate::fs::read_executable_file(path));

    if let Some(elf_data) = elf_data {
        match TaskControlBlock::new_user(&elf_data) {
            Ok(task) => {
                log::info!("[task] Init process loaded, pid={}", task.pid.0);
                set_orphan_reaper(task.clone());
                manager::add_task(task);
            }
            Err(e) => {
                log::error!("[task] Failed to load init process: {:?}", e);
                if !try_start_runtime_test_harness() {
                    report_no_init_and_maybe_shutdown("init ELF parse/load failed");
                }
            }
        }
    } else {
        log::warn!("[task] No init program found in filesystem");
        if !try_start_runtime_test_harness() {
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
    console_write("[boot-error] hint: default builds do not embed sdcard tests; local fallback requires the dev-preload feature.\n");
    console_write("[boot-error] hint: with virtio disk, kernel mounts ext4 at boot and the harness discovers scripts from that disk.\n");

    // 默认开发模式下直接关机，避免无任务时长时间 idle 看起来像"卡死"。
    // 如需保留 idle，可使用 cargo feature: `--features no-init-idle`.
    #[cfg(not(feature = "no-init-idle"))]
    {
        log::error!("[task] shutdown due to missing init");
        polyhal::instruction::shutdown();
    }
}

/// Discover runtime test scripts from the mounted EXT4 root. Dev preload may
/// provide a MemFS fallback only when the explicit feature is enabled.
fn basename(path: &str) -> &str {
    path.rsplit('/')
        .find(|part| !part.is_empty())
        .unwrap_or(path)
}

fn testcode_stem(path: &str) -> Option<&str> {
    let name = basename(path);
    let stem = name.strip_suffix(".sh")?;
    stem.strip_suffix("_testcode")
}

fn is_testcode_script(path: &str) -> bool {
    testcode_stem(path).is_some()
}

fn is_default_regression_script(path: &str) -> bool {
    matches!(testcode_stem(path), Some("basic" | "busybox"))
}

fn script_rank(path: &str) -> usize {
    let libc_rank = if path.starts_with("/glibc/") {
        0usize
    } else if path.starts_with("/musl/") {
        1
    } else {
        2
    };
    let suite_rank = match testcode_stem(path) {
        Some("basic") => 0usize,
        Some("busybox") => 10,
        Some(_) => 20,
        None => 99,
    };
    suite_rank + libc_rank
}

fn collect_script_paths() -> Vec<String> {
    let mut scripts: Vec<String> = crate::fs::ext4_vol::ext4_list_all_file_paths()
        .into_iter()
        .filter(|path| is_testcode_script(path) && is_default_regression_script(path))
        .collect();

    #[cfg(feature = "dev-preload")]
    if scripts.is_empty() {
        scripts = crate::fs::list_files()
            .into_iter()
            .filter(|path| is_testcode_script(path) && is_default_regression_script(path))
            .collect();
    }

    scripts.sort_by(|a, b| script_rank(a).cmp(&script_rank(b)).then_with(|| a.cmp(b)));
    scripts.dedup();
    scripts
}

fn try_start_runtime_test_harness() -> bool {
    let scripts = collect_script_paths();

    if scripts.is_empty() {
        return false;
    }

    let task = TaskControlBlock::new_kernel_task(run_runtime_test_harness);
    set_orphan_reaper(task.clone());
    manager::add_task(task);
    true
}

fn run_runtime_test_harness() -> ! {
    let scripts = collect_script_paths();

    for script in &scripts {
        if !run_script_via_busybox(script) {
            console_write("[harness] failed to launch script: ");
            console_write(script);
            console_write("\n");
        }
    }

    *crate::trap::FOREGROUND_MODE.lock() = false;
    polyhal::instruction::shutdown();
}

fn run_user_program_spec_foreground(spec: &UserProgramSpec) -> bool {
    let task = match TaskControlBlock::new_user_with_args_env_cwd(spec) {
        Ok(task) => task,
        Err(_err) => {
            return false;
        }
    };

    // From launch until the foreground runner exits, timer interrupts must not
    // reschedule the kernel harness task away.
    let harness = current_task();
    if let Some(ref h) = harness {
        let ptr = Arc::into_raw(h.clone()) as usize;
        crate::trap::set_foreground_harness(ptr);
    }
    *crate::trap::FOREGROUND_MODE.lock() = true;

    run_user_task_foreground(task.clone());
    if let Some(ref h) = harness {
        h.set_status(TaskStatus::Running);
        *CURRENT_TASK.lock() = Some(h.clone());
    }

    let ptr = crate::trap::get_foreground_harness();
    if ptr != 0 {
        // SAFETY: ptr was created by Arc::into_raw above for this foreground run.
        let _harness = unsafe { Arc::from_raw(ptr as *const TaskControlBlock) };
    }
    crate::trap::set_foreground_harness(0);
    *crate::trap::FOREGROUND_MODE.lock() = false;

    true
}

fn abort_foreground_task_tree(root: &Arc<TaskControlBlock>) {
    fn abort_one(task: &Arc<TaskControlBlock>, aborted: &mut Vec<usize>) {
        if aborted.iter().any(|pid| *pid == task.pid.0) {
            return;
        }
        aborted.push(task.pid.0);
        task.set_exit_code(-2);
        task.set_status(TaskStatus::Zombie);
        *task.trap_frame.lock() = None;
        crate::fs::fd::flush_console_buffer_for_pid(task.pid.0);

        let children = {
            let mut inner = task.inner.lock();
            core::mem::take(&mut inner.children)
        };
        for child in children {
            abort_one(&child, aborted);
        }
    }

    let mut aborted = Vec::new();
    abort_one(root, &mut aborted);
    manager::retain_tasks(|queued| {
        if queued.is_kernel {
            true
        } else {
            abort_one(queued, &mut aborted);
            false
        }
    });

    let mut current = CURRENT_TASK.lock();
    if current
        .as_ref()
        .map(|task| aborted.iter().any(|pid| *pid == task.pid.0))
        .unwrap_or(false)
    {
        *current = None;
    }
}

pub(crate) fn run_user_task_foreground(task: Arc<TaskControlBlock>) {
    const TIMEOUT_TICKS: usize = 1_000;
    let mut waited = 0usize;

    task.set_status(TaskStatus::Ready);
    manager::add_task(task.clone());

    loop {
        if task.status() == TaskStatus::Zombie && !manager::has_task() {
            break;
        }
        if waited >= TIMEOUT_TICKS {
            console_write("[harness] TIMEOUT pid=");
            console_write(&format!("{}", task.pid.0));
            console_write("\n");
            abort_foreground_task_tree(&task);
            break;
        }

        let Some(active) = manager::fetch_task() else {
            waited += 1;
            continue;
        };
        if active.status() == TaskStatus::Zombie {
            continue;
        }

        active.set_status(TaskStatus::Running);
        *CURRENT_TASK.lock() = Some(active.clone());

        // Get trap frame
        let mut tf_guard = active.trap_frame.lock();
        let mut ctx = match tf_guard.as_ref() {
            None => {
                break;
            }
            Some(_) => tf_guard.take().unwrap(),
        };
        drop(tf_guard);

        // Activate address space and run user task
        {
            let ms = active.memory_set.lock();
            ms.activate();
        }

        let _reason = run_user_task(&mut ctx);
        crate::trap::restore_kernel_page_table();

        // execve special-case
        let execve_done = crate::trap::take_execve_done();
        if execve_done {
            *active.trap_frame.lock() = Some(ctx);
            {
                let ms = active.memory_set.lock();
                ms.activate();
            }
            let mut tf = active.trap_frame.lock().take().unwrap();
            let _ = run_user_task(&mut tf);
            crate::trap::restore_kernel_page_table();
            if active.status() != TaskStatus::Zombie {
                *active.trap_frame.lock() = Some(tf);
            }
        } else {
            // Put trap frame back for next iteration
            if active.status() != TaskStatus::Zombie {
                *active.trap_frame.lock() = Some(ctx);
            }
        }

        if active.status() != TaskStatus::Zombie {
            active.set_status(TaskStatus::Ready);
            manager::add_task(active);
        }

        waited += 1;
    }

    *CURRENT_TASK.lock() = None;
}

fn logical_path_for_script(script_path: &str) -> Option<(String, String)> {
    if let Some(rest) = script_path.strip_prefix("/glibc") {
        Some((String::from("/glibc"), crate::fs::normalize_path(rest)))
    } else if let Some(rest) = script_path.strip_prefix("/musl") {
        Some((String::from("/musl"), crate::fs::normalize_path(rest)))
    } else if script_path.starts_with('/') {
        Some((String::from("/"), crate::fs::normalize_path(script_path)))
    } else {
        None
    }
}

fn dirname(path: &str) -> String {
    let norm = crate::fs::normalize_path(path);
    match norm.rfind('/') {
        Some(0) => String::from("/"),
        Some(idx) => norm[..idx].to_string(),
        None => String::from("/"),
    }
}

fn ensure_busybox_applet_alias(root: &str, busybox_host: &str, applet: &str) -> Option<()> {
    let alias_host = crate::fs::apply_root(root, &alloc::format!("/{}", applet));
    if crate::fs::file_exists(&alias_host) {
        return Some(());
    }
    let busybox = crate::fs::read_executable_file(busybox_host)?;
    crate::fs::MEM_FS.lock().add_file(&alias_host, busybox);
    Some(())
}

fn busybox_script_spec(script_path: &str) -> Option<UserProgramSpec> {
    let (root, logical_script) = logical_path_for_script(script_path)?;
    let busybox_path = String::from("/busybox");
    let busybox_host = crate::fs::apply_root(&root, &busybox_path);
    let script_host = crate::fs::apply_root(&root, &logical_script);

    crate::fs::read_executable_file(&busybox_host)?;
    crate::fs::read_file(&script_host)?;
    ensure_busybox_applet_alias(&root, &busybox_host, "ls")?;

    Some(UserProgramSpec {
        path: busybox_path.clone(),
        argv: alloc::vec![busybox_path.clone(), String::from("sh"), logical_script.clone(),],
        envp: alloc::vec![
            String::from("PATH=.:/:/bin:/usr/bin"),
            String::from("LD_LIBRARY_PATH=/lib"),
            alloc::format!("SHELL={}", busybox_path),
        ],
        cwd: dirname(&logical_script),
        root,
        marker_name: None,
    })
}

/// Execute a script through the real BusyBox shell in the matching libc root.
/// Returns `true` if the shell task was launched, `false` if required files are
/// missing or the script is outside a known root.
pub fn run_script_via_busybox(script_path: &str) -> bool {
    let Some(spec) = busybox_script_spec(script_path) else {
        return false;
    };
    run_user_program_spec_foreground(&spec)
}

/// 开始运行任务
///
/// 这是调度器的入口函数，从内核 main 函数调用
/// 循环从就绪队列中获取任务并执行。
/// 在正常模式下永不返回（idle_loop WFI）；在 FOREGROUND_MODE 下可能返回。
pub fn run_tasks() {
    log::info!("[task] Starting task scheduler...");
    loop {
        run_next_task();
        // run_next_task should not return in normal mode. If it does, panic.
        if !*crate::trap::FOREGROUND_MODE.lock() {
            panic!("run_tasks: run_next_task returned unexpectedly in non-FOREGROUND_MODE");
        }
        // In FOREGROUND_MODE, run_next_task can return when there's no task to run.
        // This is expected; break out and let the harness continue.
        break;
    }
}

/// 挂起当前任务并运行下一个
///
/// 将当前任务放回就绪队列，然后切换到下一个任务
/// NOTE: Kernel tasks (trap_frame=None) can't be properly context-switched.
/// When a kernel task yields, we restart it from the beginning instead of resuming.
pub fn suspend_current_and_run_next() {
    if let Some(task) = current_task() {
        if is_kernel_task(&task) {
            task.set_status(TaskStatus::Ready);
            manager::add_task(task.clone());
            *CURRENT_TASK.lock() = None;
            switch_kernel_task_back_to_scheduler(&task);
            return;
        }
        if let Some(tf) = crate::trap::clone_current_trapframe() {
            *task.trap_frame.lock() = Some(tf);
        }
        task.set_status(TaskStatus::Ready);
        manager::add_task(task.clone());
        *CURRENT_TASK.lock() = None;
        run_next_task();
    }
}

/// 退出当前任务并运行下一个
///
/// 将当前任务标记为 Zombie，然后切换到下一个任务
/// - exit_code: 退出码
pub fn exit_current_and_run_next(exit_code: i32) {
    if let Some(task) = current_task() {
        if is_kernel_task(&task) {
            log::info!(
                "[task] Kernel task {} exiting with code {}",
                task.pid.0,
                exit_code
            );
            task.set_exit_code(exit_code);
            task.set_status(TaskStatus::Zombie);
            *CURRENT_TASK.lock() = None;
            switch_kernel_task_back_to_scheduler(&task);
            return;
        }
        log::info!("[task] Task {} exiting with code {}", task.pid.0, exit_code);
        let clear_child_tid = task.inner.lock().clear_child_tid;
        if clear_child_tid != 0 {
            // Linux clears this user word for set_tid_address/CLONE_CHILD_CLEARTID
            // before the parent observes task exit.  A real futex wake can be
            // added later; clearing the word already matches glibc's ABI check.
            let bytes = 0i32.to_ne_bytes();
            let memory_set = task.memory_set.lock();
            if let Err(err) =
                crate::syscall::user::copy_to_user_in_memory_set(&memory_set, clear_child_tid, &bytes)
            {
                log::debug!(
                    "[task] clear_child_tid failed pid={} addr={:#x} err={:?}",
                    task.pid.0,
                    clear_child_tid,
                    err
                );
            }
        }
        task.set_exit_code(exit_code);
        task.set_status(TaskStatus::Zombie);
        crate::fs::fd::flush_console_buffer_for_pid(task.pid.0);

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
        *CURRENT_TASK.lock() = None;
    }
    if *crate::trap::FOREGROUND_MODE.lock() {
        return;
    }
    run_next_task();
}

/// 运行下一个任务
///
/// 从就绪队列中获取下一个任务并切换到它。
/// 在 FOREGROUND_MODE 下，如果没有可运行任务则返回，由前台驱动继续执行。
pub(crate) fn run_next_task() {
    // UART marker: 'S' = scheduler entry

    if let Some(task) = manager::fetch_task() {
        // 设置当前任务
        *CURRENT_TASK.lock() = Some(task.clone());

        // 设置任务状态为运行中
        task.set_status(TaskStatus::Running);

        log::debug!("[task] Switching to task pid={}", task.pid.0);
        // UART marker: 'T' = about to get trap_frame
        // 获取任务的 TrapFrame（用户态上下文）
        // 如果任务有保存的 TrapFrame，从那里恢复
        // 否则这是一个新任务，需要初始化
        // 只有用户态任务才切换其地址空间。
        // `MemorySet::new_bare()` + RISC-V `PageTable::restore()` 会清零根页表「低半」条目；
        // 本项目内核链接在 `0x80200000`，落在该低半区——若对纯内核线程切换 SATP，
        // 会在用户页表里丢失内核代码映射而卡死。
        let (has_user_ctx, tf_opt, ms_arc) = {
            let tf = task.trap_frame.lock().take();
            let has_user = tf.is_some();
            let ms = task.memory_set.clone();
            (has_user, tf, ms)
        };
        if has_user_ctx {
            {
                let _ms_lock = ms_arc.lock();
                _ms_lock.activate();
            }
        }

        if let Some(mut ctx) = tf_opt {
            // 恢复任务的 TrapFrame 并返回用户态
            log::debug!("[task] Restoring TrapFrame for task {}", task.pid.0);
            let reason = run_user_task(&mut ctx);
            crate::trap::restore_kernel_page_table();
            log::debug!("[task] User task returned with reason: {:?}", reason);
            let execve_done = crate::trap::take_execve_done();
            if execve_done {
                task.memory_set.lock().activate();
                let sepc = ctx[TrapFrameArgs::SEPC];
                *task.trap_frame.lock() = Some(ctx);
                log::info!(
                    "[task] execve done, re-running task {} with new program at sepc={:#x}",
                    task.pid.0,
                    sepc
                );
                task.set_status(TaskStatus::Running);
                let _reason2 = run_user_task(&mut *task.trap_frame.lock().as_mut().unwrap());
                crate::trap::restore_kernel_page_table();
                ctx = task.trap_frame.lock().take().unwrap();
                task.set_status(TaskStatus::Ready);
                manager::add_task(task);
                *CURRENT_TASK.lock() = None;
                if *crate::trap::FOREGROUND_MODE.lock() {
                    return;
                }
                run_next_task();
                return;
            }
            // Normal case: put ctx back and requeue task
            *task.trap_frame.lock() = Some(ctx);
            if task.status() != TaskStatus::Zombie {
                task.set_status(TaskStatus::Ready);
                manager::add_task(task.clone());
            }
            *CURRENT_TASK.lock() = None;
            if *crate::trap::FOREGROUND_MODE.lock() {
                return;
            }
            run_next_task();
            return;
        } else {
            // 内核任务：使用 task_ctx 进行上下文切换
            let task_ctx = task_ctx_ptr(&task);
            unsafe {
                SCHEDULER_CONTEXT.ra = kernel_task_return as usize;
                SCHEDULER_CONTEXT.sp = 0;
                SCHEDULER_CONTEXT.s = [0; 12];
            }
            let idle_ctx = core::ptr::addr_of_mut!(SCHEDULER_CONTEXT);
            SCHEDULER_CONTEXT_PTR.store(idle_ctx as usize, Ordering::SeqCst);

            log::debug!(
                "[task] Starting kernel task {} via context switch",
                task.pid.0
            );

            unsafe {
                context::switch_to(idle_ctx, task_ctx as *const TaskContext);
            }

            SCHEDULER_CONTEXT_PTR.store(0, Ordering::SeqCst);
            return;
        }
    } else {
        // 没有可运行任务
        // 在 FOREGROUND_MODE 下：返回，让前台驱动继续（可能超时退出）
        // 正常模式：进入 idle 循环
        if *crate::trap::FOREGROUND_MODE.lock() {
            return;
        }
        log::debug!("[task] No tasks available, idling");
        idle_loop();
    }
}

/// 内核任务返回点
///
/// 当内核任务通过 suspend_current_and_run_next 让出 CPU 时，
/// 最终会回到这里，然后继续调度下一个任务。
#[no_mangle]
extern "C" fn kernel_task_return() {
    *CURRENT_TASK.lock() = None;
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
/// 当没有任务时执行，等待中断。
/// 在 FOREGROUND_MODE（测试 harness）下，如果没有可运行任务则直接返回，
/// 让调度器退出到前台驱动层，由驱动层继续执行下一个测试用例。
fn idle_loop() {
    if *crate::trap::FOREGROUND_MODE.lock() {
        return;
    }

    loop {
        // 等待中断
        #[cfg(target_arch = "riscv64")]
        unsafe {
            core::arch::asm!("wfi");
        }

        #[cfg(target_arch = "loongarch64")]
        unsafe {
            core::arch::asm!("idle 0");
        }

        // 检查是否有新任务
        if manager::has_task() {
            run_next_task();
        }
        // In non-FOREGROUND_MODE, this loops forever (WFI).
        // WFI永远不会返回...
    }
}

/// 获取当前任务
pub fn current_task() -> Option<Arc<TaskControlBlock>> {
    CURRENT_TASK.lock().clone()
}

/// Wrapper for kernel task context stored outside the mutex.
///
/// Kernel tasks hold `task.inner` lock indefinitely, but trap handlers also need to
/// lock `task.inner`. By keeping `TaskContext` in an `UnsafeCell` (outside the mutex)
/// and wrapping it in `KernelCtx` with explicit `Send + Sync`, we can context-switch
/// without holding the lock.
pub(crate) struct KernelCtx {
    ctx: core::cell::UnsafeCell<context::TaskContext>,
}
impl KernelCtx {
    pub(crate) fn new(ctx: context::TaskContext) -> Self {
        Self {
            ctx: core::cell::UnsafeCell::new(ctx),
        }
    }
}
unsafe impl Send for KernelCtx {}
unsafe impl Sync for KernelCtx {}

/// 任务控制块
///
/// 每个进程/线程对应一个 TaskControlBlock
pub struct TaskControlBlock {
    pub pid: pid::Pid,
    /// True for kernel-only tasks that are switched by TaskContext instead of TrapFrame.
    pub is_kernel: bool,
    /// Inner data protected by mutex (fd_table, children, cwd, etc.)
    pub inner: Mutex<TaskControlBlockInner>,
    /// Kernel task context. Outside inner to avoid deadlock.
    pub(crate) task_ctx: KernelCtx,
    /// User address space. Outside inner to avoid deadlock with activate().
    pub memory_set: SharedMemorySet,
    /// User trap frame. Outside inner for foreground driver.
    pub trap_frame: Mutex<Option<TrapFrame>>,
    /// Task status. Outside inner to avoid deadlock.
    pub status: Mutex<TaskStatus>,
}

unsafe impl Send for TaskControlBlock {}
unsafe impl Sync for TaskControlBlock {}

/// 任务控制块内部数据
pub struct TaskControlBlockInner {
    pub exit_code: i32,
    pub clone_flags: usize,
    pub parent: Option<Arc<TaskControlBlock>>,
    pub children: Vec<Arc<TaskControlBlock>>,
    pub fd_table: SharedFdTable,
    pub cwd: String,
    pub root: String,
    pub exec_path: String,
    pub program_break: usize,
    pub mapped_break: usize,
    pub next_mmap: usize,
    pub clear_child_tid: usize,
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
