use alloc::sync::Arc;
use alloc::string::String;
use alloc::vec::Vec;
use spin::Mutex;

use super::{
    TaskControlBlock, TaskControlBlockInner, TaskStatus, new_shared_fd_table,
    new_shared_memory_set,
};
use crate::console::putchar;
use crate::mm::memory_set::MemorySet;
use crate::task::pid::Pid;
use crate::task::context::TaskContext;
use crate::utils::error::SysErrNo;

/// 任务控制块实现
impl TaskControlBlock {
    /// 创建一个新的用户任务控制块
    ///
    /// 用于从 ELF 加载用户程序（init 进程或 harness 启动的测试 ELF）
    pub fn new_user(elf_data: &[u8]) -> Result<Arc<Self>, SysErrNo> {
        use crate::mm::elf_loader::{ElfFile, PT_PHDR, PT_LOAD};
        use polyhal::VirtAddr;
        use polyhal_trap::trapframe::{TrapFrame, TrapFrameArgs};

        let elf = ElfFile::parse(elf_data)?;

        let phdr_vaddr = elf.program_headers.iter()
            .find(|ph| ph.p_type == PT_PHDR)
            .map(|ph| ph.p_vaddr)
            .unwrap_or_else(|| {
                elf.program_headers.iter()
                    .filter(|ph| ph.p_type == PT_LOAD)
                    .map(|ph| ph.p_vaddr)
                    .min()
                    .unwrap_or(0)
                    + elf.header.e_phoff
            });
        let phnum = elf.header.e_phnum as usize;

        let (memory_set, user_stack_top, entry) = elf.load()?;

        // UART marker: 'G' = elf.load() returned
        #[cfg(target_arch = "riscv64")]
        unsafe { core::arch::asm!("li t0, 0x10000000; li t1, 0x47; sb t1, 0(t0)"); }

        // 在用户栈上构造最小的 argc/argv/auxv
        let sp = crate::syscall::process::setup_user_stack_for_init(
            &memory_set,
            user_stack_top,
            entry,
            phdr_vaddr,
            phnum,
        );

        let mut trap_frame = TrapFrame::new();
        trap_frame[TrapFrameArgs::SP] = sp;
        trap_frame[TrapFrameArgs::SEPC] = entry;
        trap_frame[TrapFrameArgs::RET] = 0;

        let task = Arc::new(Self {
            pid: Pid::alloc(),
            inner: Mutex::new(TaskControlBlockInner {
                status: TaskStatus::Ready,
                memory_set: new_shared_memory_set(memory_set),
                task_ctx: TaskContext::zero_init(),
                trap_frame: Some(trap_frame),
                exit_code: 0,
                clone_flags: 0,
                parent: None,
                children: Vec::new(),
                fd_table: new_shared_fd_table(),
                cwd: String::from("/"),
                program_break: crate::config::USER_HEAP_START,
                mapped_break: crate::config::USER_HEAP_START,
                next_mmap: 0x4000_0000,
            }),
        });

        // UART marker: '7' = task created
        #[cfg(target_arch = "riscv64")]
        unsafe { core::arch::asm!("li t0, 0x10000000; li t1, 0x37; sb t1, 0(t0)") }

        log::info!("[task] Created user task pid={} entry={:#x} sp={:#x}", task.pid.0, entry, sp);
        // UART marker: '8' = log printed
        #[cfg(target_arch = "riscv64")]
        unsafe { core::arch::asm!("li t0, 0x10000000; li t1, 0x38; sb t1, 0(t0)") }
        Ok(task)
    }

    /// 创建一个新的任务控制块
    ///
    /// 用于创建内核任务或初始化进程
    pub fn new(elf_data: &[u8]) -> Arc<Self> {
        let memory_set = MemorySet::new_bare();

        Arc::new(Self {
            pid: Pid::alloc(),
            inner: Mutex::new(TaskControlBlockInner {
                status: TaskStatus::Ready,
                memory_set: new_shared_memory_set(memory_set),
                task_ctx: TaskContext::zero_init(),
                trap_frame: None,
                exit_code: 0,
                clone_flags: 0,
                parent: None,
                children: Vec::new(),
                fd_table: new_shared_fd_table(),
                cwd: String::from("/"),
                program_break: crate::config::USER_HEAP_START,
                mapped_break: crate::config::USER_HEAP_START,
                next_mmap: 0x4000_0000,
            }),
        })
    }

    /// 创建一个新的内核任务
    /// 
    /// 用于创建内核线程
    pub fn new_kernel_task(entry: fn() -> !) -> Arc<Self> {
        let memory_set = MemorySet::new_bare();
        let mut task_ctx = TaskContext::zero_init();
        
        // 分配内核栈
        let kernel_stack = alloc_kernel_stack();
        task_ctx.set_sp(kernel_stack);
        task_ctx.set_ra(entry as usize);
        
        Arc::new(Self {
            pid: Pid::alloc(),
            inner: Mutex::new(TaskControlBlockInner {
                status: TaskStatus::Ready,
                memory_set: new_shared_memory_set(memory_set),
                task_ctx,
                trap_frame: None,
                exit_code: 0,
                clone_flags: 0,
                parent: None,
                children: Vec::new(),
                fd_table: new_shared_fd_table(),
                cwd: String::from("/"),
                program_break: crate::config::USER_HEAP_START,
                mapped_break: crate::config::USER_HEAP_START,
                next_mmap: 0x4000_0000,
            }),
        })
    }

    /// 获取任务状态
    pub fn status(&self) -> TaskStatus {
        self.inner.lock().status
    }

    /// 设置任务状态
    pub fn set_status(&self, status: TaskStatus) {
        self.inner.lock().status = status;
    }

    /// 获取任务上下文
    pub fn task_ctx(&self) -> TaskContext {
        self.inner.lock().task_ctx.clone()
    }

    /// 设置任务上下文
    pub fn set_task_ctx(&self, ctx: TaskContext) {
        self.inner.lock().task_ctx = ctx;
    }

    /// 获取退出码
    pub fn exit_code(&self) -> i32 {
        self.inner.lock().exit_code
    }

    /// 设置退出码
    pub fn set_exit_code(&self, code: i32) {
        self.inner.lock().exit_code = code;
    }
}

/// 分配内核栈
/// 
/// 为任务分配一个内核栈
/// 返回栈顶地址
fn alloc_kernel_stack() -> usize {
    use crate::config::PAGE_SIZE;
    use crate::mm::frame_allocator;

    let pages = 2usize;
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
