use alloc::string::String;
use core::sync::atomic::{AtomicUsize, Ordering};

use crate::task::{TaskStatus, NO_CPU};

const FIRST_REPORT_AT_US: usize = 240 * 1_000_000;
const REPORT_INTERVAL_US: usize = 2 * 60 * 1_000_000;
static NEXT_REPORT_AT_US: AtomicUsize = AtomicUsize::new(FIRST_REPORT_AT_US);
static REPORT_SEQUENCE: AtomicUsize = AtomicUsize::new(0);
const SYSCALL_SLOTS: usize = 512;
const PHASE_NAMES: [&str; 40] = [
    "readlink_user_path",
    "readlink_lookup",
    "readlink_copy_out",
    "open_resolve_path",
    "open_vfs_and_fd",
    "close_fd_remove",
    "close_drop",
    "statx_user_path",
    "statx_resolve_path",
    "statx_vfs",
    "statx_copy_out",
    "readlink_parent_resolve",
    "readlink_search_access",
    "readlink_memfs",
    "readlink_ext4",
    "symlink_probe_memfs",
    "symlink_probe_ext4",
    "ext4_close_reclaim_lock",
    "ext4_close_dirty_flush",
    "ext4_close_clean_discard",
    "ext4_close_dirty_discard",
    "ext4_close_not_cached",
    "ext4_close_dirty_deferred",
    "metadata_lookup_parent_resolve",
    "metadata_lookup_search_access",
    "metadata_lookup_metadata",
    "metadata_vfat",
    "metadata_memfs",
    "metadata_ext4_metadata_with_kind",
    "metadata_ext4_follow_symlink",
    "metadata_ext4_convert",
    "open_pseudo_refresh",
    "open_parent_resolve",
    "open_final_symlink",
    "open_backend_probe",
    "open_dir_probe",
    "open_ext4_regular",
    "open_create_probe",
    "open_create_commit",
    "open_missing_errno",
];
static CURRENT_SYSCALL: [AtomicUsize; crate::config::MAX_CPUS] =
    [const { AtomicUsize::new(0) }; crate::config::MAX_CPUS];
static SYSCALL_STARTED_AT_US: [AtomicUsize; crate::config::MAX_CPUS] =
    [const { AtomicUsize::new(0) }; crate::config::MAX_CPUS];
static CURRENT_SYSCALL_TASK: [AtomicUsize; crate::config::MAX_CPUS] =
    [const { AtomicUsize::new(0) }; crate::config::MAX_CPUS];
static SYSCALL_COUNTS: [AtomicUsize; SYSCALL_SLOTS] =
    [const { AtomicUsize::new(0) }; SYSCALL_SLOTS];
static SYSCALL_SAMPLES: [AtomicUsize; SYSCALL_SLOTS] =
    [const { AtomicUsize::new(0) }; SYSCALL_SLOTS];
static SYSCALL_TOTAL_US: [AtomicUsize; SYSCALL_SLOTS] =
    [const { AtomicUsize::new(0) }; SYSCALL_SLOTS];
static SYSCALL_MAX_US: [AtomicUsize; SYSCALL_SLOTS] =
    [const { AtomicUsize::new(0) }; SYSCALL_SLOTS];
static PHASE_COUNTS: [AtomicUsize; PHASE_NAMES.len()] =
    [const { AtomicUsize::new(0) }; PHASE_NAMES.len()];
static PHASE_TOTAL_US: [AtomicUsize; PHASE_NAMES.len()] =
    [const { AtomicUsize::new(0) }; PHASE_NAMES.len()];
static PHASE_MAX_US: [AtomicUsize; PHASE_NAMES.len()] =
    [const { AtomicUsize::new(0) }; PHASE_NAMES.len()];
static PPOLL_CALLS: AtomicUsize = AtomicUsize::new(0);
static PPOLL_NFDS_ZERO: AtomicUsize = AtomicUsize::new(0);
static PPOLL_NFDS_ONE: AtomicUsize = AtomicUsize::new(0);
static PPOLL_NFDS_MULTI: AtomicUsize = AtomicUsize::new(0);
static PPOLL_KEYED_SLEEP: AtomicUsize = AtomicUsize::new(0);
static PPOLL_UNKEYED_SLEEP: AtomicUsize = AtomicUsize::new(0);

pub(crate) fn note_phase(phase: usize, elapsed_us: usize) {
    if phase < PHASE_NAMES.len() {
        PHASE_COUNTS[phase].fetch_add(1, Ordering::Relaxed);
        PHASE_TOTAL_US[phase].fetch_add(elapsed_us, Ordering::Relaxed);
        PHASE_MAX_US[phase].fetch_max(elapsed_us, Ordering::Relaxed);
    }
}

pub(crate) fn note_ppoll_call(nfds: usize) {
    PPOLL_CALLS.fetch_add(1, Ordering::Relaxed);
    match nfds {
        0 => {
            PPOLL_NFDS_ZERO.fetch_add(1, Ordering::Relaxed);
        }
        1 => {
            PPOLL_NFDS_ONE.fetch_add(1, Ordering::Relaxed);
        }
        _ => {
            PPOLL_NFDS_MULTI.fetch_add(1, Ordering::Relaxed);
        }
    }
}

pub(crate) fn note_ppoll_sleep(keyed: bool) {
    if keyed {
        PPOLL_KEYED_SLEEP.fetch_add(1, Ordering::Relaxed);
    } else {
        PPOLL_UNKEYED_SLEEP.fetch_add(1, Ordering::Relaxed);
    }
}

pub(crate) fn note_syscall_enter(syscall_id: usize) {
    let cpu = crate::platform::current_cpu_index();
    if cpu < crate::config::MAX_CPUS {
        let task = crate::task::current_task()
            .map(|task| task.pid.0.saturating_add(1))
            .unwrap_or(0);
        SYSCALL_STARTED_AT_US[cpu].store(crate::timer::get_time_us(), Ordering::Relaxed);
        CURRENT_SYSCALL_TASK[cpu].store(task, Ordering::Relaxed);
        CURRENT_SYSCALL[cpu].store(syscall_id.saturating_add(1), Ordering::Release);
    }
    if syscall_id < SYSCALL_SLOTS {
        SYSCALL_COUNTS[syscall_id].fetch_add(1, Ordering::Relaxed);
    }
}

pub(crate) fn note_syscall_exit(syscall_id: usize) {
    let cpu = crate::platform::current_cpu_index();
    if cpu < crate::config::MAX_CPUS {
        let encoded = syscall_id.saturating_add(1);
        let task = crate::task::current_task()
            .map(|task| task.pid.0.saturating_add(1))
            .unwrap_or(0);
        if task != 0
            && CURRENT_SYSCALL_TASK[cpu].load(Ordering::Relaxed) == task
            && CURRENT_SYSCALL[cpu]
            .compare_exchange(encoded, 0, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
            && syscall_id < SYSCALL_SLOTS
        {
            let elapsed = crate::timer::get_time_us()
                .saturating_sub(SYSCALL_STARTED_AT_US[cpu].load(Ordering::Relaxed));
            SYSCALL_SAMPLES[syscall_id].fetch_add(1, Ordering::Relaxed);
            SYSCALL_TOTAL_US[syscall_id].fetch_add(elapsed, Ordering::Relaxed);
            SYSCALL_MAX_US[syscall_id].fetch_max(elapsed, Ordering::Relaxed);
        }
    }
}

pub(crate) fn maybe_report() {
    let now_us = crate::timer::get_time_us();
    let next_report = NEXT_REPORT_AT_US.load(Ordering::Acquire);
    if now_us < next_report {
        return;
    }
    if NEXT_REPORT_AT_US
        .compare_exchange(
            next_report,
            now_us.saturating_add(REPORT_INTERVAL_US),
            Ordering::AcqRel,
            Ordering::Acquire,
        )
        .is_err()
    {
        return;
    }

    let sequence = REPORT_SEQUENCE.fetch_add(1, Ordering::Relaxed) + 1;
    let mut tasks = crate::task::manager::all_user_tasks();
    tasks.sort_by_key(|task| task.pid.0);
    crate::println!(
        "[buildstorm-diag] snapshot={} begin now_us={} cpu={} online_mask={:#x} user_tasks={} ready_user={} ready_kernel={}",
        sequence,
        now_us,
        crate::platform::current_cpu_index(),
        crate::platform::online_cpu_mask(),
        tasks.len(),
        crate::task::manager::user_queue_len(),
        crate::task::manager::kernel_queue_len()
    );

    for cpu in 0..crate::config::MAX_CPUS {
        let encoded = CURRENT_SYSCALL[cpu].load(Ordering::Acquire);
        if encoded != 0 {
            crate::println!(
                "[buildstorm-diag] cpu={} current_syscall={}",
                cpu,
                encoded - 1
            );
        }
    }
    for (syscall_id, counter) in SYSCALL_COUNTS.iter().enumerate() {
        let count = counter.load(Ordering::Relaxed);
        if count != 0 {
            crate::println!(
                "[buildstorm-diag] syscall id={} count={} total_us={} max_us={} samples={}",
                syscall_id,
                count,
                SYSCALL_TOTAL_US[syscall_id].load(Ordering::Relaxed),
                SYSCALL_MAX_US[syscall_id].load(Ordering::Relaxed),
                SYSCALL_SAMPLES[syscall_id].load(Ordering::Relaxed)
            );
        }
    }
    for (phase, name) in PHASE_NAMES.iter().enumerate() {
        let count = PHASE_COUNTS[phase].load(Ordering::Relaxed);
        if count != 0 {
            crate::println!(
                "[buildstorm-diag] phase name={} count={} total_us={} max_us={}",
                name,
                count,
                PHASE_TOTAL_US[phase].load(Ordering::Relaxed),
                PHASE_MAX_US[phase].load(Ordering::Relaxed)
            );
        }
    }
    crate::println!(
        "[buildstorm-diag] ppoll calls={} nfds0={} nfds1={} nfdsmulti={} keyed_sleep={} unkeyed_sleep={}",
        PPOLL_CALLS.load(Ordering::Relaxed),
        PPOLL_NFDS_ZERO.load(Ordering::Relaxed),
        PPOLL_NFDS_ONE.load(Ordering::Relaxed),
        PPOLL_NFDS_MULTI.load(Ordering::Relaxed),
        PPOLL_KEYED_SLEEP.load(Ordering::Relaxed),
        PPOLL_UNKEYED_SLEEP.load(Ordering::Relaxed)
    );
    let (cache_entries, dirty_entries, cache_bytes) =
        crate::fs::ext4_vol::diagnostic_regular_cache_stats();
    crate::println!(
        "[buildstorm-diag] ext4-regular-cache entries={} dirty={} bytes={}",
        cache_entries,
        dirty_entries,
        cache_bytes
    );
    let (exec_entries, exec_bytes, exec_hits, exec_misses) =
        crate::fs::ext4_vol::diagnostic_executable_cache_stats();
    crate::println!(
        "[buildstorm-diag] executable-cache entries={} bytes={} hits={} misses={}",
        exec_entries,
        exec_bytes,
        exec_hits,
        exec_misses
    );

    #[cfg(feature = "perf-counters")]
    {
        let counters = crate::perf_counters::diagnostic_snapshot();
        crate::println!(
            "[buildstorm-diag] perf faults={} clean_faults={} user_pt={} kernel_pt={} block_reads={} block_bytes={} block_same={} block_seq={} block_random={}",
            counters.0,
            counters.1,
            counters.2,
            counters.3,
            counters.4,
            counters.5,
            counters.6,
            counters.7,
            counters.8
        );
    }

    for task in tasks {
        let status = task.status.try_lock().map(|guard| *guard);
        let reason = task.block_reason.try_lock().map(|guard| *guard);
        let outcome = task.wait_outcome.try_lock().map(|guard| *guard);
        let exec = task
            .inner
            .try_lock()
            .map(|inner| inner.exec_path.clone())
            .unwrap_or_else(|| String::from("<locked>"));
        let running_cpu = task.running_cpu.load(Ordering::Acquire);
        let blocking_cpu = task.blocking_cpu.load(Ordering::Acquire);
        crate::println!(
            "[buildstorm-diag] task pid={} tgid={} exec={} status={:?} reason={:?} outcome={:?} token={} running_cpu={} blocking_cpu={} affinity={:#x}",
            task.pid.0,
            task.thread_group.tgid(),
            exec,
            status,
            reason,
            outcome,
            task.current_wait_token(),
            running_cpu,
            blocking_cpu,
            task.affinity_mask.load(Ordering::Acquire)
        );
        if matches!(status, Some(TaskStatus::Ready))
            && (running_cpu != NO_CPU || blocking_cpu != NO_CPU)
        {
            crate::println!(
                "[buildstorm-diag] ownership-anomaly pid={} status=Ready running_cpu={} blocking_cpu={}",
                task.pid.0,
                running_cpu,
                blocking_cpu
            );
        }
        if matches!(status, Some(TaskStatus::Running)) && running_cpu == NO_CPU {
            crate::println!(
                "[buildstorm-diag] ownership-anomaly pid={} status=Running without owner",
                task.pid.0
            );
        }
    }

    crate::task::wait_queue::diagnostic_dump_waiters();
    crate::syscall::other::diagnostic_dump_futex_waiters();
    crate::println!(
        "[buildstorm-diag] snapshot={} end now_us={}",
        sequence,
        crate::timer::get_time_us()
    );
}
