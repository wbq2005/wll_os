use alloc::format;
use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec::Vec;
use polyhal_trap::trap::run_user_task;

use super::{
    current_task, manager, requeue_after_user_run, set_orphan_reaper, TaskControlBlock, TaskStatus,
    UserProgramSpec, CURRENT_TASK,
};
use crate::console::putchar;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TestGroup {
    Basic,
    Busybox,
    Lua,
    LibcTest,
    Iozone,
    UnixBench,
    Ltp,
    Iperf,
    Netperf,
}

impl TestGroup {
    fn aliases(self) -> &'static [&'static str] {
        match self {
            TestGroup::Basic => &["basic"],
            TestGroup::Busybox => &["busybox"],
            TestGroup::Lua => &["lua"],
            TestGroup::LibcTest => &["libc-test", "libctest"],
            TestGroup::Iozone => &["iozone"],
            TestGroup::UnixBench => &["unixbench", "UnixBench"],
            TestGroup::Ltp => &["ltp"],
            TestGroup::Iperf => &["iperf"],
            TestGroup::Netperf => &["netperf"],
        }
    }

    fn rank(self) -> usize {
        match self {
            TestGroup::Basic => 0,
            TestGroup::Busybox => 10,
            TestGroup::Lua => 20,
            TestGroup::LibcTest => 30,
            TestGroup::Iozone => 40,
            TestGroup::UnixBench => 50,
            TestGroup::Ltp => 60,
            TestGroup::Iperf => 70,
            TestGroup::Netperf => 71,
        }
    }

    fn from_stem(stem: &str) -> Option<Self> {
        const GROUPS: &[TestGroup] = &[
            TestGroup::Basic,
            TestGroup::Busybox,
            TestGroup::Lua,
            TestGroup::LibcTest,
            TestGroup::Iozone,
            TestGroup::UnixBench,
            TestGroup::Ltp,
            TestGroup::Iperf,
            TestGroup::Netperf,
        ];

        GROUPS
            .iter()
            .copied()
            .find(|group| group.aliases().iter().any(|alias| *alias == stem))
    }
}

#[cfg(not(feature = "libctest"))]
const DEFAULT_ENABLED_GROUPS: &[TestGroup] =
    &[TestGroup::Basic, TestGroup::Busybox, TestGroup::Lua];

#[cfg(feature = "libctest")]
const DEFAULT_ENABLED_GROUPS: &[TestGroup] = &[TestGroup::LibcTest];

fn console_write(msg: &str) {
    for b in msg.bytes() {
        putchar(b);
    }
}

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

fn is_enabled_script(path: &str) -> bool {
    let Some(group) = testcode_stem(path).and_then(TestGroup::from_stem) else {
        return false;
    };
    DEFAULT_ENABLED_GROUPS
        .iter()
        .any(|enabled| *enabled == group)
}

fn script_rank(path: &str) -> usize {
    let libc_rank = if path.starts_with("/glibc/") {
        0usize
    } else if path.starts_with("/musl/") {
        1
    } else {
        2
    };
    let suite_rank = testcode_stem(path)
        .and_then(TestGroup::from_stem)
        .map(TestGroup::rank)
        .unwrap_or(99);
    suite_rank + libc_rank
}

fn discover_script_paths() -> Vec<String> {
    let scripts: Vec<String> = crate::fs::ext4_vol::ext4_list_all_file_paths()
        .into_iter()
        .filter(|path| is_testcode_script(path))
        .collect();

    #[cfg(feature = "dev-preload")]
    if scripts.is_empty() {
        return crate::fs::list_files()
            .into_iter()
            .filter(|path| is_testcode_script(path))
            .collect();
    }

    scripts
}

fn filter_script_paths(paths: Vec<String>) -> Vec<String> {
    let mut scripts: Vec<String> = paths
        .into_iter()
        .filter(|path| is_enabled_script(path))
        .collect();
    scripts.sort_by(|a, b| script_rank(a).cmp(&script_rank(b)).then_with(|| a.cmp(b)));
    scripts.dedup();
    scripts
}

pub fn collect_script_paths() -> Vec<String> {
    filter_script_paths(discover_script_paths())
}

pub fn try_start_runtime_test_harness() -> bool {
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
        console_write("[harness] SCRIPT ");
        console_write(script);
        console_write("\n");
        if !run_script_via_busybox(script) {
            console_write("[harness] failed to launch script: ");
            console_write(script);
            console_write("\n");
        }
    }

    crate::trap::leave_foreground_driver();
    polyhal::instruction::shutdown();
}

struct ForegroundDriverGuard;

impl ForegroundDriverGuard {
    fn enter() -> Self {
        crate::trap::enter_foreground_driver();
        Self
    }
}

impl Drop for ForegroundDriverGuard {
    fn drop(&mut self) {
        crate::trap::leave_foreground_driver();
    }
}

fn run_user_program_spec_foreground(spec: &UserProgramSpec) -> bool {
    let task = match TaskControlBlock::new_user_with_args_env_cwd(spec) {
        Ok(task) => task,
        Err(_err) => {
            return false;
        }
    };

    let harness = current_task();
    let _foreground = ForegroundDriverGuard::enter();

    run_user_task_foreground(task.clone());
    if let Some(ref h) = harness {
        h.set_status(TaskStatus::Running);
        *CURRENT_TASK.lock() = Some(h.clone());
    }

    true
}

fn abort_foreground_task_tree(root: &Arc<TaskControlBlock>) {
    fn remember_pid(pids: &mut Vec<usize>, pid: usize) {
        if !pids.iter().any(|seen| *seen == pid) {
            pids.push(pid);
        }
    }

    fn terminate_group(
        task: &Arc<TaskControlBlock>,
        killed_tgids: &mut Vec<usize>,
        killed_pids: &mut Vec<usize>,
    ) {
        if task.is_kernel {
            return;
        }
        let tgid = task.thread_group.tgid();
        if killed_tgids.iter().any(|seen| *seen == tgid) {
            return;
        }
        killed_tgids.push(tgid);

        let mut members = task.thread_group.user_members();
        if members.is_empty() {
            members.push(task.clone());
        }
        for member in &members {
            remember_pid(killed_pids, member.pid.0);
            *member.trap_frame.lock() = None;
        }
        crate::task::terminate_task_group(task, -2);
    }

    let mut killed_tgids = Vec::new();
    let mut killed_pids = Vec::new();
    let mut tasks = manager::all_user_tasks();
    if !tasks.iter().any(|task| task.pid.0 == root.pid.0) {
        tasks.push(root.clone());
    }

    for task in tasks {
        if task.status() != TaskStatus::Zombie {
            terminate_group(&task, &mut killed_tgids, &mut killed_pids);
        }
    }

    manager::retain_tasks(|queued| {
        queued.is_kernel || !killed_pids.iter().any(|pid| *pid == queued.pid.0)
    });

    let mut current = CURRENT_TASK.lock();
    if current
        .as_ref()
        .map(|task| killed_pids.iter().any(|pid| *pid == task.pid.0))
        .unwrap_or(false)
    {
        *current = None;
    }
}

fn run_user_task_foreground(task: Arc<TaskControlBlock>) {
    const TIMEOUT_TICKS: usize = 1_000;
    let mut waited = 0usize;

    task.set_status(TaskStatus::Ready);
    manager::add_task(task.clone());

    loop {
        crate::timer::wake_expired_timers();
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
        if matches!(active.status(), TaskStatus::Zombie | TaskStatus::Blocked) {
            continue;
        }

        active.set_status(TaskStatus::Running);
        *CURRENT_TASK.lock() = Some(active.clone());

        let mut tf_guard = active.trap_frame.lock();
        let mut ctx = match tf_guard.as_ref() {
            None => {
                break;
            }
            Some(_) => tf_guard.take().unwrap(),
        };
        drop(tf_guard);

        {
            let ms = active.memory_set.lock();
            ms.activate();
        }

        if !crate::syscall::signal::handle_pending_for_user(&mut ctx) {
            crate::trap::restore_kernel_page_table();
            if active.status() != TaskStatus::Zombie {
                *active.trap_frame.lock() = Some(ctx);
            }
            requeue_after_user_run(active);
            waited += 1;
            continue;
        }

        let _reason = run_user_task(&mut ctx);
        crate::trap::restore_kernel_page_table();

        if active.status() != TaskStatus::Zombie {
            *active.trap_frame.lock() = Some(ctx);
        }

        requeue_after_user_run(active);
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
    let alias_logical = if applet.starts_with('/') {
        applet.to_string()
    } else {
        alloc::format!("/{}", applet)
    };
    let alias_host = crate::fs::apply_root(root, &alias_logical);
    if crate::fs::metadata(&alias_host, true)
        .map(|meta| meta.kind == crate::fs::VfsNodeKind::Regular && (meta.mode & 0o111) != 0)
        .unwrap_or(false)
    {
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
    ensure_busybox_applet_alias(&root, &busybox_host, "sh")?;
    ensure_busybox_applet_alias(&root, &busybox_host, "/bin/sh")?;

    Some(UserProgramSpec {
        path: busybox_path.clone(),
        argv: alloc::vec![
            busybox_path.clone(),
            String::from("sh"),
            logical_script.clone(),
        ],
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

pub fn run_script_via_busybox(script_path: &str) -> bool {
    let Some(spec) = busybox_script_spec(script_path) else {
        return false;
    };
    run_user_program_spec_foreground(&spec)
}
