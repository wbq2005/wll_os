pub mod context;
pub mod harness;
pub mod manager;
pub mod pid;
pub mod processor;
pub mod task;
pub mod wait_queue;

use alloc::string::String;
use alloc::sync::{Arc, Weak};
use alloc::vec;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicUsize, Ordering};
use lazy_static::lazy_static;
use polyhal_trap::trap::run_user_task;
use polyhal_trap::trapframe::TrapFrame;
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
const SIGNAL_EXIT_CODE_BASE: i32 = -0x1000;

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
    static ref FOREGROUND_ACTIVE_TASKS: Mutex<Vec<usize>> = Mutex::new(Vec::new());
    static ref FOREGROUND_REQUEUE_FRONT: Mutex<Vec<usize>> = Mutex::new(Vec::new());
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
pub type SharedFsContext = Arc<Mutex<FsContext>>;
pub type SharedMmContext = Arc<Mutex<MmContext>>;

#[derive(Clone)]
pub struct FsContext {
    pub cwd: String,
    pub root: String,
}

#[derive(Clone, Copy)]
pub struct MmContext {
    pub program_break: usize,
    pub mapped_break: usize,
    pub next_mmap: usize,
}

pub struct ThreadGroup {
    tgid: usize,
    members: Mutex<Vec<Weak<TaskControlBlock>>>,
    exit_code: Mutex<i32>,
    process_zombie: AtomicUsize,
}

impl ThreadGroup {
    pub fn new(tgid: usize) -> Arc<Self> {
        Arc::new(Self {
            tgid,
            members: Mutex::new(Vec::new()),
            exit_code: Mutex::new(0),
            process_zombie: AtomicUsize::new(0),
        })
    }

    pub fn tgid(&self) -> usize {
        self.tgid
    }

    pub fn add_member(&self, task: &Arc<TaskControlBlock>) {
        self.members.lock().push(Arc::downgrade(task));
    }

    pub fn user_members(&self) -> Vec<Arc<TaskControlBlock>> {
        let mut out = Vec::new();
        let mut members = self.members.lock();
        let mut index = 0usize;
        while index < members.len() {
            if let Some(task) = members[index].upgrade() {
                if !task.is_kernel {
                    out.push(task);
                }
                index += 1;
            } else {
                members.remove(index);
            }
        }
        out
    }

    pub fn all_user_members_zombie(&self) -> bool {
        let members = self.user_members();
        !members.is_empty()
            && members
                .iter()
                .all(|task| task.status() == TaskStatus::Zombie)
    }

    pub fn mark_process_zombie(&self, exit_code: i32) -> bool {
        let first = self.process_zombie.swap(1, Ordering::SeqCst) == 0;
        if first {
            *self.exit_code.lock() = exit_code;
        }
        first
    }

    pub fn is_process_zombie(&self) -> bool {
        self.process_zombie.load(Ordering::SeqCst) != 0
    }

    pub fn exit_code(&self) -> i32 {
        *self.exit_code.lock()
    }
}

#[inline]
pub fn new_shared_memory_set(ms: MemorySet) -> SharedMemorySet {
    Arc::new(Mutex::new(ms))
}

#[inline]
pub fn new_shared_fd_table() -> SharedFdTable {
    Arc::new(Mutex::new(FileDescriptorTable::new()))
}

#[inline]
pub fn new_shared_fs_context(cwd: String, root: String) -> SharedFsContext {
    Arc::new(Mutex::new(FsContext { cwd, root }))
}

#[inline]
pub fn dup_fs_context(src: &SharedFsContext) -> SharedFsContext {
    Arc::new(Mutex::new(src.lock().clone()))
}

#[inline]
pub fn new_shared_mm_context(
    program_break: usize,
    mapped_break: usize,
    next_mmap: usize,
) -> SharedMmContext {
    Arc::new(Mutex::new(MmContext {
        program_break,
        mapped_break,
        next_mmap,
    }))
}

#[inline]
pub fn dup_mm_context(src: &SharedMmContext) -> SharedMmContext {
    let ctx = *src.lock();
    new_shared_mm_context(ctx.program_break, ctx.mapped_break, ctx.next_mmap)
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

pub(crate) fn enter_foreground_user_task(pid: usize) {
    if crate::trap::foreground_driver_active() {
        FOREGROUND_ACTIVE_TASKS.lock().push(pid);
    }
}

pub(crate) fn leave_foreground_user_task(pid: usize) {
    if crate::trap::foreground_driver_active() {
        let mut active = FOREGROUND_ACTIVE_TASKS.lock();
        if let Some(index) = active.iter().rposition(|active_pid| *active_pid == pid) {
            active.remove(index);
        }
    }
}

fn is_foreground_user_task_active(pid: usize) -> bool {
    crate::trap::foreground_driver_active() && FOREGROUND_ACTIVE_TASKS.lock().contains(&pid)
}

pub(crate) fn request_foreground_requeue_front(pid: usize) {
    if !crate::trap::foreground_driver_active() {
        return;
    }
    let mut pids = FOREGROUND_REQUEUE_FRONT.lock();
    if !pids.contains(&pid) {
        pids.push(pid);
    }
}

fn take_foreground_requeue_front(pid: usize) -> bool {
    if !crate::trap::foreground_driver_active() {
        return false;
    }
    let mut pids = FOREGROUND_REQUEUE_FRONT.lock();
    if let Some(index) = pids.iter().position(|queued_pid| *queued_pid == pid) {
        pids.remove(index);
        true
    } else {
        false
    }
}

fn fetch_dispatchable_task() -> Option<Arc<TaskControlBlock>> {
    let mut skipped_active = Vec::new();
    loop {
        let Some(task) = manager::fetch_task() else {
            for task in skipped_active {
                manager::add_task(task);
            }
            return None;
        };
        if is_foreground_user_task_active(task.pid.0) {
            skipped_active.push(task);
            continue;
        }
        for task in skipped_active {
            manager::add_task(task);
        }
        return Some(task);
    }
}

pub(crate) fn signal_exit_code(signum: i32) -> i32 {
    SIGNAL_EXIT_CODE_BASE - signum
}

pub(crate) fn wait_status_from_exit_code(exit_code: i32) -> i32 {
    if exit_code < SIGNAL_EXIT_CODE_BASE {
        let signum = SIGNAL_EXIT_CODE_BASE - exit_code;
        if (1..=64).contains(&signum) {
            return signum;
        }
    }
    (exit_code & 0xff) << 8
}

pub(crate) fn detach_child_from_parent(task: &Arc<TaskControlBlock>) -> bool {
    let parent = task.inner.lock().parent.clone();
    let Some(parent) = parent else {
        return false;
    };

    let mut parent_inner = parent.inner.lock();
    if let Some(index) = parent_inner.children.iter().position(|child| {
        Arc::ptr_eq(child, task)
            || child.pid.0 == task.pid.0
            || child.thread_group.tgid() == task.thread_group.tgid()
    }) {
        parent_inner.children.remove(index);
        drop(parent_inner);
        task.inner.lock().parent = None;
        true
    } else {
        false
    }
}

pub(crate) fn purge_exited_user_task_for_foreground(task: &Arc<TaskControlBlock>) {
    if task.is_kernel {
        return;
    }
    purge_wait_state_for_task(task);
    if task.status() == TaskStatus::Zombie {
        detach_child_from_parent(task);
    }
}

fn is_kernel_task(task: &Arc<TaskControlBlock>) -> bool {
    task.is_kernel
}

fn task_ctx_ptr(task: &Arc<TaskControlBlock>) -> *mut TaskContext {
    task.task_ctx_ptr()
}

pub(crate) fn requeue_after_user_run(task: Arc<TaskControlBlock>) {
    match task.status() {
        TaskStatus::Zombie | TaskStatus::Blocked => {}
        TaskStatus::Running | TaskStatus::Ready => {
            *task.block_reason.lock() = None;
            task.set_status(TaskStatus::Ready);
            if take_foreground_requeue_front(task.pid.0) {
                manager::add_task_front(task);
            } else {
                manager::add_task(task);
            }
        }
    }
}

pub fn block_current_for_reason(reason: wait_queue::BlockReason) {
    block_current_for_reason_until(reason, None);
}

pub fn block_current_for_reason_until(reason: wait_queue::BlockReason, deadline_us: Option<usize>) {
    if let Some(task) = current_task() {
        if task.wait_outcome.lock().is_some() {
            return;
        }
        *task.block_reason.lock() = Some(reason);
    }
    block_current_and_run_next(deadline_us);
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
                if !harness::try_start_runtime_test_harness() {
                    report_no_init_and_maybe_shutdown("init ELF parse/load failed");
                }
            }
        }
    } else {
        log::warn!("[task] No init program found in filesystem");
        if !harness::try_start_runtime_test_harness() {
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

/// 开始运行任务
///
/// 这是调度器的入口函数，从内核 main 函数调用
/// 循环从就绪队列中获取任务并执行。
/// 在正常模式下永不返回（idle_loop WFI）；在 foreground driver 下可能返回。
pub fn run_tasks() {
    log::info!("[task] Starting task scheduler...");
    loop {
        run_next_task();
        // run_next_task should not return in normal mode. If it does, panic.
        if !crate::trap::foreground_driver_active() {
            panic!("run_tasks: run_next_task returned unexpectedly without foreground driver");
        }
        // In foreground driver mode, run_next_task can return when there's no task to run.
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

pub fn yield_current_once() -> bool {
    let Some(task) = current_task() else {
        return false;
    };
    if is_kernel_task(&task) {
        return false;
    }

    crate::timer::wake_expired_timers();
    if crate::trap::foreground_driver_active() {
        task.set_status(TaskStatus::Ready);
        return manager::has_task();
    }
    suspend_current_and_run_next();
    true
}

pub fn block_current_and_run_next(deadline_us: Option<usize>) {
    const FOREGROUND_NO_RUNNABLE_SPINS: usize = 1024;
    let Some(task) = current_task() else {
        return;
    };
    if is_kernel_task(&task) {
        task.set_status(TaskStatus::Blocked);
        *CURRENT_TASK.lock() = None;
        switch_kernel_task_back_to_scheduler(&task);
        return;
    }
    if let Some(tf) = crate::trap::clone_current_trapframe() {
        *task.trap_frame.lock() = Some(tf);
    }
    task.set_status(TaskStatus::Blocked);
    *CURRENT_TASK.lock() = None;

    let mut no_runnable_spins = 0usize;
    let mut parked_syscall = false;
    while task.status() == TaskStatus::Blocked {
        if crate::trap::foreground_driver_active() {
            if run_ready_task_once() {
                no_runnable_spins = 0;
                continue;
            }
        }
        crate::timer::wake_expired_timers();
        if task.status() != TaskStatus::Blocked {
            break;
        }
        if !crate::trap::foreground_driver_active() && run_ready_task_once() {
            no_runnable_spins = 0;
            continue;
        }
        if crate::trap::foreground_driver_active() {
            no_runnable_spins += 1;
            if no_runnable_spins >= FOREGROUND_NO_RUNNABLE_SPINS {
                if matches!(*task.block_reason.lock(), Some(wait_queue::BlockReason::Futex)) {
                    crate::trap::signal_syscall_parked();
                    parked_syscall = true;
                    break;
                } else {
                    if deadline_us.is_none() {
                        wake_blocked_task(&task, wait_queue::WaitOutcome::Interrupted);
                        break;
                    }
                }
            }
            core::hint::spin_loop();
        } else {
            wait_for_interrupt();
        }
    }

    manager::remove_task_instances(&task);
    if parked_syscall {
        return;
    }
    if task.status() == TaskStatus::Ready {
        *task.block_reason.lock() = None;
        task.set_status(TaskStatus::Running);
    }
    if task.status() != TaskStatus::Zombie {
        task.memory_set.lock().activate();
        *CURRENT_TASK.lock() = Some(task);
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
        finish_task_exit(&task, exit_code);
        if task.thread_group.all_user_members_zombie() {
            finish_process_exit(&task, exit_code);
        }
        *CURRENT_TASK.lock() = None;
    }
    if crate::trap::foreground_driver_active() {
        return;
    }
    run_next_task();
}

pub fn exit_thread_group_and_run_next(exit_code: i32) {
    if let Some(task) = current_task() {
        if is_kernel_task(&task) {
            exit_current_and_run_next(exit_code);
            return;
        }
        log::info!(
            "[task] Thread group {} exiting with code {}",
            task.thread_group.tgid(),
            exit_code
        );
        let members = task.thread_group.user_members();
        for member in &members {
            finish_task_exit(member, exit_code);
        }
        finish_process_exit(&task, exit_code);
        *CURRENT_TASK.lock() = None;
    }
    if crate::trap::foreground_driver_active() {
        return;
    }
    run_next_task();
}

pub(crate) fn terminate_task_group(task: &Arc<TaskControlBlock>, exit_code: i32) {
    if task.is_kernel {
        return;
    }
    let members = task.thread_group.user_members();
    for member in &members {
        finish_task_exit(member, exit_code);
    }
    finish_process_exit(task, exit_code);
}

pub(crate) fn terminate_thread_group_peers_for_exec(task: &Arc<TaskControlBlock>) {
    if task.is_kernel {
        return;
    }
    let members = task.thread_group.user_members();
    for member in &members {
        if member.pid.0 != task.pid.0 && member.status() != TaskStatus::Zombie {
            finish_task_exit(member, 0);
        }
    }
}

fn finish_task_exit(task: &Arc<TaskControlBlock>, exit_code: i32) {
    crate::syscall::other::process_robust_list_on_exit(task);
    let clear_child_tid = {
        let inner = task.inner.lock();
        let clear_child_tid = inner.clear_child_tid;
        clear_child_tid
    };
    if clear_child_tid != 0 {
        let bytes = 0i32.to_ne_bytes();
        let memory_set = task.memory_set.lock();
        if let Err(err) =
            crate::syscall::user::copy_to_user_in_memory_set(&memory_set, clear_child_tid, &bytes)
        {
            log::debug!(
                "[task] clear_child_tid failed tid={} addr={:#x} err={:?}",
                task.pid.0,
                clear_child_tid,
                err
            );
        }
        crate::syscall::other::futex_wake_addr_for_task(task, clear_child_tid, usize::MAX);
    }
    purge_wait_state_for_task(task);
    task.set_exit_code(exit_code);
    *task.trap_frame.lock() = None;
    *task.block_reason.lock() = None;
    *task.wait_outcome.lock() = None;
    task.set_status(TaskStatus::Zombie);
    crate::task::manager::record_exited_task(task.pid.0, task.thread_group.tgid());
    crate::fs::fd::flush_console_buffer_for_pid(task.pid.0);
    crate::fs::fd::flush_console_buffer_for_pid(task.thread_group.tgid());
}

fn purge_wait_state_for_task(task: &Arc<TaskControlBlock>) {
    let removed = manager::remove_task_instances(task)
        + wait_queue::remove_core_waiters_for_task(task)
        + crate::timer::remove_task_timer_waiters(task)
        + crate::syscall::other::remove_futex_waiters_for_task(task)
        + crate::syscall::signal::remove_signal_waiters_for_task(task);
    if removed != 0 {
        log::debug!(
            "[task] purged {} queued wait entries for pid={}",
            removed,
            task.pid.0
        );
    }
}

fn finish_process_exit(task: &Arc<TaskControlBlock>, exit_code: i32) {
    if !task.thread_group.mark_process_zombie(exit_code) {
        return;
    }

    crate::syscall::signal::notify_child_exit(task);

    let mut orphans = Vec::new();
    for member in task.thread_group.user_members() {
        let mut inner = member.inner.lock();
        orphans.extend(core::mem::take(&mut inner.children));
    }
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
    crate::task::wait_queue::wake_child_waiters();
}

/// 运行下一个任务
///
/// 从就绪队列中获取下一个任务并切换到它。
/// 在 foreground driver 下，如果没有可运行任务则返回，由前台驱动继续执行。
pub(crate) fn run_next_task() {
    // UART marker: 'S' = scheduler entry

    if let Some(task) = fetch_dispatchable_task() {
        if matches!(task.status(), TaskStatus::Zombie | TaskStatus::Blocked) {
            if crate::trap::foreground_driver_active() {
                return;
            }
            run_next_task();
            return;
        }
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
            if !crate::syscall::signal::handle_pending_for_user(&mut ctx) {
                crate::trap::restore_kernel_page_table();
                if task.status() != TaskStatus::Zombie {
                    *task.trap_frame.lock() = Some(ctx);
                }
                requeue_after_user_run(task.clone());
                *CURRENT_TASK.lock() = None;
                if crate::trap::foreground_driver_active() {
                    return;
                }
                run_next_task();
                return;
            }
            crate::trap::prepare_user_trapframe(&mut ctx);
            enter_foreground_user_task(task.pid.0);
            let reason = run_user_task(&mut ctx);
            leave_foreground_user_task(task.pid.0);
            crate::trap::restore_kernel_page_table();
            log::debug!("[task] User task returned with reason: {:?}", reason);
            // Normal case: put ctx back and requeue task
            *task.trap_frame.lock() = Some(ctx);
            requeue_after_user_run(task.clone());
            *CURRENT_TASK.lock() = None;
            if crate::trap::foreground_driver_active() {
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
        // 在 foreground driver 下：返回，让前台驱动继续（可能超时退出）
        // 正常模式：进入 idle 循环
        if crate::trap::foreground_driver_active() {
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
/// 在 foreground driver（测试 harness）下，如果没有可运行任务则直接返回，
/// 让调度器退出到前台驱动层，由驱动层继续执行下一个测试用例。
fn idle_loop() {
    if crate::trap::foreground_driver_active() {
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
        crate::timer::wake_expired_timers();
        if manager::has_task() {
            run_next_task();
        }
        // Outside foreground driver mode, this loops forever (WFI).
        // WFI永远不会返回...
    }
}

/// Wait for the next interrupt while there is no runnable task.
fn wait_for_interrupt() {
    #[cfg(target_arch = "riscv64")]
    unsafe {
        core::arch::asm!("wfi");
    }

    #[cfg(target_arch = "loongarch64")]
    unsafe {
        core::arch::asm!("idle 0");
    }
}

pub(crate) fn run_ready_task_once() -> bool {
    let Some(active) = fetch_dispatchable_task() else {
        return false;
    };
    if matches!(active.status(), TaskStatus::Zombie | TaskStatus::Blocked) {
        return true;
    }

    active.set_status(TaskStatus::Running);
    *CURRENT_TASK.lock() = Some(active.clone());

    let (has_user_ctx, tf_opt, ms_arc) = {
        let tf = active.trap_frame.lock().take();
        let has_user = tf.is_some();
        let ms = active.memory_set.clone();
        (has_user, tf, ms)
    };
    if has_user_ctx {
        ms_arc.lock().activate();
    }

    if let Some(mut ctx) = tf_opt {
        if !crate::syscall::signal::handle_pending_for_user(&mut ctx) {
            crate::trap::restore_kernel_page_table();
            if active.status() != TaskStatus::Zombie {
                *active.trap_frame.lock() = Some(ctx);
            }
            requeue_after_user_run(active);
            *CURRENT_TASK.lock() = None;
            return true;
        }
        crate::trap::prepare_user_trapframe(&mut ctx);
        enter_foreground_user_task(active.pid.0);
        let _reason = run_user_task(&mut ctx);
        leave_foreground_user_task(active.pid.0);
        crate::trap::restore_kernel_page_table();
        if active.status() != TaskStatus::Zombie {
            *active.trap_frame.lock() = Some(ctx);
        }

        requeue_after_user_run(active);
        *CURRENT_TASK.lock() = None;
        true
    } else {
        let task_ctx = task_ctx_ptr(&active);
        unsafe {
            SCHEDULER_CONTEXT.ra = kernel_task_return as usize;
            SCHEDULER_CONTEXT.sp = 0;
            SCHEDULER_CONTEXT.s = [0; 12];
        }
        let idle_ctx = core::ptr::addr_of_mut!(SCHEDULER_CONTEXT);
        SCHEDULER_CONTEXT_PTR.store(idle_ctx as usize, Ordering::SeqCst);
        unsafe {
            context::switch_to(idle_ctx, task_ctx as *const TaskContext);
        }
        SCHEDULER_CONTEXT_PTR.store(0, Ordering::SeqCst);
        true
    }
}

fn mark_blocked_task_ready(task: &Arc<TaskControlBlock>, outcome: wait_queue::WaitOutcome) -> bool {
    let mut status = task.status.lock();
    if *status != TaskStatus::Blocked {
        return false;
    }
    *status = TaskStatus::Ready;
    *task.block_reason.lock() = None;
    *task.wait_outcome.lock() = Some(outcome);
    drop(status);
    manager::add_task(task.clone());
    true
}

pub(crate) fn wake_task_token_with(
    task: &Arc<TaskControlBlock>,
    token: usize,
    outcome: wait_queue::WaitOutcome,
) -> bool {
    if task.current_wait_token() != token {
        return false;
    }
    if task.status() != TaskStatus::Blocked {
        if task.block_reason.lock().is_some() {
            *task.wait_outcome.lock() = Some(outcome);
            return true;
        }
        return false;
    }
    mark_blocked_task_ready(task, outcome)
}

pub(crate) fn wake_blocked_task(
    task: &Arc<TaskControlBlock>,
    outcome: wait_queue::WaitOutcome,
) -> bool {
    mark_blocked_task_ready(task, outcome)
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
    pub thread_group: Arc<ThreadGroup>,
    /// True for kernel-only tasks that are switched by TaskContext instead of TrapFrame.
    pub is_kernel: bool,
    /// Inner data protected by mutex (fd_table, children, cwd, etc.)
    pub inner: Mutex<TaskControlBlockInner>,
    /// Kernel task context. Outside inner to avoid deadlock.
    pub(crate) task_ctx: KernelCtx,
    /// User address space. Outside inner to avoid deadlock with activate().
    pub memory_set: SharedMemorySet,
    pub fs: SharedFsContext,
    pub mm: SharedMmContext,
    pub signal_actions: crate::syscall::signal::SharedSignalActions,
    pub signal_state: Mutex<crate::syscall::signal::SignalState>,
    /// User trap frame. Outside inner for foreground driver.
    pub trap_frame: Mutex<Option<TrapFrame>>,
    /// Task status. Outside inner to avoid deadlock.
    pub status: Mutex<TaskStatus>,
    /// Last reason this task intentionally entered Blocked state.
    pub block_reason: Mutex<Option<wait_queue::BlockReason>>,
    /// Source that moved the task out of Blocked state for the active wait.
    pub wait_outcome: Mutex<Option<wait_queue::WaitOutcome>>,
    /// Monotonic wait token used to reject stale timeout wakeups.
    pub wait_token: AtomicUsize,
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
    pub rlimit_nofile: usize,
    pub rlimit_nofile_max: usize,
    pub clear_child_tid: usize,
    pub robust_list_head: usize,
    pub robust_list_len: usize,
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
