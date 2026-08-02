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
use core::sync::atomic::{fence, AtomicUsize, Ordering};
use lazy_static::lazy_static;
use polyhal_trap::trap::{run_user_task, EscapeReason};
use polyhal_trap::trapframe::TrapFrame;
use spin::Mutex;

use crate::console::putchar;
use crate::cpu::CpuLocal;
use crate::fs::fd::FileDescriptorTable;
use crate::mm::memory_set::MemorySet;
use crate::task::context::TaskContext;
use crate::task::processor::Processor;

struct SchedulerSlot {
    context: core::cell::UnsafeCell<TaskContext>,
    active: AtomicUsize,
}

impl SchedulerSlot {
    const fn new() -> Self {
        Self {
            context: core::cell::UnsafeCell::new(TaskContext {
                ra: 0,
                sp: 0,
                s: [0; 12],
            }),
            active: AtomicUsize::new(0),
        }
    }
}

unsafe impl Sync for SchedulerSlot {}

static SCHEDULER_SLOTS: [SchedulerSlot; crate::config::MAX_CPUS] =
    [const { SchedulerSlot::new() }; crate::config::MAX_CPUS];
pub(crate) const SIGNAL_EXIT_CODE_BASE: i32 = -0x1000;
pub const SCHED_OTHER: usize = 0;
pub const SCHED_FIFO: usize = 1;
pub const SCHED_RR: usize = 2;
pub const SCHED_BATCH: usize = 3;
pub const SCHED_IDLE: usize = 5;
pub const SCHED_DEADLINE: usize = 6;
pub const NO_CPU: usize = usize::MAX;

/// Flag set when the scheduler is context-switching FROM a user task that called exit()
/// (via ECANCELED in the trap handler). When this flag is set, kernel_task_return()
/// should NOT call run_next_task() again, because we already switched to the harness
/// and the harness's loop will handle scheduling. This prevents double-scheduling.
pub(crate) static RUNNING_FROM_ECANCELED: AtomicUsize = AtomicUsize::new(0);
static FOREGROUND_DEADLINE_US: AtomicUsize = AtomicUsize::new(0);

lazy_static! {
    /// 当前运行的任务
    pub static ref CURRENT_TASK: CpuLocal<Option<Arc<TaskControlBlock>>> =
        CpuLocal::new_with(|_| None);

    /// CPU 处理器状态
    pub static ref PROCESSOR: CpuLocal<Processor> = CpuLocal::new_with(|_| Processor::new());

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

pub const LINUX_NGROUPS_MAX: usize = 65_536;
pub const MAX_SUPPLEMENTARY_GROUPS: usize = 32;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Credentials {
    pub real_uid: u32,
    pub effective_uid: u32,
    pub saved_uid: u32,
    pub fsuid: u32,
    pub real_gid: u32,
    pub effective_gid: u32,
    pub saved_gid: u32,
    pub fsgid: u32,
    supplementary_groups: Vec<u32>,
}

impl Credentials {
    pub fn root() -> Self {
        Self {
            real_uid: 0,
            effective_uid: 0,
            saved_uid: 0,
            fsuid: 0,
            real_gid: 0,
            effective_gid: 0,
            saved_gid: 0,
            fsgid: 0,
            supplementary_groups: vec![0],
        }
    }

    pub fn has_uid(&self, uid: u32) -> bool {
        uid == self.real_uid || uid == self.effective_uid || uid == self.saved_uid
    }

    pub fn has_gid(&self, gid: u32) -> bool {
        gid == self.real_gid || gid == self.effective_gid || gid == self.saved_gid
    }

    pub fn has_uid_or_fsuid(&self, uid: u32) -> bool {
        self.has_uid(uid) || uid == self.fsuid
    }

    pub fn has_gid_or_fsgid(&self, gid: u32) -> bool {
        self.has_gid(gid) || gid == self.fsgid
    }

    pub fn is_root_capable(&self) -> bool {
        self.effective_uid == 0
    }

    pub fn supplementary_groups(&self) -> &[u32] {
        self.supplementary_groups.as_slice()
    }

    pub fn is_in_group(&self, gid: u32, effective: bool) -> bool {
        let primary = if effective {
            self.effective_gid
        } else {
            self.real_gid
        };
        primary == gid || self.supplementary_groups.iter().any(|group| *group == gid)
    }

    pub fn is_in_filesystem_group(&self, gid: u32) -> bool {
        self.fsgid == gid || self.supplementary_groups.iter().any(|group| *group == gid)
    }

    pub fn sync_fsuid_to_effective(&mut self) {
        self.fsuid = self.effective_uid;
    }

    pub fn sync_fsgid_to_effective(&mut self) {
        self.fsgid = self.effective_gid;
    }

    pub fn set_supplementary_groups(&mut self, groups: &[u32]) {
        self.supplementary_groups.clear();
        self.supplementary_groups.extend_from_slice(groups);
    }
}

#[derive(Clone)]
pub struct FsContext {
    pub cwd: String,
    pub root: String,
    pub umask: u32,
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
    stopped: AtomicUsize,
    child_wait: Mutex<ChildWaitState>,
}

#[derive(Clone, Copy)]
pub enum ChildWaitEvent {
    Stopped(i32),
    Continued(i32),
}

struct ChildWaitState {
    stopped_signal: Option<i32>,
    stopped_consumed: bool,
    continued_signal: Option<i32>,
    continued_consumed: bool,
}

impl ChildWaitState {
    fn new() -> Self {
        Self {
            stopped_signal: None,
            stopped_consumed: true,
            continued_signal: None,
            continued_consumed: true,
        }
    }
}

impl ThreadGroup {
    pub fn new(tgid: usize) -> Arc<Self> {
        Arc::new(Self {
            tgid,
            members: Mutex::new(Vec::new()),
            exit_code: Mutex::new(0),
            process_zombie: AtomicUsize::new(0),
            stopped: AtomicUsize::new(0),
            child_wait: Mutex::new(ChildWaitState::new()),
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

    pub fn mark_stopped(&self, signum: i32) {
        self.stopped.store(1, Ordering::SeqCst);
        let mut wait = self.child_wait.lock();
        wait.stopped_signal = Some(signum);
        wait.stopped_consumed = false;
        wait.continued_signal = None;
        wait.continued_consumed = true;
    }

    pub fn mark_continued(&self, signum: i32) -> bool {
        let was_stopped = self.stopped.swap(0, Ordering::SeqCst) != 0;
        let mut wait = self.child_wait.lock();
        wait.stopped_signal = None;
        wait.stopped_consumed = true;
        if was_stopped {
            wait.continued_signal = Some(signum);
            wait.continued_consumed = false;
        }
        was_stopped
    }

    pub fn take_child_wait_event(
        &self,
        want_stopped: bool,
        want_continued: bool,
        consume: bool,
    ) -> Option<ChildWaitEvent> {
        let mut wait = self.child_wait.lock();
        if want_stopped && !wait.stopped_consumed {
            if let Some(signum) = wait.stopped_signal {
                if consume {
                    wait.stopped_consumed = true;
                }
                return Some(ChildWaitEvent::Stopped(signum));
            }
        }
        if want_continued && !wait.continued_consumed {
            if let Some(signum) = wait.continued_signal {
                if consume {
                    wait.continued_consumed = true;
                }
                return Some(ChildWaitEvent::Continued(signum));
            }
        }
        None
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
    Arc::new(Mutex::new(FsContext {
        cwd,
        root,
        umask: 0o022,
    }))
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

fn fetch_dispatchable_user_task() -> Option<Arc<TaskControlBlock>> {
    let mut skipped_active = Vec::new();
    loop {
        let Some(task) = manager::fetch_user_task_for_foreground() else {
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

fn fetch_dispatchable_task() -> Option<Arc<TaskControlBlock>> {
    if let Some(task) = fetch_dispatchable_user_task() {
        return Some(task);
    }
    if crate::trap::foreground_driver_active() {
        return None;
    }
    manager::fetch_kernel_task()
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

pub(crate) fn set_foreground_deadline_us(deadline_us: usize) {
    FOREGROUND_DEADLINE_US.store(deadline_us, Ordering::Relaxed);
}

pub(crate) fn clear_foreground_deadline_us() {
    FOREGROUND_DEADLINE_US.store(0, Ordering::Relaxed);
}

pub(crate) fn foreground_deadline_expired() -> bool {
    let deadline_us = FOREGROUND_DEADLINE_US.load(Ordering::Relaxed);
    deadline_us != 0
        && crate::trap::foreground_driver_active()
        && crate::timer::get_time_us() >= deadline_us
}

fn abort_foreground_user_tasks(exit_code: i32) {
    let tasks = manager::all_user_tasks();
    let mut killed_tgids = Vec::new();
    for task in &tasks {
        let tgid = task.thread_group.tgid();
        if task.status() != TaskStatus::Zombie && !killed_tgids.iter().any(|seen| *seen == tgid) {
            killed_tgids.push(tgid);
            terminate_task_group(task, exit_code);
        }
    }
    for task in tasks {
        purge_exited_user_task_for_foreground(&task);
    }
}

fn is_kernel_task(task: &Arc<TaskControlBlock>) -> bool {
    task.is_kernel
}

fn task_ctx_ptr(task: &Arc<TaskControlBlock>) -> *mut TaskContext {
    task.task_ctx_ptr()
}

pub(crate) fn requeue_after_user_run(task: Arc<TaskControlBlock>) {
    let status = *task.status.lock();
    match status {
        TaskStatus::Zombie | TaskStatus::Stopped => {
            task.blocking_cpu.store(NO_CPU, Ordering::Release);
        }
        TaskStatus::Blocked => {
            // The syscall path kept ownership while unwinding out of
            // run_user_task(), so a racing waker could publish Ready without
            // enqueueing the task. Hand ownership to the global scheduler only
            // after CURRENT_TASK has been cleared, then compensate either side
            // of the handoff. ReadyQueue deduplication makes the overlap safe.
            task.blocking_cpu.store(NO_CPU, Ordering::Release);
            fence(Ordering::SeqCst);
            if task.status() == TaskStatus::Ready {
                manager::add_task(task);
            }
        }
        TaskStatus::Running | TaskStatus::Ready => {
            task.blocking_cpu.store(NO_CPU, Ordering::Release);
            *task.block_reason.lock() = None;
            {
                let mut status = task.status.lock();
                if !matches!(*status, TaskStatus::Running | TaskStatus::Ready) {
                    return;
                }
                *status = TaskStatus::Ready;
            }
            if take_foreground_requeue_front(task.pid.0) {
                manager::add_task_front(task);
            } else {
                manager::add_task(task);
            }
        }
    }
}

fn current_cpu_owns_running_user_task(task: &Arc<TaskControlBlock>) -> bool {
    if task.status() != TaskStatus::Running {
        return false;
    }

    let cpu = crate::platform::current_cpu_index();
    if task.running_cpu.load(Ordering::Acquire) != cpu
        || task.blocking_cpu.load(Ordering::Acquire) != NO_CPU
    {
        return false;
    }

    CURRENT_TASK
        .lock()
        .as_ref()
        .map(|current| Arc::ptr_eq(current, task))
        .unwrap_or(false)
}

/// Keep running the current user task across traps that do not require a
/// scheduling decision.  A normal syscall and a successfully handled page
/// fault preserve the task's CPU ownership; timer/IPI traps, yield, blocking,
/// stop, and exit return to the scheduler.
pub(crate) fn run_current_user_task_until_reschedule(
    task: &Arc<TaskControlBlock>,
    ctx: &mut TrapFrame,
) {
    loop {
        if !current_cpu_owns_running_user_task(task) {
            crate::trap::restore_kernel_page_table();
            break;
        }
        if !crate::syscall::signal::handle_pending_for_user(ctx) {
            crate::trap::restore_kernel_page_table();
            break;
        }
        if !current_cpu_owns_running_user_task(task) {
            crate::trap::restore_kernel_page_table();
            break;
        }

        task.memory_set.lock().activate();
        crate::trap::prepare_user_trapframe(ctx);
        enter_foreground_user_task(task.pid.0);
        crate::trap::interrupts::disable_interrupt();
        let reason = run_user_task(ctx);
        leave_foreground_user_task(task.pid.0);

        if !matches!(reason, EscapeReason::SysCall | EscapeReason::NoReason)
            || !current_cpu_owns_running_user_task(task)
        {
            crate::trap::restore_kernel_page_table();
            break;
        }
    }
}

fn run_current_user_task_one_boundary(task: &Arc<TaskControlBlock>, ctx: &mut TrapFrame) {
    if !current_cpu_owns_running_user_task(task) {
        return;
    }
    if !crate::syscall::signal::handle_pending_for_user(ctx) {
        return;
    }
    if !current_cpu_owns_running_user_task(task) {
        return;
    }

    task.memory_set.lock().activate();
    crate::trap::prepare_user_trapframe(ctx);
    enter_foreground_user_task(task.pid.0);
    crate::trap::interrupts::disable_interrupt();
    let _reason = run_user_task(ctx);
    leave_foreground_user_task(task.pid.0);
    crate::trap::restore_kernel_page_table();
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
    let slot = &SCHEDULER_SLOTS[crate::platform::current_cpu_index()];
    if slot.active.load(Ordering::Acquire) == 0 {
        log::error!(
            "[task] missing scheduler context for kernel task {}",
            task.pid.0
        );
        return;
    }
    let current_ctx_ptr = task_ctx_ptr(task);
    let scheduler_ctx_ptr = slot.context.get();
    unsafe {
        context::switch_to(current_ctx_ptr, scheduler_ctx_ptr as *const TaskContext);
    }
}

fn switch_to_kernel_task(task: &Arc<TaskControlBlock>) {
    let task_ctx = task_ctx_ptr(task);
    let slot = &SCHEDULER_SLOTS[crate::platform::current_cpu_index()];
    let scheduler_ctx = slot.context.get();
    unsafe {
        (*scheduler_ctx).ra = kernel_task_return as usize;
        (*scheduler_ctx).sp = 0;
        (*scheduler_ctx).s = [0; 12];
    }
    slot.active.store(1, Ordering::Release);

    log::debug!(
        "[task] Starting kernel task {} via context switch",
        task.pid.0
    );

    unsafe {
        context::switch_to(scheduler_ctx, task_ctx as *const TaskContext);
    }

    slot.active.store(0, Ordering::Release);
}

pub fn init_kernel_page() {
    crate::mm::page_table::init_kernel_page_table();
}

/// 添加 init 进程
pub fn add_initproc() {
    // 尝试从文件系统加载 init 程序
    log::info!("[task] Loading init process...");

    // 手动演示/录屏入口。普通评测仍走 /init 或 harness；只有编译时设置
    // WLL_INTERACTIVE=1 才提前启动 BusyBox shell。这样可以避免把交互逻辑
    // 混入 judge 路径，也不会影响默认自动测试。
    if interactive_mode_enabled() {
        if start_interactive_shell() {
            return;
        }
        console_write("[interactive] failed to start BusyBox shell; falling back to normal init/harness\n");
    }

    // A compile-time harness filter is an explicit test-runner request.  It
    // must take precedence over an image-provided /init so official suites
    // execute their selected script rather than the image's normal boot flow.
    if harness::harness_filter_active() && harness::try_start_runtime_test_harness() {
        return;
    }

    let init_candidates = ["init", "/init"];
    let elf_data = init_candidates//读取elf文件
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

fn interactive_mode_enabled() -> bool {
    matches!(
        option_env!("WLL_INTERACTIVE"),
        Some("1") | Some("true") | Some("TRUE") | Some("yes") | Some("YES") | Some("on") | Some("ON")
    )
}

fn start_interactive_shell() -> bool {
    // 运行时 ext4 镜像在不同 libc 根目录下可能都有 busybox；优先选择能被
    // VFS 读到的第一个候选路径。这里不假设 /bin/sh 一定存在，因为测试镜像
    // 中常常只提供 /musl/busybox 或 /glibc/busybox。
    let shell_candidates = ["/busybox", "/bin/busybox", "/musl/busybox", "/glibc/busybox"];
    let Some(shell_path) = shell_candidates
        .iter()
        .find(|path| crate::fs::read_executable_file(path).is_some())
    else {
        console_write("[interactive] BusyBox not found. Build with DEV_PRELOAD=1 and attach sdcard image.\n");
        return false;
    };

    console_write("[interactive] starting BusyBox shell: ");
    console_write(shell_path);
    console_write(" sh -i\n");

    let spec = UserProgramSpec {
        path: String::from(*shell_path),
        argv: vec![String::from(*shell_path), String::from("sh"), String::from("-i")],
        envp: vec![
            // /tmp 放在最前面，是为了让内核安装的 demo 脚本优先被找到。
            // 后续路径覆盖根目录、普通 /bin，以及 musl/glibc 镜像中的工具。
            String::from("PATH=/tmp:/:/bin:/usr/bin:/musl/bin:/glibc/bin"),
            String::from("LD_LIBRARY_PATH=/lib:/"),
            String::from("SHELL=/busybox"),
            String::from("HOME=/"),
            String::from("PS1=wll_OS # "),
            String::from("TERM=vt100"),
        ],
        cwd: String::from("/"),
        // root 保持 /，避免 path=/musl/busybox 时再叠加 /musl 造成
        // /musl/musl/busybox 之类的错误解析。不同 libc 目录通过 PATH 访问。
        root: String::from("/"),
        marker_name: None,
    };

    match TaskControlBlock::new_user_with_args_env_cwd(&spec) {
        Ok(task) => {
            log::info!("[interactive] shell loaded, pid={}", task.pid.0);
            set_orphan_reaper(task.clone());
            manager::add_task(task);
            true
        }
        Err(err) => {
            log::error!("[interactive] failed to load shell: {:?}", err);
            false
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

/// Scheduler loop used by secondary CPUs. It shares the global ready queues,
/// while current task and scheduler context remain CPU-local.
pub fn run_secondary_tasks() -> ! {
    loop {
        #[cfg(feature = "buildstorm-diagnostics")]
        crate::buildstorm_diagnostics::maybe_report();
        crate::timer::wake_expired_timers();
        if run_ready_task_once() || drain_kernel_ready_once() {
            continue;
        }
        wait_for_interrupt();
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
            // A kernel task is still executing on this CPU until the context
            // switch has completed.  Let the scheduler publish it only after
            // switch_to_kernel_task() returns, so another CPU cannot enter the
            // same kernel stack concurrently.
    let _ = task.set_status_if(TaskStatus::Running, TaskStatus::Ready);
            switch_kernel_task_back_to_scheduler(&task);
            return;
        }
        if let Some(tf) = crate::trap::clone_current_trapframe() {
            *task.trap_frame.lock() = Some(tf);
        }
        // Drop the execution claim before making Ready visible in the shared
        // queue.  A secondary scheduler may dequeue immediately after add_task.
        *CURRENT_TASK.lock() = None;
        task.release_running_cpu();
        task.set_status(TaskStatus::Ready);
        manager::add_task(task.clone());
        if crate::trap::foreground_driver_active() {
            return;
        }
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
        return manager::has_user_task();
    }
    suspend_current_and_run_next();
    true
}

pub fn block_current_and_run_next(deadline_us: Option<usize>) {
    const FOREGROUND_NO_RUNNABLE_SPINS: usize = 1024;
    let Some(task) = current_task() else {
        return;
    };
    // Keep the current CPU as the transition owner until the user-run wrapper
    // has completely unwound. A waker may set Ready during this interval, but
    // must not enqueue the same TCB while it is still executing here.
    task.blocking_cpu.store(
        crate::platform::current_cpu_index(),
        Ordering::Release,
    );
    if is_kernel_task(&task) {
        task.set_status(TaskStatus::Blocked);
        switch_kernel_task_back_to_scheduler(&task);
        return;
    }
    if let Some(tf) = crate::trap::clone_current_trapframe() {
        *task.trap_frame.lock() = Some(tf);
    }
    if !task.set_status_if(TaskStatus::Running, TaskStatus::Blocked) {
        task.blocking_cpu.store(NO_CPU, Ordering::Release);
        return;
    }
    // A wakeup may race with the transition above.  When it arrives while the
    // task is still Running, the waker records `wait_outcome` instead of
    // enqueueing a task that is still executing on this CPU.  Recheck after
    // publishing Blocked so that such an early wakeup cannot be overwritten
    // by clearing CURRENT_TASK and entering the wait loop.
    if task.wait_outcome.lock().is_some() {
        task.blocking_cpu.store(NO_CPU, Ordering::Release);
        let _ = task.set_status_if(TaskStatus::Blocked, TaskStatus::Running)
            || task.set_status_if(TaskStatus::Ready, TaskStatus::Running);
        return;
    }
    *CURRENT_TASK.lock() = None;

    if crate::trap::foreground_driver_active() && deadline_us.is_none() {
        crate::trap::signal_syscall_parked();
        return;
    }

    let mut no_runnable_spins = 0usize;
    while task.status() == TaskStatus::Blocked {
        // Timed waits must be observed even when other foreground tasks keep
        // the ready queue non-empty.
        crate::timer::wake_expired_timers();
        if foreground_deadline_expired() {
            abort_foreground_user_tasks(-2);
            break;
        }
        if task.status() != TaskStatus::Blocked {
            break;
        }
        if crate::trap::foreground_driver_active() {
            if run_ready_task_once_for_blocked_owner() {
                no_runnable_spins = 0;
                continue;
            }
        }
        if !crate::trap::foreground_driver_active() {
            if run_ready_task_once_for_blocked_owner() {
                no_runnable_spins = 0;
                continue;
            }
            if drain_kernel_ready_once() {
                no_runnable_spins = 0;
                continue;
            }
        }
        if crate::trap::foreground_driver_active() {
            no_runnable_spins += 1;
            if no_runnable_spins >= FOREGROUND_NO_RUNNABLE_SPINS {
                core::hint::spin_loop();
            }
            core::hint::spin_loop();
        } else {
            wait_for_interrupt();
        }
    }

    task.blocking_cpu.store(NO_CPU, Ordering::Release);
    manager::remove_task_instances(&task);
    if task.set_status_if(TaskStatus::Ready, TaskStatus::Running) {
        *task.block_reason.lock() = None;
    }
    if task.status() != TaskStatus::Zombie {
        // Resume the blocked syscall on the kernel page table.  The scheduler
        // activates the user address space only immediately before user_restore.
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
            switch_kernel_task_back_to_scheduler(&task);
            return;
        }
        log::info!("[task] Task {} exiting with code {}", task.pid.0, exit_code);
        finish_task_exit(&task, exit_code);
        if task.thread_group.all_user_members_zombie() {
            finish_process_exit(&task, exit_code);
        }
        *CURRENT_TASK.lock() = None;
        task.release_running_cpu();
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
            if Arc::ptr_eq(member, &task) {
                finish_task_exit(member, exit_code);
            } else {
                request_task_exit(member, exit_code);
            }
        }
        crate::platform::notify_runnable();
        finish_process_exit(&task, exit_code);
        *CURRENT_TASK.lock() = None;
        task.release_running_cpu();
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
    let current = current_task();
    let members = task.thread_group.user_members();
    for member in &members {
        if current
            .as_ref()
            .map(|active| Arc::ptr_eq(active, member))
            .unwrap_or(false)
        {
            finish_task_exit(member, exit_code);
        } else {
            request_task_exit(member, exit_code);
        }
    }
    crate::platform::notify_runnable();
    finish_process_exit(task, exit_code);
}

pub(crate) fn stop_task_group(task: &Arc<TaskControlBlock>, signum: i32) {
    if task.is_kernel || task.thread_group.is_process_zombie() {
        return;
    }
    task.thread_group.mark_stopped(signum);
    for member in task.thread_group.user_members() {
        if member.status() == TaskStatus::Zombie {
            continue;
        }
        purge_wait_state_for_task(&member);
        manager::remove_task_instances(&member);
        *member.block_reason.lock() = None;
        *member.wait_outcome.lock() = None;
        member.set_status(TaskStatus::Stopped);
    }
    wait_queue::wake_child_waiters();
}

pub(crate) fn continue_task_group(task: &Arc<TaskControlBlock>, signum: i32) {
    if task.is_kernel || task.thread_group.is_process_zombie() {
        return;
    }
    let was_stopped = task.thread_group.mark_continued(signum);
    if !was_stopped {
        return;
    }
    for member in task.thread_group.user_members() {
        if member.status() == TaskStatus::Stopped {
            member.set_status(TaskStatus::Ready);
            manager::add_task(member);
        }
    }
    wait_queue::wake_child_waiters();
}

pub(crate) fn terminate_thread_group_peers_for_exec(task: &Arc<TaskControlBlock>) {
    if task.is_kernel {
        return;
    }
    let members = task.thread_group.user_members();
    for member in &members {
        if member.pid.0 != task.pid.0 && member.status() != TaskStatus::Zombie {
            request_task_exit(member, 0);
        }
    }
    crate::platform::notify_runnable();
}

fn request_task_exit(task: &Arc<TaskControlBlock>, exit_code: i32) {
    {
        let mut status = task.status.lock();
        if *status == TaskStatus::Zombie {
            return;
        }
        task.set_exit_code(exit_code);
        *status = TaskStatus::Zombie;
    }
    purge_wait_state_for_task(task);
    *task.trap_frame.lock() = None;
    *task.block_reason.lock() = None;
    *task.wait_outcome.lock() = None;
    crate::task::manager::record_exited_task(task.pid.0, task.thread_group.tgid());
    crate::fs::fd::flush_console_buffer_for_pid(task.pid.0);
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
        {
            let mut memory_set = task.memory_set.lock();
            if let Err(err) = crate::syscall::user::copy_to_user_in_memory_set(
                &mut memory_set,
                clear_child_tid,
                &bytes,
            ) {
                log::debug!(
                    "[task] clear_child_tid failed tid={} addr={:#x} err={:?}",
                    task.pid.0,
                    clear_child_tid,
                    err
                );
            }
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

fn detach_thread_group_shared_memory(members: &[Arc<TaskControlBlock>]) {
    let mut seen_memory_sets = Vec::new();
    for member in members {
        let key = Arc::as_ptr(&member.memory_set) as usize;
        if seen_memory_sets.iter().any(|seen| *seen == key) {
            continue;
        }
        seen_memory_sets.push(key);
        crate::syscall::mm::detach_task_shared_memory(member);
    }
}

fn release_process_runtime_resources(members: &[Arc<TaskControlBlock>]) {
    crate::trap::restore_kernel_page_table();

    let mut closed_fd_tables = Vec::new();
    let mut reset_mm_contexts = Vec::new();

    for member in members {
        // Another CPU can still be returning from this thread group's user
        // context after exit_group has marked every member zombie. Keep the
        // shared address space intact until the last owning task is dropped;
        // MemorySet's normal Drop path then releases page tables and frames.

        let fd_table = member.inner.lock().fd_table.clone();
        let fd_key = Arc::as_ptr(&fd_table) as usize;
        if !closed_fd_tables.iter().any(|seen| *seen == fd_key) {
            closed_fd_tables.push(fd_key);
            fd_table.lock().close_all();
        }

        let mm_key = Arc::as_ptr(&member.mm) as usize;
        if !reset_mm_contexts.iter().any(|seen| *seen == mm_key) {
            reset_mm_contexts.push(mm_key);
            let mut mm = member.mm.lock();
            mm.program_break = crate::config::USER_HEAP_START;
            mm.mapped_break = crate::config::USER_HEAP_START;
            mm.next_mmap = 0x2000_0000;
        }

        let mut inner = member.inner.lock();
        inner.program_break = crate::config::USER_HEAP_START;
        inner.mapped_break = crate::config::USER_HEAP_START;
        inner.next_mmap = 0x2000_0000;
        inner.clear_child_tid = 0;
        inner.robust_list_head = 0;
        inner.robust_list_len = 0;
        inner.interval_timers = crate::syscall::other::EMPTY_INTERVAL_TIMERS;
        inner.default_timer_slack_ns = inner.current_timer_slack_ns;
    }
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

    // 先记录父进程。后续 release_process_runtime_resources 会关闭 fd、释放
    // 地址空间等资源；父子关系本身仍需保留给 wait4/waitpid 回收 zombie。
    let parent = task.inner.lock().parent.clone();
    let members = task.thread_group.user_members();
    crate::syscall::fs::release_file_locks_for_pid(task.thread_group.tgid());
    detach_thread_group_shared_memory(&members);
    release_process_runtime_resources(&members);
    crate::syscall::signal::notify_child_exit(task);

    let mut orphans = Vec::new();
    for member in &members {
        let mut inner = member.inner.lock();//获取任务的锁
        orphans.extend(core::mem::take(&mut inner.children));//&mut inner.children 意思是：可变借用 children
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
    if let Some(parent) = parent {
        // 正常路径下 wake_child_waiters() 会唤醒所有等待子进程退出的任务。
        // 这里再定向唤醒一次直接父进程，是为了覆盖交互 shell 这类场景：
        // shell 启动外部命令后阻塞在 wait4，子进程已经输出并退出，但父进程
        // 若处在 wait 队列注册/调度切换的边界，单纯广播可能无法立刻让它
        // 回到 Ready。定向唤醒可确保父进程看到 zombie child 并完成回收。
        if matches!(
            *parent.block_reason.lock(),
            Some(wait_queue::BlockReason::ChildExit)
        ) {
            wake_blocked_task(&parent, wait_queue::WaitOutcome::Woken);
        }
    }
    crate::task::wait_queue::wake_child_waiters();
}

/// 运行下一个任务
///
/// 从就绪队列中获取下一个任务并切换到它。
/// 在 foreground driver 下，如果没有可运行任务则返回，由前台驱动继续执行。
pub(crate) fn run_next_task() {

    if let Some(task) = fetch_dispatchable_task() {
        if matches!(
            task.status(),
            TaskStatus::Zombie | TaskStatus::Blocked | TaskStatus::Stopped
        ) {
            if crate::trap::foreground_driver_active() {
                return;
            }
            run_next_task();
            return;
        }
        // 设置当前任务
        if !task.try_start_running() {
            if crate::trap::foreground_driver_active() {
                return;
            }
            run_next_task();
            return;
        }
        *CURRENT_TASK.lock() = Some(task.clone());

        // 设置任务状态为运行中
        log::debug!("[task] Switching to task pid={}", task.pid.0);
        if task.is_kernel {
            switch_to_kernel_task(&task);
            finish_kernel_task_switch(task);
            return;
        }
        // 获取任务的 TrapFrame（用户态上下文）
        // 如果任务有保存的 TrapFrame，从那里恢复
        // 否则这是一个新任务，需要初始化
        // 只有用户态任务才切换其地址空间。
        // `MemorySet::new_bare()` + RISC-V `PageTable::restore()` 会清零根页表「低半」条目；
        // 本项目内核链接在 `0x80200000`，落在该低半区——若对纯内核线程切换 SATP，
        // 会在用户页表里丢失内核代码映射而卡死。
        let tf_opt = task.trap_frame.lock().take();
        if let Some(mut ctx) = tf_opt {
            // 恢复任务的 TrapFrame 并返回用户态
            log::debug!("[task] Restoring TrapFrame for task {}", task.pid.0);
            run_current_user_task_until_reschedule(&task, &mut ctx);
            if task.status() != TaskStatus::Zombie {
                *task.trap_frame.lock() = Some(ctx);
            }
            *CURRENT_TASK.lock() = None;
            task.release_running_cpu();
            requeue_after_user_run(task.clone());
            if crate::trap::foreground_driver_active() {
                return;
            }
            run_next_task();
            return;
        } else {
            log::error!("[task] user task {} missing trap frame", task.pid.0);
            *CURRENT_TASK.lock() = None;
            task.release_running_cpu();
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
        wait_for_interrupt();

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
    crate::platform::prepare_idle();
    if manager::has_task() {
        crate::platform::finish_idle();
        return;
    }
    crate::trap::interrupts::enable_interrupt();
    #[cfg(target_arch = "riscv64")]
    unsafe {
        core::arch::asm!("wfi");
    }

    #[cfg(target_arch = "loongarch64")]
    unsafe {
        core::arch::asm!("idle 0");
    }
    crate::trap::interrupts::disable_interrupt();
    crate::platform::finish_idle();
}

pub(crate) fn run_ready_task_once() -> bool {
    let Some(active) = fetch_dispatchable_user_task() else {
        return false;
    };
    if matches!(
        active.status(),
        TaskStatus::Zombie | TaskStatus::Blocked | TaskStatus::Stopped
    ) {
        return true;
    }
    if !active.try_start_running() {
        return true;
    }

    *CURRENT_TASK.lock() = Some(active.clone());

    let tf_opt = active.trap_frame.lock().take();
    if let Some(mut ctx) = tf_opt {
        run_current_user_task_until_reschedule(&active, &mut ctx);
        if active.status() != TaskStatus::Zombie {
            *active.trap_frame.lock() = Some(ctx);
        }

        *CURRENT_TASK.lock() = None;
        active.release_running_cpu();
        requeue_after_user_run(active);
        true
    } else {
        log::error!("[task] ready user task {} missing trap frame", active.pid.0);
        *CURRENT_TASK.lock() = None;
        active.release_running_cpu();
        true
    }
}

fn run_ready_task_once_for_blocked_owner() -> bool {
    let Some(active) = fetch_dispatchable_user_task() else {
        return false;
    };
    if matches!(
        active.status(),
        TaskStatus::Zombie | TaskStatus::Blocked | TaskStatus::Stopped
    ) {
        return true;
    }
    if !active.try_start_running() {
        return true;
    }

    *CURRENT_TASK.lock() = Some(active.clone());

    let tf_opt = active.trap_frame.lock().take();
    if let Some(mut ctx) = tf_opt {
        run_current_user_task_one_boundary(&active, &mut ctx);
        if active.status() != TaskStatus::Zombie {
            *active.trap_frame.lock() = Some(ctx);
        }

        *CURRENT_TASK.lock() = None;
        active.release_running_cpu();
        requeue_after_user_run(active);
        true
    } else {
        log::error!("[task] ready user task {} missing trap frame", active.pid.0);
        *CURRENT_TASK.lock() = None;
        active.release_running_cpu();
        true
    }
}

pub(crate) fn drain_kernel_ready_once() -> bool {
    if crate::trap::foreground_driver_active() {
        return false;
    }
    let Some(active) = manager::fetch_kernel_task() else {
        return false;
    };
    if matches!(
        active.status(),
        TaskStatus::Zombie | TaskStatus::Blocked | TaskStatus::Stopped
    ) {
        return true;
    }
    if !active.try_start_running() {
        return true;
    }

    *CURRENT_TASK.lock() = Some(active.clone());
    switch_to_kernel_task(&active);
    finish_kernel_task_switch(active);
    true
}

fn finish_kernel_task_switch(task: Arc<TaskControlBlock>) {
    // The scheduler is executing again, so the old CPU no longer touches the
    // task's kernel stack.  Release ownership before publishing a yielded or
    // concurrently-woken task to the global queue.
    *CURRENT_TASK.lock() = None;
    task.release_running_cpu();
    task.blocking_cpu.store(NO_CPU, Ordering::Release);
    fence(Ordering::SeqCst);
    if task.status() == TaskStatus::Ready {
        manager::add_task(task);
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
    let owner = task.blocking_cpu.load(Ordering::Acquire);
    if owner == NO_CPU {
        manager::add_task(task.clone());
    } else {
        crate::platform::notify_cpu(owner);
    }
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
    let mut status = task.status.lock();
    if *status != TaskStatus::Blocked {
        let transitioning = task.block_reason.lock().is_some();
        if transitioning {
            *task.wait_outcome.lock() = Some(outcome);
        }
        return transitioning;
    }
    *status = TaskStatus::Ready;
    *task.block_reason.lock() = None;
    *task.wait_outcome.lock() = Some(outcome);
    drop(status);
    let owner = task.blocking_cpu.load(Ordering::Acquire);
    if owner == NO_CPU {
        manager::add_task(task.clone());
    } else {
        crate::platform::notify_cpu(owner);
    }
    true
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
    pub start_time_us: usize,
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
    pub credentials: Mutex<Credentials>,
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
    /// Linux scheduling policy requested by sched_setscheduler(2).
    pub sched_policy: AtomicUsize,
    /// Static scheduling priority requested by sched_setscheduler/setparam.
    pub sched_priority: AtomicUsize,
    /// Linux-visible logical CPU affinity mask.
    pub affinity_mask: AtomicUsize,
    /// CPU synchronously waiting to resume this blocked syscall, or NO_CPU.
    pub blocking_cpu: AtomicUsize,
    /// CPU that currently owns this task's user context, or NO_CPU.
    pub running_cpu: AtomicUsize,
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
    pub has_execed: bool,
    pub pgid: usize,
    pub sid: usize,
    pub program_break: usize,
    pub mapped_break: usize,
    pub next_mmap: usize,
    pub rlimit_nofile: usize,
    pub rlimit_nofile_max: usize,
    pub rlimit_fsize: usize,
    pub rlimit_fsize_max: usize,
    pub rlimit_core: usize,
    pub rlimit_core_max: usize,
    pub personality: usize,
    pub clear_child_tid: usize,
    pub robust_list_head: usize,
    pub robust_list_len: usize,
    pub interval_timers: [crate::syscall::other::IntervalTimer; 3],
    pub default_timer_slack_ns: usize,
    pub current_timer_slack_ns: usize,
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
    Stopped,
}

impl TaskControlBlock {
    pub fn effective_sched_priority(&self) -> usize {
        match self.sched_policy.load(Ordering::Relaxed) {
            SCHED_FIFO | SCHED_RR => self.sched_priority.load(Ordering::Relaxed),
            _ => 0,
        }
    }

    pub fn set_sched_params(&self, policy: usize, priority: usize) {
        self.sched_priority.store(priority, Ordering::Relaxed);
        self.sched_policy.store(policy, Ordering::Relaxed);
    }

    pub fn affinity_mask(&self) -> usize {
        self.affinity_mask.load(Ordering::Acquire) & crate::platform::online_cpu_mask()
    }

    pub fn set_affinity_mask(&self, mask: usize) {
        self.affinity_mask.store(mask, Ordering::Release);
    }

    pub fn can_run_on_cpu(&self, cpu: usize) -> bool {
        self.affinity_mask() & (1usize << cpu) != 0
    }

    pub(crate) fn release_running_cpu(&self) {
        let cpu = crate::platform::current_cpu_index();
        if self.running_cpu.load(Ordering::Acquire) == NO_CPU {
            return;
        }
        if self
            .running_cpu
            .compare_exchange(cpu, NO_CPU, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            log::error!(
                "[task] pid={} released by cpu={} with owner={}",
                self.pid.0,
                cpu,
                self.running_cpu.load(Ordering::Acquire)
            );
        }
    }
}
