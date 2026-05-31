use super::SyscallRet;
use crate::fs::read_file;
use crate::mm::elf_loader::ElfFile;
use crate::task::{
    current_task, dup_fd_table, exit_current_and_run_next, new_shared_memory_set,
    suspend_current_and_run_next,
};
use crate::utils::error::SysErrNo;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use polyhal::VirtAddr;
use polyhal_trap::trapframe::{TrapFrame, TrapFrameArgs};
use spin::Mutex;

/// Linux clone 的低 8 位为发往父进程的信号 (`CSIGNAL`)。
const CSIGNAL: usize = 0xff;
/// fork/__clone 可忽略的附加位（不提供 pthread 共享语义）。
/// **不包含** `CLONE_VM` / `CLONE_FILES` / `CLONE_SIGHAND` / `CLONE_THREAD`：遇到即 `EINVAL`。
const ALLOWED_CLONE_FLAGS: usize = 0x00000200 /* CLONE_FS */
    | 0x00040000 /* CLONE_SYSVSEM */
    | 0x00080000 /* CLONE_SETTLS */
    | 0x00200000 /* CLONE_PARENT_SETTID */
    | 0x01000000; /* CLONE_CHILD_CLEARTID */

const CLONE_VM: usize = 0x00000100;

const CLONE_FILES: usize = 0x00000400;

const CLONE_SIGHAND: usize = 0x00000800;

const CLONE_THREAD: usize = 0x00010000;

const CLONE_SETTLS: usize = 0x00080000;

const THREAD_SHARING_FLAGS: usize = CLONE_VM | CLONE_FILES | CLONE_SIGHAND | CLONE_THREAD;

const WNOHANG: usize = 0x0000_0001;

fn read_user_usize(addr: usize) -> Result<usize, SysErrNo> {
    super::user::read_usize(addr)
}

fn write_user_i32(addr: usize, value: i32) -> Result<(), SysErrNo> {
    super::user::write_i32(addr, value)
}

fn read_user_str_array(base: usize) -> Result<Vec<String>, SysErrNo> {
    let mut result = Vec::new();
    if base == 0 {
        return Ok(result);
    }
    for i in 0..256 {
        let str_ptr = read_user_usize(base + i * core::mem::size_of::<usize>())?;
        if str_ptr == 0 {
            break;
        }
        let s = read_user_cstr(str_ptr as *const u8)?;
        if s.is_empty() {
            break;
        }
        result.push(s);
    }
    Ok(result)
}

fn reset_exec_trapframe(tf: &mut TrapFrame, entry: usize, sp: usize, argc: usize) {
    #[cfg(target_arch = "riscv64")]
    {
        tf.x = [0; 32];
        tf.fsx = [0; 2];
    }
    #[cfg(target_arch = "loongarch64")]
    {
        tf.regs = [0; 32];
    }

    tf[TrapFrameArgs::SP] = sp;
    tf[TrapFrameArgs::SEPC] = entry;
    tf[TrapFrameArgs::ARG0] = argc;
    tf[TrapFrameArgs::ARG1] = sp + core::mem::size_of::<usize>();
}

fn read_user_cstr(ptr: *const u8) -> Result<String, SysErrNo> {
    super::user::read_cstr_null_empty(ptr as usize)
}

/// 在用户栈上构造 argc/argv/envp/auxv 布局，返回新的栈顶。
/// 布局（从高到低）：
///   - 字符串数据（argv[i] 的 c-string 内容, envp[i] 的内容）
///   - 对齐填充
///   - auxv[]  (AT_NULL 结尾)
///   - NULL    (envp 终止)
///   - envp[0..n] 指针
///   - NULL    (argv 终止)
///   - argv[0..n] 指针
///   - argc    (usize)
///   ← SP 指向这里
pub(crate) fn setup_user_stack(
    memory_set: &crate::mm::memory_set::MemorySet,
    stack_top: usize,
    argv: &[String],
    envp: &[String],
    at_entry: usize,
    phdr_vaddr: usize,
    phnum: usize,
    interp_base: usize,
) -> usize {
    let mut sp = stack_top;

    // 辅助函数：往栈上写 bytes
    let write_bytes = |ms: &crate::mm::memory_set::MemorySet, sp: &mut usize, data: &[u8]| {
        *sp -= data.len();
        for (i, &b) in data.iter().enumerate() {
            let va = VirtAddr::new(*sp + i);
            if let Some(pa) = ms.translate(va) {
                unsafe {
                    *(pa.raw() as *mut u8) = b;
                }
            }
        }
    };

    let write_usize = |ms: &crate::mm::memory_set::MemorySet, sp: &mut usize, val: usize| {
        *sp -= core::mem::size_of::<usize>();
        let bytes = val.to_le_bytes();
        for (i, &b) in bytes.iter().enumerate() {
            let va = VirtAddr::new(*sp + i);
            if let Some(pa) = ms.translate(va) {
                unsafe {
                    *(pa.raw() as *mut u8) = b;
                }
            }
        }
    };

    // 1) 将所有 argv/envp 字符串写到栈顶区域，记录各自地址
    let mut argv_ptrs: Vec<usize> = Vec::new();
    for arg in argv.iter().rev() {
        write_bytes(memory_set, &mut sp, &[0u8]); // null terminator
        write_bytes(memory_set, &mut sp, arg.as_bytes());
        argv_ptrs.push(sp);
    }
    argv_ptrs.reverse();

    let mut envp_ptrs: Vec<usize> = Vec::new();
    for env in envp.iter().rev() {
        write_bytes(memory_set, &mut sp, &[0u8]);
        write_bytes(memory_set, &mut sp, env.as_bytes());
        envp_ptrs.push(sp);
    }
    envp_ptrs.reverse();

    // 随机数据（16 bytes for AT_RANDOM）
    sp -= 16;
    let random_addr = sp;
    for i in 0..16u8 {
        let va = VirtAddr::new(sp + i as usize);
        if let Some(pa) = memory_set.translate(va) {
            unsafe {
                *(pa.raw() as *mut u8) = i.wrapping_mul(37).wrapping_add(7);
            }
        }
    }

    // 2) 对齐到 16 字节
    sp &= !0xF;

    // 3) 计算总共需要多少 usize 条目来放 auxv/envp/argv/argc
    //    auxv: 若干 (key,val) 对 + (AT_NULL, 0)
    //    envp: envp_ptrs.len() + 1 (NULL)
    //    argv: argv_ptrs.len() + 1 (NULL)
    //    argc: 1
    let auxv_entries = 8; // AT_PHDR, AT_PHENT, AT_PHNUM, AT_PAGESZ, AT_BASE, AT_ENTRY, AT_RANDOM, AT_NULL
    let total_slots = 1 + (argv_ptrs.len() + 1) + (envp_ptrs.len() + 1) + auxv_entries * 2;
    // 确保 sp 在写完后 16 字节对齐
    sp -= total_slots * core::mem::size_of::<usize>();
    sp &= !0xF;

    let final_sp = sp;

    // 4) 从 sp 开始依次写 argc, argv[], NULL, envp[], NULL, auxv[]
    // argc
    write_usize(memory_set, &mut sp, 0); // placeholder, rewrite below
                                         // 因为 write_usize 减少 sp，我们改用直接偏移写法

    // 重新做：用绝对偏移写入
    let mut pos = final_sp;
    let write_at = |ms: &crate::mm::memory_set::MemorySet, pos: usize, val: usize| {
        let bytes = val.to_le_bytes();
        for (i, &b) in bytes.iter().enumerate() {
            let va = VirtAddr::new(pos + i);
            if let Some(pa) = ms.translate(va) {
                unsafe {
                    *(pa.raw() as *mut u8) = b;
                }
            }
        }
    };

    // 重置 sp 回 final_sp 再重新用绝对方式
    sp = final_sp; // 不再用 write_usize

    let sz = core::mem::size_of::<usize>();

    // argc
    write_at(memory_set, pos, argv_ptrs.len());
    pos += sz;

    // argv pointers
    for &ptr in &argv_ptrs {
        write_at(memory_set, pos, ptr);
        pos += sz;
    }
    write_at(memory_set, pos, 0); // argv NULL terminator
    pos += sz;

    // envp pointers
    for &ptr in &envp_ptrs {
        write_at(memory_set, pos, ptr);
        pos += sz;
    }
    write_at(memory_set, pos, 0); // envp NULL terminator
    pos += sz;

    // auxv
    const AT_NULL: usize = 0;
    const AT_PHDR: usize = 3;
    const AT_PHENT: usize = 4;
    const AT_PHNUM: usize = 5;
    const AT_PAGESZ: usize = 6;
    const AT_BASE: usize = 7;
    const AT_ENTRY: usize = 9;
    const AT_RANDOM: usize = 25;

    let auxv_pairs: [(usize, usize); 8] = [
        (AT_PHDR, phdr_vaddr),
        (AT_PHENT, 56), // sizeof(Elf64_Phdr)
        (AT_PHNUM, phnum),
        (AT_PAGESZ, crate::config::PAGE_SIZE),
        (AT_BASE, interp_base),
        (AT_ENTRY, at_entry),
        (AT_RANDOM, random_addr),
        (AT_NULL, 0),
    ];
    for (key, val) in auxv_pairs {
        write_at(memory_set, pos, key);
        pos += sz;
        write_at(memory_set, pos, val);
        pos += sz;
    }

    final_sp
}

/// execve 系统调用
///
/// 加载并执行新程序
pub fn sys_execve(path: *const u8, argv_ptr: usize, envp_ptr: usize) -> SyscallRet {
    let path_str = read_user_cstr(path)?;
    log::info!("[syscall] execve(path='{}')", path_str);

    // 在替换地址空间之前，从旧地址空间读取 argv/envp
    let argv = read_user_str_array(argv_ptr)?;
    let mut envp = read_user_str_array(envp_ptr)?;
    if envp.is_empty() {
        envp.push(String::from("PATH=/bin:/basic:/"));
        envp.push(String::from("LD_LIBRARY_PATH=/lib"));
    }

    let (root, cwd) = if let Some(task) = current_task() {
        let inner = task.inner.lock();
        (inner.root.clone(), inner.cwd.clone())
    } else {
        (String::from("/"), String::from("/"))
    };
    let logical_path = crate::fs::resolve_path(&cwd, &path_str);
    let host_path = crate::fs::apply_root(&root, &logical_path);

    let elf_data = match super::with_kernel_page_table(|| read_file(&host_path)) {
        Some(data) => data,
        None => {
            log::error!(
                "[syscall] execve: file not found: {} ({})",
                path_str,
                host_path
            );
            return Err(SysErrNo::ENOENT);
        }
    };

    let elf = match ElfFile::parse(&elf_data) {
        Ok(elf) => elf,
        Err(e) => {
            log::error!("[syscall] execve: failed to parse ELF: {:?}", e);
            return Err(e);
        }
    };

    // 从 ELF 头中获取 phdr 信息用于 auxv
    let mut launch_argv = if argv.is_empty() {
        alloc::vec![logical_path.clone()]
    } else {
        argv
    };

    let mut interp_path_opt = elf.interp_path();
    #[cfg(target_arch = "loongarch64")]
    if elf.can_enter_without_interpreter() {
        log::info!(
            "[syscall] execve: self-contained PIE detected, entering target directly: {}",
            path_str
        );
        interp_path_opt = None;
    }

    let (new_memory_set, user_stack_top, entry, phdr_vaddr, phnum, interp_base) =
        if let Some(interp) = interp_path_opt {
            let (interp_path, interp_host_path, interp_data) =
                super::with_kernel_page_table(|| crate::fs::read_interpreter(&root, interp))
                    .ok_or_else(|| {
                        log::error!(
                            "[syscall] execve: interpreter not found: {} ({})",
                            crate::fs::normalize_path(interp),
                            crate::fs::apply_root(&root, interp)
                        );
                        SysErrNo::ENOENT
                    })?;
            log::info!(
                "[syscall] execve: interpreter {} resolved to {}",
                interp_path,
                interp_host_path
            );
            let interp_elf = ElfFile::parse(&interp_data)?;
            let interp_bias = 0x0010_0000usize;
            let target_bias = if elf.header.e_type == 3 {
                0x0040_0000
            } else {
                0
            };
            let mut memory_set = crate::mm::memory_set::MemorySet::from_kernel();
            log::info!("[syscall] execve: loading target segments");
            elf.load_segments_into(&mut memory_set, target_bias)?;
            log::info!("[syscall] execve: loading interpreter segments");
            interp_elf.load_segments_into(&mut memory_set, interp_bias)?;
            log::info!("[syscall] execve: mapping user stack");

            let user_stack_top = crate::config::USER_STACK_TOP;
            let user_stack_bottom = user_stack_top - crate::config::USER_STACK_SIZE;
            memory_set.insert_framed_area(
                VirtAddr::new(user_stack_bottom),
                VirtAddr::new(user_stack_top),
                crate::mm::page_table::PTEFlags::U
                    | crate::mm::page_table::PTEFlags::R
                    | crate::mm::page_table::PTEFlags::W
                    | crate::mm::page_table::PTEFlags::V,
            );

            (
                memory_set,
                user_stack_top,
                interp_elf.entry_with_bias(interp_bias),
                elf.phdr_vaddr(target_bias),
                elf.phnum(),
                interp_bias,
            )
        } else {
            let phdr_vaddr = elf.phdr_vaddr(0);
            let phnum = elf.phnum();
            if elf.header.e_type == 3 {
                let bias = 0x0040_0000usize;
                let (memory_set, user_stack_top, entry) = match elf.load_at(bias) {
                    Ok(result) => result,
                    Err(e) => {
                        log::error!("[syscall] execve: failed to load ELF: {:?}", e);
                        return Err(e);
                    }
                };
                (
                    memory_set,
                    user_stack_top,
                    entry,
                    elf.phdr_vaddr(bias),
                    phnum,
                    0,
                )
            } else {
                let (memory_set, user_stack_top, entry) = match elf.load() {
                    Ok(result) => result,
                    Err(e) => {
                        log::error!("[syscall] execve: failed to load ELF: {:?}", e);
                        return Err(e);
                    }
                };
                (memory_set, user_stack_top, entry, phdr_vaddr, phnum, 0)
            }
        };

    let argv_with_path = launch_argv;

    /*
    let _old_phdr_vaddr = elf.program_headers.iter()
        .find(|ph| ph.p_type == crate::mm::elf_loader::PT_PHDR)
        .map(|ph| ph.p_vaddr)
        .unwrap_or_else(|| {
            elf.program_headers.iter()
                .filter(|ph| ph.p_type == crate::mm::elf_loader::PT_LOAD)
                .map(|ph| ph.p_vaddr)
                .min()
                .unwrap_or(0)
                + elf.header.e_phoff
        });
    let phnum = elf.header.e_phnum as usize;

    let (new_memory_set, user_stack_top, entry) = match elf.load() {
        Ok(result) => result,
        Err(e) => {
            log::error!("[syscall] execve: failed to load ELF: {:?}", e);
            return Err(e);
        }
    };

    // 在新地址空间的用户栈上构造 argc/argv/envp/auxv
    let argv_with_path = if argv.is_empty() {
        alloc::vec![path_str.clone()]
    } else {
        argv
    };
    */
    let at_entry = if interp_base != 0 {
        let target_bias = if elf.header.e_type == 3 {
            0x0040_0000
        } else {
            0
        };
        elf.entry_with_bias(target_bias)
    } else {
        entry
    };

    let sp = setup_user_stack(
        &new_memory_set,
        user_stack_top,
        &argv_with_path,
        &envp,
        at_entry,
        phdr_vaddr,
        phnum,
        interp_base,
    );
    #[cfg(target_arch = "riscv64")]
    log::info!(
        "[syscall] execve: probe 0x15a10 before install = {:?}",
        new_memory_set.page_table.translate(VirtAddr::new(0x15a10))
    );

    if let Some(task) = current_task() {
        {
            let mut inner = task.inner.lock();

            // 重置堆
            inner.program_break = crate::config::USER_HEAP_START;
            inner.mapped_break = crate::config::USER_HEAP_START;
            inner.next_mmap = 0x4000_0000;
        }

        {
            let mut ms = task.memory_set.lock();
            log::info!("[syscall] execve: replacing memory set");
            *ms = new_memory_set;
            log::info!("[syscall] execve: activating new memory set");
            ms.activate();
        }

        {
            log::info!("[syscall] execve: resetting trap frame");
            let mut updated_saved_tf = false;
            let mut tf_guard = task.trap_frame.lock();
            if let Some(ref mut tf) = *tf_guard {
                reset_exec_trapframe(tf, entry, sp, argv_with_path.len());
                updated_saved_tf = true;
            }
            if !updated_saved_tf {
                crate::trap::update_current_trapframe(|tf| {
                    reset_exec_trapframe(tf, entry, sp, argv_with_path.len());
                });
            }
        }

        log::info!(
            "[syscall] execve: loaded '{}' at entry={:#x}, sp={:#x}, argc={}",
            path_str,
            entry,
            sp,
            argv_with_path.len()
        );
    }

    // 通知 trap 处理：execve 已替换地址空间，跳过 syscall_ok() PC 前进
    crate::trap::signal_execve_done();
    Ok(0)
}

pub fn sys_exit(exit_code: i32) -> SyscallRet {
    log::info!("[syscall] exit(code={})", exit_code);
    exit_current_and_run_next(exit_code);
    Ok(0)
}

pub fn sys_exit_group(exit_code: i32) -> SyscallRet {
    log::info!("[syscall] exit_group(code={})", exit_code);
    // 暂未实现线程组：与 exit 等价
    sys_exit(exit_code)
}

pub fn sys_getpid() -> SyscallRet {
    if let Some(task) = current_task() {
        let pid = task.pid.0;
        log::debug!("[syscall] getpid() = {}", pid);
        Ok(pid)
    } else {
        Err(SysErrNo::ESRCH)
    }
}

pub fn sys_getppid() -> SyscallRet {
    let task = current_task().ok_or(SysErrNo::ESRCH)?;
    let inner = task.inner.lock();
    let ppid = inner.parent.as_ref().map(|p| p.pid.0).unwrap_or(1usize);
    log::debug!("[syscall] getppid() = {}", ppid);
    Ok(ppid)
}

pub fn sys_sched_yield() -> SyscallRet {
    log::debug!("[syscall] sched_yield()");
    if *crate::trap::FOREGROUND_MODE.lock() {
        return Ok(0);
    }
    suspend_current_and_run_next();
    Ok(0)
}

/// wait4：支持 `pid==-1`、`pid==0`（视作任一子进程）、指定 pid，`WNOHANG`，
/// 在未持锁状态下阻塞调度。
///
/// ## FOREGROUND_MODE 特殊处理
/// 在前台测试驱动模式下，父进程调用 wait4 但子进程尚未退出时，不能使用
/// `suspend_current_and_run_next` 让出 CPU（这会导致父进程被重新放入 FIFO 队首，
/// 永远抢在子进程之前被调度，形成活锁）。
/// 因此在 FOREGROUND_MODE 下：
///   - 如果存在可回收的僵尸子进程，立即回收并返回；
///   - 否则返回 `-ECHILD`，让父进程返回用户态。
///   - 子进程获得调度机会运行并退出成为僵尸；
///   - 父进程再次被调度时调用 wait4 可以成功回收。
pub fn sys_wait4(pid: isize, status: *mut i32, options: usize, _rusage: usize) -> SyscallRet {
    let task = current_task().ok_or(SysErrNo::ESRCH)?;
    let pid = if pid == 0 { -1 } else { pid };
    // 尝试回收僵尸子进程（两种模式都走同一逻辑）
    let try_reap = || -> Option<(usize, i32)> {
        let mut inner = task.inner.lock();
        if let Some(index) = inner.children.iter().position(|c| {
            (pid == -1 || c.pid.0 == pid as usize) && c.status() == crate::task::TaskStatus::Zombie
        }) {
            let child = inner.children.remove(index);
            let cpid = child.pid.0;
            let exit_code = child.exit_code();
            return Some((cpid, exit_code));
        }
        None
    };

    if let Some((cpid, exit_code)) = try_reap() {
        if !status.is_null() {
            write_user_i32(status as usize, exit_code << 8)?;
        }
        return Ok(cpid);
    }

    // 无僵尸子进程：WNOHANG 立即返回
    if (options & WNOHANG) != 0 {
        return Ok(0);
    }

    // 正常模式：使用调度器阻塞
    loop {
        // 检查是否有子进程
        {
            let inner = task.inner.lock();
            if inner.children.is_empty() {
                return Err(SysErrNo::ECHILD);
            }
            if pid > 0 && !inner.children.iter().any(|c| c.pid.0 == pid as usize) {
                return Err(SysErrNo::ECHILD);
            }
        }

        // 再检查一次僵尸
        if let Some((cpid, exit_code)) = try_reap() {
            if !status.is_null() {
                write_user_i32(status as usize, exit_code << 8)?;
            }
            return Ok(cpid);
        }

        if *crate::trap::FOREGROUND_MODE.lock() {
            *crate::task::CURRENT_TASK.lock() = None;
            crate::task::run_next_task();
            task.memory_set.lock().activate();
            *crate::task::CURRENT_TASK.lock() = Some(task.clone());
        } else {
            suspend_current_and_run_next();
        }
    }
}

/// clone：`fork()`（clone_bits==0）与带用户栈的独立进程形态；线程级共享标志暂未实现，一律 `EINVAL`。
pub fn sys_clone(
    flags: usize,
    stack: usize,
    _parent_tid: usize,
    tls: usize,
    _child_tid: usize,
) -> SyscallRet {
    let parent = current_task().ok_or(SysErrNo::ESRCH)?;
    let parent_pid = parent.pid.0;

    let clone_bits = flags & !CSIGNAL;
    if (clone_bits & THREAD_SHARING_FLAGS) != 0 {
        log::warn!(
            "[syscall] clone: thread-sharing clone bits not supported ({:#x})",
            clone_bits & THREAD_SHARING_FLAGS
        );
        return Err(SysErrNo::EINVAL);
    }
    if clone_bits != 0 && (clone_bits & !ALLOWED_CLONE_FLAGS) != 0 {
        log::warn!("[syscall] clone: unsupported clone bits {:#x}", clone_bits);
        return Err(SysErrNo::EINVAL);
    }

    let (area_count, page_count) = {
        let ms = parent.memory_set.lock();
        let pages = ms.areas.iter().fold(0usize, |sum, area| {
            let start = area.start_va.raw() / crate::config::PAGE_SIZE;
            let end = (area.end_va.raw() + crate::config::PAGE_SIZE - 1) / crate::config::PAGE_SIZE;
            sum + end.saturating_sub(start)
        });
        (ms.areas.len(), pages)
    };
    log::info!(
        "[syscall] clone start flags={:#x} stack={:#x} parent={} areas={} pages={}",
        flags,
        stack,
        parent_pid,
        area_count,
        page_count
    );

    let mut child_tf = crate::trap::clone_current_trapframe().ok_or(SysErrNo::EINVAL)?;
    child_tf[TrapFrameArgs::RET] = 0;
    child_tf.syscall_ok();
    if stack != 0 {
        child_tf[TrapFrameArgs::SP] = stack;
    }
    if (clone_bits & CLONE_SETTLS) != 0 {
        child_tf[TrapFrameArgs::TLS] = tls;
    }

    let memory_set = new_shared_memory_set(parent.memory_set.lock().clone());
    let (fd_table, cwd, root, program_break, mapped_break, next_mmap) = {
        let inner = parent.inner.lock();
        let fd_table = {
            let fd_guard = inner.fd_table.lock();
            dup_fd_table(&*fd_guard)
        };
        (
            fd_table,
            inner.cwd.clone(),
            inner.root.clone(),
            inner.program_break,
            inner.mapped_break,
            inner.next_mmap,
        )
    };

    let child = Arc::new(crate::task::TaskControlBlock {
        pid: crate::task::pid::Pid::alloc(),
        is_kernel: false,
        inner: Mutex::new(crate::task::TaskControlBlockInner {
            exit_code: 0,
            clone_flags: clone_bits,
            parent: Some(parent.clone()),
            children: Vec::new(),
            fd_table,
            cwd,
            root,
            program_break,
            mapped_break,
            next_mmap,
        }),
        task_ctx: crate::task::KernelCtx::new(crate::task::context::TaskContext::zero_init()),
        memory_set,
        trap_frame: Mutex::new(Some(child_tf)),
        status: Mutex::new(crate::task::TaskStatus::Ready),
    });
    let child_pid = child.pid.0;

    parent.inner.lock().children.push(child.clone());
    crate::task::manager::add_task(child);

    log::info!(
        "[syscall] clone(flags={:#x}, stack={:#x}) parent={} child={}",
        flags,
        stack,
        parent_pid,
        child_pid
    );
    Ok(child_pid)
}

/// 为 init/test 进程构造最小的用户栈（argv=["init"], envp=[], auxv）。
/// 由 `TaskControlBlock::new_user` 调用。
pub fn setup_user_stack_for_init(
    memory_set: &crate::mm::memory_set::MemorySet,
    stack_top: usize,
    elf_entry: usize,
    phdr_vaddr: usize,
    phnum: usize,
) -> usize {
    let argv = alloc::vec![String::from("/init")];
    let envp: Vec<String> = alloc::vec![
        String::from("PATH=/:/bin:/usr/bin"),
        String::from("LD_LIBRARY_PATH=/"),
    ];
    setup_user_stack(
        memory_set, stack_top, &argv, &envp, elf_entry, phdr_vaddr, phnum, 0,
    )
}
