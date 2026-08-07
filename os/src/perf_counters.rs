#[cfg(feature = "perf-counters")]
use core::sync::atomic::{AtomicUsize, Ordering};

#[cfg(feature = "perf-counters")]
static PAGE_FAULTS: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "perf-counters")]
static CLEAN_FILE_FAULTS: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "perf-counters")]
static USER_PAGE_TABLE_ACTIVATIONS: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "perf-counters")]
static KERNEL_PAGE_TABLE_RESTORES: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "perf-counters")]
static BLOCK_READ_REQUESTS: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "perf-counters")]
static BLOCK_READ_BYTES: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "perf-counters")]
static BLOCK_READ_SAME: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "perf-counters")]
static BLOCK_READ_SEQUENTIAL: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "perf-counters")]
static BLOCK_READ_RANDOM: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "perf-counters")]
static LAST_BLOCK_READ_OFFSET: [AtomicUsize; crate::config::MAX_CPUS] =
    [const { AtomicUsize::new(usize::MAX) }; crate::config::MAX_CPUS];
#[cfg(feature = "perf-counters")]
static LAST_BLOCK_READ_END: [AtomicUsize; crate::config::MAX_CPUS] =
    [const { AtomicUsize::new(usize::MAX) }; crate::config::MAX_CPUS];

#[cfg(all(feature = "perf-counters", feature = "buildstorm-diagnostics"))]
pub fn diagnostic_snapshot() -> (
    usize,
    usize,
    usize,
    usize,
    usize,
    usize,
    usize,
    usize,
    usize,
) {
    (
        PAGE_FAULTS.load(Ordering::Relaxed),
        CLEAN_FILE_FAULTS.load(Ordering::Relaxed),
        USER_PAGE_TABLE_ACTIVATIONS.load(Ordering::Relaxed),
        KERNEL_PAGE_TABLE_RESTORES.load(Ordering::Relaxed),
        BLOCK_READ_REQUESTS.load(Ordering::Relaxed),
        BLOCK_READ_BYTES.load(Ordering::Relaxed),
        BLOCK_READ_SAME.load(Ordering::Relaxed),
        BLOCK_READ_SEQUENTIAL.load(Ordering::Relaxed),
        BLOCK_READ_RANDOM.load(Ordering::Relaxed),
    )
}

#[cfg(feature = "perf-counters")]
const REPORT_FAULT_INTERVAL: usize = 16 * 1024;

#[inline]
pub fn note_page_fault() {
    #[cfg(feature = "perf-counters")]
    {
        let faults = PAGE_FAULTS.fetch_add(1, Ordering::Relaxed) + 1;
        if faults % REPORT_FAULT_INTERVAL == 0 {
            log::info!(
                "[perf-counters] faults={} clean_faults={} user_pt={} kernel_pt={} block_reads={} block_bytes={}",
                faults,
                CLEAN_FILE_FAULTS.load(Ordering::Relaxed),
                USER_PAGE_TABLE_ACTIVATIONS.load(Ordering::Relaxed),
                KERNEL_PAGE_TABLE_RESTORES.load(Ordering::Relaxed),
                BLOCK_READ_REQUESTS.load(Ordering::Relaxed),
                BLOCK_READ_BYTES.load(Ordering::Relaxed),
            );
        }
    }
}

#[inline]
pub fn note_clean_file_fault() {
    #[cfg(feature = "perf-counters")]
    CLEAN_FILE_FAULTS.fetch_add(1, Ordering::Relaxed);
}

#[inline]
pub fn note_user_page_table_activation() {
    #[cfg(feature = "perf-counters")]
    USER_PAGE_TABLE_ACTIVATIONS.fetch_add(1, Ordering::Relaxed);
}

#[inline]
pub fn note_kernel_page_table_restore() {
    #[cfg(feature = "perf-counters")]
    KERNEL_PAGE_TABLE_RESTORES.fetch_add(1, Ordering::Relaxed);
}

#[inline]
pub fn note_block_read(offset: usize, bytes: usize) {
    #[cfg(feature = "perf-counters")]
    {
        BLOCK_READ_REQUESTS.fetch_add(1, Ordering::Relaxed);
        BLOCK_READ_BYTES.fetch_add(bytes, Ordering::Relaxed);
        let cpu = crate::platform::current_cpu_index();
        if cpu < crate::config::MAX_CPUS {
            let previous_offset = LAST_BLOCK_READ_OFFSET[cpu].swap(offset, Ordering::Relaxed);
            let previous_end =
                LAST_BLOCK_READ_END[cpu].swap(offset.saturating_add(bytes), Ordering::Relaxed);
            if previous_offset == offset {
                BLOCK_READ_SAME.fetch_add(1, Ordering::Relaxed);
            } else if previous_end == offset {
                BLOCK_READ_SEQUENTIAL.fetch_add(1, Ordering::Relaxed);
            } else if previous_offset != usize::MAX {
                BLOCK_READ_RANDOM.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
    #[cfg(not(feature = "perf-counters"))]
    let _ = (offset, bytes);
}
