use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicUsize, Ordering};
use spin::Mutex;

use super::{
    new_shared_fd_table, new_shared_fs_context, new_shared_memory_set, new_shared_mm_context,
    KernelCtx, TaskControlBlock, TaskControlBlockInner, TaskStatus, ThreadGroup, UserProgramSpec,
};
use crate::mm::memory_set::MemorySet;
use crate::task::context::TaskContext;
use crate::task::pid::Pid;
use crate::utils::error::SysErrNo;

const DEFAULT_TIMER_SLACK_NS: usize = 50_000;

#[cfg(target_arch = "riscv64")]
fn init_user_trapframe(tf: &mut polyhal_trap::trapframe::TrapFrame) {
    crate::trap::prepare_user_trapframe(tf);
}

#[cfg(not(target_arch = "riscv64"))]
fn init_user_trapframe(tf: &mut polyhal_trap::trapframe::TrapFrame) {
    crate::trap::prepare_user_trapframe(tf);
}

fn resolve_program_path(root: &str, path: &str) -> String {
    crate::fs::resolve_path_with_root(root, "/", path)
}

/// 任务控制块实现
impl TaskControlBlock {
    /// 创建一个新的用户任务控制块
    ///
    /// 用于从 ELF 加载用户程序（init 进程或 harness 启动的测试 ELF）
    pub fn new_user(elf_data: &[u8]) -> Result<Arc<Self>, SysErrNo> {
        use crate::mm::elf_loader::{ElfFile, PT_LOAD, PT_PHDR};
        use polyhal_trap::trapframe::{TrapFrame, TrapFrameArgs};

        let elf = ElfFile::parse(elf_data)?;

        let phdr_vaddr = elf
            .program_headers
            .iter()
            .find(|ph| ph.p_type == PT_PHDR)
            .map(|ph| ph.p_vaddr)
            .unwrap_or_else(|| {
                elf.program_headers
                    .iter()
                    .filter(|ph| ph.p_type == PT_LOAD)
                    .map(|ph| ph.p_vaddr)
                    .min()
                    .unwrap_or(0)
                    + elf.header.e_phoff
            });
        let phnum = elf.header.e_phnum as usize;

        let (mut memory_set, user_stack_top, entry) = elf.load()?;

        // 在用户栈上构造最小的 argc/argv/auxv
        let sp = crate::syscall::process::setup_user_stack_for_init(
            &memory_set,
            user_stack_top,
            entry,
            phdr_vaddr,
            phnum,
        );

        let mut trap_frame = TrapFrame::new();
        init_user_trapframe(&mut trap_frame);
        trap_frame[TrapFrameArgs::SP] = sp;
        trap_frame[TrapFrameArgs::SEPC] = entry;
        crate::syscall::process::set_user_entry_registers(&mut trap_frame, sp, 1);
        crate::syscall::signal::install_signal_trampoline(&mut memory_set)?;

        let pid = Pid::alloc();
        let thread_group = ThreadGroup::new(pid.0);
        let pgid = pid.0;
        let task = Arc::new(Self {
            pid,
            thread_group: thread_group.clone(),
            start_time_us: crate::timer::get_time_us(),
            is_kernel: false,
            inner: Mutex::new(TaskControlBlockInner {
                exit_code: 0,
                clone_flags: 0,
                parent: None,
                children: Vec::new(),
                fd_table: new_shared_fd_table(),
                cwd: String::from("/"),
                root: String::from("/"),
                exec_path: String::from("/init"),
                has_execed: false,
                pgid,
                sid: pgid,
                program_break: crate::config::USER_HEAP_START,
                mapped_break: crate::config::USER_HEAP_START,
                next_mmap: 0x4000_0000,
                rlimit_nofile: crate::fs::fd::MAX_FD_NUM,
                rlimit_nofile_max: crate::fs::fd::MAX_FD_NUM,
                rlimit_fsize: usize::MAX,
                rlimit_fsize_max: usize::MAX,
                rlimit_core: 0,
                rlimit_core_max: 0,
                personality: 0,
                clear_child_tid: 0,
                robust_list_head: 0,
                robust_list_len: 0,
                interval_timers: crate::syscall::other::EMPTY_INTERVAL_TIMERS,
                default_timer_slack_ns: DEFAULT_TIMER_SLACK_NS,
                current_timer_slack_ns: DEFAULT_TIMER_SLACK_NS,
            }),
            task_ctx: KernelCtx::new(TaskContext::zero_init()),
            memory_set: new_shared_memory_set(memory_set),
            fs: new_shared_fs_context(String::from("/"), String::from("/")),
            mm: new_shared_mm_context(
                crate::config::USER_HEAP_START,
                crate::config::USER_HEAP_START,
                0x4000_0000,
            ),
            credentials: Mutex::new(crate::task::Credentials::root()),
            signal_actions: crate::syscall::signal::new_shared_signal_actions(),
            signal_state: Mutex::new(crate::syscall::signal::SignalState::new()),
            trap_frame: Mutex::new(Some(trap_frame)),
            status: Mutex::new(TaskStatus::Ready),
            block_reason: Mutex::new(None),
            wait_outcome: Mutex::new(None),
            wait_token: AtomicUsize::new(0),
            sched_policy: AtomicUsize::new(crate::task::SCHED_OTHER),
            sched_priority: AtomicUsize::new(0),
        });
        crate::task::manager::register_task(&task);
        thread_group.add_member(&task);

        log::info!(
            "[task] Created user task pid={} entry={:#x} sp={:#x}",
            task.pid.0,
            entry,
            sp
        );
        Ok(task)
    }

    /// Create a new user task with full control over argv, envp, cwd, and marker name.
    ///
    /// This is the primary constructor for launching test binaries with the correct
    /// working directory and environment variables. The ELF is loaded from `spec.path`.
    pub fn new_user_with_args_env_cwd(spec: &UserProgramSpec) -> Result<Arc<Self>, SysErrNo> {
        use crate::mm::elf_loader::ElfFile;
        use polyhal_trap::trapframe::{TrapFrame, TrapFrameArgs};

        let target_path = resolve_program_path(&spec.root, &spec.path);
        let target_elf_data = crate::fs::read_executable_file(&target_path).ok_or_else(|| {
            log::error!("[task] new_user_with_args: ELF not found: {}", target_path);
            SysErrNo::ENOENT
        })?;
        let target_elf = ElfFile::parse(&target_elf_data)?;

        let launch_path = spec.path.clone();
        let mut launch_argv = spec.argv.clone();
        let launch_elf_data;
        let mut interp_path_opt = target_elf.interp_path();
        #[cfg(target_arch = "loongarch64")]
        if target_elf.can_enter_without_interpreter() {
            log::info!(
                "[task] self-contained PIE detected, entering target directly: {}",
                spec.path
            );
            interp_path_opt = None;
        }
        let (mut memory_set, user_stack_top, entry, phdr_vaddr, phnum, interp_base) =
            if let Some(interp) = interp_path_opt {
                let (interp_path, interp_host_path, interp_data) =
                    crate::fs::read_interpreter(&spec.root, interp).ok_or_else(|| {
                        log::error!(
                            "[task] new_user_with_args: interpreter not found: {} (host {})",
                            crate::fs::normalize_path(interp),
                            resolve_program_path(&spec.root, interp)
                        );
                        SysErrNo::ENOENT
                    })?;
                launch_elf_data = interp_data;
                log::info!(
                    "[task] dynamic ELF detected: target={} interp={} host={} root={}",
                    spec.path,
                    interp_path,
                    interp_host_path,
                    spec.root
                );
                let interp_elf = ElfFile::parse(&launch_elf_data)?;
                let target_bias = if target_elf.header.e_type == 3 {
                    0x0040_0000
                } else {
                    0
                };
                let interp_bias =
                    ElfFile::choose_interpreter_bias(&target_elf, target_bias, &interp_elf)?;
                // Consume auxv-visible ELF metadata before loading segments so
                // later copy/zero-fill work cannot perturb the launch contract.
                let interp_entry = interp_elf.entry_with_bias(interp_bias);
                let target_phdr_vaddr = target_elf.phdr_vaddr(target_bias);
                let target_phnum = target_elf.phnum();
                let mut memory_set = MemorySet::from_kernel();
                target_elf.load_segments_into(&mut memory_set, target_bias)?;
                interp_elf.load_segments_into(&mut memory_set, interp_bias)?;

                let user_stack_top = crate::config::USER_STACK_TOP;
                let user_stack_bottom = user_stack_top - crate::config::USER_STACK_SIZE;
                memory_set.insert_framed_area(
                    polyhal::VirtAddr::new(user_stack_bottom),
                    polyhal::VirtAddr::new(user_stack_top),
                    crate::mm::page_table::PTEFlags::U
                        | crate::mm::page_table::PTEFlags::R
                        | crate::mm::page_table::PTEFlags::W
                        | crate::mm::page_table::PTEFlags::V,
                )?;

                (
                    memory_set,
                    user_stack_top,
                    interp_entry,
                    target_phdr_vaddr,
                    target_phnum,
                    interp_bias,
                )
            } else {
                let elf = ElfFile::parse(&target_elf_data)?;
                let phdr_vaddr = elf.phdr_vaddr(0);
                let phnum = elf.phnum();
                if elf.header.e_type == 3 {
                    let bias = 0x0040_0000usize;
                    let (memory_set, user_stack_top, entry) = elf.load_at(bias)?;
                    (
                        memory_set,
                        user_stack_top,
                        entry,
                        elf.phdr_vaddr(bias),
                        phnum,
                        0,
                    )
                } else {
                    let (memory_set, user_stack_top, entry) = elf.load()?;
                    (memory_set, user_stack_top, entry, phdr_vaddr, phnum, 0)
                }
            };

        let at_entry = if interp_base != 0 {
            let target_bias = if target_elf.header.e_type == 3 {
                0x0040_0000
            } else {
                0
            };
            target_elf.entry_with_bias(target_bias)
        } else {
            entry
        };

        // 2. Setup user stack with spec's argv/envp
        let sp = crate::syscall::process::setup_user_stack(
            &memory_set,
            user_stack_top,
            &launch_argv,
            &spec.envp,
            at_entry,
            phdr_vaddr,
            phnum,
            interp_base,
        );
        crate::syscall::signal::install_signal_trampoline(&mut memory_set)?;

        let mut trap_frame = TrapFrame::new();
        init_user_trapframe(&mut trap_frame);
        trap_frame[TrapFrameArgs::SP] = sp;
        trap_frame[TrapFrameArgs::SEPC] = entry;
        crate::syscall::process::set_user_entry_registers(&mut trap_frame, sp, launch_argv.len());

        // Set parent to orphan reaper so wait4 can find children.
        // When exit_current_and_run_next is called, it re-parents children to the
        // orphan reaper, but we need a valid parent for wait4 to work.
        let fg_parent = crate::task::orphan_reaper();

        let pid = Pid::alloc();
        let thread_group = ThreadGroup::new(pid.0);
        let pgid = pid.0;
        let task = Arc::new(Self {
            pid,
            thread_group: thread_group.clone(),
            start_time_us: crate::timer::get_time_us(),
            is_kernel: false,
            inner: Mutex::new(TaskControlBlockInner {
                exit_code: 0,
                clone_flags: 0,
                parent: fg_parent.clone(),
                children: Vec::new(),
                fd_table: new_shared_fd_table(),
                cwd: spec.cwd.clone(),
                root: spec.root.clone(),
                exec_path: spec.path.clone(),
                has_execed: false,
                pgid,
                sid: pgid,
                program_break: crate::config::USER_HEAP_START,
                mapped_break: crate::config::USER_HEAP_START,
                next_mmap: 0x4000_0000,
                rlimit_nofile: crate::fs::fd::MAX_FD_NUM,
                rlimit_nofile_max: crate::fs::fd::MAX_FD_NUM,
                rlimit_fsize: usize::MAX,
                rlimit_fsize_max: usize::MAX,
                rlimit_core: 0,
                rlimit_core_max: 0,
                personality: 0,
                clear_child_tid: 0,
                robust_list_head: 0,
                robust_list_len: 0,
                interval_timers: crate::syscall::other::EMPTY_INTERVAL_TIMERS,
                default_timer_slack_ns: DEFAULT_TIMER_SLACK_NS,
                current_timer_slack_ns: DEFAULT_TIMER_SLACK_NS,
            }),
            task_ctx: KernelCtx::new(TaskContext::zero_init()),
            memory_set: new_shared_memory_set(memory_set),
            fs: new_shared_fs_context(spec.cwd.clone(), spec.root.clone()),
            mm: new_shared_mm_context(
                crate::config::USER_HEAP_START,
                crate::config::USER_HEAP_START,
                0x4000_0000,
            ),
            credentials: Mutex::new(crate::task::Credentials::root()),
            signal_actions: crate::syscall::signal::new_shared_signal_actions(),
            signal_state: Mutex::new(crate::syscall::signal::SignalState::new()),
            trap_frame: Mutex::new(Some(trap_frame)),
            status: Mutex::new(TaskStatus::Ready),
            block_reason: Mutex::new(None),
            wait_outcome: Mutex::new(None),
            wait_token: AtomicUsize::new(0),
            sched_policy: AtomicUsize::new(crate::task::SCHED_OTHER),
            sched_priority: AtomicUsize::new(0),
        });
        crate::task::manager::register_task(&task);
        thread_group.add_member(&task);

        log::info!(
            "[task] new_user_with_args: pid={} path={} cwd={} argc={}",
            task.pid.0,
            launch_path,
            spec.cwd,
            launch_argv.len()
        );
        Ok(task)
    }

    /// 创建一个新的任务控制块
    ///
    /// 用于创建内核任务或初始化进程
    pub fn new(elf_data: &[u8]) -> Arc<Self> {
        let memory_set = MemorySet::new_bare();

        let pid = Pid::alloc();
        let thread_group = ThreadGroup::new(pid.0);
        let pgid = pid.0;
        let task = Arc::new(Self {
            pid,
            thread_group: thread_group.clone(),
            start_time_us: crate::timer::get_time_us(),
            is_kernel: false,
            inner: Mutex::new(TaskControlBlockInner {
                exit_code: 0,
                clone_flags: 0,
                parent: None,
                children: Vec::new(),
                fd_table: new_shared_fd_table(),
                cwd: String::from("/"),
                root: String::from("/"),
                exec_path: String::new(),
                has_execed: false,
                pgid,
                sid: pgid,
                program_break: crate::config::USER_HEAP_START,
                mapped_break: crate::config::USER_HEAP_START,
                next_mmap: 0x4000_0000,
                rlimit_nofile: crate::fs::fd::MAX_FD_NUM,
                rlimit_nofile_max: crate::fs::fd::MAX_FD_NUM,
                rlimit_fsize: usize::MAX,
                rlimit_fsize_max: usize::MAX,
                rlimit_core: 0,
                rlimit_core_max: 0,
                personality: 0,
                clear_child_tid: 0,
                robust_list_head: 0,
                robust_list_len: 0,
                interval_timers: crate::syscall::other::EMPTY_INTERVAL_TIMERS,
                default_timer_slack_ns: DEFAULT_TIMER_SLACK_NS,
                current_timer_slack_ns: DEFAULT_TIMER_SLACK_NS,
            }),
            task_ctx: KernelCtx::new(TaskContext::zero_init()),
            memory_set: new_shared_memory_set(memory_set),
            fs: new_shared_fs_context(String::from("/"), String::from("/")),
            mm: new_shared_mm_context(
                crate::config::USER_HEAP_START,
                crate::config::USER_HEAP_START,
                0x4000_0000,
            ),
            credentials: Mutex::new(crate::task::Credentials::root()),
            signal_actions: crate::syscall::signal::new_shared_signal_actions(),
            signal_state: Mutex::new(crate::syscall::signal::SignalState::new()),
            trap_frame: Mutex::new(None),
            status: Mutex::new(TaskStatus::Ready),
            block_reason: Mutex::new(None),
            wait_outcome: Mutex::new(None),
            wait_token: AtomicUsize::new(0),
            sched_policy: AtomicUsize::new(crate::task::SCHED_OTHER),
            sched_priority: AtomicUsize::new(0),
        });
        crate::task::manager::register_task(&task);
        thread_group.add_member(&task);
        task
    }

    /// 创建一个新的内核任务
    ///
    /// 用于创建内核线程
    pub fn new_kernel_task(entry: fn() -> !) -> Arc<Self> {
        let memory_set = MemorySet::new_bare();
        let mut task_ctx_val = TaskContext::zero_init();

        // 分配内核栈
        let kernel_stack = alloc_kernel_stack();
        task_ctx_val.set_sp(kernel_stack);
        task_ctx_val.set_ra(entry as usize);

        let pid = Pid::alloc();
        let thread_group = ThreadGroup::new(pid.0);
        let pgid = pid.0;
        let task = Arc::new(Self {
            pid,
            thread_group: thread_group.clone(),
            start_time_us: crate::timer::get_time_us(),
            is_kernel: true,
            inner: Mutex::new(TaskControlBlockInner {
                exit_code: 0,
                clone_flags: 0,
                parent: None,
                children: Vec::new(),
                fd_table: new_shared_fd_table(),
                cwd: String::from("/"),
                root: String::from("/"),
                exec_path: String::new(),
                has_execed: false,
                pgid,
                sid: pgid,
                program_break: crate::config::USER_HEAP_START,
                mapped_break: crate::config::USER_HEAP_START,
                next_mmap: 0x4000_0000,
                rlimit_nofile: crate::fs::fd::MAX_FD_NUM,
                rlimit_nofile_max: crate::fs::fd::MAX_FD_NUM,
                rlimit_fsize: usize::MAX,
                rlimit_fsize_max: usize::MAX,
                rlimit_core: 0,
                rlimit_core_max: 0,
                personality: 0,
                clear_child_tid: 0,
                robust_list_head: 0,
                robust_list_len: 0,
                interval_timers: crate::syscall::other::EMPTY_INTERVAL_TIMERS,
                default_timer_slack_ns: DEFAULT_TIMER_SLACK_NS,
                current_timer_slack_ns: DEFAULT_TIMER_SLACK_NS,
            }),
            task_ctx: KernelCtx::new(task_ctx_val),
            memory_set: new_shared_memory_set(memory_set),
            fs: new_shared_fs_context(String::from("/"), String::from("/")),
            mm: new_shared_mm_context(
                crate::config::USER_HEAP_START,
                crate::config::USER_HEAP_START,
                0x4000_0000,
            ),
            credentials: Mutex::new(crate::task::Credentials::root()),
            signal_actions: crate::syscall::signal::new_shared_signal_actions(),
            signal_state: Mutex::new(crate::syscall::signal::SignalState::new()),
            trap_frame: Mutex::new(None),
            status: Mutex::new(TaskStatus::Ready),
            block_reason: Mutex::new(None),
            wait_outcome: Mutex::new(None),
            wait_token: AtomicUsize::new(0),
            sched_policy: AtomicUsize::new(crate::task::SCHED_OTHER),
            sched_priority: AtomicUsize::new(0),
        });
        crate::task::manager::register_task(&task);
        thread_group.add_member(&task);
        task
    }

    /// 获取任务状态
    pub fn status(&self) -> TaskStatus {
        *self.status.lock()
    }

    /// 设置任务状态
    pub fn set_status(&self, status: TaskStatus) {
        *self.status.lock() = status;
    }

    pub fn next_wait_token(&self) -> usize {
        self.wait_token
            .fetch_add(1, Ordering::SeqCst)
            .wrapping_add(1)
    }

    pub fn current_wait_token(&self) -> usize {
        self.wait_token.load(Ordering::SeqCst)
    }

    /// 获取任务上下文
    pub fn task_ctx(&self) -> super::context::TaskContext {
        unsafe { (*self.task_ctx.ctx.get()).clone() }
    }

    /// 设置任务上下文
    pub fn set_task_ctx(&self, ctx: super::context::TaskContext) {
        unsafe {
            *self.task_ctx.ctx.get() = ctx;
        }
    }

    /// 获取退出码
    pub fn exit_code(&self) -> i32 {
        self.inner.lock().exit_code
    }

    /// 设置退出码
    pub fn set_exit_code(&self, code: i32) {
        self.inner.lock().exit_code = code;
    }

    /// Take the trap frame out (for foreground driver to avoid holding lock across run_user_task).
    /// Panics if there is no trap frame — caller must check has_tf first.
    pub fn take_trap_frame(&self) -> polyhal_trap::trapframe::TrapFrame {
        self.trap_frame
            .lock()
            .take()
            .expect("no trap_frame in foreground driver")
    }

    /// Put a trap frame back (for foreground driver).
    pub fn put_trap_frame(&self, ctx: polyhal_trap::trapframe::TrapFrame) {
        *self.trap_frame.lock() = Some(ctx);
    }

    /// Check if trap frame is present (use before take_trap_frame).
    pub fn has_trap_frame(&self) -> bool {
        self.trap_frame.lock().is_some()
    }

    /// Returns a raw pointer to task_ctx (stored in TaskControlBlock, not inner).
    pub fn task_ctx_ptr(&self) -> *mut super::context::TaskContext {
        self.task_ctx.ctx.get()
    }
}

/// 分配内核栈
///
/// 为任务分配一个内核栈
/// 返回栈顶地址
fn alloc_kernel_stack() -> usize {
    use crate::config::PAGE_SIZE;
    use crate::mm::frame_allocator;

    let pages = 16usize;
    let Some(base_ppn) = frame_allocator::alloc_contiguous_frames(pages) else {
        log::error!("[task] alloc_kernel_stack: no contiguous frames");
        return 0;
    };
    stack_top_from_contiguous_base(base_ppn * PAGE_SIZE, pages)
}

#[inline]
fn stack_top_from_contiguous_base(base_phys: usize, pages: usize) -> usize {
    base_phys + pages * crate::config::PAGE_SIZE
}
