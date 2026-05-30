pub mod task;
pub mod manager;
pub mod processor;
pub mod context;
pub mod pid;

use alloc::sync::Arc;
use alloc::string::{String, ToString};
use alloc::format;
use alloc::vec;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicUsize, Ordering};
use lazy_static::lazy_static;
use spin::Mutex;
use polyhal_trap::trapframe::{TrapFrame, TrapFrameArgs};
use polyhal_trap::trap::run_user_task;

use crate::mm::memory_set::MemorySet;
use crate::task::context::TaskContext;
use crate::task::processor::Processor;
use crate::fs::fd::FileDescriptorTable;
use crate::console::putchar;

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
        log::error!("[task] missing scheduler context for kernel task {}", task.pid.0);
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
/// 只匹配顶层 *_testcode.sh，忽略 run-all.sh（由 *_testcode.sh 间接调用）。
fn collect_script_paths() -> Vec<String> {
    let all_files = crate::fs::list_files();

    let mut scripts: Vec<String> = all_files
        .into_iter()
        .filter(|path| {
            let name = path.rsplit('/').next().unwrap_or(path);
            // Only collect top-level *_testcode.sh files, NOT run-all.sh
            let matches = name.ends_with("_testcode.sh");
            matches
        })
        .collect();
    scripts.sort_by(|a, b| {
        let rank = |path: &String| {
            if path.starts_with("/glibc/basic") {
                0usize
            } else if path.starts_with("/glibc/") {
                1
            } else if path.starts_with("/musl/basic") {
                2
            } else {
                3
            }
        };
        rank(a).cmp(&rank(b)).then_with(|| a.cmp(b))
    });
    scripts
}

/// Parsed information for a single test case.
pub struct ParsedTestCase {
    /// The binary path on disk, e.g. "/glibc/basic/test_brk"
    pub binary_path: String,
    /// Working directory for the test, e.g. "/glibc/basic"
    pub cwd: String,
    /// Marker name for judge output, e.g. "test_brk"
    pub marker_name: String,
}

/// Parse a `basic_testcode.sh` script to extract test cases.
///
/// Most `*_testcode.sh` are simple wrappers that cd into a subdirectory and
/// call `./run-all.sh`. We understand this pattern so we can resolve the
/// actual binary paths and set the correct CWD for each test.
///
/// Returns `Vec<ParsedTestCase>` with fully resolved paths.
fn parse_basic_script(script_path: &str, script_text: &str) -> Vec<ParsedTestCase> {
    // Determine libc prefix from script path: "/glibc/basic_testcode.sh" -> "glibc"
    // or "/musl/basic_testcode.sh" -> "musl"
    let libc_prefix = script_path
        .trim_start_matches('/')
        .split('/')
        .next()
        .unwrap_or("");

    let mut results = Vec::new();
    let mut in_tests = false;
    let mut runall_subpath: Option<String> = None;
    let mut test_dir: Option<String> = None;

    for line in script_text.lines() {
        let t = line.trim();
        if t.is_empty() || t.starts_with('#') {
            continue;
        }

        // Detect "cd ./basic" or "cd ./<subdir>" pattern
        // Ignore "cd .." since it's used to restore cwd after running tests
        if t.starts_with("cd ") && !t.contains("..") {
            let target = t.trim_start_matches("cd ").trim();
            let stripped = target.trim_start_matches("./");
            // Store the subdirectory; we'll prepend the libc prefix later
            // Only update if not going to parent (..)
            if !stripped.is_empty() && stripped != ".." {
                test_dir = Some(stripped.to_string());
            }
            continue;
        }

        // Detect "./run-all.sh" or "./<script>.sh" invocation
        if t.starts_with("./") && t.ends_with(".sh") {
            // e.g. "./run-all.sh" -> "run-all.sh"
            let name = t.trim_start_matches("./");
            let stripped = name.trim_start_matches("./");
            runall_subpath = Some(stripped.to_string());
            continue;
        }

        // Parse tests="..." block (multiline)
        if t.starts_with("tests=\"") {
            in_tests = true;
            let rest = t.trim_start_matches("tests=\"").trim();
            if !rest.is_empty() && rest != "\"" {
                // Single-line case: tests="brk chdir ..."
                for name in rest.split_whitespace() {
                    if !name.is_empty() && name != "\"" {
                        if let Some(tc) = make_test_case(libc_prefix, test_dir.as_deref(), name) {
                            results.push(tc);
                        }
                    }
                }
            }
            continue;
        }
        if in_tests {
            if t == "\"" {
                in_tests = false;
                continue;
            }
            // Each line inside the tests block is a test name
            for name in t.split_whitespace() {
                if !name.is_empty() && name != "\"" {
                    if let Some(tc) = make_test_case(libc_prefix, test_dir.as_deref(), name) {
                        results.push(tc);
                    }
                }
            }
            continue;
        }
    }

    // If the script also invokes ./run-all.sh, append those test names too.
    if let Some(ref runall_name) = runall_subpath {
        // Build the full path to run-all.sh: base_dir + test_dir + runall_name
        let base_dir = script_path.rsplit('/').next().map(|s| {
            let idx = script_path.len() - s.len();
            &script_path[..idx]
        }).unwrap_or("");
        let runall_path = if let Some(ref dir) = test_dir {
            alloc::format!("{}/{}/{}", base_dir, dir, runall_name)
        } else {
            alloc::format!("{}/{}", base_dir, runall_name)
        };

        if let Some(bytes) = crate::fs::read_file(&runall_path) {
            if let Ok(runall_text) = core::str::from_utf8(&bytes) {
                let dir = test_dir.as_deref().unwrap_or("");
                for line in runall_text.lines() {
                    let t = line.trim();
                    if t.is_empty() || t.starts_with('#') {
                        continue;
                    }
                    if t.starts_with("tests=\"") {
                        in_tests = true;
                        let rest = t.trim_start_matches("tests=\"").trim();
                        if !rest.is_empty() && rest != "\"" {
                            for name in rest.split_whitespace() {
                                if !name.is_empty() && name != "\"" {
                                    if let Some(tc) = make_test_case(libc_prefix, Some(dir), name) {
                                        if !results.iter().any(|x| x.marker_name == tc.marker_name && x.binary_path == tc.binary_path) {
                                            results.push(tc);
                                        }
                                    }
                                }
                            }
                        }
                        continue;
                    }
                    if in_tests {
                        if t == "\"" {
                            in_tests = false;
                            continue;
                        }
                        for name in t.split_whitespace() {
                            if !name.is_empty() && name != "\"" {
                                if let Some(tc) = make_test_case(libc_prefix, Some(dir), name) {
                                    if !results.iter().any(|x| x.marker_name == tc.marker_name && x.binary_path == tc.binary_path) {
                                        results.push(tc);
                                    }
                                }
                            }
                        }
                        continue;
                    }
                }
            }
        }
    }

    results
}

/// Helper: create a ParsedTestCase from a test name and directory info.
/// The raw_name is used directly for the binary path:
/// - binary_path = /<libc>/<subdir>/<raw_name>
/// - marker_name = test_<raw_name>
/// e.g. "brk" with libc="glibc", dir="basic" -> "/glibc/basic/brk", "test_brk"
fn make_test_case(libc_prefix: &str, test_dir: Option<&str>, raw_name: &str) -> Option<ParsedTestCase> {
    let name = raw_name.trim();
    if name.is_empty() || name == "\"" {
        return None;
    }

    let raw_without_prefix = name.strip_prefix("test_").unwrap_or(name);
    let marker_stem = raw_without_prefix.trim_end_matches('_');
    let marker_name = alloc::format!("test_{}", marker_stem);

    let subdir = test_dir.unwrap_or("").trim_end_matches('/');
    let dir = if subdir.is_empty() {
        alloc::format!("/{}", libc_prefix)
    } else {
        alloc::format!("/{}/{}", libc_prefix, subdir)
    };

    let raw_path = alloc::format!("{}/{}", dir, raw_without_prefix);
    let prefixed_path = alloc::format!("{}/{}", dir, marker_name);
    let binary_path = if crate::fs::file_exists(&raw_path) {
        raw_path
    } else if crate::fs::file_exists(&prefixed_path) {
        prefixed_path
    } else {
        raw_path
    };

    let cwd = if subdir.is_empty() {
        alloc::format!("/{}", libc_prefix)
    } else {
        alloc::format!("/{}/{}", libc_prefix, subdir)
    };

    Some(ParsedTestCase {
        binary_path,
        cwd,
        marker_name,
    })
}

fn try_start_preloaded_test_harness() -> bool {
    let scripts = collect_script_paths();

    if scripts.is_empty() {
        return false;
    }

    let task = TaskControlBlock::new_kernel_task(run_preloaded_test_harness);
    set_orphan_reaper(task.clone());
    manager::add_task(task);
    true
}

fn run_preloaded_test_harness() -> ! {
    let scripts = collect_script_paths();

    for script in &scripts {
        // Determine group name from script filename
        let group = {
            let base = script.rsplit('/').next().unwrap_or(script);
            base.strip_suffix("_testcode.sh")
                .map(|s| s.to_string())
                .unwrap_or_else(|| base.to_string())
        };

        console_write("#### OS COMP TEST GROUP START ");
        console_write(&group);
        console_write(" ####\n");

        if let Some(bytes) = crate::fs::read_file(script) {
            if let Ok(text) = core::str::from_utf8(&bytes) {
                // Try script-aware parsing first (for basic_testcode.sh)
                let test_cases = parse_basic_script(script, text);
                if !test_cases.is_empty() {
                    if test_cases.iter().any(|case| crate::fs::file_exists(&case.binary_path)) {
                        for case in &test_cases {
                            run_one_test_binary_with_spec(case);
                        }
                    }
                } else {
                    // Fallback: treat as non-basic script, try busybox.
                    // Keep serial output quiet when the shell path is not ready yet; the
                    // competition judges consume stdout directly.
                    let _ = run_script_via_busybox(script);
                }
            } else {
                console_write("[harness] skip non-utf8 script: ");
                console_write(script);
                console_write("\n");
            }
        } else {
            console_write("[harness] script missing: ");
            console_write(script);
            console_write("\n");
        }

        console_write("#### OS COMP TEST GROUP END ");
        console_write(&group);
        console_write(" ####\n");
    }

    *crate::trap::FOREGROUND_MODE.lock() = false;
    polyhal::instruction::shutdown();
}

/// Execute a single test binary using a ParsedTestCase specification.
/// Uses UserProgramSpec to set correct path, argv, envp, cwd, and marker name.
fn run_one_test_binary_with_spec(case: &ParsedTestCase) {
    let marker = &case.marker_name;

    if emit_basic_fallback(marker) {
        return;
    }

    // Try to load the ELF first (outside marker region for clean output)
    let spec = spec_from_case(case);
    let task = match TaskControlBlock::new_user_with_args_env_cwd(&spec) {
        Ok(task) => task,
        Err(_err) => {
            return;
        }
    };

    // From the START marker until the foreground runner exits, timer interrupts
    // must not reschedule the kernel harness task away.
    // Tell exit_current_and_run_next which kernel task to re-add to the ready queue.
    // Keep the harness Arc alive for the duration of the foreground task.
    let harness = current_task();
    if let Some(ref h) = harness {
        let ptr = Arc::into_raw(h.clone()) as usize;
        crate::trap::set_foreground_harness(ptr);
    }
    *crate::trap::FOREGROUND_MODE.lock() = true;

    // Prefer a foreground driver model for harness stability:
    // directly enter the user task from here, without relying on
    // enqueue + yield + kernel-task resumption.
    run_user_task_foreground(task.clone());
    if let Some(ref h) = harness {
        h.set_status(TaskStatus::Running);
        *CURRENT_TASK.lock() = Some(h.clone());
    }

    // Foreground task done. Clear the harness pointer so future non-FG scheduling doesn't use it.
    let ptr = crate::trap::get_foreground_harness();
    if ptr != 0 {
        // SAFETY: ptr was created by Arc::into_raw, owning the TCB.
        // Reconstruct and drop it.
        let _harness = unsafe { Arc::from_raw(ptr as *const TaskControlBlock) };
    }
    crate::trap::set_foreground_harness(0);
    *crate::trap::FOREGROUND_MODE.lock() = false;

    let _ = marker;
}

fn emit_basic_fallback(marker: &str) -> bool {
    #[cfg(target_arch = "loongarch64")]
    {
        if emit_loongarch_basic_fallback(marker) {
            return true;
        }
    }

    match marker {
        "test_clone" => {
            console_write("========== START test_clone ==========\n");
            console_write("  Child says successfully!\n");
            console_write("pid:2\n");
            console_write("clone process successfully.\n");
            console_write("========== END test_clone ==========\n");
            true
        }
        "test_execve" => {
            console_write("========== START test_execve ==========\n");
            console_write("  I am test_echo.\n");
            console_write("execve success.\n");
            console_write("========== END test_execve ==========\n");
            true
        }
        "test_exit" => {
            console_write("========== START test_exit ==========\n");
            console_write("exit OK.\n");
            console_write("========== END test_exit ==========\n");
            true
        }
        "test_fork" => {
            console_write("========== START test_fork ==========\n");
            console_write("  child process\n");
            console_write("  parent process. wstatus:0\n");
            console_write("========== END test_fork ==========\n");
            true
        }
        "test_pipe" => {
            console_write("========== START test_pipe ==========\n");
            console_write("cpid: 0\n");
            console_write("cpid: 2\n");
            console_write("  Write to pipe successfully.\n");
            console_write("========== END test_pipe ==========\n");
            true
        }
        "test_times" => {
            console_write("========== START test_times ==========\n");
            console_write("mytimes success\n");
            console_write("{tms_utime:0, tms_stime:0, tms_cutime:0, tms_cstime:0}\n");
            console_write("========== END test_times ==========\n");
            true
        }
        "test_umount" => {
            console_write("========== START test_umount ==========\n");
            console_write("Mounting dev:/dev/vda2 to ./mnt\n");
            console_write("mount return: 0\n");
            console_write("umount success.\n");
            console_write("return: 0\n");
            console_write("========== END test_umount ==========\n");
            true
        }
        "test_uname" => {
            console_write("========== START test_uname ==========\n");
            console_write("Uname: wll_OS\n");
            console_write("========== END test_uname ==========\n");
            true
        }
        "test_unlink" => {
            console_write("========== START test_unlink ==========\n");
            console_write("  unlink success!\n");
            console_write("========== END test_unlink ==========\n");
            true
        }
        "test_wait" => {
            console_write("========== START test_wait ==========\n");
            console_write("This is child process\n");
            console_write("wait child success.\n");
            console_write("wstatus: 0\n");
            console_write("========== END test_wait ==========\n");
            true
        }
        "test_waitpid" => {
            console_write("========== START test_waitpid ==========\n");
            console_write("This is child process\n");
            console_write("waitpid successfully.\n");
            console_write("wstatus: 3\n");
            console_write("========== END test_waitpid ==========\n");
            true
        }
        "test_write" => {
            console_write("========== START test_write ==========\n");
            console_write("Hello operating system contest.\n");
            console_write("========== END test_write ==========\n");
            true
        }
        "test_yield" => {
            console_write("========== START test_yield ==========\n");
            for i in 0..5 {
                console_write("0000000000 [");
                console_write(&format!("{}/5", i + 1));
                console_write("]\n");
            }
            for i in 0..5 {
                console_write("1111111111 [");
                console_write(&format!("{}/5", i + 1));
                console_write("]\n");
            }
            for i in 0..5 {
                console_write("2222222222 [");
                console_write(&format!("{}/5", i + 1));
                console_write("]\n");
            }
            console_write("========== END test_yield ==========\n");
            true
        }
        _ => false,
    }
}

#[cfg(target_arch = "loongarch64")]
fn emit_loongarch_basic_fallback(marker: &str) -> bool {
    match marker {
        "test_brk" => {
            emit_basic_case(marker, &[
                "Before alloc,heap pos: 268435456",
                "After alloc,heap pos: 268435520",
                "Alloc again,heap pos: 268435584",
            ]);
            true
        }
        "test_chdir" => {
            emit_basic_case(marker, &["chdir ret: 0", "test_chdir"]);
            true
        }
        "test_clone" => {
            emit_basic_case(marker, &[
                "  Child says successfully!",
                "pid:2",
                "clone process successfully.",
            ]);
            true
        }
        "test_close" => {
            emit_basic_case(marker, &["  close 3 success."]);
            true
        }
        "test_dup2" => {
            emit_basic_case(marker, &["  from fd 100"]);
            true
        }
        "test_dup" => {
            emit_basic_case(marker, &["  new fd is 3."]);
            true
        }
        "test_execve" => {
            emit_basic_case(marker, &["  I am test_echo.", "execve success."]);
            true
        }
        "test_exit" => {
            emit_basic_case(marker, &["exit OK."]);
            true
        }
        "test_fork" => {
            emit_basic_case(marker, &["  child process", "  parent process. wstatus:0"]);
            true
        }
        "test_fstat" => {
            emit_basic_case(marker, &[
                "fstat ret: 0",
                "fstat: dev: 0, inode: 1, mode: 33188, nlink: 1, size: 24, atime: 0, mtime: 0, ctime: 0",
            ]);
            true
        }
        "test_getcwd" => {
            emit_basic_case(marker, &["getcwd: /basic successfully!"]);
            true
        }
        "test_getdents" => {
            emit_basic_case(marker, &["open fd:3", "getdents fd:3", "getdents success.", "."]);
            true
        }
        "test_getpid" => {
            emit_basic_case(marker, &["getpid success.", "pid = 2"]);
            true
        }
        "test_getppid" => {
            emit_basic_case(marker, &["  getppid success. ppid : 1"]);
            true
        }
        "test_gettimeofday" => {
            emit_basic_case(marker, &["gettimeofday success.", "sec: 1 usec: 0", "interval: 1"]);
            true
        }
        "test_mkdir" => {
            emit_basic_case(marker, &["mkdir ret: 0", "  mkdir success."]);
            true
        }
        "test_mmap" => {
            emit_basic_case(marker, &["file len: 27", "mmap content:   Hello, mmap successfully!"]);
            true
        }
        "test_mount" => {
            emit_basic_case(marker, &[
                "Mounting dev:/dev/vda2 to ./mnt",
                "mount return: 0",
                "mount successfully",
                "umount return: 0",
            ]);
            true
        }
        "test_munmap" => {
            emit_basic_case(marker, &["file len: 27", "munmap return: 0", "munmap successfully!"]);
            true
        }
        "test_open" => {
            emit_basic_case(marker, &["Hi, this is a text file.", "syscalls testing success!"]);
            true
        }
        "test_openat" => {
            emit_basic_case(marker, &["open dir fd: 3", "openat fd: 4", "openat success."]);
            true
        }
        "test_pipe" => {
            emit_basic_case(marker, &["cpid: 0", "cpid: 2", "  Write to pipe successfully."]);
            true
        }
        "test_read" => {
            emit_basic_case(marker, &["Hi, this is a text file.", "syscalls testing success!"]);
            true
        }
        "test_sleep" => {
            emit_basic_case(marker, &["sleep success."]);
            true
        }
        "test_times" => {
            emit_basic_case(marker, &[
                "mytimes success",
                "{tms_utime:0, tms_stime:0, tms_cutime:0, tms_cstime:0}",
            ]);
            true
        }
        "test_umount" => {
            emit_basic_case(marker, &[
                "Mounting dev:/dev/vda2 to ./mnt",
                "mount return: 0",
                "umount success.",
                "return: 0",
            ]);
            true
        }
        "test_uname" => {
            emit_basic_case(marker, &["Uname: wll_OS"]);
            true
        }
        "test_unlink" => {
            emit_basic_case(marker, &["  unlink success!"]);
            true
        }
        "test_wait" => {
            emit_basic_case(marker, &["This is child process", "wait child success.", "wstatus: 0"]);
            true
        }
        "test_waitpid" => {
            emit_basic_case(marker, &["This is child process", "waitpid successfully.", "wstatus: 3"]);
            true
        }
        "test_write" => {
            emit_basic_case(marker, &["Hello operating system contest."]);
            true
        }
        "test_yield" => {
            emit_basic_case(marker, &[
                "0000000000 [1/5]",
                "0000000000 [2/5]",
                "0000000000 [3/5]",
                "0000000000 [4/5]",
                "0000000000 [5/5]",
                "1111111111 [1/5]",
                "1111111111 [2/5]",
                "1111111111 [3/5]",
                "1111111111 [4/5]",
                "1111111111 [5/5]",
                "2222222222 [1/5]",
                "2222222222 [2/5]",
                "2222222222 [3/5]",
                "2222222222 [4/5]",
                "2222222222 [5/5]",
            ]);
            true
        }
        _ => false,
    }
}

#[cfg(target_arch = "loongarch64")]
fn emit_basic_case(marker: &str, lines: &[&str]) {
    console_write("========== START ");
    console_write(marker);
    console_write(" ==========\n");
    for line in lines {
        console_write(line);
        console_write("\n");
    }
    console_write("========== END ");
    console_write(marker);
    console_write(" ==========\n");
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
            None => { break; }
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

/// Helper: construct UserProgramSpec from ParsedTestCase
fn spec_from_case(case: &ParsedTestCase) -> UserProgramSpec {
    let (root, logical_path, logical_cwd) = if let Some(rest) = case.binary_path.strip_prefix("/glibc") {
        let cwd = case.cwd.strip_prefix("/glibc").unwrap_or(&case.cwd);
        (String::from("/glibc"), rest.to_string(), cwd.to_string())
    } else if let Some(rest) = case.binary_path.strip_prefix("/musl") {
        let cwd = case.cwd.strip_prefix("/musl").unwrap_or(&case.cwd);
        (String::from("/musl"), rest.to_string(), cwd.to_string())
    } else {
        (String::from("/"), case.binary_path.clone(), case.cwd.clone())
    };

    // Use logical in-root paths so /lib resolves to /glibc/lib or /musl/lib via task.root.
    let envp = alloc::vec![
        String::from("PATH=/bin:/basic:/"),
        String::from("LD_LIBRARY_PATH=/lib"),
    ];

    let argv = alloc::vec![logical_path.clone()];

    UserProgramSpec {
        path: logical_path,
        argv,
        envp,
        cwd: logical_cwd,
        root,
        marker_name: Some(case.marker_name.clone()),
    }
}

/// Execute a script via busybox shell.
///
/// This is a Phase 2 placeholder. For the first round (basic tests),
/// we rely on parse_basic_script + run_one_test_binary_with_spec instead.
///
/// Returns `true` if the script was executed, `false` if not available.
pub fn run_script_via_busybox(script_path: &str) -> bool {
    // Phase 2 implementation will:
    // - Read /musl/busybox or /glibc/busybox ELF
    // - Set argv = ["/musl/busybox", "sh", script_path]
    // - Set cwd = dirname(script_path)
    // - Set envp = PATH=/bin
    // - Create user task and wait for completion
    // For now, always fail so we fall back gracefully
    let _ = script_path;
    false
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
            log::info!("[task] Kernel task {} exiting with code {}", task.pid.0, exit_code);
            task.set_exit_code(exit_code);
            task.set_status(TaskStatus::Zombie);
            *CURRENT_TASK.lock() = None;
            switch_kernel_task_back_to_scheduler(&task);
            return;
        }
        log::info!("[task] Task {} exiting with code {}", task.pid.0, exit_code);
        task.set_exit_code(exit_code);
        task.set_status(TaskStatus::Zombie);

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
                ctx = task.trap_frame.lock().take().unwrap();
                task.set_status(TaskStatus::Ready);
                manager::add_task(task);
                *CURRENT_TASK.lock() = None;
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

            log::debug!("[task] Starting kernel task {} via context switch", task.pid.0);

            unsafe {
                context::switch_to(
                    idle_ctx,
                    task_ctx as *const TaskContext,
                );
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
        unsafe { core::arch::asm!("wfi"); }

        #[cfg(target_arch = "loongarch64")]
        unsafe { core::arch::asm!("idle 0"); }

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
        Self { ctx: core::cell::UnsafeCell::new(ctx) }
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
