use super::SyscallRet;
use crate::fs::read_executable_file;
use crate::mm::elf_loader::ElfFile;
use crate::task::{
    current_task, dup_fd_table, dup_fs_context, dup_mm_context, exit_current_and_run_next,
    exit_thread_group_and_run_next, new_shared_memory_set, suspend_current_and_run_next,
    yield_current_once, Credentials, TaskControlBlock, ThreadGroup,
};
use crate::utils::error::SysErrNo;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicUsize, Ordering};
use polyhal::VirtAddr;
use polyhal_trap::trapframe::{TrapFrame, TrapFrameArgs};
use spin::Mutex;

/// Linux clone 的低 8 位为发往父进程的信号 (`CSIGNAL`)。
const CSIGNAL: usize = 0xff;
const ALLOWED_CLONE_FLAGS: usize = CLONE_VM
    | CLONE_FS
    | CLONE_FILES
    | CLONE_SIGHAND
    | CLONE_VFORK
    | CLONE_THREAD
    | CLONE_SYSVSEM
    | CLONE_SETTLS
    | CLONE_PARENT_SETTID
    | CLONE_CHILD_CLEARTID
    | CLONE_DETACHED
    | CLONE_CHILD_SETTID;

const CLONE_VM: usize = 0x00000100;

const CLONE_FS: usize = 0x00000200;

const CLONE_FILES: usize = 0x00000400;

const CLONE_SIGHAND: usize = 0x00000800;

const CLONE_VFORK: usize = 0x00004000;

const CLONE_THREAD: usize = 0x00010000;

const CLONE_SYSVSEM: usize = 0x00040000;

const CLONE_SETTLS: usize = 0x00080000;

const CLONE_PARENT_SETTID: usize = 0x00100000;

const CLONE_CHILD_CLEARTID: usize = 0x00200000;

const CLONE_DETACHED: usize = 0x00400000;

const CLONE_CHILD_SETTID: usize = 0x01000000;

const WNOHANG: usize = 0x0000_0001;
const WNOWAIT: usize = 0x0100_0000;
const WEXITED: usize = 0x0000_0004;

const P_ALL: usize = 0;
const P_PID: usize = 1;
const P_PGID: usize = 2;

const SIGCHLD: i32 = 17;
const CLD_EXITED: i32 = 1;
const CLD_KILLED: i32 = 2;

#[derive(Clone, Copy)]
enum WaitTarget {
    AnyChild,
    Tgid(usize),
    Pgid(usize),
}

#[repr(C)]
#[derive(Clone, Copy)]
struct UserWaitSigInfo {
    signo: i32,
    errno: i32,
    code: i32,
    _align: i32,
    pid: i32,
    uid: u32,
    status: i32,
    _reserved: [u8; 100],
}

fn read_user_usize(addr: usize) -> Result<usize, SysErrNo> {
    super::user::read_usize(addr)
}

fn write_user_i32(addr: usize, value: i32) -> Result<(), SysErrNo> {
    super::user::write_i32(addr, value)
}

fn child_matches_wait_target(child: &Arc<TaskControlBlock>, target: WaitTarget) -> bool {
    match target {
        WaitTarget::AnyChild => true,
        WaitTarget::Tgid(tgid) => child.thread_group.tgid() == tgid,
        WaitTarget::Pgid(pgid) => child.inner.lock().pgid == pgid,
    }
}

fn reap_zombie_child(waiter: &Arc<TaskControlBlock>, target: WaitTarget) -> Option<(usize, i32)> {
    for owner in waiter.thread_group.user_members() {
        let mut inner = owner.inner.lock();
        if let Some(index) = inner.children.iter().position(|child| {
            child_matches_wait_target(child, target) && child.thread_group.is_process_zombie()
        }) {
            let child = inner.children.remove(index);
            return Some((child.thread_group.tgid(), child.thread_group.exit_code()));
        }
    }
    None
}

fn peek_zombie_child(waiter: &Arc<TaskControlBlock>, target: WaitTarget) -> Option<(usize, i32)> {
    for owner in waiter.thread_group.user_members() {
        let inner = owner.inner.lock();
        if let Some(child) = inner
            .children
            .iter()
            .find(|child| child_matches_wait_target(child, target) && child.thread_group.is_process_zombie())
        {
            return Some((child.thread_group.tgid(), child.thread_group.exit_code()));
        }
    }
    None
}

fn has_matching_child(waiter: &Arc<TaskControlBlock>, target: WaitTarget) -> bool {
    waiter.thread_group.user_members().into_iter().any(|owner| {
        owner
            .inner
            .lock()
            .children
            .iter()
            .any(|child| child_matches_wait_target(child, target))
    })
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

struct ScriptInterpreter {
    path: String,
    arg: Option<String>,
}

fn parse_shebang(data: &[u8]) -> Option<ScriptInterpreter> {
    if data.len() < 2 || &data[..2] != b"#!" {
        return None;
    }
    let end = data
        .iter()
        .position(|byte| *byte == b'\n')
        .unwrap_or(data.len());
    let line = core::str::from_utf8(&data[2..end]).ok()?.trim();
    let mut parts = line.split_whitespace();
    let path = String::from(parts.next()?);
    let arg = parts.next().map(String::from);
    Some(ScriptInterpreter { path, arg })
}

fn script_interpreter_spec(
    root: &str,
    cwd: &str,
    script_logical_path: &str,
    interp: ScriptInterpreter,
    original_argv: &[String],
) -> Option<(String, Vec<String>)> {
    let mut interp_logical = crate::fs::resolve_path(cwd, &interp.path);
    let mut argv = Vec::new();

    let interp_exists = super::with_kernel_page_table(|| {
        crate::fs::file_exists(&crate::fs::apply_root(root, &interp_logical))
    });
    let busybox_exists = super::with_kernel_page_table(|| {
        crate::fs::file_exists(&crate::fs::apply_root(root, "/busybox"))
    });

    if !interp_exists
        && (interp_logical == "/bin/sh" || interp_logical == "/bin/busybox")
        && busybox_exists
    {
        interp_logical = String::from("/busybox");
        argv.push(interp_logical.clone());
        if interp.path.ends_with("/busybox") {
            if let Some(arg) = interp.arg {
                argv.push(arg);
            }
        } else {
            argv.push(String::from("sh"));
            if let Some(arg) = interp.arg {
                argv.push(arg);
            }
        }
    } else {
        argv.push(interp_logical.clone());
        if let Some(arg) = interp.arg {
            argv.push(arg);
        }
    }

    argv.push(String::from(script_logical_path));
    for arg in original_argv.iter().skip(1) {
        argv.push(arg.clone());
    }

    Some((interp_logical, argv))
}

pub(crate) fn set_user_entry_registers(tf: &mut TrapFrame, sp: usize, argc: usize) {
    let _ = argc;
    // Linux-style ELF entry for RISC-V and LoongArch gets argc/argv from the
    // initial user stack.  glibc treats the incoming first argument register as
    // the optional rtld_fini hook, so passing argc there corrupts the exit path.
    tf[TrapFrameArgs::ARG0] = 0;
    tf[TrapFrameArgs::ARG1] = 0;

    tf[TrapFrameArgs::SP] = sp;
}

fn reset_exec_trapframe(tf: &mut TrapFrame, entry: usize, sp: usize, argc: usize) {
    #[cfg(target_arch = "riscv64")]
    {
        tf.x = [0; 32];
    }
    #[cfg(target_arch = "loongarch64")]
    {
        tf.regs = [0; 32];
    }
    tf.clear_fp_state();

    tf[TrapFrameArgs::SEPC] = entry;
    set_user_entry_registers(tf, sp, argc);
    crate::trap::prepare_user_trapframe(tf);
}

fn read_user_cstr(ptr: *const u8) -> Result<String, SysErrNo> {
    super::user::read_cstr_null_empty(ptr as usize)
}

fn read_user_path(ptr: *const u8) -> Result<String, SysErrNo> {
    super::user::read_path_cstr(ptr as usize)
}

pub(crate) fn proc_self_exe_target(path: &str) -> Result<Option<String>, SysErrNo> {
    if path != "/proc/self/exe" && path != "/proc/thread-self/exe" {
        return Ok(None);
    }
    let task = current_task().ok_or(SysErrNo::ESRCH)?;
    let exec_path = task.inner.lock().exec_path.clone();
    if exec_path.is_empty() {
        return Err(SysErrNo::ENOENT);
    }
    Ok(Some(exec_path))
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
    let credentials = current_task()
        .map(|task| task.credentials.lock().clone())
        .unwrap_or_else(Credentials::root);
    setup_user_stack_with_credentials(
        memory_set,
        stack_top,
        argv,
        envp,
        at_entry,
        phdr_vaddr,
        phnum,
        interp_base,
        credentials,
    )
}

fn setup_user_stack_with_credentials(
    memory_set: &crate::mm::memory_set::MemorySet,
    stack_top: usize,
    argv: &[String],
    envp: &[String],
    at_entry: usize,
    phdr_vaddr: usize,
    phnum: usize,
    interp_base: usize,
    credentials: Credentials,
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

    let platform = if cfg!(target_arch = "loongarch64") {
        "loongarch64"
    } else if cfg!(target_arch = "riscv64") {
        "riscv64"
    } else {
        "unknown"
    };
    write_bytes(memory_set, &mut sp, &[0u8]);
    write_bytes(memory_set, &mut sp, platform.as_bytes());
    let platform_addr = sp;

    // Reserve stable bytes for AT_RANDOM; glibc uses them for stack/pointer
    // guards during startup and later exit-handler validation.
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
    const AT_NULL: usize = 0;
    const AT_PHDR: usize = 3;
    const AT_PHENT: usize = 4;
    const AT_PHNUM: usize = 5;
    const AT_PAGESZ: usize = 6;
    const AT_BASE: usize = 7;
    const AT_FLAGS: usize = 8;
    const AT_ENTRY: usize = 9;
    const AT_UID: usize = 11;
    const AT_EUID: usize = 12;
    const AT_GID: usize = 13;
    const AT_EGID: usize = 14;
    const AT_PLATFORM: usize = 15;
    const AT_HWCAP: usize = 16;
    const AT_CLKTCK: usize = 17;
    const AT_SECURE: usize = 23;
    const AT_RANDOM: usize = 25;
    const AT_HWCAP2: usize = 26;
    const AT_EXECFN: usize = 31;

    #[cfg(target_arch = "loongarch64")]
    const LINUX_AT_HWCAP: usize = 0x1 | 0x8; // CPUCFG | FPU
    #[cfg(not(target_arch = "loongarch64"))]
    const LINUX_AT_HWCAP: usize = 0;

    let execfn_addr = argv_ptrs.first().copied().unwrap_or(0);
    // glibc reads more of the Linux auxv contract than musl. Keep these
    // generic process credentials/capabilities here rather than teaching
    // individual tests about libc startup quirks.
    let auxv_pairs: [(usize, usize); 19] = [
        (AT_PHDR, phdr_vaddr),
        (AT_PHENT, 56), // sizeof(Elf64_Phdr)
        (AT_PHNUM, phnum),
        (AT_PAGESZ, crate::config::PAGE_SIZE),
        (AT_BASE, interp_base),
        (AT_FLAGS, 0),
        (AT_ENTRY, at_entry),
        (AT_UID, credentials.real_uid as usize),
        (AT_EUID, credentials.effective_uid as usize),
        (AT_GID, credentials.real_gid as usize),
        (AT_EGID, credentials.effective_gid as usize),
        (AT_PLATFORM, platform_addr),
        (AT_HWCAP, LINUX_AT_HWCAP),
        (AT_CLKTCK, 100),
        (AT_SECURE, 0),
        (AT_RANDOM, random_addr),
        (AT_HWCAP2, 0),
        (AT_EXECFN, execfn_addr),
        (AT_NULL, 0),
    ];
    let auxv_entries = auxv_pairs.len();
    let total_slots = 1 + (argv_ptrs.len() + 1) + (envp_ptrs.len() + 1) + auxv_entries * 2;
    // 确保 sp 在写完后 16 字节对齐
    sp -= total_slots * core::mem::size_of::<usize>();
    sp &= !0xF;

    let final_sp = sp;

    // Write argc/argv/envp/auxv from low to high addresses. The strings and
    // random bytes were already placed above this table on the same stack.
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
    let path_str = read_user_path(path)?;
    log::info!("[syscall] execve(path='{}')", path_str);

    // 在替换地址空间之前，从旧地址空间读取 argv/envp
    let argv = read_user_str_array(argv_ptr)?;
    let mut envp = read_user_str_array(envp_ptr)?;
    if envp.is_empty() {
        envp.push(String::from("PATH=/bin:/basic:/"));
        envp.push(String::from("LD_LIBRARY_PATH=/lib"));
    }

    let (root, cwd) = if let Some(task) = current_task() {
        let fs = task.fs.lock();
        (fs.root.clone(), fs.cwd.clone())
    } else {
        (String::from("/"), String::from("/"))
    };
    let requested_logical_path = crate::fs::resolve_path(&cwd, &path_str);
    let logical_path =
        proc_self_exe_target(&requested_logical_path)?.unwrap_or(requested_logical_path);
    let host_path = crate::fs::apply_root(&root, &logical_path);

    let mut elf_data = match super::with_kernel_page_table(|| read_executable_file(&host_path)) {
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

    // 从 ELF 头中获取 phdr 信息用于 auxv
    let mut launch_argv = if argv.is_empty() {
        alloc::vec![logical_path.clone()]
    } else {
        argv
    };
    let mut exec_logical_path = logical_path.clone();

    if let Some(interp) = parse_shebang(&elf_data) {
        let Some((interp_logical, script_argv)) =
            script_interpreter_spec(&root, &cwd, &logical_path, interp, &launch_argv)
        else {
            return Err(SysErrNo::ENOEXEC);
        };
        let interp_host = crate::fs::apply_root(&root, &interp_logical);
        elf_data = match super::with_kernel_page_table(|| read_executable_file(&interp_host)) {
            Some(data) => data,
            None => {
                log::error!(
                    "[syscall] execve: script interpreter not found: {} ({})",
                    interp_logical,
                    interp_host
                );
                return Err(SysErrNo::ENOENT);
            }
        };
        launch_argv = script_argv;
        exec_logical_path = interp_logical;
    }

    let elf = match ElfFile::parse(&elf_data) {
        Ok(elf) => elf,
        Err(e) => {
            log::error!("[syscall] execve: failed to parse ELF: {:?}", e);
            return Err(e);
        }
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

    let (mut new_memory_set, user_stack_top, entry, phdr_vaddr, phnum, interp_base) =
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
            let target_bias = if elf.header.e_type == 3 {
                0x0040_0000
            } else {
                0
            };
            let interp_bias = ElfFile::choose_interpreter_bias(&elf, target_bias, &interp_elf)?;
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
            )?;

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
    crate::syscall::signal::install_signal_trampoline(&mut new_memory_set)?;
    if let Some(task) = current_task() {
        crate::task::terminate_thread_group_peers_for_exec(&task);
        crate::syscall::mm::detach_task_shared_memory(&task);
        crate::syscall::signal::reset_signal_handlers_for_exec(&task);
        {
            let mut inner = task.inner.lock();

            // 重置堆
            inner.exec_path = exec_logical_path.clone();
            inner.robust_list_head = 0;
            inner.robust_list_len = 0;
            inner.fd_table.lock().close_on_exec();
        }
        {
            let mut mm = task.mm.lock();
            mm.program_break = crate::config::USER_HEAP_START;
            mm.mapped_break = crate::config::USER_HEAP_START;
            mm.next_mmap = 0x4000_0000;
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
    exit_thread_group_and_run_next(exit_code);
    Ok(0)
}

pub fn sys_getpid() -> SyscallRet {
    if let Some(task) = current_task() {
        let pid = task.thread_group.tgid();
        log::debug!("[syscall] getpid() = {}", pid);
        Ok(pid)
    } else {
        Err(SysErrNo::ESRCH)
    }
}

pub fn sys_getppid() -> SyscallRet {
    let task = current_task().ok_or(SysErrNo::ESRCH)?;
    let inner = task.inner.lock();
    let ppid = inner
        .parent
        .as_ref()
        .map(|p| p.thread_group.tgid())
        .unwrap_or(1usize);
    log::debug!("[syscall] getppid() = {}", ppid);
    Ok(ppid)
}

fn sys_wait4_thread_group(pid: isize, status: *mut i32, options: usize) -> SyscallRet {
    let task = current_task().ok_or(SysErrNo::ESRCH)?;
    let target = match pid {
        -1 | 0 => WaitTarget::AnyChild,
        n if n > 0 => WaitTarget::Tgid(n as usize),
        _ => return Err(SysErrNo::ECHILD),
    };

    if let Some((cpid, exit_code)) = reap_zombie_child(&task, target) {
        if !status.is_null() {
            write_user_i32(
                status as usize,
                crate::task::wait_status_from_exit_code(exit_code),
            )?;
        }
        return Ok(cpid);
    }

    if !has_matching_child(&task, target) {
        return Err(SysErrNo::ECHILD);
    }

    if (options & WNOHANG) != 0 {
        return Ok(0);
    }

    loop {
        if let Some((cpid, exit_code)) = reap_zombie_child(&task, target) {
            if !status.is_null() {
                write_user_i32(
                    status as usize,
                    crate::task::wait_status_from_exit_code(exit_code),
                )?;
            }
            return Ok(cpid);
        }
        if !has_matching_child(&task, target) {
            return Err(SysErrNo::ECHILD);
        }
        match crate::task::wait_queue::sleep_on_child_exit() {
            Ok(_) => {}
            Err(SysErrNo::EINTR) => {
                if let Some((cpid, exit_code)) = reap_zombie_child(&task, target) {
                    if !status.is_null() {
                        write_user_i32(
                            status as usize,
                            crate::task::wait_status_from_exit_code(exit_code),
                        )?;
                    }
                    return Ok(cpid);
                }
                if has_matching_child(&task, target) {
                    continue;
                }
                return Err(SysErrNo::ECHILD);
            }
            Err(err) => return Err(err),
        }
    }
}

pub fn sys_sched_yield() -> SyscallRet {
    log::debug!("[syscall] sched_yield()");
    if crate::trap::foreground_driver_active() {
        yield_current_once();
        return Ok(0);
    }
    suspend_current_and_run_next();
    Ok(0)
}

/// wait4：支持 `pid==-1`、`pid==0`（视作任一子进程）、指定 pid，`WNOHANG`，
/// 在未持锁状态下阻塞调度。
///
/// ## Foreground driver 特殊处理
/// 在前台测试驱动模式下，父进程调用 wait4 但子进程尚未退出时，不能使用
/// `suspend_current_and_run_next` 让出 CPU（这会导致父进程被重新放入 FIFO 队首，
/// 永远抢在子进程之前被调度，形成活锁）。
/// 因此前台驱动模式下：
///   - 如果存在可回收的僵尸子进程，立即回收并返回；
///   - 否则返回 `-ECHILD`，让父进程返回用户态。
///   - 子进程获得调度机会运行并退出成为僵尸；
///   - 父进程再次被调度时调用 wait4 可以成功回收。
pub fn sys_wait4(pid: isize, status: *mut i32, options: usize, _rusage: usize) -> SyscallRet {
    sys_wait4_thread_group(pid, status, options)
}

fn waitid_target(idtype: usize, id: usize) -> Result<WaitTarget, SysErrNo> {
    match idtype {
        P_ALL => Ok(WaitTarget::AnyChild),
        P_PID => Ok(WaitTarget::Tgid(id)),
        P_PGID => {
            if id == 0 {
                let task = current_task().ok_or(SysErrNo::ESRCH)?;
                let pgid = task.inner.lock().pgid;
                Ok(WaitTarget::Pgid(pgid))
            } else {
                Ok(WaitTarget::Pgid(id))
            }
        }
        _ => Err(SysErrNo::EINVAL),
    }
}

fn waitid_siginfo(pid: usize, exit_code: i32) -> UserWaitSigInfo {
    let (code, status) = if exit_code < crate::task::SIGNAL_EXIT_CODE_BASE {
        let signum = crate::task::SIGNAL_EXIT_CODE_BASE - exit_code;
        (CLD_KILLED, signum)
    } else {
        (CLD_EXITED, exit_code & 0xff)
    };
    UserWaitSigInfo {
        signo: SIGCHLD,
        errno: 0,
        code,
        _align: 0,
        pid: pid as i32,
        uid: 0,
        status,
        _reserved: [0; 100],
    }
}

fn write_waitid_siginfo(infop: usize, pid: usize, exit_code: i32) -> Result<(), SysErrNo> {
    if infop == 0 {
        return Err(SysErrNo::EFAULT);
    }
    super::user::copy_object_to_user(infop, &waitid_siginfo(pid, exit_code))
}

pub fn sys_waitid(idtype: usize, id: usize, infop: usize, options: usize, _rusage: usize) -> SyscallRet {
    const SUPPORTED_OPTIONS: usize = WEXITED | WNOHANG | WNOWAIT;
    if infop == 0 {
        return Err(SysErrNo::EFAULT);
    }
    if (options & !SUPPORTED_OPTIONS) != 0 || (options & WEXITED) == 0 {
        return Err(SysErrNo::EINVAL);
    }

    let task = current_task().ok_or(SysErrNo::ESRCH)?;
    let target = waitid_target(idtype, id)?;
    let nohang = (options & WNOHANG) != 0;
    let nowait = (options & WNOWAIT) != 0;

    let take_child = |task: &Arc<TaskControlBlock>| {
        if nowait {
            peek_zombie_child(task, target)
        } else {
            reap_zombie_child(task, target)
        }
    };

    if let Some((cpid, exit_code)) = take_child(&task) {
        write_waitid_siginfo(infop, cpid, exit_code)?;
        return Ok(0);
    }

    if !has_matching_child(&task, target) {
        return Err(SysErrNo::ECHILD);
    }

    if nohang {
        return Ok(0);
    }

    loop {
        if let Some((cpid, exit_code)) = take_child(&task) {
            write_waitid_siginfo(infop, cpid, exit_code)?;
            return Ok(0);
        }
        if !has_matching_child(&task, target) {
            return Err(SysErrNo::ECHILD);
        }
        match crate::task::wait_queue::sleep_on_child_exit() {
            Ok(_) | Err(SysErrNo::EINTR) => continue,
            Err(err) => return Err(err),
        }
    }
}

/// clone: supports fork-style children plus the documented shared-resource and thread flags.
pub fn sys_clone(
    flags: usize,
    stack: usize,
    parent_tid: usize,
    arg3: usize,
    arg4: usize,
) -> SyscallRet {
    let parent = current_task().ok_or(SysErrNo::ESRCH)?;
    let parent_tid_num = parent.pid.0;
    let parent_pid = parent.thread_group.tgid();
    #[cfg(target_arch = "loongarch64")]
    let (tls, child_tid) = (arg4, arg3);
    #[cfg(not(target_arch = "loongarch64"))]
    let (tls, child_tid) = (arg3, arg4);

    let clone_bits = flags & !CSIGNAL;
    if clone_bits != 0 && (clone_bits & !ALLOWED_CLONE_FLAGS) != 0 {
        log::warn!("[syscall] clone: unsupported clone bits {:#x}", clone_bits);
        return Err(SysErrNo::EINVAL);
    }
    if (clone_bits & CLONE_SIGHAND) != 0 && (clone_bits & CLONE_VM) == 0 {
        return Err(SysErrNo::EINVAL);
    }
    if (clone_bits & CLONE_THREAD) != 0 && (clone_bits & CLONE_SIGHAND) == 0 {
        return Err(SysErrNo::EINVAL);
    }

    if log::log_enabled!(log::Level::Info) {
        let (area_count, page_count) = {
            let ms = parent.memory_set.lock();
            let pages = ms.areas.iter().fold(0usize, |sum, area| {
                let start = area.start_va.raw() / crate::config::PAGE_SIZE;
                let end =
                    (area.end_va.raw() + crate::config::PAGE_SIZE - 1) / crate::config::PAGE_SIZE;
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
    }
    let mut child_tf = crate::trap::clone_current_trapframe().ok_or(SysErrNo::EINVAL)?;
    child_tf[TrapFrameArgs::RET] = 0;
    child_tf.syscall_ok();
    if stack != 0 {
        child_tf[TrapFrameArgs::SP] = stack;
    }
    if (clone_bits & CLONE_SETTLS) != 0 {
        child_tf[TrapFrameArgs::TLS] = tls;
    }
    crate::trap::prepare_user_trapframe(&mut child_tf);

    let is_vfork = (clone_bits & CLONE_VFORK) != 0;
    // vfork children exec in-place in this kernel.  Give them their own COW
    // address space so execve cannot replace the parent's shared MemorySet.
    let share_vm = (clone_bits & CLONE_VM) != 0 && !is_vfork;
    let is_thread = (clone_bits & CLONE_THREAD) != 0;
    let memory_set = if share_vm {
        parent.memory_set.clone()
    } else {
        let mut parent_memory = parent.memory_set.lock();
        let child_memory = parent_memory.fork_cow()?;
        parent_memory.activate();
        new_shared_memory_set(child_memory)
    };
    let mm = if share_vm {
        parent.mm.clone()
    } else {
        dup_mm_context(&parent.mm)
    };
    let (
        fd_table,
        exec_path,
        thread_parent,
        pgid,
        sid,
        rlimit_nofile,
        rlimit_nofile_max,
        rlimit_fsize,
        rlimit_fsize_max,
        rlimit_core,
        rlimit_core_max,
        inherited_timer_slack_ns,
    ) = {
        let inner = parent.inner.lock();
        let fd_table = if (clone_bits & CLONE_FILES) != 0 {
            inner.fd_table.clone()
        } else {
            let fd_guard = inner.fd_table.lock();
            dup_fd_table(&*fd_guard)
        };
        (
            fd_table,
            inner.exec_path.clone(),
            inner.parent.clone(),
            inner.pgid,
            inner.sid,
            inner.rlimit_nofile,
            inner.rlimit_nofile_max,
            inner.rlimit_fsize,
            inner.rlimit_fsize_max,
            inner.rlimit_core,
            inner.rlimit_core_max,
            inner.current_timer_slack_ns,
        )
    };
    let fs = if (clone_bits & CLONE_FS) != 0 {
        parent.fs.clone()
    } else {
        dup_fs_context(&parent.fs)
    };
    let fs_snapshot = fs.lock().clone();
    let mm_snapshot = *mm.lock();
    let signal_actions = if (clone_bits & CLONE_SIGHAND) != 0 {
        parent.signal_actions.clone()
    } else {
        crate::syscall::signal::dup_signal_actions(&parent.signal_actions)
    };
    let credentials = parent.credentials.lock().clone();
    let signal_blocked = parent.signal_state.lock().blocked;
    let child_pid_obj = crate::task::pid::Pid::alloc();
    let child_pid = child_pid_obj.0;
    let thread_group = if is_thread {
        parent.thread_group.clone()
    } else {
        ThreadGroup::new(child_pid)
    };
    let child_parent = if is_thread {
        thread_parent
    } else {
        Some(parent.clone())
    };

    let child = Arc::new(crate::task::TaskControlBlock {
        pid: child_pid_obj,
        thread_group: thread_group.clone(),
        start_time_us: crate::timer::get_time_us(),
        is_kernel: false,
        inner: Mutex::new(crate::task::TaskControlBlockInner {
            exit_code: 0,
            clone_flags: clone_bits,
            parent: child_parent,
            children: Vec::new(),
            fd_table,
            cwd: fs_snapshot.cwd.clone(),
            root: fs_snapshot.root.clone(),
            exec_path,
            pgid,
            sid,
            program_break: mm_snapshot.program_break,
            mapped_break: mm_snapshot.mapped_break,
            next_mmap: mm_snapshot.next_mmap,
            rlimit_nofile,
            rlimit_nofile_max,
            rlimit_fsize,
            rlimit_fsize_max,
            rlimit_core,
            rlimit_core_max,
            clear_child_tid: if (clone_bits & CLONE_CHILD_CLEARTID) != 0 {
                child_tid
            } else {
                0
            },
            robust_list_head: 0,
            robust_list_len: 0,
            interval_timers: crate::syscall::other::EMPTY_INTERVAL_TIMERS,
            default_timer_slack_ns: inherited_timer_slack_ns,
            current_timer_slack_ns: inherited_timer_slack_ns,
        }),
        task_ctx: crate::task::KernelCtx::new(crate::task::context::TaskContext::zero_init()),
        memory_set,
        fs,
        mm,
        credentials: Mutex::new(credentials),
        signal_actions,
        signal_state: Mutex::new(crate::syscall::signal::SignalState::fork_from(
            signal_blocked,
        )),
        trap_frame: Mutex::new(Some(child_tf)),
        status: Mutex::new(crate::task::TaskStatus::Ready),
        block_reason: Mutex::new(None),
        wait_outcome: Mutex::new(None),
        wait_token: AtomicUsize::new(0),
        sched_policy: AtomicUsize::new(parent.sched_policy.load(Ordering::Relaxed)),
        sched_priority: AtomicUsize::new(parent.sched_priority.load(Ordering::Relaxed)),
    });
    crate::task::manager::register_task(&child);
    thread_group.add_member(&child);
    if !share_vm {
        crate::syscall::mm::inherit_task_shared_memory(&child);
    }
    // RISC-V uses clone(flags, stack, parent_tidptr, tls, child_tidptr);
    // LoongArch musl uses clone(flags, stack, parent_tidptr, child_tidptr, tls).
    if (clone_bits & CLONE_PARENT_SETTID) != 0 && parent_tid != 0 {
        write_user_i32(parent_tid, child_pid as i32)?;
    }
    if (clone_bits & CLONE_CHILD_SETTID) != 0 && child_tid != 0 {
        let bytes = (child_pid as i32).to_ne_bytes();
        let mut child_memory = child.memory_set.lock();
        super::user::copy_to_user_in_memory_set(&mut child_memory, child_tid, &bytes)?;
    }

    if !is_thread {
        parent.inner.lock().children.push(child.clone());
    }
    if is_vfork {
        crate::task::manager::add_task_front(child);
    } else {
        crate::task::manager::add_task(child);
    }
    if crate::trap::foreground_driver_active() && !is_vfork {
        crate::task::request_foreground_requeue_front(parent.pid.0);
    }

    log::info!(
        "[syscall] clone(flags={:#x}, stack={:#x}) parent_pid={} parent_tid={} child_tid={}",
        flags,
        stack,
        parent_pid,
        parent_tid_num,
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
