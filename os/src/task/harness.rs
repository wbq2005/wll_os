use alloc::format;
use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec::Vec;
use polyhal_trap::trap::run_user_task;

use super::{
    current_task, manager, purge_exited_user_task_for_foreground, requeue_after_user_run,
    set_orphan_reaper, TaskControlBlock, TaskStatus, UserProgramSpec, CURRENT_TASK,
};
use crate::console::putchar;
use crate::utils::error::SysErrNo;

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

fn harness_filter_active() -> bool {
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

fn trace_test_commands_enabled() -> bool {
    option_env!("WLL_TRACE_TEST_COMMANDS")
        .map(|value| {
            let value = value.trim();
            !value.is_empty()
                && value != "0"
                && !value.eq_ignore_ascii_case("false")
                && !value.eq_ignore_ascii_case("off")
        })
        .unwrap_or(false)
}

fn trace_test_group_enabled(logical_script: &str) -> bool {
    let Some(filter) = option_env!("WLL_TRACE_TEST_GROUPS") else {
        return true;
    };
    if !filter.split(',').any(|part| !part.trim().is_empty()) {
        return true;
    }

    let group = testcode_stem(logical_script).and_then(TestGroup::from_stem);
    filter.split(',').any(|part| {
        let part = part.trim();
        part == "all"
            || group
                .map(|group| group.aliases().iter().any(|alias| *alias == part))
                .unwrap_or(false)
    })
}

fn trace_test_commands_enabled_for(logical_script: &str) -> bool {
    trace_test_commands_enabled() && trace_test_group_enabled(logical_script)
}

#[derive(Clone, Copy, Debug)]
enum ScriptLaunchError {
    InvalidPath,
    MissingBusybox,
    MissingScript,
    MissingApplet(&'static str),
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
        ScriptLaunchError::MissingApplet(applet) => {
            console_write("missing applet ");
            console_write(applet);
        }
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
            if group == Some(TestGroup::Ltp) {
                if let Some(cases_filter) = ltp_cases_filter() {
                    if !run_ltp_collection_harness(script, cases_filter) {
                        console_write("[harness] failed to launch LTP collector: ");
                        console_write(script);
                        console_write("\n");
                    }
                    continue;
                }
            }
            if let Err(err) = run_script_via_busybox(script) {
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
        if !libctest_group_enabled(root, SEGMENTS, EXTRA_SEGMENTS) {
            continue;
        }
        run_libctest_judge_group(root, group_name, SEGMENTS, EXTRA_SEGMENTS);
    }
}

// External diagnostic knob only. With no LTP_CASES value, the submission path
// runs the real LTP script instead of a kernel-side focused collector.
fn ltp_cases_filter() -> Option<&'static str> {
    match option_env!("LTP_CASES") {
        Some(filter) => {
            let filter = filter.trim();
            if filter.is_empty() || filter.eq_ignore_ascii_case("all") {
                None
            } else {
                Some(filter)
            }
        }
        None => None,
    }
}

fn ltp_group_name(root: &str) -> &'static str {
    match root {
        "/glibc" => "ltp-glibc",
        "/musl" => "ltp-musl",
        _ => "ltp",
    }
}

fn run_ltp_collection_harness(script_path: &str, cases_filter: &str) -> bool {
    let Some((root, _logical_script)) = logical_path_for_script(script_path) else {
        return false;
    };
    let group_name = ltp_group_name(&root);
    let mut launched = false;

    console_write("#### OS COMP TEST GROUP START ");
    console_write(group_name);
    console_write(" ####\n");

    for part in cases_filter.split(',') {
        let case = part.trim();
        if case.is_empty() {
            continue;
        }
        launched = true;
        run_ltp_case(&root, case);
    }

    console_write("#### OS COMP TEST GROUP END ");
    console_write(group_name);
    console_write(" ####\n");
    launched
}

fn run_ltp_case(root: &str, case: &str) {
    let path = format!("/ltp/testcases/bin/{}", case);

    console_write("RUN LTP CASE ");
    console_write(case);
    console_write("\n");

    let ret = run_user_program_spec_foreground_exit_code(&UserProgramSpec {
        path: path.clone(),
        argv: alloc::vec![path.clone()],
        envp: alloc::vec![
            String::from("PATH=.:/:/bin:/usr/bin:/ltp/testcases/bin"),
            String::from("LD_LIBRARY_PATH=/lib"),
            String::from("LTPROOT=/ltp"),
            String::from("TMPDIR=/tmp"),
        ],
        cwd: String::from("/"),
        root: String::from(root),
        marker_name: None,
    })
    .unwrap_or(-1);

    console_write("END LTP CASE ");
    console_write(case);
    console_write(" : ");
    console_write(&format!("{}", ret));
    console_write("\n");
}

// External diagnostic knob only; default libc-test routing is not shortened.
fn libctest_segment_enabled(name: &str) -> bool {
    match option_env!("LIBCTEST_FILTER") {
        Some(filter) => {
            let mut any = false;
            for part in filter.split(',') {
                let part = part.trim();
                if part.is_empty() {
                    continue;
                }
                any = true;
                if name.contains(part) {
                    return true;
                }
            }
            !any
        }
        None => true,
    }
}

fn libctest_group_enabled(
    root: &str,
    segments: &[(&str, &str, &str, &[&str])],
    extra_segments: &[(&str, &str, &str, &[&str])],
) -> bool {
    segments
        .iter()
        .any(|(segment_root, name, _, _)| *segment_root == root && libctest_segment_enabled(name))
        || extra_segments.iter().any(|(segment_root, name, _, _)| {
            *segment_root == root && libctest_segment_enabled(name)
        })
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
        if *segment_root != root || !libctest_segment_enabled(name) {
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
        if *segment_root != root || !libctest_segment_enabled(name) {
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
    let task = match TaskControlBlock::new_user_with_args_env_cwd(spec) {
        Ok(task) => task,
        Err(err) => return Err(err),
    };

    let harness = current_task();
    let _foreground = ForegroundDriverGuard::enter();

    run_user_task_foreground(task.clone(), foreground_timeout_us(spec));
    let exit_code = task.thread_group.exit_code();
    cleanup_foreground_task_tree(&task);
    if let Some(ref h) = harness {
        h.set_status(TaskStatus::Running);
        *CURRENT_TASK.lock() = Some(h.clone());
    }

    Ok(exit_code)
}

fn foreground_timeout_us(spec: &UserProgramSpec) -> usize {
    #[cfg(feature = "libctest")]
    const DEFAULT_RUN_TIMEOUT_US: usize = 15_000_000;
    #[cfg(not(feature = "libctest"))]
    const DEFAULT_RUN_TIMEOUT_US: usize = 120_000_000;
    const BUSYBOX_RUN_TIMEOUT_US: usize = 240_000_000;
    const IOZONE_RUN_TIMEOUT_US: usize = 240_000_000;
    const LMBENCH_RUN_TIMEOUT_US: usize = 600_000_000;

    if spec
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

    task.set_status(TaskStatus::Ready);
    manager::add_task(task.clone());

    loop {
        crate::timer::wake_expired_timers();
        if crate::timer::get_time_us() >= deadline_us {
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
        if matches!(active.status(), TaskStatus::Zombie | TaskStatus::Blocked) {
            continue;
        }

        active.set_status(TaskStatus::Running);
        *CURRENT_TASK.lock() = Some(active.clone());

        let mut tf_guard = active.trap_frame.lock();
        let mut ctx = match tf_guard.as_ref() {
            None => {
                log::error!(
                    "[harness] foreground user task {} missing trap frame",
                    active.pid.0
                );
                *CURRENT_TASK.lock() = None;
                continue;
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
            continue;
        }

        crate::trap::prepare_user_trapframe(&mut ctx);
        crate::task::enter_foreground_user_task(active.pid.0);
        let _reason = run_user_task(&mut ctx);
        crate::task::leave_foreground_user_task(active.pid.0);
        crate::trap::restore_kernel_page_table();

        if active.status() != TaskStatus::Zombie {
            *active.trap_frame.lock() = Some(ctx);
        }

        requeue_after_user_run(active);
    }

    crate::task::clear_foreground_deadline_us();
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

fn ensure_busybox_applet_alias(root: &str, busybox_data: &[u8], applet: &str) -> Option<()> {
    let alias_logical = if applet.starts_with('/') {
        applet.to_string()
    } else {
        alloc::format!("/{}", applet)
    };
    let alias_host = crate::fs::apply_root(root, &alias_logical);
    crate::fs::MEM_FS
        .lock()
        .add_file(&alias_host, busybox_data.to_vec());
    Some(())
}

fn prepare_lmbench_helpers(root: &str) {
    let lmbench_host = crate::fs::apply_root(root, "/lmbench_all");
    if !crate::fs::file_exists(&lmbench_host) {
        return;
    }

    let helper_host = crate::fs::apply_root(root, "/code/lmbench_src/bin/build/lmbench_all");
    let mut fs = crate::fs::MEM_FS.lock();
    fs.add_dir(&crate::fs::apply_root(root, "/code/lmbench_src/bin/build"));
    let _ = fs.add_symlink(&helper_host, "../../../../lmbench_all");
}

fn busybox_script_spec(script_path: &str) -> Result<UserProgramSpec, ScriptLaunchError> {
    let (root, logical_script) =
        logical_path_for_script(script_path).ok_or(ScriptLaunchError::InvalidPath)?;
    let busybox_path = String::from("/busybox");
    let busybox_host = crate::fs::apply_root(&root, &busybox_path);
    let script_host = crate::fs::apply_root(&root, &logical_script);

    let busybox_data =
        crate::fs::read_executable_file(&busybox_host).ok_or(ScriptLaunchError::MissingBusybox)?;
    crate::fs::read_file(&script_host).ok_or(ScriptLaunchError::MissingScript)?;

    let applets: &[&str] = if testcode_stem(&logical_script).and_then(TestGroup::from_stem)
        == Some(TestGroup::Cyclictest)
    {
        &["sleep"]
    } else {
        &["ls", "sh", "/bin/sh", "cp", "sleep"]
    };
    for applet in applets {
        ensure_busybox_applet_alias(&root, &busybox_data, applet)
            .ok_or(ScriptLaunchError::MissingApplet(applet))?;
    }
    if testcode_stem(&logical_script).and_then(TestGroup::from_stem) == Some(TestGroup::Lmbench) {
        prepare_lmbench_helpers(&root);
    }

    let mut envp = alloc::vec![
        String::from("PATH=.:/:/bin:/usr/bin"),
        String::from("LD_LIBRARY_PATH=/lib"),
        alloc::format!("SHELL={}", busybox_path),
    ];
    let trace_commands = trace_test_commands_enabled_for(&logical_script);
    if trace_commands {
        envp.push(String::from("PS4=[harness] CMD "));
    }

    let mut argv = alloc::vec![busybox_path.clone(), String::from("sh")];
    if trace_commands {
        argv.push(String::from("-x"));
    }
    argv.push(logical_script.clone());

    Ok(UserProgramSpec {
        path: busybox_path.clone(),
        argv,
        envp,
        cwd: dirname(&logical_script),
        root,
        marker_name: None,
    })
}

fn run_script_via_busybox(script_path: &str) -> Result<(), ScriptLaunchError> {
    let spec = busybox_script_spec(script_path)?;
    run_user_program_spec_foreground(&spec).map_err(ScriptLaunchError::CreateTask)
}
