use super::{
    current_task, manager, purge_exited_user_task_for_foreground, requeue_after_user_run,
    run_current_user_task_until_reschedule, set_orphan_reaper, TaskControlBlock, TaskStatus,
    UserProgramSpec, CURRENT_TASK,
};
use crate::console::putchar;
use crate::utils::error::SysErrNo;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec::Vec;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TestGroup {
    Basic,
    Busybox,
    Lua,
    LibcTest,
    Iozone,
    LibcBench,
    Lmbench,
    Cyclictest,
    UnixBench,
    Ltp,
    Iperf,
    Netperf,
    Cagent,
    Buildstorm,
}

impl TestGroup {
    fn aliases(self) -> &'static [&'static str] {
        match self {
            TestGroup::Basic => &["basic"],
            TestGroup::Busybox => &["busybox"],
            TestGroup::Lua => &["lua"],
            TestGroup::LibcTest => &["libc-test", "libctest"],
            TestGroup::Iozone => &["iozone"],
            TestGroup::LibcBench => &["libcbench", "libc-bench"],
            TestGroup::Lmbench => &["lmbench"],
            TestGroup::Cyclictest => &["cyclictest"],
            TestGroup::UnixBench => &["unixbench", "UnixBench"],
            TestGroup::Ltp => &["ltp"],
            TestGroup::Iperf => &["iperf"],
            TestGroup::Netperf => &["netperf"],
            TestGroup::Cagent => &["cagent"],
            TestGroup::Buildstorm => &["buildstorm"],
        }
    }

    fn rank(self) -> usize {
        match self {
            TestGroup::Basic => 0,
            TestGroup::Busybox => 10,
            TestGroup::Lua => 20,
            TestGroup::Iozone => 30,
            TestGroup::LibcTest => 40,
            TestGroup::LibcBench => 50,
            TestGroup::Cyclictest => 55,
            TestGroup::Lmbench => 58,
            TestGroup::UnixBench => 60,
            TestGroup::Ltp => 70,
            TestGroup::Iperf => 80,
            TestGroup::Netperf => 81,
            TestGroup::Cagent => 82,
            TestGroup::Buildstorm => 83,
        }
    }

    fn from_stem(stem: &str) -> Option<Self> {
        const GROUPS: &[TestGroup] = &[
            TestGroup::Basic,
            TestGroup::Busybox,
            TestGroup::Lua,
            TestGroup::LibcTest,
            TestGroup::Iozone,
            TestGroup::LibcBench,
            TestGroup::Lmbench,
            TestGroup::Cyclictest,
            TestGroup::UnixBench,
            TestGroup::Ltp,
            TestGroup::Iperf,
            TestGroup::Netperf,
            TestGroup::Cagent,
            TestGroup::Buildstorm,
        ];

        GROUPS
            .iter()
            .copied()
            .find(|group| group.aliases().iter().any(|alias| *alias == stem))
    }

    fn enabled_by_filter(self) -> bool {
        match option_env!("WLL_HARNESS_GROUPS") {
            Some(filter) => {
                for part in filter.split(',') {
                    let part = part.trim();
                    if part.is_empty() {
                        continue;
                    }
                    if self.aliases().iter().any(|alias| *alias == part) {
                        return true;
                    }
                }
                false
            }
            None => true,
        }
    }
}

pub fn harness_filter_active() -> bool {
    option_env!("WLL_HARNESS_GROUPS")
        .map(|filter| filter.split(',').any(|part| !part.trim().is_empty()))
        .unwrap_or(false)
}

#[cfg(all(
    not(feature = "libctest"),
    not(feature = "ltp"),
    not(feature = "iozone"),
    feature = "lmbench"
))]
const DEFAULT_ENABLED_GROUPS: &[TestGroup] = &[TestGroup::Lmbench];

#[cfg(all(
    not(feature = "libctest"),
    not(feature = "ltp"),
    not(feature = "iozone"),
    not(feature = "lmbench")
))]
const DEFAULT_ENABLED_GROUPS: &[TestGroup] = &[
    TestGroup::Basic,
    TestGroup::Busybox,
    TestGroup::Lua,
    TestGroup::LibcTest,
];

#[cfg(feature = "libctest")]
const DEFAULT_ENABLED_GROUPS: &[TestGroup] = &[TestGroup::LibcTest];

#[cfg(all(
    not(feature = "libctest"),
    not(feature = "ltp"),
    feature = "iozone",
    not(feature = "lmbench")
))]
const DEFAULT_ENABLED_GROUPS: &[TestGroup] = &[
    TestGroup::Basic,
    TestGroup::Busybox,
    TestGroup::Lua,
    TestGroup::Iozone,
    TestGroup::LibcTest,
    TestGroup::LibcBench,
];

#[cfg(all(
    not(feature = "libctest"),
    not(feature = "ltp"),
    feature = "iozone",
    feature = "lmbench"
))]
const DEFAULT_ENABLED_GROUPS: &[TestGroup] = &[
    TestGroup::Basic,
    TestGroup::Busybox,
    TestGroup::Lua,
    TestGroup::Iozone,
    TestGroup::LibcTest,
    TestGroup::LibcBench,
    TestGroup::Lmbench,
];

#[cfg(all(
    not(feature = "libctest"),
    feature = "ltp",
    not(feature = "iozone"),
    not(feature = "lmbench")
))]
const DEFAULT_ENABLED_GROUPS: &[TestGroup] = &[TestGroup::Ltp];

#[cfg(all(
    not(feature = "libctest"),
    feature = "ltp",
    not(feature = "iozone"),
    feature = "lmbench"
))]
const DEFAULT_ENABLED_GROUPS: &[TestGroup] = &[TestGroup::Lmbench, TestGroup::Ltp];

#[cfg(all(
    not(feature = "libctest"),
    feature = "ltp",
    feature = "iozone",
    not(feature = "lmbench")
))]
const DEFAULT_ENABLED_GROUPS: &[TestGroup] = &[
    TestGroup::Basic,
    TestGroup::Busybox,
    TestGroup::Lua,
    TestGroup::Iozone,
    TestGroup::LibcTest,
    TestGroup::LibcBench,
    TestGroup::Ltp,
];

#[cfg(all(
    not(feature = "libctest"),
    feature = "ltp",
    feature = "iozone",
    feature = "lmbench"
))]
const DEFAULT_ENABLED_GROUPS: &[TestGroup] = &[
    TestGroup::Basic,
    TestGroup::Busybox,
    TestGroup::Lua,
    TestGroup::Iozone,
    TestGroup::LibcTest,
    TestGroup::LibcBench,
    TestGroup::Lmbench,
    TestGroup::Ltp,
];

fn console_write(msg: &str) {
    for b in msg.bytes() {
        putchar(b);
    }
}

#[derive(Clone, Copy, Debug)]
enum ScriptLaunchError {
    InvalidPath,
    MissingBusybox,
    MissingScript,
    CreateTask(SysErrNo),
}

fn log_script_launch_error(script: &str, err: ScriptLaunchError) {
    console_write("[harness] failed to launch script: ");
    console_write(script);
    console_write(" (");
    match err {
        ScriptLaunchError::InvalidPath => console_write("invalid path"),
        ScriptLaunchError::MissingBusybox => console_write("missing busybox"),
        ScriptLaunchError::MissingScript => console_write("missing script"),
        ScriptLaunchError::CreateTask(errno) => {
            console_write("create task ");
            console_write(&format!("{:?}", errno));
        }
    }
    console_write(")\n");
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
    let libc_enabled = match option_env!("WLL_HARNESS_LIBC") {
        Some("glibc") | None => path.starts_with("/glibc/"),
        Some("musl") => path.starts_with("/musl/"),
        Some("both") => path.starts_with("/glibc/") || path.starts_with("/musl/"),
        Some(_) => false,
    };
    if !libc_enabled {
        return false;
    }
    if harness_filter_active() {
        group.enabled_by_filter()
    } else {
        DEFAULT_ENABLED_GROUPS
            .iter()
            .any(|enabled| *enabled == group)
    }
}

fn script_rank(path: &str) -> usize {
    let group = testcode_stem(path).and_then(TestGroup::from_stem);
    let libc_rank = match group {
        Some(TestGroup::Iozone) if path.starts_with("/musl/") => 0usize,
        Some(TestGroup::Iozone) if path.starts_with("/glibc/") => 1,
        _ if path.starts_with("/glibc/") => 0,
        _ if path.starts_with("/musl/") => 1,
        _ => 2,
    };
    let suite_rank = group.map(TestGroup::rank).unwrap_or(99);
    suite_rank + libc_rank
}

fn discover_script_paths() -> Vec<String> {
    // Official runtime images keep suite launchers near the filesystem root.
    // Check the root and one directory level first so selecting a single suite
    // does not require recursively walking toolchains and source trees.  Keep
    // the recursive walk as a compatibility fallback for other layouts.
    let mut scripts = Vec::new();
    if let Ok(root_entries) = crate::fs::ext4_vol::ext4_list_dir("/") {
        for (name, is_dir) in root_entries {
            let path = alloc::format!("/{}", name);
            if !is_dir {
                if is_testcode_script(&path) {
                    scripts.push(path);
                }
                continue;
            }
            if let Ok(entries) = crate::fs::ext4_vol::ext4_list_dir(&path) {
                for (child, child_is_dir) in entries {
                    if child_is_dir {
                        continue;
                    }
                    let child_path = alloc::format!("{}/{}", path, child);
                    if is_testcode_script(&child_path) {
                        scripts.push(child_path);
                    }
                }
            }
        }
    }

    if scripts.is_empty() {
        scripts = crate::fs::ext4_vol::ext4_list_all_file_paths()
            .into_iter()
            .filter(|path| is_testcode_script(path))
            .collect();
    }

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
    #[cfg(feature = "libctest")]
    {
        run_libctest_collection_harness(true);
        shutdown_after_harness();
    }

    #[cfg(not(feature = "libctest"))]
    {
        let scripts = collect_script_paths();
        let mut ran_libctest = false;

        for script in &scripts {
            let group = testcode_stem(script).and_then(TestGroup::from_stem);
            if group == Some(TestGroup::LibcTest) {
                if !ran_libctest {
                    run_libctest_collection_harness(false);
                    ran_libctest = true;
                }
                continue;
            }
            console_write("[harness] SCRIPT ");
            console_write(script);
            console_write("\n");
            if let Err(err) = run_script(script) {
                log_script_launch_error(script, err);
            }
        }

        shutdown_after_harness();
    }
}

fn shutdown_after_harness() -> ! {
    console_write("[harness] ALL TESTS DONE, shutting down\n");
    crate::trap::interrupts::disable_interrupt();
    crate::trap::leave_foreground_driver();
    polyhal::instruction::shutdown();
}

fn run_libctest_collection_harness(include_glibc: bool) {
    const STATIC_CASES: &[&str] = &[
        "argv",
        "basename",
        "clock_gettime",
        "dirname",
        "env",
        "fdopen",
        "iconv_open",
        "inet_pton",
        "memstream",
        "pthread_cancel",
        "pthread_cond",
        "pthread_tsd",
        "qsort",
        "random",
        "search_hsearch",
        "search_insque",
        "search_lsearch",
        "search_tsearch",
        "setjmp",
        "snprintf",
        "socket",
        "sscanf_long",
        "stat",
        "string",
        "string_memcpy",
        "string_memmem",
        "string_memset",
        "string_strchr",
        "string_strcspn",
        "string_strstr",
        "strptime",
        "strtod",
        "strtod_simple",
        "strtof",
        "strtold",
        "fflush_exit",
        "fgets_eof",
        "fpclassify_invalid_ld80",
        "ftello_unflushed_append",
        "getpwnam_r_crash",
        "getpwnam_r_errno",
        "iconv_roundtrips",
        "inet_ntop_v4mapped",
        "inet_pton_empty_last_field",
        "iswspace_null",
        "lrand48_signextend",
        "lseek_large",
        "malloc_0",
        "mbsrtowcs_overflow",
        "memmem_oob_read",
        "memmem_oob",
        "mkdtemp_failure",
        "mkstemp_failure",
        "printf_1e9_oob",
        "printf_fmt_g_round",
        "printf_fmt_g_zeros",
        "printf_fmt_n",
        "pthread_robust_detach",
        "pthread_cancel_sem_wait",
        "pthread_condattr_setclock",
        "pthread_exit_cancel",
        "pthread_once_deadlock",
        "pthread_rwlock_ebusy",
        "putenv_doublefree",
        "regex_backref_0",
        "regex_bracket_icase",
        "regex_negated_range",
        "regexec_nosub",
        "rewind_clear_error",
        "rlimit_open_files",
        "scanf_bytes_consumed",
        "scanf_match_literal_eof",
        "scanf_nullbyte_char",
        "sigprocmask_internal",
        "sscanf_eof",
        "statvfs",
        "strverscmp",
        "syscall_sign_extend",
        "uselocale_0",
        "wcsncpy_read_overflow",
        "wcsstr_false_negative",
    ];
    const DYNAMIC_CASES: &[&str] = &[
        "argv",
        "basename",
        "clock_gettime",
        "dirname",
        "dlopen",
        "env",
        "fdopen",
        "iconv_open",
        "inet_pton",
        "memstream",
        "pthread_cond",
        "pthread_tsd",
        "qsort",
        "random",
        "search_hsearch",
        "search_insque",
        "search_lsearch",
        "search_tsearch",
        "sem_init",
        "setjmp",
        "snprintf",
        "socket",
        "sscanf_long",
        "stat",
        "string",
        "string_memcpy",
        "string_memmem",
        "string_memset",
        "string_strchr",
        "string_strcspn",
        "string_strstr",
        "strptime",
        "strtod",
        "strtod_simple",
        "strtof",
        "strtold",
        "fflush_exit",
        "fgets_eof",
        "fpclassify_invalid_ld80",
        "ftello_unflushed_append",
        "getpwnam_r_crash",
        "getpwnam_r_errno",
        "iconv_roundtrips",
        "inet_ntop_v4mapped",
        "inet_pton_empty_last_field",
        "iswspace_null",
        "lrand48_signextend",
        "lseek_large",
        "malloc_0",
        "mbsrtowcs_overflow",
        "memmem_oob_read",
        "memmem_oob",
        "mkdtemp_failure",
        "mkstemp_failure",
        "printf_1e9_oob",
        "printf_fmt_g_round",
        "printf_fmt_g_zeros",
        "printf_fmt_n",
        "pthread_robust_detach",
        "pthread_condattr_setclock",
        "pthread_exit_cancel",
        "pthread_once_deadlock",
        "pthread_rwlock_ebusy",
        "putenv_doublefree",
        "regex_backref_0",
        "regex_bracket_icase",
        "regex_negated_range",
        "regexec_nosub",
        "rewind_clear_error",
        "rlimit_open_files",
        "scanf_bytes_consumed",
        "scanf_match_literal_eof",
        "scanf_nullbyte_char",
        "sigprocmask_internal",
        "sscanf_eof",
        "statvfs",
        "strverscmp",
        "syscall_sign_extend",
        "uselocale_0",
        "wcsncpy_read_overflow",
        "wcsstr_false_negative",
    ];
    const STATIC_RISK_CASES: &[&str] = &[
        "clocale_mbfuncs",
        "fnmatch",
        "fscanf",
        "fwscanf",
        "mbc",
        "pthread_cancel_points",
        "sscanf",
        "strftime",
        "strtol",
        "swprintf",
        "fgetwc_buffering",
        "regex_ere_backref",
        "regex_escaped_high_byte",
        "setvbuf_unget",
        "dn_expand_empty",
        "dn_expand_ptr_0",
        "pthread_cond_smasher",
    ];
    const DYNAMIC_RISK_CASES: &[&str] = &[
        "clocale_mbfuncs",
        "fnmatch",
        "fscanf",
        "fwscanf",
        "mbc",
        "pthread_cancel_points",
        "pthread_cancel",
        "sscanf",
        "strftime",
        "strtol",
        "swprintf",
        "fgetwc_buffering",
        "regex_ere_backref",
        "regex_escaped_high_byte",
        "setvbuf_unget",
        "dn_expand_empty",
        "dn_expand_ptr_0",
        "pthread_cond_smasher",
    ];
    const SEGMENTS: &[(&str, &str, &str, &[&str])] = &[
        (
            "/glibc",
            "libctest-glibc-static",
            "entry-static.exe",
            STATIC_CASES,
        ),
        (
            "/glibc",
            "libctest-glibc-dynamic",
            "entry-dynamic.exe",
            DYNAMIC_CASES,
        ),
        (
            "/musl",
            "libctest-musl-static",
            "entry-static.exe",
            STATIC_CASES,
        ),
        (
            "/musl",
            "libctest-musl-dynamic",
            "entry-dynamic.exe",
            DYNAMIC_CASES,
        ),
        (
            "/musl",
            "libctest-musl-static-daemon",
            "entry-static.exe",
            &["daemon_failure"],
        ),
        (
            "/musl",
            "libctest-musl-dynamic-daemon",
            "entry-dynamic.exe",
            &["daemon_failure"],
        ),
        (
            "/glibc",
            "libctest-glibc-static-late",
            "entry-static.exe",
            &["utime", "wcsstr", "wcstol"],
        ),
        (
            "/glibc",
            "libctest-glibc-dynamic-late",
            "entry-dynamic.exe",
            &["utime", "wcsstr", "wcstol"],
        ),
        (
            "/musl",
            "libctest-musl-static-late",
            "entry-static.exe",
            &["utime", "wcsstr", "wcstol"],
        ),
        (
            "/musl",
            "libctest-musl-dynamic-late",
            "entry-dynamic.exe",
            &["utime", "wcsstr", "wcstol"],
        ),
        (
            "/glibc",
            "libctest-glibc-static-tail",
            "entry-static.exe",
            &["time", "tgmath", "tls_align", "udiv", "ungetc"],
        ),
        (
            "/glibc",
            "libctest-glibc-dynamic-tail",
            "entry-dynamic.exe",
            &[
                "time",
                "tgmath",
                "tls_init",
                "tls_local_exec",
                "tls_get_new_dtv",
                "udiv",
                "ungetc",
            ],
        ),
        (
            "/musl",
            "libctest-musl-static-tail",
            "entry-static.exe",
            &["time", "tgmath", "tls_align", "udiv", "ungetc"],
        ),
        (
            "/musl",
            "libctest-musl-dynamic-tail",
            "entry-dynamic.exe",
            &[
                "time",
                "tgmath",
                "tls_init",
                "tls_local_exec",
                "tls_get_new_dtv",
                "udiv",
                "ungetc",
            ],
        ),
        (
            "/glibc",
            "libctest-glibc-static-risk",
            "entry-static.exe",
            STATIC_RISK_CASES,
        ),
        (
            "/glibc",
            "libctest-glibc-dynamic-risk",
            "entry-dynamic.exe",
            DYNAMIC_RISK_CASES,
        ),
        (
            "/musl",
            "libctest-musl-static-risk",
            "entry-static.exe",
            STATIC_RISK_CASES,
        ),
        (
            "/musl",
            "libctest-musl-dynamic-risk",
            "entry-dynamic.exe",
            DYNAMIC_RISK_CASES,
        ),
    ];
    const EXTRA_SEGMENTS: &[(&str, &str, &str, &[&str])] = &[
        (
            "/musl",
            "libctest-musl-static-extra",
            "/libctest-extra-static.exe",
            &["crypt", "pleval"],
        ),
        (
            "/musl",
            "libctest-musl-dynamic-extra",
            "/libctest-extra-dynamic.exe",
            &["crypt"],
        ),
    ];

    const GROUPS: &[(&str, &str)] = &[("/glibc", "libctest-glibc"), ("/musl", "libctest-musl")];

    for (root, group_name) in GROUPS {
        if !include_glibc && *root != "/musl" {
            continue;
        }
        run_libctest_judge_group(root, group_name, SEGMENTS, EXTRA_SEGMENTS);
    }
}

fn run_libctest_judge_group(
    root: &str,
    group_name: &str,
    segments: &[(&str, &str, &str, &[&str])],
    extra_segments: &[(&str, &str, &str, &[&str])],
) {
    console_write("[harness] LIBCTEST GROUP ");
    console_write(group_name);
    console_write("\n");
    console_write("#### OS COMP TEST GROUP START ");
    console_write(group_name);
    console_write(" ####\n");

    for (segment_root, name, entry, cases) in segments {
        if *segment_root != root {
            continue;
        }
        console_write("[harness] LIBCTEST SEGMENT ");
        console_write(name);
        console_write("\n");
        if !run_libctest_segment(root, name, entry, cases) {
            console_write("[harness] failed to launch libc-test segment: ");
            console_write(name);
            console_write("\n");
        }
    }
    for (segment_root, name, runner, cases) in extra_segments {
        if *segment_root != root {
            continue;
        }
        console_write("[harness] LIBCTEST EXTRA SEGMENT ");
        console_write(name);
        console_write("\n");
        if !run_libctest_extra_segment(root, name, runner, cases) {
            console_write("[harness] failed to launch libc-test extra segment: ");
            console_write(name);
            console_write("\n");
        }
    }

    console_write("#### OS COMP TEST GROUP END ");
    console_write(group_name);
    console_write(" ####\n");
}

fn run_libctest_segment(root: &str, name: &str, entry: &str, cases: &[&str]) -> bool {
    let mut launched = false;

    for case in cases {
        if run_libctest_case(root, entry, case) {
            launched = true;
        } else {
            console_write("[harness] failed to launch libc-test case: ");
            console_write(name);
            console_write(" ");
            console_write(case);
            console_write("\n");
        }
    }

    launched
}

fn run_libctest_case(root: &str, entry: &str, case: &str) -> bool {
    let runtest_path = String::from("/runtest.exe");
    let runtest_host = crate::fs::apply_root(root, &runtest_path);

    crate::fs::read_executable_file(&runtest_host).is_some()
        && run_user_program_spec_foreground(&UserProgramSpec {
            path: runtest_path.clone(),
            argv: alloc::vec![
                runtest_path.clone(),
                String::from("-w"),
                String::from(entry),
                String::from(case),
            ],
            envp: alloc::vec![
                String::from("PATH=.:/:/bin:/usr/bin"),
                String::from("LD_LIBRARY_PATH=/lib"),
                String::from("SHELL=/busybox"),
            ],
            cwd: String::from("/"),
            root: String::from(root),
            marker_name: None,
        })
        .is_ok()
}

fn run_libctest_extra_segment(root: &str, name: &str, runner: &str, cases: &[&str]) -> bool {
    let mut launched = false;

    for case in cases {
        if run_libctest_extra_case(root, runner, case) {
            launched = true;
        } else {
            console_write("[harness] failed to launch libc-test extra case: ");
            console_write(name);
            console_write(" ");
            console_write(case);
            console_write("\n");
        }
    }

    launched
}

fn run_libctest_extra_case(root: &str, runner: &str, case: &str) -> bool {
    let runner_path = String::from(runner);
    let runner_host = crate::fs::apply_root(root, &runner_path);

    crate::fs::read_executable_file(&runner_host).is_some()
        && run_user_program_spec_foreground(&UserProgramSpec {
            path: runner_path.clone(),
            argv: alloc::vec![runner_path.clone(), String::from(case)],
            envp: alloc::vec![
                String::from("PATH=.:/:/bin:/usr/bin"),
                String::from("LD_LIBRARY_PATH=/lib"),
                String::from("SHELL=/busybox"),
            ],
            cwd: String::from("/"),
            root: String::from(root),
            marker_name: None,
        })
        .is_ok()
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

fn run_user_program_spec_foreground(spec: &UserProgramSpec) -> Result<(), SysErrNo> {
    run_user_program_spec_foreground_exit_code(spec).map(|_| ())
}

fn run_user_program_spec_foreground_exit_code(spec: &UserProgramSpec) -> Result<i32, SysErrNo> {
    let harness = current_task();
    let task = match TaskControlBlock::new_user_with_args_env_cwd(spec) {
        Ok(task) => task,
        Err(err) => {
            #[cfg(feature = "buildstorm-diagnostics")]
            console_write(&format!(
                "BUILDSTORM_DIAG task_create_failure path={} root={} cwd={} errno={}\n",
                spec.path,
                spec.root,
                spec.cwd,
                err as usize,
            ));
            return Err(err);
        }
    };

    #[cfg(feature = "buildstorm-diagnostics")]
    let started_at_us = crate::timer::get_time_us();
    #[cfg(feature = "buildstorm-diagnostics")]
    console_write(&format!(
        "BUILDSTORM_DIAG script_task_created pid={} tgid={} path={} root={} cwd={}\n",
        task.pid.0,
        task.thread_group.tgid(),
        spec.path,
        spec.root,
        spec.cwd,
    ));

    // A real LTP binary is normally launched by a shell/test runner inside the
    // runner's session, not as a fresh session leader. Keep each foreground test
    // as its own process-group leader for signal/wait isolation, but inherit the
    // harness session so Linux setpgid(2)/setsid(2) rules see realistic state.
    if let Some(ref h) = harness {
        let harness_sid = h.inner.lock().sid;
        task.inner.lock().sid = harness_sid;
    }
    let _foreground = ForegroundDriverGuard::enter();

    run_user_task_foreground(task.clone(), foreground_timeout_us(spec));
    let exit_code = task.thread_group.exit_code();
    #[cfg(feature = "buildstorm-diagnostics")]
    console_write(&format!(
        "BUILDSTORM_DIAG foreground_wait_end pid={} tgid={} exit={} status={:?} elapsed_us={}\n",
        task.pid.0,
        task.thread_group.tgid(),
        exit_code,
        task.status(),
        crate::timer::get_time_us().saturating_sub(started_at_us),
    ));
    cleanup_foreground_task_tree(&task);
    if let Some(ref h) = harness {
        h.set_status(TaskStatus::Running);
        *CURRENT_TASK.lock() = Some(h.clone());
    }

    Ok(exit_code)
}

/// Independent user-address-space lifecycle regression.
///
/// This deliberately uses a small dynamic user program rather than an official
/// contest script. It covers ELF/interpreter loading, initial stack setup,
/// anonymous user memory, user entry, and normal process teardown.
#[cfg(feature = "smp-regression")]
pub(crate) fn run_user_memory_lifecycle_regression() {
    crate::smp_regression::reset_user_memory_lifecycle_diagnostic();
    let spec = UserProgramSpec {
        path: String::from("/busybox"),
        argv: alloc::vec![
            String::from("/busybox"),
            String::from("sh"),
            String::from("-c"),
            String::from("/busybox true; exit 0"),
        ],
        envp: alloc::vec![
            String::from("PATH=/:/bin:/usr/bin"),
            String::from("LD_LIBRARY_PATH=/lib"),
        ],
        cwd: String::from("/"),
        root: String::from("/glibc"),
        marker_name: None,
    };
    let exit_code =
        run_user_program_spec_foreground_exit_code(&spec).expect("user-memory lifecycle launch");
    #[cfg(feature = "buildstorm-diagnostics")]
    {
        if let Some(failure) = crate::buildstorm_diagnostics::take_first_page_fault_failure() {
            console_write(&format!(
                "BUILDSTORM_DIAG lifecycle_page_fault_failure stage={} errno={} vaddr={:#x}\n",
                failure.stage, failure.errno, failure.vaddr,
            ));
        }
        if let Some(failure) = crate::buildstorm_diagnostics::take_first_exec_failure() {
            console_write(&format!(
                "BUILDSTORM_DIAG lifecycle_exec_failure errno={} pid={} parent_pid={} clone_flags={:#x} stage={} vector_base={:#x} index={} entry_addr={:#x} value_ptr={:#x} slot_vma=[{:#x},{:#x}) slot_flags={:#x} backing={} resident={} page_state={} pte_pa={:#x} path_ptr={:#x} argv_ptr={:#x} envp_ptr={:#x}\n",
                failure.errno,
                failure.pid,
                failure.parent_pid,
                failure.clone_flags,
                failure.stage,
                failure.vector_base,
                failure.index,
                failure.entry_addr,
                failure.value_ptr,
                failure.vma_start,
                failure.vma_end,
                failure.vma_flags,
                failure.backing,
                failure.resident,
                failure.page_state,
                failure.pte_pa,
                failure.path_ptr,
                failure.argv_ptr,
                failure.envp_ptr,
            ));
        }
        if let Some(trap) = crate::buildstorm_diagnostics::take_first_terminal_user_trap() {
            console_write(&format!(
                "BUILDSTORM_DIAG lifecycle_terminal_user_trap kind={} pid={} vaddr={:#x} sepc={:#x} fault_pa={:#x} sepc_pa={:#x} vma=[{:#x},{:#x}) flags={:#x} backing={} resident={} page_state={}\n",
                trap.kind,
                trap.pid,
                trap.vaddr,
                trap.sepc,
                trap.fault_pa,
                trap.sepc_pa,
                trap.vma_start,
                trap.vma_end,
                trap.vma_flags,
                trap.backing,
                trap.resident,
                trap.page_state,
            ));
        }
    }
    if exit_code != 0 {
        if let Some((kind, vaddr, sepc)) =
            crate::smp_regression::user_memory_lifecycle_terminal_trap()
        {
            console_write(&format!(
                "[smp-regression] lifecycle-terminal-trap kind={} vaddr={:#x} sepc={:#x}\n",
                kind, vaddr, sepc
            ));
        }
        panic!(
            "[smp-regression] fail phase=user-memory-lifecycle exit={}",
            exit_code
        );
    }
    console_write("[smp-regression] pass phase=user-memory-lifecycle\n");

    run_directory_abi_regression();
    run_directory_metadata_abi_regression();
    run_script_root_namespace_regression();

    // The busybox probe is intentionally static/self-contained.  A separate
    // diagnostic-only dynamic ELF probe exercises the glibc interpreter and
    // fork/exec path without changing production harness behavior.
    #[cfg(all(feature = "buildstorm-diagnostics", feature = "smp-regression"))]
    run_dynamic_user_memory_lifecycle_probe();
}

#[cfg(feature = "smp-regression")]
fn run_directory_abi_regression() {
    let spec = UserProgramSpec {
        path: String::from("/busybox"),
        argv: alloc::vec![
            String::from("/busybox"),
            String::from("sh"),
            String::from("-c"),
            String::from(
                "set -e; /busybox rm -rf /.wll_dir_abi; \
                 /busybox mkdir -p /.wll_dir_abi/parent/child; \
                 /busybox test -d /.wll_dir_abi/parent/child; \
                 /busybox mkdir -p /.wll_dir_abi/parent/child; \
                 /busybox rm -rf /.wll_dir_abi",
            ),
        ],
        envp: alloc::vec![
            String::from("PATH=/:/bin:/usr/bin"),
            String::from("LD_LIBRARY_PATH=/lib"),
        ],
        cwd: String::from("/"),
        root: String::from("/glibc"),
        marker_name: None,
    };
    let exit_code =
        run_user_program_spec_foreground_exit_code(&spec).expect("directory ABI regression launch");
    if exit_code != 0 {
        panic!(
            "[smp-regression] fail phase=directory-abi exit={}",
            exit_code
        );
    }
    console_write("[smp-regression] pass phase=directory-abi\n");
}

#[cfg(feature = "smp-regression")]
fn run_directory_metadata_abi_regression() {
    let spec = UserProgramSpec {
        path: String::from("/bin/sh"),
        argv: alloc::vec![
            String::from("/bin/sh"),
            String::from("-c"),
            String::from(
                "set -e; rm -rf /.wll_dir_metadata_abi; \
                 mkdir -p -m 0710 /.wll_dir_metadata_abi/parent/child; \
                 chmod 0750 /.wll_dir_metadata_abi/parent/child; \
                 chown 0:0 /.wll_dir_metadata_abi/parent/child; \
                 test \"$(stat -c '%a' /.wll_dir_metadata_abi/parent/child)\" = 750; \
                 ln -s parent/child /.wll_dir_metadata_abi/link; \
                 test \"$(readlink /.wll_dir_metadata_abi/link)\" = parent/child; \
                 mkdir -p /.wll_dir_metadata_abi/store.fd; \
                 printf 'uefi-variable-store-probe\n' > /.wll_dir_metadata_abi/store.fd/input.fd; \
                 test -d /.wll_dir_metadata_abi/store.fd; \
                 test -f /.wll_dir_metadata_abi/store.fd/input.fd; \
                 test \"$(stat -c '%F' /.wll_dir_metadata_abi/store.fd)\" = directory; \
                 cp /.wll_dir_metadata_abi/store.fd/input.fd /.wll_dir_metadata_abi/copy.fd; \
                 cmp /.wll_dir_metadata_abi/store.fd/input.fd /.wll_dir_metadata_abi/copy.fd; \
                 ln -s store.fd /.wll_dir_metadata_abi/store-link; \
                 cp /.wll_dir_metadata_abi/store-link/input.fd /.wll_dir_metadata_abi/link-copy.fd; \
                 cmp /.wll_dir_metadata_abi/store.fd/input.fd /.wll_dir_metadata_abi/link-copy.fd; \
                 cp -a /.wll_dir_metadata_abi/store.fd /.wll_dir_metadata_abi/archive.fd; \
                 test -d /.wll_dir_metadata_abi/archive.fd; \
                 test -f /.wll_dir_metadata_abi/archive.fd/input.fd; \
                 cmp /.wll_dir_metadata_abi/store.fd/input.fd /.wll_dir_metadata_abi/archive.fd/input.fd; \
                 cp -R /.wll_dir_metadata_abi/store.fd /.wll_dir_metadata_abi/recursive.fd; \
                 test -d /.wll_dir_metadata_abi/recursive.fd; \
                 test -f /.wll_dir_metadata_abi/recursive.fd/input.fd; \
                 cmp /.wll_dir_metadata_abi/store.fd/input.fd /.wll_dir_metadata_abi/recursive.fd/input.fd; \
                 mkdir -p /.wll_dir_metadata_abi/long/one/two/three/four/five/six/seven/eight; \
                 printf 'long-link-probe\n' > /.wll_dir_metadata_abi/long/one/two/three/four/five/six/seven/eight/input.fd; \
                 ln -s /.wll_dir_metadata_abi/long/one/two/three/four/five/six/seven/eight /.wll_dir_metadata_abi/long-link; \
                 cp /.wll_dir_metadata_abi/long-link/input.fd /.wll_dir_metadata_abi/long-copy.fd; \
                 cmp /.wll_dir_metadata_abi/long/one/two/three/four/five/six/seven/eight/input.fd /.wll_dir_metadata_abi/long-copy.fd; \
                 printf 'replace-me\n' > /.wll_dir_metadata_abi/reused.fd; \
                 stat /.wll_dir_metadata_abi/reused.fd >/dev/null; \
                 rm /.wll_dir_metadata_abi/reused.fd; \
                 mkdir /.wll_dir_metadata_abi/reused.fd; \
                 printf 'replacement-directory-probe\n' > /.wll_dir_metadata_abi/reused.fd/input.fd; \
                 cp /.wll_dir_metadata_abi/reused.fd/input.fd /.wll_dir_metadata_abi/reused-copy.fd; \
                 cmp /.wll_dir_metadata_abi/reused.fd/input.fd /.wll_dir_metadata_abi/reused-copy.fd; \
                 rm -rf /.wll_dir_metadata_abi",
            ),
        ],
        envp: alloc::vec![
            String::from("PATH=/usr/bin:/bin"),
            String::from("LD_LIBRARY_PATH=/lib:/usr/lib"),
        ],
        cwd: String::from("/"),
        root: String::from("/"),
        marker_name: None,
    };
    let exit_code = run_user_program_spec_foreground_exit_code(&spec)
        .expect("directory metadata ABI regression launch");
    if exit_code != 0 {
        panic!(
            "[smp-regression] fail phase=directory-metadata-abi exit={}",
            exit_code
        );
    }
    console_write("[smp-regression] pass phase=directory-metadata-abi\n");
}

#[cfg(feature = "smp-regression")]
fn run_script_root_namespace_regression() {
    const BASE: &str = "/.__wll_script_root_namespace";
    const GLOBAL_INTERPRETER: &str = "/.__wll_script_root_namespace/interp/sh";
    const SUITE_INTERPRETER: &str =
        "/.__wll_script_root_namespace/suite/.__wll_script_root_namespace/interp/sh";
    const SCRIPT: &str = "/.__wll_script_root_namespace/suite/probe.sh";
    const COMPAT_INTERPRETER: &str = "/.__wll_script_root_namespace/suite/.__wll_compat_interp/sh";
    const COMPAT_SCRIPT: &str = "/.__wll_script_root_namespace/suite/compat.sh";

    let busybox = crate::fs::read_executable_file("/glibc/busybox")
        .expect("script-root namespace regression busybox");
    {
        let mut fs = crate::fs::MEM_FS.lock();
        fs.add_file_with_mode(GLOBAL_INTERPRETER, busybox.as_ref().clone(), 0o755);
        fs.add_file_with_mode(SUITE_INTERPRETER, busybox.as_ref().clone(), 0o755);
        fs.add_file_with_mode(COMPAT_INTERPRETER, busybox.as_ref().clone(), 0o755);
        fs.add_file_with_mode(
            SCRIPT,
            b"#!/.__wll_script_root_namespace/interp/sh\n\
set -e\n\
test -f /.__wll_script_root_namespace/global/token\n"
                .to_vec(),
            0o755,
        );
        fs.add_file_with_mode(
            &alloc::format!("{}/global/token", BASE),
            b"global-root-owned\n".to_vec(),
            0o644,
        );
        fs.add_file_with_mode(
            COMPAT_SCRIPT,
            b"#!/.__wll_compat_interp/sh\n\
set -e\n\
test -f /.__wll_compat_resource/token\n"
                .to_vec(),
            0o755,
        );
        fs.add_file_with_mode(
            &alloc::format!("{}/suite/.__wll_compat_resource/token", BASE),
            b"compat-root-owned\n".to_vec(),
            0o644,
        );
    }

    let spec = script_program_spec(SCRIPT).expect("script-root namespace regression spec");
    let selected_root = spec.root.clone();
    let exit_code = run_user_program_spec_foreground_exit_code(&spec)
        .expect("script-root namespace regression launch");
    if exit_code != 0 || selected_root != "/" {
        panic!(
            "[smp-regression] fail phase=script-root-namespace exit={} root={}",
            exit_code, selected_root
        );
    }
    console_write("[smp-regression] pass phase=script-root-namespace root=/\n");

    let compat_spec =
        script_program_spec(COMPAT_SCRIPT).expect("compat script-root namespace regression spec");
    let compat_root = compat_spec.root.clone();
    let compat_exit_code = run_user_program_spec_foreground_exit_code(&compat_spec)
        .expect("compat script-root namespace regression launch");
    let expected_compat_root = alloc::format!("{}/suite", BASE);
    if compat_exit_code != 0 || compat_root != expected_compat_root {
        panic!(
            "[smp-regression] fail phase=script-compat-root exit={} root={}",
            compat_exit_code, compat_root
        );
    }
    console_write("[smp-regression] pass phase=script-compat-root\n");
}

#[cfg(all(feature = "buildstorm-diagnostics", feature = "smp-regression"))]
fn run_dynamic_user_memory_lifecycle_probe() {
    crate::smp_regression::reset_user_memory_lifecycle_diagnostic();
    let spec = UserProgramSpec {
        path: String::from("/usr/bin/bash"),
        argv: alloc::vec![
            String::from("/usr/bin/bash"),
            String::from("-c"),
            String::from("exit 0"),
        ],
        envp: alloc::vec![
            String::from("PATH=/usr/bin:/bin:/"),
            String::from("LD_LIBRARY_PATH=/lib:/usr/lib"),
        ],
        cwd: String::from("/"),
        root: String::from("/"),
        marker_name: None,
    };
    let exit_code = match run_user_program_spec_foreground_exit_code(&spec) {
        Ok(exit_code) => exit_code,
        Err(error) => {
            console_write(&format!(
                "BUILDSTORM_DIAG dynamic_launch_failure errno={}\n",
                error as usize,
            ));
            -1
        }
    };
    if let Some(failure) = crate::buildstorm_diagnostics::take_first_page_fault_failure() {
        console_write(&format!(
            "BUILDSTORM_DIAG dynamic_page_fault_failure stage={} errno={} vaddr={:#x}\n",
            failure.stage, failure.errno, failure.vaddr,
        ));
    }
    if let Some(failure) = crate::buildstorm_diagnostics::take_first_exec_failure() {
        console_write(&format!(
            "BUILDSTORM_DIAG dynamic_exec_failure errno={} pid={} parent_pid={} clone_flags={:#x} stage={} vector_base={:#x} index={} entry_addr={:#x} value_ptr={:#x} slot_vma=[{:#x},{:#x}) slot_flags={:#x} backing={} resident={} page_state={} pte_pa={:#x} path_ptr={:#x} argv_ptr={:#x} envp_ptr={:#x}\n",
            failure.errno,
            failure.pid,
            failure.parent_pid,
            failure.clone_flags,
            failure.stage,
            failure.vector_base,
            failure.index,
            failure.entry_addr,
            failure.value_ptr,
            failure.vma_start,
            failure.vma_end,
            failure.vma_flags,
            failure.backing,
            failure.resident,
            failure.page_state,
            failure.pte_pa,
            failure.path_ptr,
            failure.argv_ptr,
            failure.envp_ptr,
        ));
    }
    if let Some(trap) = crate::buildstorm_diagnostics::take_first_terminal_user_trap() {
        console_write(&format!(
            "BUILDSTORM_DIAG dynamic_terminal_user_trap kind={} pid={} vaddr={:#x} sepc={:#x} sp={:#x} ra={:#x} tp={:#x} fault_pa={:#x} sepc_pa={:#x} vma=[{:#x},{:#x}) flags={:#x} backing={} resident={} page_state={}\n",
            trap.kind,
            trap.pid,
            trap.vaddr,
            trap.sepc,
            trap.sp,
            trap.ra,
            trap.tp,
            trap.fault_pa,
            trap.sepc_pa,
            trap.vma_start,
            trap.vma_end,
            trap.vma_flags,
            trap.backing,
            trap.resident,
            trap.page_state,
        ));
    }
    console_write(&format!(
        "BUILDSTORM_DIAG dynamic_script_exit code={}\n",
        exit_code
    ));
}

fn foreground_timeout_us(spec: &UserProgramSpec) -> usize {
    #[cfg(feature = "libctest")]
    const DEFAULT_RUN_TIMEOUT_US: usize = 15_000_000;
    #[cfg(not(feature = "libctest"))]
    const DEFAULT_RUN_TIMEOUT_US: usize = 120_000_000;
    const BUSYBOX_RUN_TIMEOUT_US: usize = 240_000_000;
    const IOZONE_RUN_TIMEOUT_US: usize = 240_000_000;
    const LMBENCH_RUN_TIMEOUT_US: usize = 600_000_000;
    // The official compile command has a 14,400-second timeout. Keep the
    // in-kernel harness outside that window so it cannot kill a valid run.
    const BUILDSTORM_RUN_TIMEOUT_US: usize = 18_000_000_000;

    if spec
        .argv
        .iter()
        .any(|arg| testcode_stem(arg).and_then(TestGroup::from_stem) == Some(TestGroup::Buildstorm))
    {
        BUILDSTORM_RUN_TIMEOUT_US
    } else if spec
        .argv
        .iter()
        .any(|arg| testcode_stem(arg).and_then(TestGroup::from_stem) == Some(TestGroup::Iozone))
    {
        IOZONE_RUN_TIMEOUT_US
    } else if spec
        .argv
        .iter()
        .any(|arg| testcode_stem(arg).and_then(TestGroup::from_stem) == Some(TestGroup::Busybox))
    {
        BUSYBOX_RUN_TIMEOUT_US
    } else if spec
        .argv
        .iter()
        .any(|arg| testcode_stem(arg).and_then(TestGroup::from_stem) == Some(TestGroup::Lmbench))
    {
        LMBENCH_RUN_TIMEOUT_US
    } else {
        DEFAULT_RUN_TIMEOUT_US
    }
}

fn cleanup_foreground_task_tree(root: &Arc<TaskControlBlock>) {
    let mut tasks = manager::all_user_tasks();
    if !tasks.iter().any(|task| task.pid.0 == root.pid.0) {
        tasks.push(root.clone());
    }

    let mut killed_tgids = Vec::new();
    for task in &tasks {
        let tgid = task.thread_group.tgid();
        if task.status() != TaskStatus::Zombie && !killed_tgids.iter().any(|seen| *seen == tgid) {
            killed_tgids.push(tgid);
            crate::task::terminate_task_group(task, -2);
        }
    }

    for task in tasks {
        purge_exited_user_task_for_foreground(&task);
    }
}

fn abort_foreground_task_tree(root: &Arc<TaskControlBlock>) {
    fn terminate_group(task: &Arc<TaskControlBlock>, killed_tgids: &mut Vec<usize>) {
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
            *member.trap_frame.lock() = None;
        }
        crate::task::terminate_task_group(task, -2);
    }

    let mut killed_tgids = Vec::new();
    let mut tasks = manager::all_user_tasks();
    if !tasks.iter().any(|task| task.pid.0 == root.pid.0) {
        tasks.push(root.clone());
    }

    for task in tasks {
        if task.status() != TaskStatus::Zombie {
            terminate_group(&task, &mut killed_tgids);
        }
    }
}

fn run_user_task_foreground(task: Arc<TaskControlBlock>, timeout_us: usize) {
    let deadline_us = crate::timer::deadline_after_us(timeout_us);
    crate::task::set_foreground_deadline_us(deadline_us);

    #[cfg(feature = "buildstorm-diagnostics")]
    let mut next_heartbeat_us = crate::timer::get_time_us().saturating_add(10_000_000);
    #[cfg(feature = "buildstorm-diagnostics")]
    console_write(&format!(
        "BUILDSTORM_DIAG foreground_wait_begin pid={} tgid={} timeout_us={} deadline_us={}\n",
        task.pid.0,
        task.thread_group.tgid(),
        timeout_us,
        deadline_us,
    ));

    task.set_status(TaskStatus::Ready);
    manager::add_task(task.clone());

    loop {
        crate::timer::wake_expired_timers();
        let now_us = crate::timer::get_time_us();
        #[cfg(feature = "buildstorm-diagnostics")]
        if !crate::buildstorm_diagnostics::exec_focus_only() && now_us >= next_heartbeat_us {
            let counts = manager::diagnostic_task_counts();
            let active_pid = CURRENT_TASK
                .lock()
                .as_ref()
                .map(|active| active.pid.0)
                .unwrap_or(0);
            console_write(&format!(
                "BUILDSTORM_DIAG foreground_heartbeat pid={} tgid={} active_pid={} live={} runnable={} blocked={} queue={} rustc_live={} rustc_runnable={} now_us={} deadline_us={}\n",
                task.pid.0,
                task.thread_group.tgid(),
                active_pid,
                counts.0,
                counts.1,
                counts.2,
                manager::queue_len(),
                counts.3,
                counts.4,
                now_us,
                deadline_us,
            ));
            next_heartbeat_us = now_us.saturating_add(10_000_000);
        }
        if now_us >= deadline_us {
            #[cfg(feature = "buildstorm-diagnostics")]
            console_write(&format!(
                "BUILDSTORM_DIAG foreground_timeout pid={} tgid={} now_us={} deadline_us={}\n",
                task.pid.0,
                task.thread_group.tgid(),
                now_us,
                deadline_us,
            ));
            console_write("[harness] TIMEOUT pid=");
            console_write(&format!("{}", task.pid.0));
            console_write("\n");
            abort_foreground_task_tree(&task);
            break;
        }
        if task.status() == TaskStatus::Zombie && !manager::has_user_task() {
            break;
        }

        let Some(active) = manager::fetch_user_task_for_foreground() else {
            continue;
        };
        let active_status = active.status();
        if matches!(
            active_status,
            TaskStatus::Zombie | TaskStatus::Blocked | TaskStatus::Stopped
        ) {
            continue;
        }
        if !active.try_start_running() {
            continue;
        }

        *CURRENT_TASK.lock() = Some(active.clone());

        let mut tf_guard = active.trap_frame.lock();
        let mut ctx = match tf_guard.as_ref() {
            None => {
                log::error!(
                    "[harness] foreground user task {} missing trap frame",
                    active.pid.0
                );
                *CURRENT_TASK.lock() = None;
                active.release_running_cpu();
                continue;
            }
            Some(_) => tf_guard.take().unwrap(),
        };
        drop(tf_guard);
        run_current_user_task_until_reschedule(&active, &mut ctx);

        if active.status() != TaskStatus::Zombie {
            *active.trap_frame.lock() = Some(ctx);
        }

        // The foreground driver is also a scheduler CPU. Clear its per-CPU
        // current slot before handing the TCB to the shared queue; otherwise a
        // secondary CPU can start the same task while it is still recorded as
        // running here.
        *CURRENT_TASK.lock() = None;
        active.release_running_cpu();
        requeue_after_user_run(active);
    }

    crate::task::clear_foreground_deadline_us();
    *CURRENT_TASK.lock() = None;
}

struct ScriptInterpreter {
    path: String,
    arg: Option<String>,
}

fn parse_script_interpreter(data: &[u8]) -> Option<ScriptInterpreter> {
    if data.len() < 2 || &data[..2] != b"#!" {
        return None;
    }
    let end = data
        .iter()
        .position(|byte| *byte == b'\n')
        .unwrap_or(data.len());
    let line = core::str::from_utf8(&data[2..end]).ok()?.trim();
    let mut parts = line.split_whitespace();
    Some(ScriptInterpreter {
        path: String::from(parts.next()?),
        arg: parts.next().map(String::from),
    })
}

fn logical_path_for_script(
    script_path: &str,
    interpreter: Option<&ScriptInterpreter>,
) -> Option<(String, String)> {
    if !script_path.starts_with('/') {
        return None;
    }

    let host_script = crate::fs::normalize_path(script_path);
    let interpreter_path = interpreter
        .map(|spec| spec.path.as_str())
        .filter(|path| path.starts_with('/'))
        .unwrap_or("/bin/sh");

    // A runnable interpreter in the global namespace makes `/` authoritative
    // for absolute paths. Compatibility images without a global userspace
    // can still fall back to an independent root below the suite directory.
    let global_interpreter = crate::fs::apply_root("/", interpreter_path);
    if crate::fs::read_executable_file(&global_interpreter).is_some() {
        return Some((String::from("/"), host_script));
    }

    let mut candidate_root = dirname(&host_script);
    while candidate_root != "/" {
        // Some runtime images are complete root filesystems, while compact
        // compatibility images place an independent userspace below a suite
        // directory. Select the nearest ancestor that actually supplies the
        // script's absolute interpreter instead of keying this decision on a
        // libc name or test group.
        let interpreter_host = crate::fs::apply_root(&candidate_root, interpreter_path);
        if crate::fs::read_executable_file(&interpreter_host).is_some() {
            let logical_script = crate::fs::normalize_path(&host_script[candidate_root.len()..]);
            return Some((candidate_root, logical_script));
        }
        candidate_root = dirname(&candidate_root);
    }

    Some((String::from("/"), host_script))
}

fn dirname(path: &str) -> String {
    let norm = crate::fs::normalize_path(path);
    match norm.rfind('/') {
        Some(0) => String::from("/"),
        Some(idx) => norm[..idx].to_string(),
        None => String::from("/"),
    }
}

fn prepare_lmbench_helpers(root: &str) {
    let lmbench_host = crate::fs::apply_root(root, "/lmbench_all");
    if !crate::fs::file_exists(&lmbench_host) {
        return;
    }

    let helper_host = crate::fs::apply_root(root, "/code/lmbench_src/bin/build/lmbench_all");
    crate::fs::MEM_FS
        .lock()
        .add_dir(&crate::fs::apply_root(root, "/code/lmbench_src/bin/build"));
    let _ = crate::fs::vfs::create_symlink("../../../../lmbench_all", &helper_host);
}

fn script_program_spec(script_path: &str) -> Result<UserProgramSpec, ScriptLaunchError> {
    if !script_path.starts_with('/') {
        return Err(ScriptLaunchError::InvalidPath);
    }
    let host_script = crate::fs::normalize_path(script_path);
    let script_data = crate::fs::read_file(&host_script).ok_or(ScriptLaunchError::MissingScript)?;
    let interpreter = parse_script_interpreter(&script_data);
    let (root, logical_script) = logical_path_for_script(script_path, interpreter.as_ref())
        .ok_or(ScriptLaunchError::InvalidPath)?;
    let script_dir = dirname(&logical_script);
    let sibling_busybox = crate::fs::resolve_path(&script_dir, "busybox");
    let busybox_path = if crate::fs::read_executable_file(&sibling_busybox).is_some() {
        sibling_busybox
    } else {
        String::from("/busybox")
    };
    let script_host = crate::fs::apply_root(&root, &logical_script);

    crate::fs::read_file(&script_host).ok_or(ScriptLaunchError::MissingScript)?;

    if testcode_stem(&logical_script).and_then(TestGroup::from_stem) == Some(TestGroup::Lmbench) {
        prepare_lmbench_helpers(&root);
    }

    let (program_path, argv) = if let Some(interpreter) = interpreter {
        let interpreter_logical = crate::fs::resolve_path("/", &interpreter.path);
        let interpreter_host = crate::fs::apply_root(&root, &interpreter_logical);
        if crate::fs::read_executable_file(&interpreter_host).is_some() {
            let mut argv = alloc::vec![interpreter_logical.clone()];
            if let Some(arg) = interpreter.arg {
                argv.push(arg);
            }
            argv.push(logical_script.clone());
            (interpreter_logical, argv)
        } else {
            let busybox_host = crate::fs::apply_root(&root, &busybox_path);
            crate::fs::read_executable_file(&busybox_host)
                .ok_or(ScriptLaunchError::MissingBusybox)?;
            let mut argv = alloc::vec![busybox_path.clone(), String::from("sh")];
            argv.push(logical_script.clone());
            (busybox_path.clone(), argv)
        }
    } else {
        let busybox_host = crate::fs::apply_root(&root, &busybox_path);
        crate::fs::read_executable_file(&busybox_host).ok_or(ScriptLaunchError::MissingBusybox)?;
        let mut argv = alloc::vec![busybox_path.clone(), String::from("sh")];
        argv.push(logical_script.clone());
        (busybox_path.clone(), argv)
    };

    let mut envp = alloc::vec![
        String::from("PATH=.:/:/bin:/usr/bin"),
        String::from("LD_LIBRARY_PATH=/lib"),
        alloc::format!("SHELL={}", program_path),
    ];
    if testcode_stem(&logical_script).and_then(TestGroup::from_stem) == Some(TestGroup::Lmbench) {
        envp.push(String::from("ENOUGH=5000"));
    }

    Ok(UserProgramSpec {
        path: program_path,
        argv,
        envp,
        cwd: dirname(&logical_script),
        root,
        marker_name: None,
    })
}

fn run_script(script_path: &str) -> Result<(), ScriptLaunchError> {
    let spec = match script_program_spec(script_path) {
        Ok(spec) => spec,
        Err(error) => {
            #[cfg(feature = "buildstorm-diagnostics")]
            console_write(&format!(
                "BUILDSTORM_DIAG script_start_failure path={} error={:?}\n",
                script_path, error
            ));
            return Err(error);
        }
    };
    #[cfg(feature = "buildstorm-diagnostics")]
    console_write(&format!(
        "BUILDSTORM_DIAG script_start path={} exec_path={} argv0={} root={} cwd={}\n",
        script_path,
        spec.path,
        spec.argv.first().map(String::as_str).unwrap_or(""),
        spec.root,
        spec.cwd,
    ));
    #[cfg(feature = "buildstorm-diagnostics")]
    {
        // One lifecycle boundary record per harness script.  This is not a
        // syscall/page-fault trace and does not alter the production result:
        // the non-diagnostic path below also treats a launched script as a
        // completed harness item regardless of its user exit status.
        let exit_code = run_user_program_spec_foreground_exit_code(&spec)
            .map_err(ScriptLaunchError::CreateTask)?;
        if let Some(failure) = crate::buildstorm_diagnostics::take_first_page_fault_failure() {
            console_write(&format!(
                "BUILDSTORM_DIAG page_fault_failure stage={} errno={} vaddr={:#x}\n",
                failure.stage, failure.errno, failure.vaddr,
            ));
        }
        if let Some(failure) = crate::buildstorm_diagnostics::take_first_exec_failure() {
            console_write(&format!(
                "BUILDSTORM_DIAG exec_failure errno={} pid={} parent_pid={} clone_flags={:#x} stage={} vector_base={:#x} index={} entry_addr={:#x} value_ptr={:#x} slot_vma=[{:#x},{:#x}) slot_flags={:#x} backing={} resident={} page_state={} pte_pa={:#x} path_ptr={:#x} argv_ptr={:#x} envp_ptr={:#x}\n",
                failure.errno,
                failure.pid,
                failure.parent_pid,
                failure.clone_flags,
                failure.stage,
                failure.vector_base,
                failure.index,
                failure.entry_addr,
                failure.value_ptr,
                failure.vma_start,
                failure.vma_end,
                failure.vma_flags,
                failure.backing,
                failure.resident,
                failure.page_state,
                failure.pte_pa,
                failure.path_ptr,
                failure.argv_ptr,
                failure.envp_ptr,
            ));
        }
        if let Some(trap) = crate::buildstorm_diagnostics::take_first_terminal_user_trap() {
            console_write(&format!(
                "BUILDSTORM_DIAG terminal_user_trap kind={} pid={} vaddr={:#x} sepc={:#x} sp={:#x} ra={:#x} tp={:#x} fault_pa={:#x} sepc_pa={:#x} vma=[{:#x},{:#x}) flags={:#x} backing={} resident={} page_state={}\n",
                trap.kind,
                trap.pid,
                trap.vaddr,
                trap.sepc,
                trap.sp,
                trap.ra,
                trap.tp,
                trap.fault_pa,
                trap.sepc_pa,
                trap.vma_start,
                trap.vma_end,
                trap.vma_flags,
                trap.backing,
                trap.resident,
                trap.page_state,
            ));
        }
        console_write(&format!(
            "BUILDSTORM_DIAG script_exit path={} code={}\n",
            script_path, exit_code
        ));
        return Ok(());
    }
    #[cfg(not(feature = "buildstorm-diagnostics"))]
    run_user_program_spec_foreground(&spec).map_err(ScriptLaunchError::CreateTask)
}
