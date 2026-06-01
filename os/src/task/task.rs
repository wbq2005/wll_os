use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use spin::Mutex;

use super::{
    new_shared_fd_table, new_shared_memory_set, KernelCtx, TaskControlBlock, TaskControlBlockInner,
    TaskStatus, UserProgramSpec,
};
use crate::mm::memory_set::MemorySet;
use crate::task::context::TaskContext;
use crate::task::pid::Pid;
use crate::utils::error::SysErrNo;

#[cfg(target_arch = "riscv64")]
fn init_user_trapframe(tf: &mut polyhal_trap::trapframe::TrapFrame) {
    let bits = unsafe { core::mem::transmute::<_, usize>(tf.sstatus) };
    let bits = (bits & !(1 << 8)) | (1 << 5);
    tf.sstatus = unsafe { core::mem::transmute(bits) };
    debug_assert_eq!(bits & (1 << 8), 0);
}

#[cfg(not(target_arch = "riscv64"))]
fn init_user_trapframe(_tf: &mut polyhal_trap::trapframe::TrapFrame) {}

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

        let (memory_set, user_stack_top, entry) = elf.load()?;

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

        let task = Arc::new(Self {
            pid: Pid::alloc(),
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
                program_break: crate::config::USER_HEAP_START,
                mapped_break: crate::config::USER_HEAP_START,
                next_mmap: 0x4000_0000,
                clear_child_tid: 0,
            }),
            task_ctx: KernelCtx::new(TaskContext::zero_init()),
            memory_set: new_shared_memory_set(memory_set),
            trap_frame: Mutex::new(Some(trap_frame)),
            status: Mutex::new(TaskStatus::Ready),
        });

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
        let (memory_set, user_stack_top, entry, phdr_vaddr, phnum, interp_base) =
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
                let interp_bias = 0x0010_0000usize;
                let target_bias = if target_elf.header.e_type == 3 {
                    0x0040_0000
                } else {
                    0
                };
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

        let mut trap_frame = TrapFrame::new();
        init_user_trapframe(&mut trap_frame);
        trap_frame[TrapFrameArgs::SP] = sp;
        trap_frame[TrapFrameArgs::SEPC] = entry;
        crate::syscall::process::set_user_entry_registers(&mut trap_frame, sp, launch_argv.len());

        // Set parent to orphan reaper so wait4 can find children.
        // When exit_current_and_run_next is called, it re-parents children to the
        // orphan reaper, but we need a valid parent for wait4 to work.
        let fg_parent = crate::task::orphan_reaper();

        let task = Arc::new(Self {
            pid: Pid::alloc(),
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
                program_break: crate::config::USER_HEAP_START,
                mapped_break: crate::config::USER_HEAP_START,
                next_mmap: 0x4000_0000,
                clear_child_tid: 0,
            }),
            task_ctx: KernelCtx::new(TaskContext::zero_init()),
            memory_set: new_shared_memory_set(memory_set),
            trap_frame: Mutex::new(Some(trap_frame)),
            status: Mutex::new(TaskStatus::Ready),
        });

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

        Arc::new(Self {
            pid: Pid::alloc(),
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
                program_break: crate::config::USER_HEAP_START,
                mapped_break: crate::config::USER_HEAP_START,
                next_mmap: 0x4000_0000,
                clear_child_tid: 0,
            }),
            task_ctx: KernelCtx::new(TaskContext::zero_init()),
            memory_set: new_shared_memory_set(memory_set),
            trap_frame: Mutex::new(None),
            status: Mutex::new(TaskStatus::Ready),
        })
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

        Arc::new(Self {
            pid: Pid::alloc(),
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
                program_break: crate::config::USER_HEAP_START,
                mapped_break: crate::config::USER_HEAP_START,
                next_mmap: 0x4000_0000,
                clear_child_tid: 0,
            }),
            task_ctx: KernelCtx::new(task_ctx_val),
            memory_set: new_shared_memory_set(memory_set),
            trap_frame: Mutex::new(None),
            status: Mutex::new(TaskStatus::Ready),
        })
    }

    /// 获取任务状态
    pub fn status(&self) -> TaskStatus {
        *self.status.lock()
    }

    /// 设置任务状态
    pub fn set_status(&self, status: TaskStatus) {
        *self.status.lock() = status;
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
