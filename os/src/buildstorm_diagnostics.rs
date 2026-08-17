//! BuildStorm-only aggregate diagnostics.
//!
//! The hot-path interface is intentionally limited to relaxed atomics and fixed-size
//! arrays.  This module is compiled only with `buildstorm-diagnostics`; production
//! kernels neither include these counters nor emit these reports.

use core::ops::{Deref, DerefMut};
use core::sync::atomic::{AtomicUsize, Ordering};

use spin::{Mutex, MutexGuard};

use crate::task::wait_queue::{BlockReason, WaitOutcome};

const REPORT_INTERVAL_US: usize = 10 * 1_000_000;
// This diagnostic build is dedicated to the terminal exec stall. Keeping the
// periodic report compact avoids turning serial output into a second workload.
const EXEC_FOCUS_ONLY: bool = true;
const CPU_SLOTS: usize = crate::config::MAX_CPUS;

#[inline]
pub(crate) const fn exec_focus_only() -> bool {
    EXEC_FOCUS_ONLY
}
const SYSCALL_SLOTS: usize = 512;
const SYSCALL_SAMPLE_SHIFT: usize = 6;
const SYSCALL_SAMPLE_MASK: usize = (1 << SYSCALL_SAMPLE_SHIFT) - 1;
const USER_TRAP_SLOTS: usize = 9;
const USER_TRAP_NAMES: [&str; USER_TRAP_SLOTS] = [
    "unused",
    "syscall",
    "timer",
    "ipi",
    "store_page_fault",
    "load_page_fault",
    "instruction_page_fault",
    "privilege_page_fault",
    "other",
];
const MMAP_PROTOCOL_SLOTS: usize = 16;
const EXITED_MMAP_PROTOCOL_SLOTS: usize = 128;

struct MmapReservationSlot {
    // Even values publish sequence << 1; the low bit is a transient writer lock.
    state: AtomicUsize,
    start: AtomicUsize,
    end: AtomicUsize,
    trims: AtomicUsize,
}

impl MmapReservationSlot {
    const fn new() -> Self {
        Self {
            state: AtomicUsize::new(0),
            start: AtomicUsize::new(0),
            end: AtomicUsize::new(0),
            trims: AtomicUsize::new(0),
        }
    }
}

/// Fixed, thread-group-owned state for correlating generic large anonymous
/// reservations with their subsequent prefix/suffix trims.
pub(crate) struct MmapProtocol {
    next_sequence: AtomicUsize,
    attempts: AtomicUsize,
    select_enomem: AtomicUsize,
    commit_failures: AtomicUsize,
    successes: AtomicUsize,
    active: AtomicUsize,
    evictions: AtomicUsize,
    trim_first: AtomicUsize,
    trim_second: AtomicUsize,
    trim_extra: AtomicUsize,
    trim_prefix: AtomicUsize,
    trim_suffix: AtomicUsize,
    full_release: AtomicUsize,
    munmap_unmatched: AtomicUsize,
    requested_bytes: AtomicUsize,
    successful_bytes: AtomicUsize,
    pending_slots: AtomicUsize,
    slots: [MmapReservationSlot; MMAP_PROTOCOL_SLOTS],
}

impl MmapProtocol {
    pub(crate) const fn new() -> Self {
        Self {
            next_sequence: AtomicUsize::new(0),
            attempts: AtomicUsize::new(0),
            select_enomem: AtomicUsize::new(0),
            commit_failures: AtomicUsize::new(0),
            successes: AtomicUsize::new(0),
            active: AtomicUsize::new(0),
            evictions: AtomicUsize::new(0),
            trim_first: AtomicUsize::new(0),
            trim_second: AtomicUsize::new(0),
            trim_extra: AtomicUsize::new(0),
            trim_prefix: AtomicUsize::new(0),
            trim_suffix: AtomicUsize::new(0),
            full_release: AtomicUsize::new(0),
            munmap_unmatched: AtomicUsize::new(0),
            requested_bytes: AtomicUsize::new(0),
            successful_bytes: AtomicUsize::new(0),
            pending_slots: AtomicUsize::new(0),
            slots: [const { MmapReservationSlot::new() }; MMAP_PROTOCOL_SLOTS],
        }
    }

    fn snapshot(&self, tgid: usize) -> MmapProtocolSnapshot {
        MmapProtocolSnapshot {
            tgid,
            last_sequence: self.next_sequence.load(Ordering::Relaxed),
            attempts: self.attempts.load(Ordering::Relaxed),
            select_enomem: self.select_enomem.load(Ordering::Relaxed),
            commit_failures: self.commit_failures.load(Ordering::Relaxed),
            successes: self.successes.load(Ordering::Relaxed),
            active: self.active.load(Ordering::Relaxed),
            evictions: self.evictions.load(Ordering::Relaxed),
            trim_first: self.trim_first.load(Ordering::Relaxed),
            trim_second: self.trim_second.load(Ordering::Relaxed),
            trim_extra: self.trim_extra.load(Ordering::Relaxed),
            trim_prefix: self.trim_prefix.load(Ordering::Relaxed),
            trim_suffix: self.trim_suffix.load(Ordering::Relaxed),
            full_release: self.full_release.load(Ordering::Relaxed),
            munmap_unmatched: self.munmap_unmatched.load(Ordering::Relaxed),
            requested_bytes: self.requested_bytes.load(Ordering::Relaxed),
            successful_bytes: self.successful_bytes.load(Ordering::Relaxed),
        }
    }
}

#[derive(Clone, Copy)]
struct MmapProtocolSnapshot {
    tgid: usize,
    last_sequence: usize,
    attempts: usize,
    select_enomem: usize,
    commit_failures: usize,
    successes: usize,
    active: usize,
    evictions: usize,
    trim_first: usize,
    trim_second: usize,
    trim_extra: usize,
    trim_prefix: usize,
    trim_suffix: usize,
    full_release: usize,
    munmap_unmatched: usize,
    requested_bytes: usize,
    successful_bytes: usize,
}

impl MmapProtocolSnapshot {
    const EMPTY: Self = Self {
        tgid: 0,
        last_sequence: 0,
        attempts: 0,
        select_enomem: 0,
        commit_failures: 0,
        successes: 0,
        active: 0,
        evictions: 0,
        trim_first: 0,
        trim_second: 0,
        trim_extra: 0,
        trim_prefix: 0,
        trim_suffix: 0,
        full_release: 0,
        munmap_unmatched: 0,
        requested_bytes: 0,
        successful_bytes: 0,
    };
}

struct ExitedMmapProtocolArchive {
    entries: [MmapProtocolSnapshot; EXITED_MMAP_PROTOCOL_SLOTS],
    head: usize,
    len: usize,
    archived: usize,
    reported: usize,
    dropped: usize,
}

impl ExitedMmapProtocolArchive {
    const fn new() -> Self {
        Self {
            entries: [MmapProtocolSnapshot::EMPTY; EXITED_MMAP_PROTOCOL_SLOTS],
            head: 0,
            len: 0,
            archived: 0,
            reported: 0,
            dropped: 0,
        }
    }

    fn push(&mut self, snapshot: MmapProtocolSnapshot) {
        self.archived = self.archived.saturating_add(1);
        if self.len == EXITED_MMAP_PROTOCOL_SLOTS {
            self.dropped = self.dropped.saturating_add(1);
            return;
        }
        let index = (self.head + self.len) % EXITED_MMAP_PROTOCOL_SLOTS;
        self.entries[index] = snapshot;
        self.len += 1;
    }

    fn pop(&mut self) -> Option<(usize, MmapProtocolSnapshot)> {
        if self.len == 0 {
            return None;
        }
        let snapshot = self.entries[self.head];
        self.head = (self.head + 1) % EXITED_MMAP_PROTOCOL_SLOTS;
        self.len -= 1;
        self.reported = self.reported.saturating_add(1);
        Some((self.reported, snapshot))
    }
}

static EXITED_MMAP_PROTOCOLS: Mutex<ExitedMmapProtocolArchive> =
    Mutex::new(ExitedMmapProtocolArchive::new());

#[derive(Clone, Copy)]
#[repr(usize)]
pub(crate) enum WorkClass {
    OpenAt,
    Statx,
    Readlink,
    Read,
    Pread,
    Mmap,
    Munmap,
    Mprotect,
    PageFaultLoad,
    PageFaultStore,
    PageFaultExec,
    PageFaultAnonymousAllocate,
    PageFaultAnonymousInstall,
    PageFaultAnonymousSplit,
    PageFaultAnonymousMap,
    PageFaultAnonymousCoalesce,
    VirtioRead,
    VirtioWrite,
    ExtentLookup,
    AddressSpaceActivation,
    TlbShootdown,
    CleanCacheMetadata,
    CleanCachePrefetch,
    CleanCacheGetRun,
}

const WORK_NAMES: [&str; 24] = [
    "openat",
    "statx",
    "readlink",
    "read",
    "pread",
    "mmap",
    "munmap",
    "mprotect",
    "pf_load",
    "pf_store",
    "pf_exec",
    "pf_anonymous_allocate",
    "pf_anonymous_install",
    "pf_anonymous_split",
    "pf_anonymous_map",
    "pf_anonymous_coalesce",
    "virtio_read",
    "virtio_write",
    "extent_lookup",
    "address_space_activation",
    "tlb_shootdown",
    "clean_cache_metadata",
    "clean_cache_prefetch",
    "clean_cache_get_run",
];
const WORK_SLOTS: usize = WORK_NAMES.len();

// VFS and ext4 paths already expose these stable phase boundaries.  Keep their
// numeric IDs source-compatible while recording only fixed-size aggregates:
// diagnostics must not log individual syscalls, path lookups, or block I/O.
const PHASE_NAMES: [&str; 59] = [
    "readlink_user_path",
    "readlink_target",
    "readlink_copy_out",
    "openat_path_resolve",
    "openat_vfs_and_fd",
    "close_fd_remove",
    "close_descriptor_drop",
    "statx_user_path",
    "statx_path_resolve",
    "statx_vfs_metadata",
    "statx_copy_out",
    "vfs_readlink_parent_resolve",
    "vfs_readlink_search_access",
    "vfs_readlink_mem_probe",
    "vfs_readlink_ext4_lookup",
    "vfs_symlink_backend_probe",
    "vfs_symlink_ext4_lookup",
    "ext4_close_reclaim_lock",
    "reserved_18",
    "ext4_close_discard",
    "reserved_20",
    "ext4_close_not_cached",
    "ext4_close_dirty_queued",
    "vfs_lookup_parent_symlinks",
    "vfs_lookup_search_access",
    "vfs_lookup_metadata",
    "vfs_metadata_backend_select",
    "vfs_metadata_mem",
    "vfs_metadata_ext4",
    "vfs_metadata_ext4_symlink",
    "vfs_metadata_convert",
    "vfs_open_pseudo_refresh",
    "vfs_open_parent_symlink",
    "vfs_open_final_symlink",
    "vfs_open_backend_probe",
    "vfs_open_directory_probe",
    "vfs_open_ext4_regular_lookup",
    "vfs_open_create_parent_probe",
    "vfs_open_create",
    "vfs_open_missing_errno",
    "vfs_metadata_normalize_whiteout",
    "vfs_metadata_tmpfs_route",
    "vfs_metadata_ext4_path",
    "vfs_metadata_mounted_ext4",
    "vfs_metadata_missing_errno",
    "exec_replace_swap",
    "exec_replace_old_drop",
    "exec_old_asid_retire",
    "exec_old_page_table_drop",
    "exec_old_resident_drop",
    "namespace_create_parent_missing",
    "namespace_create_backend_enoent",
    "namespace_rename_old_parent_missing",
    "namespace_rename_new_parent_missing",
    "namespace_rename_source_missing",
    "namespace_unlink_parent_missing",
    "namespace_unlink_target_missing",
    "namespace_positive_generation_crossing",
    "namespace_negative_generation_crossing",
];
const PHASE_SLOTS: usize = PHASE_NAMES.len();

#[derive(Clone, Copy)]
#[repr(usize)]
pub(crate) enum PageFaultSource {
    HardwareLoad,
    HardwareStore,
    HardwareExec,
    PrepareRead,
    PrepareWrite,
}

const PAGE_FAULT_SOURCE_NAMES: [&str; 5] = [
    "hardware_load",
    "hardware_store",
    "hardware_exec",
    "prepare_read",
    "prepare_write",
];
const PAGE_FAULT_SOURCE_SLOTS: usize = PAGE_FAULT_SOURCE_NAMES.len();

/// Successful resolution branch taken by `MemorySet::handle_page_fault()`.
/// This is deliberately independent of the hardware/kernel entry source.
#[derive(Clone, Copy)]
#[repr(usize)]
pub(crate) enum PageFaultResolution {
    Cow,
    ExistingMapping,
    FileResident,
    AnonymousDemand,
    CleanFileCache,
    ExistingFrame,
    BackingRead,
}

const PAGE_FAULT_RESOLUTION_NAMES: [&str; 7] = [
    "cow",
    "existing_mapping",
    "file_resident",
    "anonymous_demand",
    "clean_file_cache",
    "existing_frame",
    "backing_read",
];
const PAGE_FAULT_RESOLUTION_SLOTS: usize = PAGE_FAULT_RESOLUTION_NAMES.len();

#[derive(Clone, Copy)]
#[repr(usize)]
pub(crate) enum LockClass {
    Vfs,
    Ext4,
    BlockCache,
    Virtio,
    TaskManager,
    WaitQueue,
    FrameAllocator,
    Heap,
    MemorySet,
    MemorySetActivation,
    FdTable,
}

const LOCK_NAMES: [&str; 11] = [
    "vfs",
    "ext4",
    "block_cache",
    "virtio",
    "task_manager",
    "wait_queue",
    "frame_allocator",
    "heap",
    "memory_set",
    "memory_set_activation",
    "fd_table",
];
const LOCK_SLOTS: usize = LOCK_NAMES.len();
const HOT_LOCK_SAMPLE_SHIFT: usize = 10;
const HOT_LOCK_SAMPLE_MASK: usize = (1 << HOT_LOCK_SAMPLE_SHIFT) - 1;
const HOT_LOCK_SLOTS: usize = 6;
const WORK_SAMPLE_SHIFT: usize = 10;
const WORK_SAMPLE_MASK: usize = (1 << WORK_SAMPLE_SHIFT) - 1;
const MEMORY_SET_OWNER_UNKNOWN: usize = MEMORY_SET_LOCK_SITE_SLOTS;
const MEMORY_SET_OWNER_SLOTS: usize = MEMORY_SET_LOCK_SITE_SLOTS + 1;
const HEAP_SIZE_BUCKETS: usize = 11;

#[repr(align(64))]
struct PerCpuCounter(AtomicUsize);

impl PerCpuCounter {
    const fn new() -> Self {
        Self(AtomicUsize::new(0))
    }

    #[inline]
    fn load(&self, ordering: Ordering) -> usize {
        self.0.load(ordering)
    }

    #[inline]
    fn store(&self, value: usize, ordering: Ordering) {
        self.0.store(value, ordering);
    }

    #[inline]
    fn fetch_add(&self, value: usize, ordering: Ordering) -> usize {
        self.0.fetch_add(value, ordering)
    }
}

/// Why a shared user address-space lock was acquired.
///
/// These are deliberately call-path classes, not process, command, crate, or
/// path names. They make the aggregate MemorySet lock total actionable without
/// emitting an event for any individual syscall or fault.
#[derive(Clone, Copy)]
#[repr(usize)]
pub(crate) enum MemorySetLockSite {
    Other,
    UserEntryActivation,
    HardwarePageFault,
    UserCopyRead,
    UserCopyWrite,
    Brk,
    MmapSelect,
    MmapCommit,
    Mprotect,
    Madvise,
    Munmap,
    FileInvalidate,
    FileWriteback,
    SharedMemory,
    ForkCow,
    ExecReplace,
    Futex,
}

const MEMORY_SET_LOCK_SITE_NAMES: [&str; 17] = [
    "other",
    "user_entry_activation",
    "hardware_page_fault",
    "user_copy_read",
    "user_copy_write",
    "brk",
    "mmap_select",
    "mmap_commit",
    "mprotect",
    "madvise",
    "munmap",
    "file_invalidate",
    "file_writeback",
    "shared_memory",
    "fork_cow",
    "exec_replace",
    "futex",
];
const MEMORY_SET_LOCK_SITE_SLOTS: usize = MEMORY_SET_LOCK_SITE_NAMES.len();

static MMAP_ANONYMOUS: AtomicUsize = AtomicUsize::new(0);
static MMAP_FILE: AtomicUsize = AtomicUsize::new(0);
static MMAP_FIXED: AtomicUsize = AtomicUsize::new(0);
static MMAP_HINTED: AtomicUsize = AtomicUsize::new(0);
static MMAP_LAZY: AtomicUsize = AtomicUsize::new(0);
static MMAP_EAGER: AtomicUsize = AtomicUsize::new(0);
static MMAP_COMMIT_RESELECT: AtomicUsize = AtomicUsize::new(0);
const MMAP_LENGTH_BUCKETS: usize = 5;
static MMAP_LENGTH_COUNTS: [AtomicUsize; MMAP_LENGTH_BUCKETS] =
    [const { AtomicUsize::new(0) }; MMAP_LENGTH_BUCKETS];
static MMAP_FAILED_LENGTH_COUNTS: [AtomicUsize; MMAP_LENGTH_BUCKETS] =
    [const { AtomicUsize::new(0) }; MMAP_LENGTH_BUCKETS];
static MMAP_SELECT_OK: AtomicUsize = AtomicUsize::new(0);
static MMAP_SELECT_ENOMEM: AtomicUsize = AtomicUsize::new(0);
static MMAP_COMMIT_RESELECT_OK: AtomicUsize = AtomicUsize::new(0);
static MMAP_COMMIT_RESELECT_ENOMEM: AtomicUsize = AtomicUsize::new(0);
static MMAP_FAILED_LENGTH_MAX: AtomicUsize = AtomicUsize::new(0);
static MMAP_FAILED_GAP_MAX: AtomicUsize = AtomicUsize::new(0);
static MMAP_FAILED_VMAS_MAX: AtomicUsize = AtomicUsize::new(0);
static MMAP_HIGH_ARENA_PLACEMENTS: AtomicUsize = AtomicUsize::new(0);
static MMAP_LOW_ARENA_PLACEMENTS: AtomicUsize = AtomicUsize::new(0);
static MADVISE_DONTNEED_CALLS: AtomicUsize = AtomicUsize::new(0);
static MADVISE_DONTNEED_REQUESTED_PAGES: AtomicUsize = AtomicUsize::new(0);
static MADVISE_DONTNEED_DISCARDED_PAGES: AtomicUsize = AtomicUsize::new(0);
static MUNMAP_BELOW_CURSOR_LENGTH_COUNTS: [AtomicUsize; MMAP_LENGTH_BUCKETS] =
    [const { AtomicUsize::new(0) }; MMAP_LENGTH_BUCKETS];
static MUNMAP_OVERLAPS_CURSOR: AtomicUsize = AtomicUsize::new(0);
static MUNMAP_AT_OR_ABOVE_CURSOR: AtomicUsize = AtomicUsize::new(0);
static PROCESS_EXITS: AtomicUsize = AtomicUsize::new(0);
static CHILD_WAKE_CALLS: AtomicUsize = AtomicUsize::new(0);
static CHILD_WOKEN: AtomicUsize = AtomicUsize::new(0);
static CLONE_CALLS: AtomicUsize = AtomicUsize::new(0);
static CLONE_VFORK_CALLS: AtomicUsize = AtomicUsize::new(0);
static CLONE_SHARE_VM_CALLS: AtomicUsize = AtomicUsize::new(0);
static CLONE_FORK_COW_CALLS: AtomicUsize = AtomicUsize::new(0);
static CLONE_FORK_COW_TOTAL_US: AtomicUsize = AtomicUsize::new(0);
static CLONE_FORK_COW_MAX_US: AtomicUsize = AtomicUsize::new(0);
static CLONE_VFORK_FORK_COW_CALLS: AtomicUsize = AtomicUsize::new(0);
static CLONE_VFORK_FORK_COW_TOTAL_US: AtomicUsize = AtomicUsize::new(0);
static CLONE_FORK_COW_AREAS: AtomicUsize = AtomicUsize::new(0);
static CLONE_FORK_COW_RESIDENT_PAGES: AtomicUsize = AtomicUsize::new(0);

const BLOCK_NAMES: [&str; 7] = [
    "block_io",
    "pipe_fd_ready",
    "poll",
    "wait4_waitid",
    "timer",
    "futex",
    "signal",
];
const BLOCK_SLOTS: usize = BLOCK_NAMES.len();
const BLOCK_ACTOR_NAMES: [&str; 3] = ["rustc", "cargo", "other"];
const BLOCK_ACTOR_SLOTS: usize = BLOCK_ACTOR_NAMES.len();

static NEXT_REPORT_AT_US: AtomicUsize = AtomicUsize::new(0);
static REPORT_SEQUENCE: AtomicUsize = AtomicUsize::new(0);

// Lifecycle markers are intentionally sampled.  A full compiler workload can
// execute thousands of short-lived helpers; emitting every boundary would
// hide the first stuck transition in serial output and perturb the workload.
static EXEC_SUCCESS_MARKERS: AtomicUsize = AtomicUsize::new(0);
static EXEC_FAILURE_MARKERS: AtomicUsize = AtomicUsize::new(0);
static WAIT4_MARKERS: AtomicUsize = AtomicUsize::new(0);
static WAIT4_BLOCK_MARKERS: AtomicUsize = AtomicUsize::new(0);
static EXIT_MARKERS: AtomicUsize = AtomicUsize::new(0);

#[inline]
fn lifecycle_sample(sequence: usize) -> bool {
    sequence <= 64 || sequence % 256 == 0
}

#[inline]
pub(crate) fn note_exec_success_marker() -> Option<usize> {
    if EXEC_FOCUS_ONLY {
        return None;
    }
    let sequence = EXEC_SUCCESS_MARKERS.fetch_add(1, Ordering::Relaxed) + 1;
    lifecycle_sample(sequence).then_some(sequence)
}

#[inline]
pub(crate) fn note_wait4_marker() -> Option<usize> {
    if EXEC_FOCUS_ONLY {
        return None;
    }
    let sequence = WAIT4_MARKERS.fetch_add(1, Ordering::Relaxed) + 1;
    lifecycle_sample(sequence).then_some(sequence)
}

#[inline]
pub(crate) fn note_wait4_block_marker() -> Option<usize> {
    if EXEC_FOCUS_ONLY {
        return None;
    }
    let sequence = WAIT4_BLOCK_MARKERS.fetch_add(1, Ordering::Relaxed) + 1;
    lifecycle_sample(sequence).then_some(sequence)
}

#[inline]
pub(crate) fn note_exit_marker() -> Option<usize> {
    if EXEC_FOCUS_ONLY {
        return None;
    }
    let sequence = EXIT_MARKERS.fetch_add(1, Ordering::Relaxed) + 1;
    lifecycle_sample(sequence).then_some(sequence)
}

// Terminal user traps are exceptional lifecycle boundaries, rather than a
// page-fault trace.  Capture just the first one so a failed diagnostic run can
// be attributed without perturbing the normal MM hot path.
//
// State: empty -> recording -> ready -> reported.  The release store makes
// every field visible to the harness before it formats the single record.
static FIRST_TERMINAL_USER_TRAP_STATE: AtomicUsize = AtomicUsize::new(0);
static FIRST_TERMINAL_USER_TRAP_KIND: AtomicUsize = AtomicUsize::new(0);
static FIRST_TERMINAL_USER_TRAP_PID: AtomicUsize = AtomicUsize::new(0);
static FIRST_TERMINAL_USER_TRAP_VADDR: AtomicUsize = AtomicUsize::new(0);
static FIRST_TERMINAL_USER_TRAP_SEPC: AtomicUsize = AtomicUsize::new(0);
static FIRST_TERMINAL_USER_TRAP_SP: AtomicUsize = AtomicUsize::new(0);
static FIRST_TERMINAL_USER_TRAP_RA: AtomicUsize = AtomicUsize::new(0);
static FIRST_TERMINAL_USER_TRAP_TP: AtomicUsize = AtomicUsize::new(0);
static FIRST_TERMINAL_USER_TRAP_FAULT_PA: AtomicUsize = AtomicUsize::new(0);
static FIRST_TERMINAL_USER_TRAP_SEPC_PA: AtomicUsize = AtomicUsize::new(0);
static FIRST_TERMINAL_USER_TRAP_VMA_START: AtomicUsize = AtomicUsize::new(0);
static FIRST_TERMINAL_USER_TRAP_VMA_END: AtomicUsize = AtomicUsize::new(0);
static FIRST_TERMINAL_USER_TRAP_VMA_FLAGS: AtomicUsize = AtomicUsize::new(0);
static FIRST_TERMINAL_USER_TRAP_BACKING: AtomicUsize = AtomicUsize::new(0);
static FIRST_TERMINAL_USER_TRAP_RESIDENT: AtomicUsize = AtomicUsize::new(0);
static FIRST_TERMINAL_USER_TRAP_PAGE_STATE: AtomicUsize = AtomicUsize::new(0);

// The first failed fault-resolution branch is kept separate from the terminal
// trap.  A syscall-side user-copy may fail without becoming a signal, whereas
// a terminal trap needs its register snapshot.  Both records are single-shot.
static FIRST_PAGE_FAULT_FAILURE_STATE: AtomicUsize = AtomicUsize::new(0);
static FIRST_PAGE_FAULT_FAILURE_STAGE: AtomicUsize = AtomicUsize::new(0);
static FIRST_PAGE_FAULT_FAILURE_ERRNO: AtomicUsize = AtomicUsize::new(0);
static FIRST_PAGE_FAULT_FAILURE_VADDR: AtomicUsize = AtomicUsize::new(0);

// Keep only the first execve error.  This is a failure-boundary record, not a
// syscall trace; production builds do not compile this state or its output.
static FIRST_EXEC_FAILURE_STATE: AtomicUsize = AtomicUsize::new(0);
static FIRST_EXEC_FAILURE_ERRNO: AtomicUsize = AtomicUsize::new(0);
static FIRST_EXEC_FAILURE_PID: AtomicUsize = AtomicUsize::new(0);
static FIRST_EXEC_FAILURE_PATH_PTR: AtomicUsize = AtomicUsize::new(0);
static FIRST_EXEC_FAILURE_ARGV_PTR: AtomicUsize = AtomicUsize::new(0);
static FIRST_EXEC_FAILURE_ENVP_PTR: AtomicUsize = AtomicUsize::new(0);
static FIRST_EXEC_FAILURE_STAGE: AtomicUsize = AtomicUsize::new(0);
static FIRST_EXEC_FAILURE_VECTOR_BASE: AtomicUsize = AtomicUsize::new(0);
static FIRST_EXEC_FAILURE_INDEX: AtomicUsize = AtomicUsize::new(0);
static FIRST_EXEC_FAILURE_ENTRY_ADDR: AtomicUsize = AtomicUsize::new(0);
static FIRST_EXEC_FAILURE_VALUE_PTR: AtomicUsize = AtomicUsize::new(0);
static FIRST_EXEC_FAILURE_PARENT_PID: AtomicUsize = AtomicUsize::new(0);
static FIRST_EXEC_FAILURE_CLONE_FLAGS: AtomicUsize = AtomicUsize::new(0);
static FIRST_EXEC_FAILURE_VMA_START: AtomicUsize = AtomicUsize::new(0);
static FIRST_EXEC_FAILURE_VMA_END: AtomicUsize = AtomicUsize::new(0);
static FIRST_EXEC_FAILURE_VMA_FLAGS: AtomicUsize = AtomicUsize::new(0);
static FIRST_EXEC_FAILURE_BACKING: AtomicUsize = AtomicUsize::new(0);
static FIRST_EXEC_FAILURE_RESIDENT: AtomicUsize = AtomicUsize::new(0);
static FIRST_EXEC_FAILURE_PAGE_STATE: AtomicUsize = AtomicUsize::new(0);
static FIRST_EXEC_FAILURE_PTE_PA: AtomicUsize = AtomicUsize::new(0);

#[derive(Clone, Copy)]
pub(crate) struct TerminalUserTrap {
    pub kind: usize,
    pub pid: usize,
    pub vaddr: usize,
    pub sepc: usize,
    pub sp: usize,
    pub ra: usize,
    pub tp: usize,
    pub fault_pa: usize,
    pub sepc_pa: usize,
    pub vma_start: usize,
    pub vma_end: usize,
    pub vma_flags: usize,
    pub backing: usize,
    pub resident: usize,
    pub page_state: usize,
}

#[derive(Clone, Copy)]
pub(crate) struct PageFaultFailure {
    pub stage: usize,
    pub errno: usize,
    pub vaddr: usize,
}

#[derive(Clone, Copy)]
pub(crate) struct ExecFailure {
    pub errno: usize,
    pub pid: usize,
    pub path_ptr: usize,
    pub argv_ptr: usize,
    pub envp_ptr: usize,
    pub stage: usize,
    pub vector_base: usize,
    pub index: usize,
    pub entry_addr: usize,
    pub value_ptr: usize,
    pub parent_pid: usize,
    pub clone_flags: usize,
    pub vma_start: usize,
    pub vma_end: usize,
    pub vma_flags: usize,
    pub backing: usize,
    pub resident: usize,
    pub page_state: usize,
    pub pte_pa: usize,
}

pub(crate) const EXEC_FAILURE_STAGE_FALLBACK: usize = 0;
pub(crate) const EXEC_FAILURE_STAGE_PATH: usize = 1;
pub(crate) const EXEC_FAILURE_STAGE_ARGV_ENTRY: usize = 2;
pub(crate) const EXEC_FAILURE_STAGE_ARGV_STRING: usize = 3;
pub(crate) const EXEC_FAILURE_STAGE_ENVP_ENTRY: usize = 4;
pub(crate) const EXEC_FAILURE_STAGE_ENVP_STRING: usize = 5;

/// Failure-stage value for the clean executable/file-page cache path.
pub(crate) const PAGE_FAULT_FAILURE_CLEAN_CACHE: usize = 1;
/// One-shot page-fault error boundaries.  These constants are only used by
/// the diagnostics feature; production kernels neither format nor emit them.
pub(crate) const PAGE_FAULT_FAILURE_CLEAN_EMPTY: usize = 2;
pub(crate) const PAGE_FAULT_FAILURE_CLEAN_WINDOW: usize = 3;
pub(crate) const PAGE_FAULT_FAILURE_CLEAN_SPLIT_LOOKUP: usize = 4;
pub(crate) const PAGE_FAULT_FAILURE_CLEAN_SPLIT_SHAPE: usize = 5;
pub(crate) const PAGE_FAULT_FAILURE_FALLBACK_SPLIT_LOOKUP: usize = 6;
pub(crate) const PAGE_FAULT_FAILURE_FALLBACK_SPLIT_SHAPE: usize = 7;
pub(crate) const PAGE_FAULT_FAILURE_FALLBACK_ALLOC: usize = 8;
pub(crate) const PAGE_FAULT_FAILURE_BACKING_READ: usize = 9;
pub(crate) const PAGE_FAULT_FAILURE_RESIDENT_INSERT: usize = 10;
pub(crate) const PAGE_FAULT_FAILURE_PREPARE_WRITE_NO_VMA: usize = 11;
pub(crate) const PAGE_FAULT_FAILURE_PREPARE_WRITE_PERM: usize = 12;
pub(crate) const PAGE_FAULT_FAILURE_PREPARE_READ_NO_VMA: usize = 13;
pub(crate) const PAGE_FAULT_FAILURE_PREPARE_READ_PERM: usize = 14;
pub(crate) const PAGE_FAULT_FAILURE_HANDLER_NO_VMA: usize = 15;
pub(crate) const PAGE_FAULT_FAILURE_HANDLER_NO_LEAF: usize = 16;
pub(crate) const PAGE_FAULT_FAILURE_HANDLER_EXEC_PERM: usize = 17;
pub(crate) const PAGE_FAULT_FAILURE_HANDLER_STORE_PERM: usize = 18;
pub(crate) const PAGE_FAULT_FAILURE_HANDLER_READ_PERM: usize = 19;
pub(crate) const PAGE_FAULT_FAILURE_HANDLER_COW: usize = 20;

#[inline]
pub(crate) fn note_first_page_fault_failure(stage: usize, errno: usize, vaddr: usize) {
    if FIRST_PAGE_FAULT_FAILURE_STATE
        .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return;
    }
    FIRST_PAGE_FAULT_FAILURE_STAGE.store(stage, Ordering::Relaxed);
    FIRST_PAGE_FAULT_FAILURE_ERRNO.store(errno, Ordering::Relaxed);
    FIRST_PAGE_FAULT_FAILURE_VADDR.store(vaddr, Ordering::Relaxed);
    FIRST_PAGE_FAULT_FAILURE_STATE.store(2, Ordering::Release);
}

pub(crate) fn take_first_page_fault_failure() -> Option<PageFaultFailure> {
    FIRST_PAGE_FAULT_FAILURE_STATE
        .compare_exchange(2, 3, Ordering::AcqRel, Ordering::Acquire)
        .ok()?;
    Some(PageFaultFailure {
        stage: FIRST_PAGE_FAULT_FAILURE_STAGE.load(Ordering::Relaxed),
        errno: FIRST_PAGE_FAULT_FAILURE_ERRNO.load(Ordering::Relaxed),
        vaddr: FIRST_PAGE_FAULT_FAILURE_VADDR.load(Ordering::Relaxed),
    })
}

#[inline]
pub(crate) fn note_first_exec_failure_boundary(
    errno: usize,
    pid: usize,
    path_ptr: usize,
    argv_ptr: usize,
    envp_ptr: usize,
    stage: usize,
    vector_base: usize,
    index: usize,
    entry_addr: usize,
    value_ptr: usize,
    parent_pid: usize,
    clone_flags: usize,
    vma_start: usize,
    vma_end: usize,
    vma_flags: usize,
    backing: usize,
    resident: usize,
    page_state: usize,
    pte_pa: usize,
) {
    if FIRST_EXEC_FAILURE_STATE
        .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return;
    }
    FIRST_EXEC_FAILURE_ERRNO.store(errno, Ordering::Relaxed);
    FIRST_EXEC_FAILURE_PID.store(pid, Ordering::Relaxed);
    FIRST_EXEC_FAILURE_PATH_PTR.store(path_ptr, Ordering::Relaxed);
    FIRST_EXEC_FAILURE_ARGV_PTR.store(argv_ptr, Ordering::Relaxed);
    FIRST_EXEC_FAILURE_ENVP_PTR.store(envp_ptr, Ordering::Relaxed);
    FIRST_EXEC_FAILURE_STAGE.store(stage, Ordering::Relaxed);
    FIRST_EXEC_FAILURE_VECTOR_BASE.store(vector_base, Ordering::Relaxed);
    FIRST_EXEC_FAILURE_INDEX.store(index, Ordering::Relaxed);
    FIRST_EXEC_FAILURE_ENTRY_ADDR.store(entry_addr, Ordering::Relaxed);
    FIRST_EXEC_FAILURE_VALUE_PTR.store(value_ptr, Ordering::Relaxed);
    FIRST_EXEC_FAILURE_PARENT_PID.store(parent_pid, Ordering::Relaxed);
    FIRST_EXEC_FAILURE_CLONE_FLAGS.store(clone_flags, Ordering::Relaxed);
    FIRST_EXEC_FAILURE_VMA_START.store(vma_start, Ordering::Relaxed);
    FIRST_EXEC_FAILURE_VMA_END.store(vma_end, Ordering::Relaxed);
    FIRST_EXEC_FAILURE_VMA_FLAGS.store(vma_flags, Ordering::Relaxed);
    FIRST_EXEC_FAILURE_BACKING.store(backing, Ordering::Relaxed);
    FIRST_EXEC_FAILURE_RESIDENT.store(resident, Ordering::Relaxed);
    FIRST_EXEC_FAILURE_PAGE_STATE.store(page_state, Ordering::Relaxed);
    FIRST_EXEC_FAILURE_PTE_PA.store(pte_pa, Ordering::Relaxed);
    FIRST_EXEC_FAILURE_STATE.store(2, Ordering::Release);

    // Emit the failure boundary immediately.  A foreground script that hangs
    // after an exec error may never return to the harness to drain the saved
    // one-shot record, so the serial log must contain a self-contained marker.
    let sequence = EXEC_FAILURE_MARKERS.fetch_add(1, Ordering::Relaxed) + 1;
    if lifecycle_sample(sequence) {
        crate::println!(
            "BUILDSTORM_DIAG exec_failure seq={} errno={} pid={} stage={} vector_base={:#x} index={} entry_addr={:#x} value_ptr={:#x}",
            sequence,
            errno,
            pid,
            stage,
            vector_base,
            index,
            entry_addr,
            value_ptr,
        );
    }
}

#[inline]
pub(crate) fn note_first_exec_failure(
    errno: usize,
    pid: usize,
    path_ptr: usize,
    argv_ptr: usize,
    envp_ptr: usize,
) {
    note_first_exec_failure_boundary(
        errno,
        pid,
        path_ptr,
        argv_ptr,
        envp_ptr,
        EXEC_FAILURE_STAGE_FALLBACK,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
    );
}

pub(crate) fn take_first_exec_failure() -> Option<ExecFailure> {
    FIRST_EXEC_FAILURE_STATE
        .compare_exchange(2, 3, Ordering::AcqRel, Ordering::Acquire)
        .ok()?;
    Some(ExecFailure {
        errno: FIRST_EXEC_FAILURE_ERRNO.load(Ordering::Relaxed),
        pid: FIRST_EXEC_FAILURE_PID.load(Ordering::Relaxed),
        path_ptr: FIRST_EXEC_FAILURE_PATH_PTR.load(Ordering::Relaxed),
        argv_ptr: FIRST_EXEC_FAILURE_ARGV_PTR.load(Ordering::Relaxed),
        envp_ptr: FIRST_EXEC_FAILURE_ENVP_PTR.load(Ordering::Relaxed),
        stage: FIRST_EXEC_FAILURE_STAGE.load(Ordering::Relaxed),
        vector_base: FIRST_EXEC_FAILURE_VECTOR_BASE.load(Ordering::Relaxed),
        index: FIRST_EXEC_FAILURE_INDEX.load(Ordering::Relaxed),
        entry_addr: FIRST_EXEC_FAILURE_ENTRY_ADDR.load(Ordering::Relaxed),
        value_ptr: FIRST_EXEC_FAILURE_VALUE_PTR.load(Ordering::Relaxed),
        parent_pid: FIRST_EXEC_FAILURE_PARENT_PID.load(Ordering::Relaxed),
        clone_flags: FIRST_EXEC_FAILURE_CLONE_FLAGS.load(Ordering::Relaxed),
        vma_start: FIRST_EXEC_FAILURE_VMA_START.load(Ordering::Relaxed),
        vma_end: FIRST_EXEC_FAILURE_VMA_END.load(Ordering::Relaxed),
        vma_flags: FIRST_EXEC_FAILURE_VMA_FLAGS.load(Ordering::Relaxed),
        backing: FIRST_EXEC_FAILURE_BACKING.load(Ordering::Relaxed),
        resident: FIRST_EXEC_FAILURE_RESIDENT.load(Ordering::Relaxed),
        page_state: FIRST_EXEC_FAILURE_PAGE_STATE.load(Ordering::Relaxed),
        pte_pa: FIRST_EXEC_FAILURE_PTE_PA.load(Ordering::Relaxed),
    })
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn note_first_terminal_user_trap(
    kind: usize,
    pid: usize,
    vaddr: usize,
    sepc: usize,
    sp: usize,
    ra: usize,
    tp: usize,
    fault_pa: usize,
    sepc_pa: usize,
    vma_start: usize,
    vma_end: usize,
    vma_flags: usize,
    backing: usize,
    resident: usize,
    page_state: usize,
) {
    if FIRST_TERMINAL_USER_TRAP_STATE
        .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return;
    }
    FIRST_TERMINAL_USER_TRAP_KIND.store(kind, Ordering::Relaxed);
    FIRST_TERMINAL_USER_TRAP_PID.store(pid, Ordering::Relaxed);
    FIRST_TERMINAL_USER_TRAP_VADDR.store(vaddr, Ordering::Relaxed);
    FIRST_TERMINAL_USER_TRAP_SEPC.store(sepc, Ordering::Relaxed);
    FIRST_TERMINAL_USER_TRAP_SP.store(sp, Ordering::Relaxed);
    FIRST_TERMINAL_USER_TRAP_RA.store(ra, Ordering::Relaxed);
    FIRST_TERMINAL_USER_TRAP_TP.store(tp, Ordering::Relaxed);
    FIRST_TERMINAL_USER_TRAP_FAULT_PA.store(fault_pa, Ordering::Relaxed);
    FIRST_TERMINAL_USER_TRAP_SEPC_PA.store(sepc_pa, Ordering::Relaxed);
    FIRST_TERMINAL_USER_TRAP_VMA_START.store(vma_start, Ordering::Relaxed);
    FIRST_TERMINAL_USER_TRAP_VMA_END.store(vma_end, Ordering::Relaxed);
    FIRST_TERMINAL_USER_TRAP_VMA_FLAGS.store(vma_flags, Ordering::Relaxed);
    FIRST_TERMINAL_USER_TRAP_BACKING.store(backing, Ordering::Relaxed);
    FIRST_TERMINAL_USER_TRAP_RESIDENT.store(resident, Ordering::Relaxed);
    FIRST_TERMINAL_USER_TRAP_PAGE_STATE.store(page_state, Ordering::Relaxed);
    FIRST_TERMINAL_USER_TRAP_STATE.store(2, Ordering::Release);
}

/// Return the first terminal user trap exactly once for a harness record.
pub(crate) fn take_first_terminal_user_trap() -> Option<TerminalUserTrap> {
    FIRST_TERMINAL_USER_TRAP_STATE
        .compare_exchange(2, 3, Ordering::AcqRel, Ordering::Acquire)
        .ok()?;
    Some(TerminalUserTrap {
        kind: FIRST_TERMINAL_USER_TRAP_KIND.load(Ordering::Relaxed),
        pid: FIRST_TERMINAL_USER_TRAP_PID.load(Ordering::Relaxed),
        vaddr: FIRST_TERMINAL_USER_TRAP_VADDR.load(Ordering::Relaxed),
        sepc: FIRST_TERMINAL_USER_TRAP_SEPC.load(Ordering::Relaxed),
        sp: FIRST_TERMINAL_USER_TRAP_SP.load(Ordering::Relaxed),
        ra: FIRST_TERMINAL_USER_TRAP_RA.load(Ordering::Relaxed),
        tp: FIRST_TERMINAL_USER_TRAP_TP.load(Ordering::Relaxed),
        fault_pa: FIRST_TERMINAL_USER_TRAP_FAULT_PA.load(Ordering::Relaxed),
        sepc_pa: FIRST_TERMINAL_USER_TRAP_SEPC_PA.load(Ordering::Relaxed),
        vma_start: FIRST_TERMINAL_USER_TRAP_VMA_START.load(Ordering::Relaxed),
        vma_end: FIRST_TERMINAL_USER_TRAP_VMA_END.load(Ordering::Relaxed),
        vma_flags: FIRST_TERMINAL_USER_TRAP_VMA_FLAGS.load(Ordering::Relaxed),
        backing: FIRST_TERMINAL_USER_TRAP_BACKING.load(Ordering::Relaxed),
        resident: FIRST_TERMINAL_USER_TRAP_RESIDENT.load(Ordering::Relaxed),
        page_state: FIRST_TERMINAL_USER_TRAP_PAGE_STATE.load(Ordering::Relaxed),
    })
}

static USER_TICKS: [AtomicUsize; CPU_SLOTS] = [const { AtomicUsize::new(0) }; CPU_SLOTS];
static KERNEL_TICKS: [AtomicUsize; CPU_SLOTS] = [const { AtomicUsize::new(0) }; CPU_SLOTS];
static IDLE_TICKS: [AtomicUsize; CPU_SLOTS] = [const { AtomicUsize::new(0) }; CPU_SLOTS];
// Time spent inside one user entry/return cycle.  On RISC-V this includes the
// resumed user execution and the trap callback that makes `run_user_task()`
// return.  It deliberately has no task-name or syscall-number dimension.
static USER_RUN_COUNT: [AtomicUsize; CPU_SLOTS] = [const { AtomicUsize::new(0) }; CPU_SLOTS];
static USER_RUN_TOTAL_US: [AtomicUsize; CPU_SLOTS] = [const { AtomicUsize::new(0) }; CPU_SLOTS];
static USER_RUN_MAX_US: [AtomicUsize; CPU_SLOTS] = [const { AtomicUsize::new(0) }; CPU_SLOTS];
static USER_RUN_SYSCALL: [AtomicUsize; CPU_SLOTS] = [const { AtomicUsize::new(0) }; CPU_SLOTS];
static USER_RUN_TIMER: [AtomicUsize; CPU_SLOTS] = [const { AtomicUsize::new(0) }; CPU_SLOTS];
static USER_RUN_IRQ: [AtomicUsize; CPU_SLOTS] = [const { AtomicUsize::new(0) }; CPU_SLOTS];
static USER_RUN_OTHER: [AtomicUsize; CPU_SLOTS] = [const { AtomicUsize::new(0) }; CPU_SLOTS];
static SYSCALL_COUNTS_PER_CPU: [[AtomicUsize; SYSCALL_SLOTS]; CPU_SLOTS] =
    [const { [const { AtomicUsize::new(0) }; SYSCALL_SLOTS] }; CPU_SLOTS];
static SYSCALL_SAMPLES: [AtomicUsize; SYSCALL_SLOTS] =
    [const { AtomicUsize::new(0) }; SYSCALL_SLOTS];
static SYSCALL_TOTAL_US: [AtomicUsize; SYSCALL_SLOTS] =
    [const { AtomicUsize::new(0) }; SYSCALL_SLOTS];
static SYSCALL_MAX_US: [AtomicUsize; SYSCALL_SLOTS] =
    [const { AtomicUsize::new(0) }; SYSCALL_SLOTS];
static USER_TRAP_COUNTS_PER_CPU: [[AtomicUsize; USER_TRAP_SLOTS]; CPU_SLOTS] =
    [const { [const { AtomicUsize::new(0) }; USER_TRAP_SLOTS] }; CPU_SLOTS];
static USER_TRAP_SAMPLES: [AtomicUsize; USER_TRAP_SLOTS] =
    [const { AtomicUsize::new(0) }; USER_TRAP_SLOTS];
static USER_TRAP_TOTAL_US: [AtomicUsize; USER_TRAP_SLOTS] =
    [const { AtomicUsize::new(0) }; USER_TRAP_SLOTS];
static USER_TRAP_MAX_US: [AtomicUsize; USER_TRAP_SLOTS] =
    [const { AtomicUsize::new(0) }; USER_TRAP_SLOTS];
static CONTEXT_SWITCHES: [AtomicUsize; CPU_SLOTS] = [const { AtomicUsize::new(0) }; CPU_SLOTS];
static TASK_MIGRATIONS: [AtomicUsize; CPU_SLOTS] = [const { AtomicUsize::new(0) }; CPU_SLOTS];
static RUNQUEUE_MAX: [AtomicUsize; CPU_SLOTS] = [const { AtomicUsize::new(0) }; CPU_SLOTS];
// A user syscall that blocks keeps its original CPU synchronously executing
// the wait continuation.  These counters distinguish that ownership model
// from ordinary globally queued work without changing the scheduler itself.
static BLOCKED_OWNER_CURRENT: [AtomicUsize; CPU_SLOTS] = [const { AtomicUsize::new(0) }; CPU_SLOTS];
static BLOCKED_OWNER_MAX: [AtomicUsize; CPU_SLOTS] = [const { AtomicUsize::new(0) }; CPU_SLOTS];
static BLOCKED_OWNER_ENTERS: [AtomicUsize; CPU_SLOTS] = [const { AtomicUsize::new(0) }; CPU_SLOTS];
static BLOCKED_OWNER_EXITS: [AtomicUsize; CPU_SLOTS] = [const { AtomicUsize::new(0) }; CPU_SLOTS];
static BLOCKED_OWNER_LOOP_DISPATCHES: [AtomicUsize; CPU_SLOTS] =
    [const { AtomicUsize::new(0) }; CPU_SLOTS];
static BLOCKED_OWNER_WAKES: [AtomicUsize; CPU_SLOTS] = [const { AtomicUsize::new(0) }; CPU_SLOTS];
static BLOCKED_GLOBAL_WAKES: AtomicUsize = AtomicUsize::new(0);
static BLOCKED_OWNER_TIMED_LOOPS: [AtomicUsize; CPU_SLOTS] =
    [const { AtomicUsize::new(0) }; CPU_SLOTS];
static BLOCKED_OWNER_TIMED_TOTAL_US: [AtomicUsize; CPU_SLOTS] =
    [const { AtomicUsize::new(0) }; CPU_SLOTS];
static BLOCKED_OWNER_TIMED_MAX_US: [AtomicUsize; CPU_SLOTS] =
    [const { AtomicUsize::new(0) }; CPU_SLOTS];
static BLOCKED_OWNER_EMPTY_ITERATIONS: [AtomicUsize; CPU_SLOTS] =
    [const { AtomicUsize::new(0) }; CPU_SLOTS];

static WORK_COUNTS_PER_CPU: [[PerCpuCounter; CPU_SLOTS]; WORK_SLOTS] =
    [const { [const { PerCpuCounter::new() }; CPU_SLOTS] }; WORK_SLOTS];
static WORK_SAMPLES: [AtomicUsize; WORK_SLOTS] = [const { AtomicUsize::new(0) }; WORK_SLOTS];
static WORK_TOTAL_US: [AtomicUsize; WORK_SLOTS] = [const { AtomicUsize::new(0) }; WORK_SLOTS];
static WORK_MAX_US: [AtomicUsize; WORK_SLOTS] = [const { AtomicUsize::new(0) }; WORK_SLOTS];
static PHASE_COUNTS: [AtomicUsize; PHASE_SLOTS] = [const { AtomicUsize::new(0) }; PHASE_SLOTS];
static PHASE_TOTAL_US: [AtomicUsize; PHASE_SLOTS] = [const { AtomicUsize::new(0) }; PHASE_SLOTS];
static PHASE_MAX_US: [AtomicUsize; PHASE_SLOTS] = [const { AtomicUsize::new(0) }; PHASE_SLOTS];
static PAGE_FAULT_SOURCE_COUNTS: [AtomicUsize; PAGE_FAULT_SOURCE_SLOTS] =
    [const { AtomicUsize::new(0) }; PAGE_FAULT_SOURCE_SLOTS];
static PAGE_FAULT_SOURCE_TOTAL_US: [AtomicUsize; PAGE_FAULT_SOURCE_SLOTS] =
    [const { AtomicUsize::new(0) }; PAGE_FAULT_SOURCE_SLOTS];
static PAGE_FAULT_SOURCE_MAX_US: [AtomicUsize; PAGE_FAULT_SOURCE_SLOTS] =
    [const { AtomicUsize::new(0) }; PAGE_FAULT_SOURCE_SLOTS];
static PAGE_FAULT_RESOLUTION_COUNTS: [AtomicUsize; PAGE_FAULT_RESOLUTION_SLOTS] =
    [const { AtomicUsize::new(0) }; PAGE_FAULT_RESOLUTION_SLOTS];
static PAGE_FAULT_RESOLUTION_TOTAL_US: [AtomicUsize; PAGE_FAULT_RESOLUTION_SLOTS] =
    [const { AtomicUsize::new(0) }; PAGE_FAULT_RESOLUTION_SLOTS];
static PAGE_FAULT_RESOLUTION_MAX_US: [AtomicUsize; PAGE_FAULT_RESOLUTION_SLOTS] =
    [const { AtomicUsize::new(0) }; PAGE_FAULT_RESOLUTION_SLOTS];
static PAGE_FAULT_RESOLUTION_PAGES: [AtomicUsize; PAGE_FAULT_RESOLUTION_SLOTS] =
    [const { AtomicUsize::new(0) }; PAGE_FAULT_RESOLUTION_SLOTS];

// Shape of anonymous demand-fault installations before `split_area_at()`
// mutates the VMA vector. These counters test whether a populated window can
// usually extend an adjacent fully-resident anonymous VMA in place.
static ANONYMOUS_VMA_INSTALLS: AtomicUsize = AtomicUsize::new(0);
static ANONYMOUS_VMA_LEFT_MERGEABLE: AtomicUsize = AtomicUsize::new(0);
static ANONYMOUS_VMA_RIGHT_MERGEABLE: AtomicUsize = AtomicUsize::new(0);
static ANONYMOUS_VMA_BOTH_MERGEABLE: AtomicUsize = AtomicUsize::new(0);
static ANONYMOUS_VMA_NEITHER_MERGEABLE: AtomicUsize = AtomicUsize::new(0);
static ANONYMOUS_VMA_CURRENT: AtomicUsize = AtomicUsize::new(0);
static ANONYMOUS_VMA_MAX: AtomicUsize = AtomicUsize::new(0);
static ANONYMOUS_VMA_PAGES: AtomicUsize = AtomicUsize::new(0);
static ANONYMOUS_VMA_PAGES_MAX: AtomicUsize = AtomicUsize::new(0);
static ANONYMOUS_FAULT_SHAPE_CALLS: AtomicUsize = AtomicUsize::new(0);
static ANONYMOUS_FAULT_SHAPE_SAMPLES: AtomicUsize = AtomicUsize::new(0);
static ANONYMOUS_FAULT_LEFT_RESIDENT: AtomicUsize = AtomicUsize::new(0);
static ANONYMOUS_FAULT_RIGHT_RESIDENT: AtomicUsize = AtomicUsize::new(0);
static ANONYMOUS_FAULT_BOTH_RESIDENT: AtomicUsize = AtomicUsize::new(0);
static ANONYMOUS_FAULT_ISOLATED: AtomicUsize = AtomicUsize::new(0);
static ANONYMOUS_FAULT_FORWARD_PAGES: AtomicUsize = AtomicUsize::new(0);
static ANONYMOUS_FAULT_BACKWARD_PAGES: AtomicUsize = AtomicUsize::new(0);
static ANONYMOUS_FAULT_FORWARD_MAX: AtomicUsize = AtomicUsize::new(0);
static ANONYMOUS_FAULT_BACKWARD_MAX: AtomicUsize = AtomicUsize::new(0);
static ANONYMOUS_MOVE_LEFT_SELECTED: AtomicUsize = AtomicUsize::new(0);
static ANONYMOUS_MOVE_LEFT_NO_REALLOC: AtomicUsize = AtomicUsize::new(0);
static ANONYMOUS_MOVE_LEFT_SHRINK: AtomicUsize = AtomicUsize::new(0);
static ANONYMOUS_MOVE_LEFT_REMOVE: AtomicUsize = AtomicUsize::new(0);
static ANONYMOUS_MOVE_LEFT_FRAME_RELOCATE: AtomicUsize = AtomicUsize::new(0);
static ANONYMOUS_MOVE_LEFT_REMOVE_SUFFIX: AtomicUsize = AtomicUsize::new(0);
static ANONYMOUS_MOVE_LEFT_REMOVE_SUFFIX_MAX: AtomicUsize = AtomicUsize::new(0);
static ANONYMOUS_MOVE_RIGHT_SELECTED: AtomicUsize = AtomicUsize::new(0);
static ANONYMOUS_MOVE_RIGHT_NO_REALLOC: AtomicUsize = AtomicUsize::new(0);
static ANONYMOUS_MOVE_RIGHT_SHRINK: AtomicUsize = AtomicUsize::new(0);
static ANONYMOUS_MOVE_RIGHT_REMOVE: AtomicUsize = AtomicUsize::new(0);
static ANONYMOUS_MOVE_RIGHT_FRAME_MOVE: AtomicUsize = AtomicUsize::new(0);
static ANONYMOUS_MOVE_RIGHT_REMOVE_SUFFIX: AtomicUsize = AtomicUsize::new(0);
static ANONYMOUS_MOVE_RIGHT_REMOVE_SUFFIX_MAX: AtomicUsize = AtomicUsize::new(0);
static ANONYMOUS_MOVE_ZERO_METADATA: AtomicUsize = AtomicUsize::new(0);
static ANONYMOUS_COALESCE_CALLS: AtomicUsize = AtomicUsize::new(0);
static ANONYMOUS_COALESCE_AREAS: AtomicUsize = AtomicUsize::new(0);
static ANONYMOUS_COALESCE_AREAS_MAX: AtomicUsize = AtomicUsize::new(0);
static ANONYMOUS_COALESCE_SORT_US: AtomicUsize = AtomicUsize::new(0);
static ANONYMOUS_COALESCE_SCAN_US: AtomicUsize = AtomicUsize::new(0);
static ANONYMOUS_COALESCE_TOTAL_US: AtomicUsize = AtomicUsize::new(0);
static ANONYMOUS_COALESCE_MERGES: AtomicUsize = AtomicUsize::new(0);
static ANONYMOUS_COALESCE_MERGED_FRAMES: AtomicUsize = AtomicUsize::new(0);
static ANONYMOUS_COALESCE_FRAME_REALLOCS: AtomicUsize = AtomicUsize::new(0);
static ANONYMOUS_COALESCE_FRAME_RELOCATE: AtomicUsize = AtomicUsize::new(0);

static BLOCK_COUNTS: [AtomicUsize; BLOCK_SLOTS] = [const { AtomicUsize::new(0) }; BLOCK_SLOTS];
static BLOCK_TOTAL_US: [AtomicUsize; BLOCK_SLOTS] = [const { AtomicUsize::new(0) }; BLOCK_SLOTS];
static BLOCK_MAX_US: [AtomicUsize; BLOCK_SLOTS] = [const { AtomicUsize::new(0) }; BLOCK_SLOTS];
static BLOCK_WOKEN: [AtomicUsize; BLOCK_SLOTS] = [const { AtomicUsize::new(0) }; BLOCK_SLOTS];
static BLOCK_TIMED_OUT: [AtomicUsize; BLOCK_SLOTS] = [const { AtomicUsize::new(0) }; BLOCK_SLOTS];
static BLOCK_INTERRUPTED: [AtomicUsize; BLOCK_SLOTS] = [const { AtomicUsize::new(0) }; BLOCK_SLOTS];
// Time from a waker publishing Ready to the task next being dispatched.  This
// keeps event delivery separate from scheduler handoff delay.
static WAKE_TO_RUN_COUNTS: [AtomicUsize; BLOCK_SLOTS] =
    [const { AtomicUsize::new(0) }; BLOCK_SLOTS];
static WAKE_TO_RUN_TOTAL_US: [AtomicUsize; BLOCK_SLOTS] =
    [const { AtomicUsize::new(0) }; BLOCK_SLOTS];
static WAKE_TO_RUN_MAX_US: [AtomicUsize; BLOCK_SLOTS] =
    [const { AtomicUsize::new(0) }; BLOCK_SLOTS];
static BLOCK_ACTOR_COUNTS: [AtomicUsize; BLOCK_ACTOR_SLOTS] =
    [const { AtomicUsize::new(0) }; BLOCK_ACTOR_SLOTS];
static BLOCK_ACTOR_TOTAL_US: [AtomicUsize; BLOCK_ACTOR_SLOTS] =
    [const { AtomicUsize::new(0) }; BLOCK_ACTOR_SLOTS];

static PPOLL_CALLS: AtomicUsize = AtomicUsize::new(0);
static PPOLL_KEYED_SLEEPS: AtomicUsize = AtomicUsize::new(0);
static PPOLL_UNKEYED_SLEEPS: AtomicUsize = AtomicUsize::new(0);

// Cargo jobserver / pipe descriptor lifecycle.  These are deliberately
// aggregate counters: no fd numbers, command lines, or per-event records are
// retained.  The alias mismatch counter detects a general Linux OFD semantic
// violation where duplicated pipe descriptors disagree on O_NONBLOCK.
static PIPE2_CALLS: AtomicUsize = AtomicUsize::new(0);
static PIPE2_NONBLOCK: AtomicUsize = AtomicUsize::new(0);
static PIPE2_CLOEXEC: AtomicUsize = AtomicUsize::new(0);
static PIPE_READ_CALLS: AtomicUsize = AtomicUsize::new(0);
static PIPE_READ_BYTES: AtomicUsize = AtomicUsize::new(0);
static PIPE_READ_EAGAIN: AtomicUsize = AtomicUsize::new(0);
static PIPE_READ_EOF: AtomicUsize = AtomicUsize::new(0);
static PIPE_WRITE_CALLS: AtomicUsize = AtomicUsize::new(0);
static PIPE_WRITE_BYTES: AtomicUsize = AtomicUsize::new(0);
static PIPE_WRITE_EAGAIN: AtomicUsize = AtomicUsize::new(0);
static PIPE_WRITE_EPIPE: AtomicUsize = AtomicUsize::new(0);
static PIPE_READ_WAITS: AtomicUsize = AtomicUsize::new(0);
static PIPE_WRITE_WAITS: AtomicUsize = AtomicUsize::new(0);
static PIPE_READER_WAKE_CALLS: AtomicUsize = AtomicUsize::new(0);
static PIPE_READER_WOKEN: AtomicUsize = AtomicUsize::new(0);
static PIPE_WRITER_WAKE_CALLS: AtomicUsize = AtomicUsize::new(0);
static PIPE_WRITER_WOKEN: AtomicUsize = AtomicUsize::new(0);
static DUP_CALLS: AtomicUsize = AtomicUsize::new(0);
static DUP_PIPE_CALLS: AtomicUsize = AtomicUsize::new(0);
static FCNTL_GETFD: AtomicUsize = AtomicUsize::new(0);
static FCNTL_SETFD: AtomicUsize = AtomicUsize::new(0);
static FCNTL_GETFL: AtomicUsize = AtomicUsize::new(0);
static FCNTL_SETFL: AtomicUsize = AtomicUsize::new(0);
static FCNTL_PIPE_GETFL: AtomicUsize = AtomicUsize::new(0);
static FCNTL_PIPE_SETFL: AtomicUsize = AtomicUsize::new(0);
static PIPE_ALIAS_OBSERVATIONS: AtomicUsize = AtomicUsize::new(0);
static PIPE_ALIAS_MISMATCHES: AtomicUsize = AtomicUsize::new(0);
static PIPE_EXTERNAL_ALIASES_AT_SETFL: AtomicUsize = AtomicUsize::new(0);
static EXEC_CALLS: AtomicUsize = AtomicUsize::new(0);
static EXEC_PIPE_ENDPOINTS: AtomicUsize = AtomicUsize::new(0);
static EXEC_PIPE_CLOEXEC: AtomicUsize = AtomicUsize::new(0);
static EXEC_PIPE_CLOSED: AtomicUsize = AtomicUsize::new(0);
static SCHED_GETAFFINITY_CALLS: AtomicUsize = AtomicUsize::new(0);
static SCHED_GETAFFINITY_MASK_BITS: AtomicUsize = AtomicUsize::new(0);
static SCHED_GETAFFINITY_MAX_BITS: AtomicUsize = AtomicUsize::new(0);

static USER_ROOT_ACTIVATIONS: AtomicUsize = AtomicUsize::new(0);
static KERNEL_ROOT_ACTIVATIONS: AtomicUsize = AtomicUsize::new(0);
static REDUNDANT_ROOT_ACTIVATIONS: AtomicUsize = AtomicUsize::new(0);
static PAGE_TABLE_WRITES: AtomicUsize = AtomicUsize::new(0);
static LOCAL_TLB_FLUSHES: AtomicUsize = AtomicUsize::new(0);
static REMOTE_SHOOTDOWNS: AtomicUsize = AtomicUsize::new(0);
static REMOTE_SHOOTDOWN_TARGETS: AtomicUsize = AtomicUsize::new(0);
static PAGE_TABLE_BATCHES: AtomicUsize = AtomicUsize::new(0);
static PAGE_TABLE_BATCH_WRITES: AtomicUsize = AtomicUsize::new(0);
static PAGE_TABLE_BATCH_MAX: AtomicUsize = AtomicUsize::new(0);
static PAGE_TABLE_BATCH_LOCAL_FLUSHES: AtomicUsize = AtomicUsize::new(0);

static PATH_COMPONENT_LOOKUPS: AtomicUsize = AtomicUsize::new(0);
static PATH_CACHE_HITS: AtomicUsize = AtomicUsize::new(0);
static PATH_CACHE_MISSES: AtomicUsize = AtomicUsize::new(0);
static NEGATIVE_PATH_CACHE_HITS: AtomicUsize = AtomicUsize::new(0);
static DIR_CACHE_HITS: AtomicUsize = AtomicUsize::new(0);
static DIR_CACHE_MISSES: AtomicUsize = AtomicUsize::new(0);
static INODE_METADATA_CACHE_HITS: AtomicUsize = AtomicUsize::new(0);
static INODE_METADATA_CACHE_MISSES: AtomicUsize = AtomicUsize::new(0);
static INODE_CACHE_HITS: AtomicUsize = AtomicUsize::new(0);
static INODE_CACHE_MISSES: AtomicUsize = AtomicUsize::new(0);
static METADATA_CACHE_HITS: AtomicUsize = AtomicUsize::new(0);
static METADATA_CACHE_MISSES: AtomicUsize = AtomicUsize::new(0);
static BLOCK_CACHE_HITS: AtomicUsize = AtomicUsize::new(0);
static BLOCK_CACHE_MISSES: AtomicUsize = AtomicUsize::new(0);
static VIRTIO_REQUESTS: AtomicUsize = AtomicUsize::new(0);
static VIRTIO_BYTES: AtomicUsize = AtomicUsize::new(0);
static VIRTIO_QUEUE_US: AtomicUsize = AtomicUsize::new(0);
static VIRTIO_COMPLETE_US: AtomicUsize = AtomicUsize::new(0);
static LOGICAL_VMA_CURRENT: AtomicUsize = AtomicUsize::new(0);
static LOGICAL_VMA_MAX: AtomicUsize = AtomicUsize::new(0);

static LOCK_ACQUIRES: [AtomicUsize; LOCK_SLOTS] = [const { AtomicUsize::new(0) }; LOCK_SLOTS];
static LOCK_CONTENDED: [AtomicUsize; LOCK_SLOTS] = [const { AtomicUsize::new(0) }; LOCK_SLOTS];
static LOCK_WAIT_TOTAL_US: [AtomicUsize; LOCK_SLOTS] = [const { AtomicUsize::new(0) }; LOCK_SLOTS];
static LOCK_WAIT_MAX_US: [AtomicUsize; LOCK_SLOTS] = [const { AtomicUsize::new(0) }; LOCK_SLOTS];
static LOCK_HOLD_TOTAL_US: [AtomicUsize; LOCK_SLOTS] = [const { AtomicUsize::new(0) }; LOCK_SLOTS];
static LOCK_HOLD_MAX_US: [AtomicUsize; LOCK_SLOTS] = [const { AtomicUsize::new(0) }; LOCK_SLOTS];
static HOT_LOCK_ACQUIRES: [[PerCpuCounter; CPU_SLOTS]; HOT_LOCK_SLOTS] =
    [const { [const { PerCpuCounter::new() }; CPU_SLOTS] }; HOT_LOCK_SLOTS];
static HOT_LOCK_SAMPLES: [AtomicUsize; HOT_LOCK_SLOTS] =
    [const { AtomicUsize::new(0) }; HOT_LOCK_SLOTS];
static MEMORY_SET_LOCK_SITE_CONTENDED: [AtomicUsize; MEMORY_SET_LOCK_SITE_SLOTS] =
    [const { AtomicUsize::new(0) }; MEMORY_SET_LOCK_SITE_SLOTS];
static MEMORY_SET_LOCK_SITE_WAIT_TOTAL_US: [AtomicUsize; MEMORY_SET_LOCK_SITE_SLOTS] =
    [const { AtomicUsize::new(0) }; MEMORY_SET_LOCK_SITE_SLOTS];
static MEMORY_SET_LOCK_SITE_WAIT_MAX_US: [AtomicUsize; MEMORY_SET_LOCK_SITE_SLOTS] =
    [const { AtomicUsize::new(0) }; MEMORY_SET_LOCK_SITE_SLOTS];
static MEMORY_SET_LOCK_SITE_HOLD_TOTAL_US: [AtomicUsize; MEMORY_SET_LOCK_SITE_SLOTS] =
    [const { AtomicUsize::new(0) }; MEMORY_SET_LOCK_SITE_SLOTS];
static MEMORY_SET_LOCK_SITE_HOLD_MAX_US: [AtomicUsize; MEMORY_SET_LOCK_SITE_SLOTS] =
    [const { AtomicUsize::new(0) }; MEMORY_SET_LOCK_SITE_SLOTS];
static MEMORY_SET_SITE_ACQUIRES_PER_CPU: [[PerCpuCounter; CPU_SLOTS]; MEMORY_SET_LOCK_SITE_SLOTS] =
    [const { [const { PerCpuCounter::new() }; CPU_SLOTS] }; MEMORY_SET_LOCK_SITE_SLOTS];
static MEMORY_SET_OWNER_MUTEX: [PerCpuCounter; CPU_SLOTS] =
    [const { PerCpuCounter::new() }; CPU_SLOTS];
static MEMORY_SET_OWNER_SITE: [PerCpuCounter; CPU_SLOTS] =
    [const { PerCpuCounter::new() }; CPU_SLOTS];
static MEMORY_SET_OWNER_WAITER_SAMPLES: [[AtomicUsize; MEMORY_SET_OWNER_SLOTS];
    MEMORY_SET_LOCK_SITE_SLOTS] =
    [const { [const { AtomicUsize::new(0) }; MEMORY_SET_OWNER_SLOTS] }; MEMORY_SET_LOCK_SITE_SLOTS];
static HEAP_ALLOC_SAMPLES: [[AtomicUsize; CPU_SLOTS]; HEAP_SIZE_BUCKETS] =
    [const { [const { AtomicUsize::new(0) }; CPU_SLOTS] }; HEAP_SIZE_BUCKETS];
static HEAP_FREE_SAMPLES: [[AtomicUsize; CPU_SLOTS]; HEAP_SIZE_BUCKETS] =
    [const { [const { AtomicUsize::new(0) }; CPU_SLOTS] }; HEAP_SIZE_BUCKETS];
static HEAP_ALLOC_SAMPLE_BYTES: [[AtomicUsize; CPU_SLOTS]; HEAP_SIZE_BUCKETS] =
    [const { [const { AtomicUsize::new(0) }; CPU_SLOTS] }; HEAP_SIZE_BUCKETS];
static HEAP_FREE_SAMPLE_BYTES: [[AtomicUsize; CPU_SLOTS]; HEAP_SIZE_BUCKETS] =
    [const { [const { AtomicUsize::new(0) }; CPU_SLOTS] }; HEAP_SIZE_BUCKETS];
static ACTIVE_USER_PID: [AtomicUsize; CPU_SLOTS] = [const { AtomicUsize::new(0) }; CPU_SLOTS];
static ACTIVE_USER_SINCE_US: [AtomicUsize; CPU_SLOTS] = [const { AtomicUsize::new(0) }; CPU_SLOTS];
static ACTIVE_USER_LAST_EXIT_US: [AtomicUsize; CPU_SLOTS] =
    [const { AtomicUsize::new(0) }; CPU_SLOTS];
static ACTIVE_USER_ENTRIES: [AtomicUsize; CPU_SLOTS] = [const { AtomicUsize::new(0) }; CPU_SLOTS];
static ACTIVE_USER_TRAP_KIND: [AtomicUsize; CPU_SLOTS] = [const { AtomicUsize::new(0) }; CPU_SLOTS];
static ACTIVE_USER_TRAP_SINCE_US: [AtomicUsize; CPU_SLOTS] =
    [const { AtomicUsize::new(0) }; CPU_SLOTS];
static ACTIVE_USER_TRAP_ENTRIES: [AtomicUsize; CPU_SLOTS] =
    [const { AtomicUsize::new(0) }; CPU_SLOTS];

// A user trap can remain active while a syscall is blocked or spins inside a
// subsystem.  Keep the last active syscall identity so a late snapshot can
// distinguish a scheduler starvation from a syscall that never returns.
static ACTIVE_SYSCALL_ID: [AtomicUsize; CPU_SLOTS] =
    [const { AtomicUsize::new(usize::MAX) }; CPU_SLOTS];
static ACTIVE_SYSCALL_SEPC: [AtomicUsize; CPU_SLOTS] = [const { AtomicUsize::new(0) }; CPU_SLOTS];
static ACTIVE_SYSCALL_SINCE_US: [AtomicUsize; CPU_SLOTS] =
    [const { AtomicUsize::new(0) }; CPU_SLOTS];
static ACTIVE_SYSCALL_ENTRIES: [AtomicUsize; CPU_SLOTS] =
    [const { AtomicUsize::new(0) }; CPU_SLOTS];
static ACTIVE_SYSCALL_LAST_EXIT_US: [AtomicUsize; CPU_SLOTS] =
    [const { AtomicUsize::new(0) }; CPU_SLOTS];

pub(crate) const EXEC_STAGE_ENTER: usize = 1;
pub(crate) const EXEC_STAGE_PATH_READY: usize = 2;
pub(crate) const EXEC_STAGE_ARGV_READY: usize = 3;
pub(crate) const EXEC_STAGE_ENVP_READY: usize = 4;
pub(crate) const EXEC_STAGE_FS_CONTEXT: usize = 5;
pub(crate) const EXEC_STAGE_TARGET_OPEN: usize = 6;
pub(crate) const EXEC_STAGE_VFS_RESOLVE_SYMLINK: usize = 7;
pub(crate) const EXEC_STAGE_VFS_MOUNT_ROUTE: usize = 8;
pub(crate) const EXEC_STAGE_VFS_MEMFS: usize = 9;
pub(crate) const EXEC_STAGE_VFS_EXT4_ENTER: usize = 10;
pub(crate) const EXEC_STAGE_EXT4_NAMESPACE_LOCK: usize = 11;
pub(crate) const EXEC_STAGE_EXT4_RESOLVE: usize = 12;
pub(crate) const EXEC_STAGE_EXT4_INODE_LOOKUP: usize = 13;
pub(crate) const EXEC_STAGE_EXT4_EXEC_CACHE: usize = 14;
pub(crate) const EXEC_STAGE_EXT4_REGULAR_CACHE: usize = 15;
pub(crate) const EXEC_STAGE_EXT4_EXTENT_READ: usize = 16;
pub(crate) const EXEC_STAGE_TARGET_OPEN_DONE: usize = 17;
pub(crate) const EXEC_STAGE_SHEBANG_OPEN: usize = 18;
pub(crate) const EXEC_STAGE_SHEBANG_OPEN_DONE: usize = 19;
pub(crate) const EXEC_STAGE_TARGET_PARSE: usize = 20;
pub(crate) const EXEC_STAGE_INTERPRETER_OPEN: usize = 21;
pub(crate) const EXEC_STAGE_INTERPRETER_OPEN_DONE: usize = 22;
pub(crate) const EXEC_STAGE_MEMORY_BUILD: usize = 23;
pub(crate) const EXEC_STAGE_STACK_SETUP: usize = 24;
pub(crate) const EXEC_STAGE_SIGNAL_TRAMPOLINE: usize = 25;
pub(crate) const EXEC_STAGE_TERMINATE_PEERS: usize = 26;
pub(crate) const EXEC_STAGE_DETACH_SHM: usize = 27;
pub(crate) const EXEC_STAGE_RESET_SIGNALS: usize = 28;
pub(crate) const EXEC_STAGE_TASK_INNER_LOCK: usize = 29;
pub(crate) const EXEC_STAGE_FD_TABLE_LOCK: usize = 30;
pub(crate) const EXEC_STAGE_CLOSE_FILES: usize = 31;
pub(crate) const EXEC_STAGE_RELEASE_POSIX_LOCKS: usize = 32;
pub(crate) const EXEC_STAGE_MM_LOCK: usize = 33;
pub(crate) const EXEC_STAGE_KERNEL_PT_RESTORE: usize = 34;
pub(crate) const EXEC_STAGE_MEMORY_SET_LOCK: usize = 35;
pub(crate) const EXEC_STAGE_MEMORY_REPLACE: usize = 36;
pub(crate) const EXEC_STAGE_OLD_MEMORY_DROP: usize = 37;
pub(crate) const EXEC_STAGE_TRAP_FRAME_LOCK: usize = 38;
pub(crate) const EXEC_STAGE_COMPLETE: usize = 39;

pub(crate) const ELF_LOAD_ROLE_TARGET: usize = 1;
pub(crate) const ELF_LOAD_ROLE_INTERPRETER: usize = 2;

pub(crate) const ELF_LOAD_PHASE_INTERPRETER_PARSE: usize = 1;
pub(crate) const ELF_LOAD_PHASE_INTERPRETER_BIAS: usize = 2;
pub(crate) const ELF_LOAD_PHASE_ADDRESS_SPACE: usize = 3;
pub(crate) const ELF_LOAD_PHASE_SEGMENT_MAP: usize = 4;
pub(crate) const ELF_LOAD_PHASE_SEGMENT_COPY: usize = 5;
pub(crate) const ELF_LOAD_PHASE_SEGMENT_ZERO: usize = 6;
pub(crate) const ELF_LOAD_PHASE_STACK_MAP: usize = 7;

pub(crate) const ADDRESS_SPACE_CREATE_IDLE: usize = 0;
pub(crate) const ADDRESS_SPACE_CREATE_ROOT: usize = 1;
pub(crate) const ADDRESS_SPACE_CREATE_ASID: usize = 2;

static ACTIVE_EXEC_STAGE: [AtomicUsize; CPU_SLOTS] = [const { AtomicUsize::new(0) }; CPU_SLOTS];
static ACTIVE_EXEC_PID: [AtomicUsize; CPU_SLOTS] = [const { AtomicUsize::new(0) }; CPU_SLOTS];
static ACTIVE_EXEC_PATH_HASH: [AtomicUsize; CPU_SLOTS] = [const { AtomicUsize::new(0) }; CPU_SLOTS];
static ACTIVE_EXEC_PATH_LEN: [AtomicUsize; CPU_SLOTS] = [const { AtomicUsize::new(0) }; CPU_SLOTS];
static ACTIVE_EXEC_STARTED_AT_US: [AtomicUsize; CPU_SLOTS] =
    [const { AtomicUsize::new(0) }; CPU_SLOTS];
static ACTIVE_EXEC_STAGE_SINCE_US: [AtomicUsize; CPU_SLOTS] =
    [const { AtomicUsize::new(0) }; CPU_SLOTS];
static ACTIVE_EXEC_LAST_EXIT_US: [AtomicUsize; CPU_SLOTS] =
    [const { AtomicUsize::new(0) }; CPU_SLOTS];
static ACTIVE_EXEC_ENTRIES: [AtomicUsize; CPU_SLOTS] = [const { AtomicUsize::new(0) }; CPU_SLOTS];
const EXEC_PATH_SLOTS: usize = 64;
static EXEC_PATH_HASHES: [AtomicUsize; EXEC_PATH_SLOTS] =
    [const { AtomicUsize::new(0) }; EXEC_PATH_SLOTS];
static EXEC_PATH_COUNTS: [AtomicUsize; EXEC_PATH_SLOTS] =
    [const { AtomicUsize::new(0) }; EXEC_PATH_SLOTS];
static ACTIVE_ELF_LOAD_ROLE: [AtomicUsize; CPU_SLOTS] = [const { AtomicUsize::new(0) }; CPU_SLOTS];
static ACTIVE_ELF_LOAD_PHASE: [AtomicUsize; CPU_SLOTS] = [const { AtomicUsize::new(0) }; CPU_SLOTS];
static ACTIVE_ELF_LOAD_SEGMENT: [AtomicUsize; CPU_SLOTS] =
    [const { AtomicUsize::new(0) }; CPU_SLOTS];
static ACTIVE_ELF_LOAD_VADDR: [AtomicUsize; CPU_SLOTS] = [const { AtomicUsize::new(0) }; CPU_SLOTS];
static ACTIVE_ELF_LOAD_FILESZ: [AtomicUsize; CPU_SLOTS] =
    [const { AtomicUsize::new(0) }; CPU_SLOTS];
static ACTIVE_ELF_LOAD_MEMSZ: [AtomicUsize; CPU_SLOTS] = [const { AtomicUsize::new(0) }; CPU_SLOTS];
static ACTIVE_ELF_LOAD_BIAS: [AtomicUsize; CPU_SLOTS] = [const { AtomicUsize::new(0) }; CPU_SLOTS];
static ACTIVE_ELF_LOAD_PHASE_SINCE_US: [AtomicUsize; CPU_SLOTS] =
    [const { AtomicUsize::new(0) }; CPU_SLOTS];
static ACTIVE_ADDRESS_SPACE_CREATE_PHASE: [AtomicUsize; CPU_SLOTS] =
    [const { AtomicUsize::new(0) }; CPU_SLOTS];
static ACTIVE_ADDRESS_SPACE_CREATE_SINCE_US: [AtomicUsize; CPU_SLOTS] =
    [const { AtomicUsize::new(0) }; CPU_SLOTS];
static ASID_ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);
static ASID_REUSES: AtomicUsize = AtomicUsize::new(0);
static ASID_GLOBAL_RECYCLES: AtomicUsize = AtomicUsize::new(0);
static ASID_ZERO_FALLBACKS: AtomicUsize = AtomicUsize::new(0);
static ASID_LAST_ALLOCATED: AtomicUsize = AtomicUsize::new(0);
static ASID_MAX: AtomicUsize = AtomicUsize::new(0);
static ASID_NEXT: AtomicUsize = AtomicUsize::new(0);
static ASID_REUSABLE: AtomicUsize = AtomicUsize::new(0);
static ASID_RETIRED: AtomicUsize = AtomicUsize::new(0);

#[inline]
fn now_us() -> usize {
    crate::timer::get_time_us()
}

#[inline]
fn cpu_slot() -> usize {
    crate::platform::current_cpu_index().min(CPU_SLOTS.saturating_sub(1))
}

fn exec_stage_name(stage: usize) -> &'static str {
    match stage {
        EXEC_STAGE_ENTER => "enter",
        EXEC_STAGE_PATH_READY => "path_ready",
        EXEC_STAGE_ARGV_READY => "argv_ready",
        EXEC_STAGE_ENVP_READY => "envp_ready",
        EXEC_STAGE_FS_CONTEXT => "fs_context",
        EXEC_STAGE_TARGET_OPEN => "target_open",
        EXEC_STAGE_VFS_RESOLVE_SYMLINK => "vfs_resolve_symlink",
        EXEC_STAGE_VFS_MOUNT_ROUTE => "vfs_mount_route",
        EXEC_STAGE_VFS_MEMFS => "vfs_memfs",
        EXEC_STAGE_VFS_EXT4_ENTER => "vfs_ext4_enter",
        EXEC_STAGE_EXT4_NAMESPACE_LOCK => "ext4_namespace_lock",
        EXEC_STAGE_EXT4_RESOLVE => "ext4_resolve",
        EXEC_STAGE_EXT4_INODE_LOOKUP => "ext4_inode_lookup",
        EXEC_STAGE_EXT4_EXEC_CACHE => "ext4_exec_cache",
        EXEC_STAGE_EXT4_REGULAR_CACHE => "ext4_regular_cache",
        EXEC_STAGE_EXT4_EXTENT_READ => "ext4_extent_read",
        EXEC_STAGE_TARGET_OPEN_DONE => "target_open_done",
        EXEC_STAGE_SHEBANG_OPEN => "shebang_open",
        EXEC_STAGE_SHEBANG_OPEN_DONE => "shebang_open_done",
        EXEC_STAGE_TARGET_PARSE => "target_parse",
        EXEC_STAGE_INTERPRETER_OPEN => "interpreter_open",
        EXEC_STAGE_INTERPRETER_OPEN_DONE => "interpreter_open_done",
        EXEC_STAGE_MEMORY_BUILD => "memory_build",
        EXEC_STAGE_STACK_SETUP => "stack_setup",
        EXEC_STAGE_SIGNAL_TRAMPOLINE => "signal_trampoline",
        EXEC_STAGE_TERMINATE_PEERS => "terminate_peers",
        EXEC_STAGE_DETACH_SHM => "detach_shm",
        EXEC_STAGE_RESET_SIGNALS => "reset_signals",
        EXEC_STAGE_TASK_INNER_LOCK => "task_inner_lock",
        EXEC_STAGE_FD_TABLE_LOCK => "fd_table_lock",
        EXEC_STAGE_CLOSE_FILES => "close_files",
        EXEC_STAGE_RELEASE_POSIX_LOCKS => "release_posix_locks",
        EXEC_STAGE_MM_LOCK => "mm_lock",
        EXEC_STAGE_KERNEL_PT_RESTORE => "kernel_pt_restore",
        EXEC_STAGE_MEMORY_SET_LOCK => "memory_set_lock",
        EXEC_STAGE_MEMORY_REPLACE => "memory_replace",
        EXEC_STAGE_OLD_MEMORY_DROP => "old_memory_drop",
        EXEC_STAGE_TRAP_FRAME_LOCK => "trap_frame_lock",
        EXEC_STAGE_COMPLETE => "complete",
        _ => "idle",
    }
}

fn elf_load_role_name(role: usize) -> &'static str {
    match role {
        ELF_LOAD_ROLE_TARGET => "target",
        ELF_LOAD_ROLE_INTERPRETER => "interpreter",
        _ => "none",
    }
}

fn elf_load_phase_name(phase: usize) -> &'static str {
    match phase {
        ELF_LOAD_PHASE_INTERPRETER_PARSE => "interpreter_parse",
        ELF_LOAD_PHASE_INTERPRETER_BIAS => "interpreter_bias",
        ELF_LOAD_PHASE_ADDRESS_SPACE => "address_space",
        ELF_LOAD_PHASE_SEGMENT_MAP => "segment_map",
        ELF_LOAD_PHASE_SEGMENT_COPY => "segment_copy",
        ELF_LOAD_PHASE_SEGMENT_ZERO => "segment_zero",
        ELF_LOAD_PHASE_STACK_MAP => "stack_map",
        _ => "idle",
    }
}

fn address_space_create_phase_name(phase: usize) -> &'static str {
    match phase {
        ADDRESS_SPACE_CREATE_ROOT => "root_page_table",
        ADDRESS_SPACE_CREATE_ASID => "asid_allocate",
        _ => "idle",
    }
}

pub(crate) fn note_address_space_create_phase(phase: usize) {
    let cpu = cpu_slot();
    ACTIVE_ADDRESS_SPACE_CREATE_SINCE_US[cpu].store(now_us(), Ordering::Relaxed);
    ACTIVE_ADDRESS_SPACE_CREATE_PHASE[cpu].store(phase, Ordering::Release);
}

pub(crate) fn note_asid_allocator_state(
    max_asid: usize,
    next_asid: usize,
    reusable: usize,
    retired: usize,
) {
    ASID_MAX.store(max_asid, Ordering::Relaxed);
    ASID_NEXT.store(next_asid, Ordering::Relaxed);
    ASID_REUSABLE.store(reusable, Ordering::Relaxed);
    ASID_RETIRED.store(retired, Ordering::Relaxed);
}

pub(crate) fn note_asid_allocation(asid: usize, reused: bool, global_flush: bool) {
    ASID_ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
    ASID_LAST_ALLOCATED.store(asid, Ordering::Relaxed);
    if asid == 0 {
        ASID_ZERO_FALLBACKS.fetch_add(1, Ordering::Relaxed);
    }
    if reused {
        ASID_REUSES.fetch_add(1, Ordering::Relaxed);
        if global_flush {
            ASID_GLOBAL_RECYCLES.fetch_add(1, Ordering::Relaxed);
        }
    }
}

#[inline]
fn clear_elf_load_for_cpu(cpu: usize) {
    ACTIVE_ELF_LOAD_ROLE[cpu].store(0, Ordering::Relaxed);
    ACTIVE_ELF_LOAD_PHASE[cpu].store(0, Ordering::Release);
}

pub(crate) fn note_elf_load_phase(role: usize, phase: usize, bias: usize) {
    let cpu = cpu_slot();
    if ACTIVE_EXEC_STAGE[cpu].load(Ordering::Acquire) != EXEC_STAGE_MEMORY_BUILD {
        return;
    }
    ACTIVE_ELF_LOAD_ROLE[cpu].store(role, Ordering::Relaxed);
    ACTIVE_ELF_LOAD_SEGMENT[cpu].store(0, Ordering::Relaxed);
    ACTIVE_ELF_LOAD_VADDR[cpu].store(0, Ordering::Relaxed);
    ACTIVE_ELF_LOAD_FILESZ[cpu].store(0, Ordering::Relaxed);
    ACTIVE_ELF_LOAD_MEMSZ[cpu].store(0, Ordering::Relaxed);
    ACTIVE_ELF_LOAD_BIAS[cpu].store(bias, Ordering::Relaxed);
    ACTIVE_ELF_LOAD_PHASE_SINCE_US[cpu].store(now_us(), Ordering::Relaxed);
    ACTIVE_ELF_LOAD_PHASE[cpu].store(phase, Ordering::Release);
}

pub(crate) fn note_elf_load_segment(
    phase: usize,
    segment: usize,
    p_vaddr: usize,
    p_filesz: usize,
    p_memsz: usize,
) {
    let cpu = cpu_slot();
    if ACTIVE_EXEC_STAGE[cpu].load(Ordering::Acquire) != EXEC_STAGE_MEMORY_BUILD {
        return;
    }
    ACTIVE_ELF_LOAD_SEGMENT[cpu].store(segment, Ordering::Relaxed);
    ACTIVE_ELF_LOAD_VADDR[cpu].store(p_vaddr, Ordering::Relaxed);
    ACTIVE_ELF_LOAD_FILESZ[cpu].store(p_filesz, Ordering::Relaxed);
    ACTIVE_ELF_LOAD_MEMSZ[cpu].store(p_memsz, Ordering::Relaxed);
    ACTIVE_ELF_LOAD_PHASE_SINCE_US[cpu].store(now_us(), Ordering::Relaxed);
    ACTIVE_ELF_LOAD_PHASE[cpu].store(phase, Ordering::Release);
}

pub(crate) fn clear_elf_load() {
    clear_elf_load_for_cpu(cpu_slot());
}

pub(crate) struct ExecDiagnosticScope {
    cpu: usize,
}

impl ExecDiagnosticScope {
    pub(crate) fn enter(pid: usize) -> Self {
        let cpu = cpu_slot();
        let now = now_us();
        ACTIVE_EXEC_PID[cpu].store(pid, Ordering::Relaxed);
        ACTIVE_EXEC_PATH_HASH[cpu].store(0, Ordering::Relaxed);
        ACTIVE_EXEC_PATH_LEN[cpu].store(0, Ordering::Relaxed);
        clear_elf_load_for_cpu(cpu);
        ACTIVE_EXEC_STARTED_AT_US[cpu].store(now, Ordering::Relaxed);
        ACTIVE_EXEC_STAGE_SINCE_US[cpu].store(now, Ordering::Relaxed);
        ACTIVE_EXEC_ENTRIES[cpu].fetch_add(1, Ordering::Relaxed);
        ACTIVE_EXEC_STAGE[cpu].store(EXEC_STAGE_ENTER, Ordering::Release);
        Self { cpu }
    }

    pub(crate) fn set_path(&self, path: &str) {
        let mut hash = 0xcbf2_9ce4_8422_2325usize;
        for byte in path.as_bytes() {
            hash ^= *byte as usize;
            hash = hash.wrapping_mul(0x0100_0000_01b3usize);
        }
        ACTIVE_EXEC_PATH_HASH[self.cpu].store(hash, Ordering::Relaxed);
        ACTIVE_EXEC_PATH_LEN[self.cpu].store(path.len(), Ordering::Relaxed);
        let slot = hash % EXEC_PATH_SLOTS;
        let previous = EXEC_PATH_HASHES[slot].swap(hash, Ordering::AcqRel);
        let count = if previous == hash {
            EXEC_PATH_COUNTS[slot].fetch_add(1, Ordering::Relaxed) + 1
        } else {
            EXEC_PATH_COUNTS[slot].store(1, Ordering::Relaxed);
            1
        };
        if count <= 4 || count.is_power_of_two() {
            crate::println!(
                "BUILDSTORM_DIAG exec_path pid={} hash={:#x} count={} path={}",
                ACTIVE_EXEC_PID[self.cpu].load(Ordering::Relaxed),
                hash,
                count,
                path,
            );
        }
    }
}

impl Drop for ExecDiagnosticScope {
    fn drop(&mut self) {
        clear_elf_load_for_cpu(self.cpu);
        ACTIVE_EXEC_PID[self.cpu].store(0, Ordering::Relaxed);
        ACTIVE_EXEC_LAST_EXIT_US[self.cpu].store(now_us(), Ordering::Relaxed);
        ACTIVE_EXEC_STAGE[self.cpu].store(0, Ordering::Release);
    }
}

#[inline]
pub(crate) fn note_exec_stage(stage: usize) {
    let cpu = cpu_slot();
    if ACTIVE_EXEC_STAGE[cpu].load(Ordering::Acquire) == 0 {
        return;
    }
    ACTIVE_EXEC_STAGE_SINCE_US[cpu].store(now_us(), Ordering::Relaxed);
    ACTIVE_EXEC_STAGE[cpu].store(stage, Ordering::Release);
}

#[inline]
fn block_slot(reason: BlockReason) -> usize {
    match reason {
        BlockReason::Io => 0,
        BlockReason::PipeFdReady => 1,
        BlockReason::Poll => 2,
        BlockReason::ChildExit => 3,
        BlockReason::Timer => 4,
        BlockReason::Futex => 5,
        BlockReason::Signal => 6,
    }
}

#[inline]
fn block_actor_slot(task: &crate::task::TaskControlBlock) -> usize {
    let Some(inner) = task.inner.try_lock() else {
        return 2;
    };
    if inner.exec_path.contains("rustc") {
        0
    } else if inner.exec_path.contains("cargo") {
        1
    } else {
        2
    }
}

#[inline]
pub(crate) fn note_user_tick() {
    USER_TICKS[cpu_slot()].fetch_add(1, Ordering::Relaxed);
    if let Some(task) = crate::task::current_task() {
        task.diagnostic_user_ticks.fetch_add(1, Ordering::Relaxed);
    }
}

#[inline]
pub(crate) fn note_kernel_tick() {
    KERNEL_TICKS[cpu_slot()].fetch_add(1, Ordering::Relaxed);
    if let Some(task) = crate::task::current_task() {
        task.diagnostic_kernel_ticks.fetch_add(1, Ordering::Relaxed);
    }
}

#[inline]
pub(crate) fn note_idle_tick() {
    IDLE_TICKS[cpu_slot()].fetch_add(1, Ordering::Relaxed);
}

#[inline]
pub(crate) fn note_user_run(elapsed_us: usize, escape_kind: usize) {
    let cpu = cpu_slot();
    USER_RUN_COUNT[cpu].fetch_add(1, Ordering::Relaxed);
    USER_RUN_TOTAL_US[cpu].fetch_add(elapsed_us, Ordering::Relaxed);
    USER_RUN_MAX_US[cpu].fetch_max(elapsed_us, Ordering::Relaxed);
    if let Some(task) = crate::task::current_task() {
        task.diagnostic_user_run_count
            .fetch_add(1, Ordering::Relaxed);
        task.diagnostic_user_run_total_us
            .fetch_add(elapsed_us, Ordering::Relaxed);
        task.diagnostic_user_run_max_us
            .fetch_max(elapsed_us, Ordering::Relaxed);
    }
    match escape_kind {
        0 => USER_RUN_SYSCALL[cpu].fetch_add(1, Ordering::Relaxed),
        1 => USER_RUN_TIMER[cpu].fetch_add(1, Ordering::Relaxed),
        2 => USER_RUN_IRQ[cpu].fetch_add(1, Ordering::Relaxed),
        _ => USER_RUN_OTHER[cpu].fetch_add(1, Ordering::Relaxed),
    };
}

#[inline]
pub(crate) fn note_user_run_enter(pid: usize) {
    let cpu = cpu_slot();
    ACTIVE_USER_SINCE_US[cpu].store(now_us(), Ordering::Relaxed);
    ACTIVE_USER_ENTRIES[cpu].fetch_add(1, Ordering::Relaxed);
    ACTIVE_USER_PID[cpu].store(pid, Ordering::Release);
}

#[inline]
pub(crate) fn note_user_run_exit() {
    let cpu = cpu_slot();
    ACTIVE_USER_PID[cpu].store(0, Ordering::Release);
    ACTIVE_USER_LAST_EXIT_US[cpu].store(now_us(), Ordering::Relaxed);
}

pub(crate) struct UserTrapScope {
    cpu: usize,
    kind: usize,
    started_at: Option<usize>,
}

impl Drop for UserTrapScope {
    fn drop(&mut self) {
        if let Some(started_at) = self.started_at {
            let elapsed = now_us().saturating_sub(started_at);
            USER_TRAP_SAMPLES[self.kind].fetch_add(1, Ordering::Relaxed);
            USER_TRAP_TOTAL_US[self.kind].fetch_add(elapsed, Ordering::Relaxed);
            USER_TRAP_MAX_US[self.kind].fetch_max(elapsed, Ordering::Relaxed);
        }
        ACTIVE_USER_TRAP_KIND[self.cpu].store(0, Ordering::Release);
    }
}

#[inline]
pub(crate) fn note_user_trap_enter(kind: usize) -> UserTrapScope {
    let cpu = cpu_slot();
    let kind = kind.min(USER_TRAP_SLOTS - 1);
    let sequence = USER_TRAP_COUNTS_PER_CPU[cpu][kind].fetch_add(1, Ordering::Relaxed);
    ACTIVE_USER_TRAP_SINCE_US[cpu].store(now_us(), Ordering::Relaxed);
    ACTIVE_USER_TRAP_ENTRIES[cpu].fetch_add(1, Ordering::Relaxed);
    ACTIVE_USER_TRAP_KIND[cpu].store(kind, Ordering::Release);
    UserTrapScope {
        cpu,
        kind,
        started_at: ((sequence & SYSCALL_SAMPLE_MASK) == 0).then(now_us),
    }
}

#[inline]
pub(crate) fn note_context_switch() {
    CONTEXT_SWITCHES[cpu_slot()].fetch_add(1, Ordering::Relaxed);
}

#[inline]
pub(crate) fn note_migration() {
    TASK_MIGRATIONS[cpu_slot()].fetch_add(1, Ordering::Relaxed);
}

#[inline]
pub(crate) fn note_runqueue_len(len: usize) {
    RUNQUEUE_MAX[cpu_slot()].fetch_max(len, Ordering::Relaxed);
}

#[inline]
pub(crate) fn note_blocked_owner_enter() {
    let cpu = cpu_slot();
    let current = BLOCKED_OWNER_CURRENT[cpu].fetch_add(1, Ordering::Relaxed) + 1;
    BLOCKED_OWNER_MAX[cpu].fetch_max(current, Ordering::Relaxed);
    BLOCKED_OWNER_ENTERS[cpu].fetch_add(1, Ordering::Relaxed);
}

#[inline]
pub(crate) fn note_blocked_owner_exit() {
    let current = &BLOCKED_OWNER_CURRENT[cpu_slot()];
    let previous = current.fetch_sub(1, Ordering::Relaxed);
    // Keep a diagnostic accounting imbalance from wrapping the counter; this
    // cannot affect the feature-off production scheduler.
    if previous == 0 {
        current.store(0, Ordering::Relaxed);
    }
    BLOCKED_OWNER_EXITS[cpu_slot()].fetch_add(1, Ordering::Relaxed);
}

#[inline]
pub(crate) fn note_blocked_owner_loop_dispatch() {
    BLOCKED_OWNER_LOOP_DISPATCHES[cpu_slot()].fetch_add(1, Ordering::Relaxed);
}

#[inline]
pub(crate) fn note_blocked_owner_wake(owner_cpu: Option<usize>) {
    if let Some(cpu) = owner_cpu {
        BLOCKED_OWNER_WAKES[cpu.min(CPU_SLOTS.saturating_sub(1))].fetch_add(1, Ordering::Relaxed);
    } else {
        BLOCKED_GLOBAL_WAKES.fetch_add(1, Ordering::Relaxed);
    }
}

#[inline]
pub(crate) fn note_blocked_owner_timed_loop(elapsed_us: usize, empty_iterations: usize) {
    let cpu = cpu_slot();
    BLOCKED_OWNER_TIMED_LOOPS[cpu].fetch_add(1, Ordering::Relaxed);
    BLOCKED_OWNER_TIMED_TOTAL_US[cpu].fetch_add(elapsed_us, Ordering::Relaxed);
    BLOCKED_OWNER_TIMED_MAX_US[cpu].fetch_max(elapsed_us, Ordering::Relaxed);
    BLOCKED_OWNER_EMPTY_ITERATIONS[cpu].fetch_add(empty_iterations, Ordering::Relaxed);
}

#[inline]
pub(crate) fn note_path_component_lookup(components: usize) {
    PATH_COMPONENT_LOOKUPS.fetch_add(components, Ordering::Relaxed);
}

#[inline]
pub(crate) fn note_path_cache(hit: bool, negative_hit: bool) {
    if hit {
        PATH_CACHE_HITS.fetch_add(1, Ordering::Relaxed);
    } else {
        PATH_CACHE_MISSES.fetch_add(1, Ordering::Relaxed);
    }
    if negative_hit {
        NEGATIVE_PATH_CACHE_HITS.fetch_add(1, Ordering::Relaxed);
    }
}

#[inline]
pub(crate) fn note_dir_cache(hit: bool) {
    if hit {
        DIR_CACHE_HITS.fetch_add(1, Ordering::Relaxed);
    } else {
        DIR_CACHE_MISSES.fetch_add(1, Ordering::Relaxed);
    }
}

#[inline]
pub(crate) fn note_inode_metadata_cache(hit: bool) {
    if hit {
        INODE_METADATA_CACHE_HITS.fetch_add(1, Ordering::Relaxed);
    } else {
        INODE_METADATA_CACHE_MISSES.fetch_add(1, Ordering::Relaxed);
    }
}

#[inline]
pub(crate) fn note_vma_count(count: usize) {
    LOGICAL_VMA_CURRENT.store(count, Ordering::Relaxed);
    LOGICAL_VMA_MAX.fetch_max(count, Ordering::Relaxed);
    if let Some(task) = crate::task::current_task() {
        task.diagnostic_vma_current.store(count, Ordering::Relaxed);
        task.diagnostic_vma_max.fetch_max(count, Ordering::Relaxed);
    }
}

#[inline]
pub(crate) fn note_cache(inode_hit: bool, metadata_hit: bool, block_hit: bool) {
    if inode_hit {
        INODE_CACHE_HITS.fetch_add(1, Ordering::Relaxed);
    } else {
        INODE_CACHE_MISSES.fetch_add(1, Ordering::Relaxed);
    }
    if metadata_hit {
        METADATA_CACHE_HITS.fetch_add(1, Ordering::Relaxed);
    } else {
        METADATA_CACHE_MISSES.fetch_add(1, Ordering::Relaxed);
    }
    if block_hit {
        BLOCK_CACHE_HITS.fetch_add(1, Ordering::Relaxed);
    } else {
        BLOCK_CACHE_MISSES.fetch_add(1, Ordering::Relaxed);
    }
}

#[inline]
pub(crate) fn note_block_cache(hit: bool) {
    if hit {
        BLOCK_CACHE_HITS.fetch_add(1, Ordering::Relaxed);
    } else {
        BLOCK_CACHE_MISSES.fetch_add(1, Ordering::Relaxed);
    }
}

#[inline]
pub(crate) fn note_root_activation(user: bool, redundant: bool) {
    if user {
        USER_ROOT_ACTIVATIONS.fetch_add(1, Ordering::Relaxed);
    } else {
        KERNEL_ROOT_ACTIVATIONS.fetch_add(1, Ordering::Relaxed);
    }
    if redundant {
        REDUNDANT_ROOT_ACTIVATIONS.fetch_add(1, Ordering::Relaxed);
    } else {
        PAGE_TABLE_WRITES.fetch_add(1, Ordering::Relaxed);
    }
}

#[inline]
pub(crate) fn note_local_tlb_flush() {
    LOCAL_TLB_FLUSHES.fetch_add(1, Ordering::Relaxed);
}

#[inline]
pub(crate) fn note_remote_shootdown(targets: usize) {
    REMOTE_SHOOTDOWNS.fetch_add(1, Ordering::Relaxed);
    REMOTE_SHOOTDOWN_TARGETS.fetch_add(targets, Ordering::Relaxed);
}

#[inline]
pub(crate) fn note_page_table_batch(writes: usize, local_flush: bool) {
    PAGE_TABLE_BATCHES.fetch_add(1, Ordering::Relaxed);
    PAGE_TABLE_BATCH_WRITES.fetch_add(writes, Ordering::Relaxed);
    PAGE_TABLE_BATCH_MAX.fetch_max(writes, Ordering::Relaxed);
    if local_flush {
        PAGE_TABLE_BATCH_LOCAL_FLUSHES.fetch_add(1, Ordering::Relaxed);
    }
}

#[inline]
pub(crate) fn note_anonymous_vma_install(
    left_mergeable: bool,
    right_mergeable: bool,
    vma_count: usize,
    pages: usize,
) {
    ANONYMOUS_VMA_INSTALLS.fetch_add(1, Ordering::Relaxed);
    if left_mergeable {
        ANONYMOUS_VMA_LEFT_MERGEABLE.fetch_add(1, Ordering::Relaxed);
    }
    if right_mergeable {
        ANONYMOUS_VMA_RIGHT_MERGEABLE.fetch_add(1, Ordering::Relaxed);
    }
    if left_mergeable && right_mergeable {
        ANONYMOUS_VMA_BOTH_MERGEABLE.fetch_add(1, Ordering::Relaxed);
    } else if !left_mergeable && !right_mergeable {
        ANONYMOUS_VMA_NEITHER_MERGEABLE.fetch_add(1, Ordering::Relaxed);
    }
    ANONYMOUS_VMA_CURRENT.store(vma_count, Ordering::Relaxed);
    ANONYMOUS_VMA_MAX.fetch_max(vma_count, Ordering::Relaxed);
    ANONYMOUS_VMA_PAGES.fetch_add(pages, Ordering::Relaxed);
    ANONYMOUS_VMA_PAGES_MAX.fetch_max(pages, Ordering::Relaxed);
}

#[inline]
pub(crate) fn sample_anonymous_fault_shape() -> bool {
    ANONYMOUS_FAULT_SHAPE_CALLS.fetch_add(1, Ordering::Relaxed) & WORK_SAMPLE_MASK == 0
}

#[inline]
pub(crate) fn note_anonymous_fault_shape(
    left_resident: bool,
    right_resident: bool,
    forward_pages: usize,
    backward_pages: usize,
) {
    ANONYMOUS_FAULT_SHAPE_SAMPLES.fetch_add(1, Ordering::Relaxed);
    if left_resident {
        ANONYMOUS_FAULT_LEFT_RESIDENT.fetch_add(1, Ordering::Relaxed);
    }
    if right_resident {
        ANONYMOUS_FAULT_RIGHT_RESIDENT.fetch_add(1, Ordering::Relaxed);
    }
    if left_resident && right_resident {
        ANONYMOUS_FAULT_BOTH_RESIDENT.fetch_add(1, Ordering::Relaxed);
    } else if !left_resident && !right_resident {
        ANONYMOUS_FAULT_ISOLATED.fetch_add(1, Ordering::Relaxed);
    }
    ANONYMOUS_FAULT_FORWARD_PAGES.fetch_add(forward_pages, Ordering::Relaxed);
    ANONYMOUS_FAULT_BACKWARD_PAGES.fetch_add(backward_pages, Ordering::Relaxed);
    ANONYMOUS_FAULT_FORWARD_MAX.fetch_max(forward_pages, Ordering::Relaxed);
    ANONYMOUS_FAULT_BACKWARD_MAX.fetch_max(backward_pages, Ordering::Relaxed);
}

#[inline]
pub(crate) fn note_anonymous_move_shape(
    left_selected: bool,
    no_frame_realloc: bool,
    shrink_empty_vma: bool,
    existing_frames: usize,
    remove_suffix: usize,
) {
    if left_selected {
        ANONYMOUS_MOVE_LEFT_SELECTED.fetch_add(1, Ordering::Relaxed);
        if no_frame_realloc {
            ANONYMOUS_MOVE_LEFT_NO_REALLOC.fetch_add(1, Ordering::Relaxed);
        } else {
            ANONYMOUS_MOVE_LEFT_FRAME_RELOCATE.fetch_add(existing_frames, Ordering::Relaxed);
        }
        if shrink_empty_vma {
            ANONYMOUS_MOVE_LEFT_SHRINK.fetch_add(1, Ordering::Relaxed);
        } else {
            ANONYMOUS_MOVE_LEFT_REMOVE.fetch_add(1, Ordering::Relaxed);
            ANONYMOUS_MOVE_LEFT_REMOVE_SUFFIX.fetch_add(remove_suffix, Ordering::Relaxed);
            ANONYMOUS_MOVE_LEFT_REMOVE_SUFFIX_MAX.fetch_max(remove_suffix, Ordering::Relaxed);
        }
    } else {
        ANONYMOUS_MOVE_RIGHT_SELECTED.fetch_add(1, Ordering::Relaxed);
        if no_frame_realloc {
            ANONYMOUS_MOVE_RIGHT_NO_REALLOC.fetch_add(1, Ordering::Relaxed);
        }
        ANONYMOUS_MOVE_RIGHT_FRAME_MOVE.fetch_add(existing_frames, Ordering::Relaxed);
        if shrink_empty_vma {
            ANONYMOUS_MOVE_RIGHT_SHRINK.fetch_add(1, Ordering::Relaxed);
        } else {
            ANONYMOUS_MOVE_RIGHT_REMOVE.fetch_add(1, Ordering::Relaxed);
            ANONYMOUS_MOVE_RIGHT_REMOVE_SUFFIX.fetch_add(remove_suffix, Ordering::Relaxed);
            ANONYMOUS_MOVE_RIGHT_REMOVE_SUFFIX_MAX.fetch_max(remove_suffix, Ordering::Relaxed);
        }
    }
    if no_frame_realloc && shrink_empty_vma && (left_selected || existing_frames == 0) {
        ANONYMOUS_MOVE_ZERO_METADATA.fetch_add(1, Ordering::Relaxed);
    }
}

#[inline]
pub(crate) fn note_anonymous_coalesce_detail(
    areas: usize,
    sort_us: usize,
    scan_us: usize,
    total_us: usize,
    merges: usize,
    merged_frames: usize,
    frame_reallocs: usize,
    frame_relocate: usize,
) {
    ANONYMOUS_COALESCE_CALLS.fetch_add(1, Ordering::Relaxed);
    ANONYMOUS_COALESCE_AREAS.fetch_add(areas, Ordering::Relaxed);
    ANONYMOUS_COALESCE_AREAS_MAX.fetch_max(areas, Ordering::Relaxed);
    ANONYMOUS_COALESCE_SORT_US.fetch_add(sort_us, Ordering::Relaxed);
    ANONYMOUS_COALESCE_SCAN_US.fetch_add(scan_us, Ordering::Relaxed);
    ANONYMOUS_COALESCE_TOTAL_US.fetch_add(total_us, Ordering::Relaxed);
    ANONYMOUS_COALESCE_MERGES.fetch_add(merges, Ordering::Relaxed);
    ANONYMOUS_COALESCE_MERGED_FRAMES.fetch_add(merged_frames, Ordering::Relaxed);
    ANONYMOUS_COALESCE_FRAME_REALLOCS.fetch_add(frame_reallocs, Ordering::Relaxed);
    ANONYMOUS_COALESCE_FRAME_RELOCATE.fetch_add(frame_relocate, Ordering::Relaxed);
}

#[inline]
pub(crate) fn note_block_start(task: &crate::task::TaskControlBlock, reason: BlockReason) {
    let block_slot = block_slot(reason);
    let actor_slot = block_actor_slot(task);
    let encoded = ((actor_slot + 1) << 8) | (block_slot + 1);
    task.diagnostic_block_reason
        .store(encoded, Ordering::Relaxed);
    task.diagnostic_block_started_at
        .store(now_us(), Ordering::Release);
    task.diagnostic_woken_at.store(0, Ordering::Release);
    task.diagnostic_woken_reason.store(0, Ordering::Release);
    BLOCK_COUNTS[block_slot].fetch_add(1, Ordering::Relaxed);
    BLOCK_ACTOR_COUNTS[actor_slot].fetch_add(1, Ordering::Relaxed);
}

#[inline]
pub(crate) fn note_block_end(task: &crate::task::TaskControlBlock, outcome: WaitOutcome) {
    let started = task.diagnostic_block_started_at.swap(0, Ordering::AcqRel);
    let encoded = task.diagnostic_block_reason.swap(0, Ordering::AcqRel);
    if started == 0 || encoded == 0 {
        return;
    }
    let slot = (encoded & 0xff).saturating_sub(1);
    if slot >= BLOCK_SLOTS {
        return;
    }
    let actor_slot = ((encoded >> 8).saturating_sub(1)).min(BLOCK_ACTOR_SLOTS - 1);
    let now = now_us();
    let elapsed = now.saturating_sub(started);
    BLOCK_TOTAL_US[slot].fetch_add(elapsed, Ordering::Relaxed);
    BLOCK_MAX_US[slot].fetch_max(elapsed, Ordering::Relaxed);
    BLOCK_ACTOR_TOTAL_US[actor_slot].fetch_add(elapsed, Ordering::Relaxed);
    task.diagnostic_block_count.fetch_add(1, Ordering::Relaxed);
    task.diagnostic_block_total_us
        .fetch_add(elapsed, Ordering::Relaxed);
    match outcome {
        WaitOutcome::Woken => BLOCK_WOKEN[slot].fetch_add(1, Ordering::Relaxed),
        WaitOutcome::TimedOut => BLOCK_TIMED_OUT[slot].fetch_add(1, Ordering::Relaxed),
        WaitOutcome::Interrupted => BLOCK_INTERRUPTED[slot].fetch_add(1, Ordering::Relaxed),
    };
    // Leave the completion stamp for the eventual dispatch site. The atomic
    // exchange there ensures each completed wait contributes at most once.
    task.diagnostic_woken_reason
        .store(encoded, Ordering::Release);
    task.diagnostic_woken_at.store(now, Ordering::Release);
}

/// Record the scheduling delay after a waiter was made Ready. This is called
/// only at dispatch/resume boundaries, never in individual syscall logging.
#[inline]
pub(crate) fn note_woken_task_dispatched(task: &crate::task::TaskControlBlock) {
    let woken_at = task.diagnostic_woken_at.swap(0, Ordering::AcqRel);
    let encoded = task.diagnostic_woken_reason.swap(0, Ordering::AcqRel);
    if woken_at == 0 || encoded == 0 {
        return;
    }
    let slot = (encoded & 0xff).saturating_sub(1);
    if slot >= BLOCK_SLOTS {
        return;
    }
    let elapsed = now_us().saturating_sub(woken_at);
    WAKE_TO_RUN_COUNTS[slot].fetch_add(1, Ordering::Relaxed);
    WAKE_TO_RUN_TOTAL_US[slot].fetch_add(elapsed, Ordering::Relaxed);
    WAKE_TO_RUN_MAX_US[slot].fetch_max(elapsed, Ordering::Relaxed);
}

pub(crate) struct WorkScope {
    class: WorkClass,
    started_at: usize,
    sampled: bool,
}

impl WorkScope {
    #[inline]
    pub(crate) fn new(class: WorkClass) -> Self {
        let slot = class as usize;
        let cpu = cpu_slot();
        let sampled =
            WORK_COUNTS_PER_CPU[slot][cpu].fetch_add(1, Ordering::Relaxed) & WORK_SAMPLE_MASK == 0;
        Self {
            class,
            started_at: sampled.then(now_us).unwrap_or(0),
            sampled,
        }
    }
}

impl Drop for WorkScope {
    fn drop(&mut self) {
        if !self.sampled {
            return;
        }
        let slot = self.class as usize;
        let elapsed = now_us().saturating_sub(self.started_at);
        WORK_SAMPLES[slot].fetch_add(1, Ordering::Relaxed);
        WORK_TOTAL_US[slot].fetch_add(elapsed, Ordering::Relaxed);
        WORK_MAX_US[slot].fetch_max(elapsed, Ordering::Relaxed);
    }
}

/// Attribute `handle_page_fault()` calls by their entry path.  The same MM
/// helper services hardware traps and kernel user-copy preparation, so the
/// existing load/store/exec totals alone do not identify a hardware-fault hot
/// path.
pub(crate) struct PageFaultSourceScope {
    source: PageFaultSource,
    started_at: usize,
}

impl PageFaultSourceScope {
    #[inline]
    pub(crate) fn new(source: PageFaultSource) -> Self {
        Self {
            source,
            started_at: now_us(),
        }
    }
}

impl Drop for PageFaultSourceScope {
    fn drop(&mut self) {
        let slot = self.source as usize;
        let elapsed = now_us().saturating_sub(self.started_at);
        PAGE_FAULT_SOURCE_COUNTS[slot].fetch_add(1, Ordering::Relaxed);
        PAGE_FAULT_SOURCE_TOTAL_US[slot].fetch_add(elapsed, Ordering::Relaxed);
        PAGE_FAULT_SOURCE_MAX_US[slot].fetch_max(elapsed, Ordering::Relaxed);
    }
}

/// Measures only successfully resolved fault paths. Call `finish` at the
/// branch that supplies the missing mapping; failed permission/address checks
/// intentionally remain outside this successful-path aggregate.
pub(crate) struct PageFaultResolutionScope {
    started_at: usize,
}

impl PageFaultResolutionScope {
    #[inline]
    pub(crate) fn new() -> Self {
        Self {
            started_at: now_us(),
        }
    }

    #[inline]
    pub(crate) fn finish(self, resolution: PageFaultResolution, pages: usize) {
        let slot = resolution as usize;
        let elapsed = now_us().saturating_sub(self.started_at);
        PAGE_FAULT_RESOLUTION_COUNTS[slot].fetch_add(1, Ordering::Relaxed);
        PAGE_FAULT_RESOLUTION_TOTAL_US[slot].fetch_add(elapsed, Ordering::Relaxed);
        PAGE_FAULT_RESOLUTION_MAX_US[slot].fetch_max(elapsed, Ordering::Relaxed);
        PAGE_FAULT_RESOLUTION_PAGES[slot].fetch_add(pages, Ordering::Relaxed);
    }
}

pub(crate) struct TimedLockGuard<'a, T> {
    guard: Option<MutexGuard<'a, T>>,
    class: LockClass,
    memory_set_site: Option<MemorySetLockSite>,
    acquired_at: usize,
    sampled: bool,
    owner_cpu: Option<usize>,
}

impl<T> Deref for TimedLockGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &Self::Target {
        self.guard.as_ref().expect("diagnostic lock guard missing")
    }
}
impl<T> DerefMut for TimedLockGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.guard.as_mut().expect("diagnostic lock guard missing")
    }
}
impl<T> Drop for TimedLockGuard<'_, T> {
    fn drop(&mut self) {
        if self.sampled {
            let slot = self.class as usize;
            let hold = now_us().saturating_sub(self.acquired_at);
            LOCK_HOLD_TOTAL_US[slot].fetch_add(hold, Ordering::Relaxed);
            LOCK_HOLD_MAX_US[slot].fetch_max(hold, Ordering::Relaxed);
            if let Some(site) = self.memory_set_site {
                let site = site as usize;
                MEMORY_SET_LOCK_SITE_HOLD_TOTAL_US[site].fetch_add(hold, Ordering::Relaxed);
                MEMORY_SET_LOCK_SITE_HOLD_MAX_US[site].fetch_max(hold, Ordering::Relaxed);
            }
        }
        drop(self.guard.take());
        if let Some(cpu) = self.owner_cpu {
            MEMORY_SET_OWNER_MUTEX[cpu].store(0, Ordering::Release);
            MEMORY_SET_OWNER_SITE[cpu].store(0, Ordering::Relaxed);
        }
    }
}

#[inline]
pub(crate) fn lock<'a, T>(class: LockClass, mutex: &'a Mutex<T>) -> TimedLockGuard<'a, T> {
    lock_with_memory_set_site(class, None, None, mutex)
}

#[inline]
pub(crate) fn lock_heap<'a, T>(
    mutex: &'a Mutex<T>,
    size: usize,
    allocation: bool,
) -> TimedLockGuard<'a, T> {
    lock_with_memory_set_site(LockClass::Heap, None, Some((size, allocation)), mutex)
}

#[inline]
pub(crate) fn lock_memory_set<'a, T>(
    site: MemorySetLockSite,
    mutex: &'a Mutex<T>,
) -> TimedLockGuard<'a, T> {
    lock_with_memory_set_site(LockClass::MemorySet, Some(site), None, mutex)
}

#[inline]
pub(crate) fn lock_memory_set_activation<'a, T>(mutex: &'a Mutex<T>) -> TimedLockGuard<'a, T> {
    lock_with_memory_set_site(
        LockClass::MemorySetActivation,
        Some(MemorySetLockSite::UserEntryActivation),
        None,
        mutex,
    )
}

#[inline]
pub(crate) fn note_mmap_shape(anonymous: bool, fixed: bool, hinted: bool, lazy: bool) {
    if anonymous {
        MMAP_ANONYMOUS.fetch_add(1, Ordering::Relaxed);
    } else {
        MMAP_FILE.fetch_add(1, Ordering::Relaxed);
    }
    if fixed {
        MMAP_FIXED.fetch_add(1, Ordering::Relaxed);
    }
    if hinted {
        MMAP_HINTED.fetch_add(1, Ordering::Relaxed);
    }
    if lazy {
        MMAP_LAZY.fetch_add(1, Ordering::Relaxed);
    } else {
        MMAP_EAGER.fetch_add(1, Ordering::Relaxed);
    }
}

#[inline]
pub(crate) fn note_madvise_dontneed(requested_pages: usize, discarded_pages: usize) {
    MADVISE_DONTNEED_CALLS.fetch_add(1, Ordering::Relaxed);
    MADVISE_DONTNEED_REQUESTED_PAGES.fetch_add(requested_pages, Ordering::Relaxed);
    MADVISE_DONTNEED_DISCARDED_PAGES.fetch_add(discarded_pages, Ordering::Relaxed);
}

#[inline]
fn mmap_length_bucket(length: usize) -> usize {
    if length <= 64 * 1024 {
        0
    } else if length <= 1024 * 1024 {
        1
    } else if length <= 16 * 1024 * 1024 {
        2
    } else if length <= 256 * 1024 * 1024 {
        3
    } else {
        4
    }
}

#[inline]
pub(crate) fn note_mmap_request(length: usize) {
    MMAP_LENGTH_COUNTS[mmap_length_bucket(length)].fetch_add(1, Ordering::Relaxed);
}

#[inline]
pub(crate) fn note_mmap_placement(start: usize) {
    if crate::config::user_va::HIGH_MMAP_ARENA.contains_addr(start) {
        MMAP_HIGH_ARENA_PLACEMENTS.fetch_add(1, Ordering::Relaxed);
    } else if crate::config::user_va::LOW_MMAP_ARENA.contains_addr(start) {
        MMAP_LOW_ARENA_PLACEMENTS.fetch_add(1, Ordering::Relaxed);
    }
}

#[inline]
pub(crate) fn note_mmap_select_ok() {
    MMAP_SELECT_OK.fetch_add(1, Ordering::Relaxed);
}

#[inline]
pub(crate) fn note_mmap_select_enomem(length: usize, vmas: usize) -> bool {
    let prior = MMAP_SELECT_ENOMEM.fetch_add(1, Ordering::Relaxed);
    MMAP_FAILED_LENGTH_COUNTS[mmap_length_bucket(length)].fetch_add(1, Ordering::Relaxed);
    MMAP_FAILED_LENGTH_MAX.fetch_max(length, Ordering::Relaxed);
    MMAP_FAILED_VMAS_MAX.fetch_max(vmas, Ordering::Relaxed);
    prior == 0 || prior % 4096 == 0
}

#[inline]
pub(crate) fn note_munmap_cursor(start: usize, end: usize, cursor: usize) {
    if end <= cursor {
        MUNMAP_BELOW_CURSOR_LENGTH_COUNTS[mmap_length_bucket(end.saturating_sub(start))]
            .fetch_add(1, Ordering::Relaxed);
    } else if start < cursor {
        MUNMAP_OVERLAPS_CURSOR.fetch_add(1, Ordering::Relaxed);
    } else {
        MUNMAP_AT_OR_ABOVE_CURSOR.fetch_add(1, Ordering::Relaxed);
    }
}

#[inline]
pub(crate) fn note_mmap_failed_gap(max_gap: usize) {
    MMAP_FAILED_GAP_MAX.fetch_max(max_gap, Ordering::Relaxed);
}

#[inline]
pub(crate) fn note_mmap_commit_reselect(ok: bool) {
    MMAP_COMMIT_RESELECT.fetch_add(1, Ordering::Relaxed);
    if ok {
        MMAP_COMMIT_RESELECT_OK.fetch_add(1, Ordering::Relaxed);
    } else {
        MMAP_COMMIT_RESELECT_ENOMEM.fetch_add(1, Ordering::Relaxed);
    }
}

pub(crate) struct MmapProtocolAttempt<'a> {
    protocol: &'a MmapProtocol,
    sequence: usize,
    finished: bool,
}

impl MmapProtocolAttempt<'_> {
    pub(crate) fn select_enomem(mut self) {
        self.protocol.select_enomem.fetch_add(1, Ordering::Relaxed);
        self.finished = true;
    }

    pub(crate) fn success(mut self, start: usize, end: usize) {
        note_mmap_protocol_success(self.protocol, self.sequence, start, end);
        self.finished = true;
    }
}

impl Drop for MmapProtocolAttempt<'_> {
    fn drop(&mut self) {
        if !self.finished {
            self.protocol
                .commit_failures
                .fetch_add(1, Ordering::Relaxed);
        }
    }
}

#[inline]
pub(crate) fn begin_mmap_protocol(
    group: &crate::task::ThreadGroup,
    length: usize,
) -> MmapProtocolAttempt<'_> {
    let protocol = &group.diagnostic_mmap_protocol;
    let sequence = protocol.next_sequence.fetch_add(1, Ordering::Relaxed) + 1;
    protocol.attempts.fetch_add(1, Ordering::Relaxed);
    protocol
        .requested_bytes
        .fetch_add(length, Ordering::Relaxed);
    MmapProtocolAttempt {
        protocol,
        sequence,
        finished: false,
    }
}

fn note_mmap_protocol_success(protocol: &MmapProtocol, sequence: usize, start: usize, end: usize) {
    let slot_index = (sequence - 1) % MMAP_PROTOCOL_SLOTS;
    let slot = &protocol.slots[slot_index];
    let mut previous = slot.state.load(Ordering::Acquire);
    let mut acquired = false;
    for _ in 0..4 {
        if previous & 1 != 0 {
            core::hint::spin_loop();
            previous = slot.state.load(Ordering::Acquire);
            continue;
        }
        match slot.state.compare_exchange(
            previous,
            previous | 1,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => {
                acquired = true;
                break;
            }
            Err(value) => previous = value,
        }
    }
    if !acquired {
        protocol.evictions.fetch_add(1, Ordering::Relaxed);
        protocol.successes.fetch_add(1, Ordering::Relaxed);
        protocol
            .successful_bytes
            .fetch_add(end.saturating_sub(start), Ordering::Relaxed);
        return;
    }
    if previous == 0 {
        protocol.active.fetch_add(1, Ordering::Relaxed);
    } else {
        protocol.evictions.fetch_add(1, Ordering::Relaxed);
    }
    slot.start.store(start, Ordering::Relaxed);
    slot.end.store(end, Ordering::Relaxed);
    slot.trims.store(0, Ordering::Relaxed);
    slot.state.store(sequence << 1, Ordering::Release);
    protocol
        .pending_slots
        .fetch_or(1usize << slot_index, Ordering::Release);
    protocol.successes.fetch_add(1, Ordering::Relaxed);
    protocol
        .successful_bytes
        .fetch_add(end.saturating_sub(start), Ordering::Relaxed);
}

pub(crate) fn note_munmap_protocol(group: &crate::task::ThreadGroup, start: usize, end: usize) {
    let protocol = &group.diagnostic_mmap_protocol;
    let pending = protocol.pending_slots.load(Ordering::Acquire);
    if pending == 0 {
        if protocol.attempts.load(Ordering::Relaxed) != 0 {
            protocol.munmap_unmatched.fetch_add(1, Ordering::Relaxed);
        }
        return;
    }
    for slot_index in 0..MMAP_PROTOCOL_SLOTS {
        if pending & (1usize << slot_index) == 0 {
            continue;
        }
        let slot = &protocol.slots[slot_index];
        let state = slot.state.load(Ordering::Acquire);
        if state == 0 || state & 1 != 0 {
            continue;
        }
        if slot
            .state
            .compare_exchange(state, state | 1, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            continue;
        }

        let reservation_start = slot.start.load(Ordering::Relaxed);
        let reservation_end = slot.end.load(Ordering::Relaxed);
        if start == reservation_start && end == reservation_end {
            protocol
                .pending_slots
                .fetch_and(!(1usize << slot_index), Ordering::Release);
            protocol.active.fetch_sub(1, Ordering::Relaxed);
            slot.state.store(0, Ordering::Release);
            protocol.full_release.fetch_add(1, Ordering::Relaxed);
            return;
        }

        let prefix = start == reservation_start && end > start && end < reservation_end;
        let suffix = end == reservation_end && start > reservation_start && start < end;
        if prefix || suffix {
            if prefix {
                slot.start.store(end, Ordering::Relaxed);
                protocol.trim_prefix.fetch_add(1, Ordering::Relaxed);
            } else {
                slot.end.store(start, Ordering::Relaxed);
                protocol.trim_suffix.fetch_add(1, Ordering::Relaxed);
            }
            let prior_trims = slot.trims.fetch_add(1, Ordering::Relaxed);
            match prior_trims {
                0 => &protocol.trim_first,
                1 => &protocol.trim_second,
                _ => &protocol.trim_extra,
            }
            .fetch_add(1, Ordering::Relaxed);
            if prior_trims >= 1 {
                protocol
                    .pending_slots
                    .fetch_and(!(1usize << slot_index), Ordering::Release);
                protocol.active.fetch_sub(1, Ordering::Relaxed);
                slot.state.store(0, Ordering::Release);
            } else {
                slot.state.store(state, Ordering::Release);
            }
            return;
        }
        slot.state.store(state, Ordering::Release);
    }
    protocol.munmap_unmatched.fetch_add(1, Ordering::Relaxed);
}

fn print_mmap_protocol(label: &str, archive_sequence: usize, snapshot: MmapProtocolSnapshot) {
    if snapshot.attempts == 0 {
        return;
    }
    crate::println!(
        "BUILDSTORM_DIAG {} archive_sequence={} tgid={} last_sequence={} attempts={} select_enomem={} commit_failures={} successes={} active={} evictions={} trim_first={} trim_second={} trim_extra={} trim_prefix={} trim_suffix={} full_release={} munmap_unmatched={} requested_bytes={} successful_bytes={}",
        label,
        archive_sequence,
        snapshot.tgid,
        snapshot.last_sequence,
        snapshot.attempts,
        snapshot.select_enomem,
        snapshot.commit_failures,
        snapshot.successes,
        snapshot.active,
        snapshot.evictions,
        snapshot.trim_first,
        snapshot.trim_second,
        snapshot.trim_extra,
        snapshot.trim_prefix,
        snapshot.trim_suffix,
        snapshot.full_release,
        snapshot.munmap_unmatched,
        snapshot.requested_bytes,
        snapshot.successful_bytes,
    );
}

pub(crate) fn report_mmap_protocol(group: &crate::task::ThreadGroup) {
    print_mmap_protocol(
        "mmap_protocol",
        0,
        group.diagnostic_mmap_protocol.snapshot(group.tgid()),
    );
}

pub(crate) fn archive_mmap_protocol(group: &crate::task::ThreadGroup) {
    let snapshot = group.diagnostic_mmap_protocol.snapshot(group.tgid());
    if snapshot.attempts != 0 {
        EXITED_MMAP_PROTOCOLS.lock().push(snapshot);
    }
}

fn report_exited_mmap_protocols() {
    loop {
        let archived = { EXITED_MMAP_PROTOCOLS.lock().pop() };
        let Some((archive_sequence, snapshot)) = archived else {
            break;
        };
        print_mmap_protocol("mmap_protocol_exit", archive_sequence, snapshot);
    }
    let (archived, reported, dropped, pending) = {
        let archive = EXITED_MMAP_PROTOCOLS.lock();
        (
            archive.archived,
            archive.reported,
            archive.dropped,
            archive.len,
        )
    };
    crate::println!(
        "BUILDSTORM_DIAG mmap_protocol_archive archived={} reported={} dropped={} pending={} capacity={}",
        archived,
        reported,
        dropped,
        pending,
        EXITED_MMAP_PROTOCOL_SLOTS,
    );
}

#[inline]
pub(crate) fn note_process_exit() {
    PROCESS_EXITS.fetch_add(1, Ordering::Relaxed);
}

#[inline]
pub(crate) fn note_clone(
    is_vfork: bool,
    share_vm: bool,
    elapsed_us: usize,
    areas: usize,
    resident_pages: usize,
) {
    CLONE_CALLS.fetch_add(1, Ordering::Relaxed);
    if is_vfork {
        CLONE_VFORK_CALLS.fetch_add(1, Ordering::Relaxed);
    }
    if share_vm {
        CLONE_SHARE_VM_CALLS.fetch_add(1, Ordering::Relaxed);
        return;
    }
    CLONE_FORK_COW_CALLS.fetch_add(1, Ordering::Relaxed);
    CLONE_FORK_COW_TOTAL_US.fetch_add(elapsed_us, Ordering::Relaxed);
    CLONE_FORK_COW_MAX_US.fetch_max(elapsed_us, Ordering::Relaxed);
    CLONE_FORK_COW_AREAS.fetch_add(areas, Ordering::Relaxed);
    CLONE_FORK_COW_RESIDENT_PAGES.fetch_add(resident_pages, Ordering::Relaxed);
    if is_vfork {
        CLONE_VFORK_FORK_COW_CALLS.fetch_add(1, Ordering::Relaxed);
        CLONE_VFORK_FORK_COW_TOTAL_US.fetch_add(elapsed_us, Ordering::Relaxed);
    }
}

#[inline]
pub(crate) fn note_child_wake(woken: usize) {
    CHILD_WAKE_CALLS.fetch_add(1, Ordering::Relaxed);
    CHILD_WOKEN.fetch_add(woken, Ordering::Relaxed);
}

#[inline]
fn lock_with_memory_set_site<'a, T>(
    class: LockClass,
    memory_set_site: Option<MemorySetLockSite>,
    heap_layout: Option<(usize, bool)>,
    mutex: &'a Mutex<T>,
) -> TimedLockGuard<'a, T> {
    let slot = class as usize;
    let cpu = cpu_slot();
    let hot_slot = match class {
        LockClass::Heap => Some(0),
        LockClass::MemorySet => Some(1),
        LockClass::MemorySetActivation => Some(2),
        LockClass::TaskManager => Some(3),
        LockClass::FrameAllocator => Some(4),
        LockClass::BlockCache => Some(5),
        _ => None,
    };
    let sampled = if let Some(hot_slot) = hot_slot {
        HOT_LOCK_ACQUIRES[hot_slot][cpu].fetch_add(1, Ordering::Relaxed) & HOT_LOCK_SAMPLE_MASK == 0
    } else {
        LOCK_ACQUIRES[slot].fetch_add(1, Ordering::Relaxed);
        true
    };
    if let Some(site) = memory_set_site {
        MEMORY_SET_SITE_ACQUIRES_PER_CPU[site as usize][cpu].fetch_add(1, Ordering::Relaxed);
    }
    if sampled {
        if let Some((size, allocation)) = heap_layout {
            let bucket = heap_size_bucket(size);
            let (counts, bytes) = if allocation {
                (&HEAP_ALLOC_SAMPLES, &HEAP_ALLOC_SAMPLE_BYTES)
            } else {
                (&HEAP_FREE_SAMPLES, &HEAP_FREE_SAMPLE_BYTES)
            };
            counts[bucket][cpu].fetch_add(1, Ordering::Relaxed);
            bytes[bucket][cpu].fetch_add(size, Ordering::Relaxed);
        }
    }

    let mutex_addr = mutex as *const Mutex<T> as usize;
    let started = sampled.then(now_us).unwrap_or(0);
    let (guard, contended) = if sampled {
        match mutex.try_lock() {
            Some(guard) => (guard, false),
            None => {
                if let Some(waiter_site) = memory_set_site {
                    note_memory_set_owner_waiter_sample(mutex_addr, waiter_site);
                }
                (mutex.lock(), true)
            }
        }
    } else {
        (mutex.lock(), false)
    };
    let acquired = sampled.then(now_us).unwrap_or(0);
    if let Some(hot_slot) = hot_slot {
        if sampled {
            HOT_LOCK_SAMPLES[hot_slot].fetch_add(1, Ordering::Relaxed);
        }
    }
    if contended {
        let waited = acquired.saturating_sub(started);
        LOCK_CONTENDED[slot].fetch_add(1, Ordering::Relaxed);
        LOCK_WAIT_TOTAL_US[slot].fetch_add(waited, Ordering::Relaxed);
        LOCK_WAIT_MAX_US[slot].fetch_max(waited, Ordering::Relaxed);
        if let Some(site) = memory_set_site {
            let site = site as usize;
            MEMORY_SET_LOCK_SITE_CONTENDED[site].fetch_add(1, Ordering::Relaxed);
            MEMORY_SET_LOCK_SITE_WAIT_TOTAL_US[site].fetch_add(waited, Ordering::Relaxed);
            MEMORY_SET_LOCK_SITE_WAIT_MAX_US[site].fetch_max(waited, Ordering::Relaxed);
        }
    }
    let owner_cpu = memory_set_site.map(|site| {
        MEMORY_SET_OWNER_SITE[cpu].store(site as usize + 1, Ordering::Relaxed);
        MEMORY_SET_OWNER_MUTEX[cpu].store(mutex_addr, Ordering::Release);
        cpu
    });
    TimedLockGuard {
        guard: Some(guard),
        class,
        memory_set_site,
        acquired_at: acquired,
        sampled,
        owner_cpu,
    }
}

#[inline]
fn note_memory_set_owner_waiter_sample(mutex_addr: usize, waiter_site: MemorySetLockSite) {
    let mut owner = MEMORY_SET_OWNER_UNKNOWN;
    for cpu in 0..CPU_SLOTS {
        if MEMORY_SET_OWNER_MUTEX[cpu].load(Ordering::Acquire) == mutex_addr {
            let encoded = MEMORY_SET_OWNER_SITE[cpu].load(Ordering::Relaxed);
            if encoded != 0 {
                owner = (encoded - 1).min(MEMORY_SET_OWNER_UNKNOWN);
            }
            break;
        }
    }
    MEMORY_SET_OWNER_WAITER_SAMPLES[waiter_site as usize][owner].fetch_add(1, Ordering::Relaxed);
}

#[inline]
fn hot_lock_slot(class: usize) -> Option<usize> {
    match class {
        value if value == LockClass::Heap as usize => Some(0),
        value if value == LockClass::MemorySet as usize => Some(1),
        value if value == LockClass::MemorySetActivation as usize => Some(2),
        value if value == LockClass::TaskManager as usize => Some(3),
        value if value == LockClass::FrameAllocator as usize => Some(4),
        value if value == LockClass::BlockCache as usize => Some(5),
        _ => None,
    }
}

fn lock_acquire_count(slot: usize) -> usize {
    if let Some(hot) = hot_lock_slot(slot) {
        HOT_LOCK_ACQUIRES[hot]
            .iter()
            .map(|count| count.load(Ordering::Relaxed))
            .sum()
    } else {
        LOCK_ACQUIRES[slot].load(Ordering::Relaxed)
    }
}

fn memory_set_site_acquire_count(site: usize) -> usize {
    MEMORY_SET_SITE_ACQUIRES_PER_CPU[site]
        .iter()
        .map(|count| count.load(Ordering::Relaxed))
        .sum()
}

#[inline]
fn heap_size_bucket(size: usize) -> usize {
    match size.max(1).next_power_of_two().trailing_zeros() as usize {
        0..=4 => 0,
        5 => 1,
        6 => 2,
        7 => 3,
        8 => 4,
        9 => 5,
        10 => 6,
        11 => 7,
        12 => 8,
        13..=16 => 9,
        _ => 10,
    }
}

#[inline]
pub(crate) fn note_lock_acquire(class: LockClass, contended: bool, wait_us: usize) {
    let slot = class as usize;
    LOCK_ACQUIRES[slot].fetch_add(1, Ordering::Relaxed);
    if contended {
        LOCK_CONTENDED[slot].fetch_add(1, Ordering::Relaxed);
        LOCK_WAIT_TOTAL_US[slot].fetch_add(wait_us, Ordering::Relaxed);
        LOCK_WAIT_MAX_US[slot].fetch_max(wait_us, Ordering::Relaxed);
    }
}

#[inline]
pub(crate) fn note_lock_hold(class: LockClass, hold_us: usize) {
    let slot = class as usize;
    LOCK_HOLD_TOTAL_US[slot].fetch_add(hold_us, Ordering::Relaxed);
    LOCK_HOLD_MAX_US[slot].fetch_max(hold_us, Ordering::Relaxed);
}

#[inline]
pub(crate) fn note_virtio_request(bytes: usize, queue_us: usize, complete_us: usize) {
    VIRTIO_REQUESTS.fetch_add(1, Ordering::Relaxed);
    VIRTIO_BYTES.fetch_add(bytes, Ordering::Relaxed);
    VIRTIO_QUEUE_US.fetch_add(queue_us, Ordering::Relaxed);
    VIRTIO_COMPLETE_US.fetch_add(complete_us, Ordering::Relaxed);
}

// Phase hooks are aggregate-only and remain feature-gated by every callsite.
// Out-of-range IDs are ignored so diagnostic coverage cannot change the
// production path's error behavior.
#[inline]
pub(crate) fn note_phase(phase: usize, elapsed_us: usize) {
    if phase >= PHASE_SLOTS {
        return;
    }
    PHASE_COUNTS[phase].fetch_add(1, Ordering::Relaxed);
    PHASE_TOTAL_US[phase].fetch_add(elapsed_us, Ordering::Relaxed);
    PHASE_MAX_US[phase].fetch_max(elapsed_us, Ordering::Relaxed);
}
#[inline]
pub(crate) fn note_ppoll_call(_nfds: usize) {
    PPOLL_CALLS.fetch_add(1, Ordering::Relaxed);
}
#[inline]
pub(crate) fn note_ppoll_sleep(keyed: bool) {
    if keyed {
        PPOLL_KEYED_SLEEPS.fetch_add(1, Ordering::Relaxed);
    } else {
        PPOLL_UNKEYED_SLEEPS.fetch_add(1, Ordering::Relaxed);
    }
}

#[inline]
pub(crate) fn note_pipe2(flags: usize, nonblock_flag: usize, cloexec_flag: usize) {
    PIPE2_CALLS.fetch_add(1, Ordering::Relaxed);
    if flags & nonblock_flag != 0 {
        PIPE2_NONBLOCK.fetch_add(1, Ordering::Relaxed);
    }
    if flags & cloexec_flag != 0 {
        PIPE2_CLOEXEC.fetch_add(1, Ordering::Relaxed);
    }
}

#[inline]
pub(crate) fn note_pipe_read(result: Result<usize, crate::utils::error::SysErrNo>) {
    use crate::utils::error::SysErrNo;
    PIPE_READ_CALLS.fetch_add(1, Ordering::Relaxed);
    match result {
        Ok(0) => {
            PIPE_READ_EOF.fetch_add(1, Ordering::Relaxed);
        }
        Ok(bytes) => {
            PIPE_READ_BYTES.fetch_add(bytes, Ordering::Relaxed);
        }
        Err(SysErrNo::EAGAIN) => {
            PIPE_READ_EAGAIN.fetch_add(1, Ordering::Relaxed);
        }
        Err(_) => {}
    }
}

#[inline]
pub(crate) fn note_pipe_write(result: Result<usize, crate::utils::error::SysErrNo>) {
    use crate::utils::error::SysErrNo;
    PIPE_WRITE_CALLS.fetch_add(1, Ordering::Relaxed);
    match result {
        Ok(bytes) => {
            PIPE_WRITE_BYTES.fetch_add(bytes, Ordering::Relaxed);
        }
        Err(SysErrNo::EAGAIN) => {
            PIPE_WRITE_EAGAIN.fetch_add(1, Ordering::Relaxed);
        }
        Err(SysErrNo::EPIPE) => {
            PIPE_WRITE_EPIPE.fetch_add(1, Ordering::Relaxed);
        }
        Err(_) => {}
    }
}

#[inline]
pub(crate) fn note_pipe_wait(read: bool) {
    if read {
        PIPE_READ_WAITS.fetch_add(1, Ordering::Relaxed);
    } else {
        PIPE_WRITE_WAITS.fetch_add(1, Ordering::Relaxed);
    }
}

#[inline]
pub(crate) fn note_pipe_wake(readers: bool, woken: usize) {
    if readers {
        PIPE_READER_WAKE_CALLS.fetch_add(1, Ordering::Relaxed);
        PIPE_READER_WOKEN.fetch_add(woken, Ordering::Relaxed);
    } else {
        PIPE_WRITER_WAKE_CALLS.fetch_add(1, Ordering::Relaxed);
        PIPE_WRITER_WOKEN.fetch_add(woken, Ordering::Relaxed);
    }
}

#[inline]
pub(crate) fn note_dup(pipe: bool) {
    DUP_CALLS.fetch_add(1, Ordering::Relaxed);
    if pipe {
        DUP_PIPE_CALLS.fetch_add(1, Ordering::Relaxed);
    }
}

#[inline]
pub(crate) fn note_fcntl(
    cmd: usize,
    pipe: bool,
    getfd: usize,
    setfd: usize,
    getfl: usize,
    setfl: usize,
) {
    if cmd == getfd {
        FCNTL_GETFD.fetch_add(1, Ordering::Relaxed);
    } else if cmd == setfd {
        FCNTL_SETFD.fetch_add(1, Ordering::Relaxed);
    } else if cmd == getfl {
        FCNTL_GETFL.fetch_add(1, Ordering::Relaxed);
        if pipe {
            FCNTL_PIPE_GETFL.fetch_add(1, Ordering::Relaxed);
        }
    } else if cmd == setfl {
        FCNTL_SETFL.fetch_add(1, Ordering::Relaxed);
        if pipe {
            FCNTL_PIPE_SETFL.fetch_add(1, Ordering::Relaxed);
        }
    }
}

#[inline]
pub(crate) fn note_pipe_aliases(aliases: usize, mismatches: usize, external_aliases: usize) {
    PIPE_ALIAS_OBSERVATIONS.fetch_add(aliases, Ordering::Relaxed);
    PIPE_ALIAS_MISMATCHES.fetch_add(mismatches, Ordering::Relaxed);
    PIPE_EXTERNAL_ALIASES_AT_SETFL.fetch_add(external_aliases, Ordering::Relaxed);
}

#[inline]
pub(crate) fn note_exec_pipes(endpoints: usize, cloexec: usize, closed: usize) {
    EXEC_CALLS.fetch_add(1, Ordering::Relaxed);
    EXEC_PIPE_ENDPOINTS.fetch_add(endpoints, Ordering::Relaxed);
    EXEC_PIPE_CLOEXEC.fetch_add(cloexec, Ordering::Relaxed);
    EXEC_PIPE_CLOSED.fetch_add(closed, Ordering::Relaxed);
}

#[inline]
pub(crate) fn note_sched_getaffinity(mask: usize) {
    let bits = mask.count_ones() as usize;
    SCHED_GETAFFINITY_CALLS.fetch_add(1, Ordering::Relaxed);
    SCHED_GETAFFINITY_MASK_BITS.fetch_add(bits, Ordering::Relaxed);
    SCHED_GETAFFINITY_MAX_BITS.fetch_max(bits, Ordering::Relaxed);
}

pub(crate) struct SyscallScope {
    syscall_id: usize,
    started_at: Option<usize>,
}

impl Drop for SyscallScope {
    fn drop(&mut self) {
        if let Some(started_at) = self.started_at {
            let elapsed = now_us().saturating_sub(started_at);
            SYSCALL_SAMPLES[self.syscall_id].fetch_add(1, Ordering::Relaxed);
            SYSCALL_TOTAL_US[self.syscall_id].fetch_add(elapsed, Ordering::Relaxed);
            SYSCALL_MAX_US[self.syscall_id].fetch_max(elapsed, Ordering::Relaxed);
        }
    }
}

#[inline]
pub(crate) fn note_syscall_enter(syscall_id: usize) -> Option<SyscallScope> {
    if syscall_id >= SYSCALL_SLOTS {
        return None;
    }
    let cpu = cpu_slot();
    let sequence = SYSCALL_COUNTS_PER_CPU[cpu][syscall_id].fetch_add(1, Ordering::Relaxed);
    Some(SyscallScope {
        syscall_id,
        started_at: ((sequence & SYSCALL_SAMPLE_MASK) == 0).then(now_us),
    })
}
#[inline]
pub(crate) fn note_active_syscall_enter(syscall_id: usize, sepc: usize) {
    let cpu = cpu_slot();
    ACTIVE_SYSCALL_ID[cpu].store(syscall_id, Ordering::Release);
    ACTIVE_SYSCALL_SEPC[cpu].store(sepc, Ordering::Relaxed);
    ACTIVE_SYSCALL_SINCE_US[cpu].store(now_us(), Ordering::Relaxed);
    ACTIVE_SYSCALL_ENTRIES[cpu].fetch_add(1, Ordering::Relaxed);
}

#[inline]
pub(crate) fn note_syscall_exit(_syscall_id: usize) {
    let cpu = cpu_slot();
    ACTIVE_SYSCALL_ID[cpu].store(usize::MAX, Ordering::Release);
    ACTIVE_SYSCALL_LAST_EXIT_US[cpu].store(now_us(), Ordering::Relaxed);
}

pub(crate) fn maybe_report() {
    let now = now_us();
    let next = NEXT_REPORT_AT_US.load(Ordering::Acquire);
    if next == 0 {
        let _ = NEXT_REPORT_AT_US.compare_exchange(
            0,
            now.saturating_add(REPORT_INTERVAL_US),
            Ordering::AcqRel,
            Ordering::Acquire,
        );
        return;
    }
    if now < next
        || NEXT_REPORT_AT_US
            .compare_exchange(
                next,
                now.saturating_add(REPORT_INTERVAL_US),
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_err()
    {
        return;
    }

    let sequence = REPORT_SEQUENCE.fetch_add(1, Ordering::Relaxed) + 1;
    let task_counts = crate::task::manager::diagnostic_task_counts();
    crate::println!("BUILDSTORM_DIAG snapshot={} now_us={} cpu={} user_tasks_live={} user_tasks_runnable={} user_tasks_blocked={} rustc_tasks_live={} rustc_tasks_runnable={} user_processes_live={} user_processes_runnable={} user_processes_blocked={} rustc_processes_live={} rustc_processes_runnable={} rq_current={}", sequence, now, crate::platform::current_cpu_index(), task_counts.0, task_counts.1, task_counts.2, task_counts.3, task_counts.4, task_counts.5, task_counts.6, task_counts.7, task_counts.8, task_counts.9, crate::task::manager::queue_len());
    if EXEC_FOCUS_ONLY {
        let mut active = false;
        let mut stuck = false;
        for cpu in 0..CPU_SLOTS {
            let stage = ACTIVE_EXEC_STAGE[cpu].load(Ordering::Acquire);
            if stage == 0 {
                continue;
            }
            active = true;
            let syscall_id = ACTIVE_SYSCALL_ID[cpu].load(Ordering::Acquire);
            let started_at = ACTIVE_EXEC_STARTED_AT_US[cpu].load(Ordering::Relaxed);
            let stage_since = ACTIVE_EXEC_STAGE_SINCE_US[cpu].load(Ordering::Relaxed);
            let total_elapsed = now.saturating_sub(started_at);
            stuck |= total_elapsed >= 5_000_000;
            crate::println!(
                "BUILDSTORM_DIAG syscall_active cpu={} id={} sepc={:#x} since_us={} elapsed_us={} entries={}",
                cpu,
                syscall_id,
                ACTIVE_SYSCALL_SEPC[cpu].load(Ordering::Relaxed),
                ACTIVE_SYSCALL_SINCE_US[cpu].load(Ordering::Relaxed),
                if syscall_id == usize::MAX {
                    0
                } else {
                    now.saturating_sub(ACTIVE_SYSCALL_SINCE_US[cpu].load(Ordering::Relaxed))
                },
                ACTIVE_SYSCALL_ENTRIES[cpu].load(Ordering::Relaxed),
            );
            crate::println!(
                "BUILDSTORM_DIAG exec_active cpu={} pid={} stage={} name={} path_hash={:#x} path_len={} total_elapsed_us={} stage_elapsed_us={} entries={}",
                cpu,
                ACTIVE_EXEC_PID[cpu].load(Ordering::Relaxed),
                stage,
                exec_stage_name(stage),
                ACTIVE_EXEC_PATH_HASH[cpu].load(Ordering::Relaxed),
                ACTIVE_EXEC_PATH_LEN[cpu].load(Ordering::Relaxed),
                total_elapsed,
                now.saturating_sub(stage_since),
                ACTIVE_EXEC_ENTRIES[cpu].load(Ordering::Relaxed),
            );
            let elf_phase = ACTIVE_ELF_LOAD_PHASE[cpu].load(Ordering::Acquire);
            if stage == EXEC_STAGE_MEMORY_BUILD && elf_phase != 0 {
                let phase_since = ACTIVE_ELF_LOAD_PHASE_SINCE_US[cpu].load(Ordering::Relaxed);
                let role = ACTIVE_ELF_LOAD_ROLE[cpu].load(Ordering::Relaxed);
                crate::println!(
                    "BUILDSTORM_DIAG elf_load_active cpu={} role={} phase={} name={} segment={} p_vaddr={:#x} p_filesz={} p_memsz={} bias={:#x} phase_elapsed_us={}",
                    cpu,
                    elf_load_role_name(role),
                    elf_phase,
                    elf_load_phase_name(elf_phase),
                    ACTIVE_ELF_LOAD_SEGMENT[cpu].load(Ordering::Relaxed),
                    ACTIVE_ELF_LOAD_VADDR[cpu].load(Ordering::Relaxed),
                    ACTIVE_ELF_LOAD_FILESZ[cpu].load(Ordering::Relaxed),
                    ACTIVE_ELF_LOAD_MEMSZ[cpu].load(Ordering::Relaxed),
                    ACTIVE_ELF_LOAD_BIAS[cpu].load(Ordering::Relaxed),
                    now.saturating_sub(phase_since),
                );
                if elf_phase == ELF_LOAD_PHASE_ADDRESS_SPACE {
                    let create_phase =
                        ACTIVE_ADDRESS_SPACE_CREATE_PHASE[cpu].load(Ordering::Acquire);
                    let create_since =
                        ACTIVE_ADDRESS_SPACE_CREATE_SINCE_US[cpu].load(Ordering::Relaxed);
                    crate::println!(
                        "BUILDSTORM_DIAG address_space_create cpu={} phase={} name={} elapsed_us={}",
                        cpu,
                        create_phase,
                        address_space_create_phase_name(create_phase),
                        if create_phase == ADDRESS_SPACE_CREATE_IDLE {
                            0
                        } else {
                            now.saturating_sub(create_since)
                        },
                    );
                }
            }
        }
        crate::println!(
            "BUILDSTORM_DIAG asid_allocator allocations={} reuses={} global_recycles={} zero_fallbacks={} last={} max={} next={} reusable={} retired={}",
            ASID_ALLOCATIONS.load(Ordering::Relaxed),
            ASID_REUSES.load(Ordering::Relaxed),
            ASID_GLOBAL_RECYCLES.load(Ordering::Relaxed),
            ASID_ZERO_FALLBACKS.load(Ordering::Relaxed),
            ASID_LAST_ALLOCATED.load(Ordering::Relaxed),
            ASID_MAX.load(Ordering::Relaxed),
            ASID_NEXT.load(Ordering::Relaxed),
            ASID_REUSABLE.load(Ordering::Relaxed),
            ASID_RETIRED.load(Ordering::Relaxed),
        );
        if !active {
            let entries: usize = ACTIVE_EXEC_ENTRIES
                .iter()
                .map(|value| value.load(Ordering::Relaxed))
                .sum();
            crate::println!("BUILDSTORM_DIAG exec_active state=idle entries={}", entries);
        }
        if stuck {
            crate::task::manager::diagnostic_dump_user_comm();
            crate::syscall::other::diagnostic_dump_futex_waiters();
        }
        // After the compile marker the hidden wrapper may leave only a
        // parent and child task blocked in wait4/IO while no exec is active.
        // Dump the ownership edges at a low rate so the first post-boot
        // deadlock is attributable without flooding the serial log.
        if !active && task_counts.1 == 0 && task_counts.2 != 0 {
            let should_dump = sequence <= 3 || sequence % 6 == 0;
            if should_dump {
                crate::println!(
                    "BUILDSTORM_DIAG idle_blocked user_tasks_blocked={} rustc_tasks_blocked={} sequence={}",
                    task_counts.2,
                    task_counts.3.saturating_sub(task_counts.4),
                    sequence,
                );
                crate::task::manager::diagnostic_dump_user_comm();
                crate::task::wait_queue::diagnostic_dump_waiters();
                crate::syscall::other::diagnostic_dump_futex_waiters();
            }
        }
        crate::println!("BUILDSTORM_DIAG snapshot_end={}", sequence);
        return;
    }
    crate::mm::heap_allocator::report_cache_diagnostics();
    for cpu in 0..CPU_SLOTS {
        crate::println!("BUILDSTORM_DIAG cpu={} user_ticks={} kernel_ticks={} idle_ticks={} context_switches={} migrations={} runqueue_max={}", cpu, USER_TICKS[cpu].load(Ordering::Relaxed), KERNEL_TICKS[cpu].load(Ordering::Relaxed), IDLE_TICKS[cpu].load(Ordering::Relaxed), CONTEXT_SWITCHES[cpu].load(Ordering::Relaxed), TASK_MIGRATIONS[cpu].load(Ordering::Relaxed), RUNQUEUE_MAX[cpu].load(Ordering::Relaxed));
    }
    for cpu in 0..CPU_SLOTS {
        crate::println!("BUILDSTORM_DIAG user_run cpu={} count={} total_us={} max_us={} syscall={} timer={} irq={} other={}", cpu, USER_RUN_COUNT[cpu].load(Ordering::Relaxed), USER_RUN_TOTAL_US[cpu].load(Ordering::Relaxed), USER_RUN_MAX_US[cpu].load(Ordering::Relaxed), USER_RUN_SYSCALL[cpu].load(Ordering::Relaxed), USER_RUN_TIMER[cpu].load(Ordering::Relaxed), USER_RUN_IRQ[cpu].load(Ordering::Relaxed), USER_RUN_OTHER[cpu].load(Ordering::Relaxed));
    }
    for syscall_id in 0..SYSCALL_SLOTS {
        let count: usize = SYSCALL_COUNTS_PER_CPU
            .iter()
            .map(|cpu| cpu[syscall_id].load(Ordering::Relaxed))
            .sum();
        if count != 0 {
            crate::println!(
                "BUILDSTORM_DIAG syscall={} count={} total_us={} max_us={} sample_count={} sample_shift={}",
                syscall_id,
                count,
                SYSCALL_TOTAL_US[syscall_id].load(Ordering::Relaxed),
                SYSCALL_MAX_US[syscall_id].load(Ordering::Relaxed),
                SYSCALL_SAMPLES[syscall_id].load(Ordering::Relaxed),
                SYSCALL_SAMPLE_SHIFT,
            );
        }
    }
    for kind in 1..USER_TRAP_SLOTS {
        let count: usize = USER_TRAP_COUNTS_PER_CPU
            .iter()
            .map(|cpu| cpu[kind].load(Ordering::Relaxed))
            .sum();
        if count != 0 {
            crate::println!(
                "BUILDSTORM_DIAG user_trap={} count={} total_us={} max_us={} sample_count={} sample_shift={}",
                USER_TRAP_NAMES[kind],
                count,
                USER_TRAP_TOTAL_US[kind].load(Ordering::Relaxed),
                USER_TRAP_MAX_US[kind].load(Ordering::Relaxed),
                USER_TRAP_SAMPLES[kind].load(Ordering::Relaxed),
                SYSCALL_SAMPLE_SHIFT,
            );
        }
    }
    for cpu in 0..CPU_SLOTS {
        let pid = ACTIVE_USER_PID[cpu].load(Ordering::Acquire);
        let since_us = ACTIVE_USER_SINCE_US[cpu].load(Ordering::Relaxed);
        let trap_kind = ACTIVE_USER_TRAP_KIND[cpu].load(Ordering::Acquire);
        let trap_since_us = ACTIVE_USER_TRAP_SINCE_US[cpu].load(Ordering::Relaxed);
        crate::println!("BUILDSTORM_DIAG user_active cpu={} pid={} since_us={} elapsed_us={} last_exit_us={} entries={} trap_kind={} trap_since_us={} trap_elapsed_us={} trap_entries={}", cpu, pid, since_us, if pid == 0 { 0 } else { now.saturating_sub(since_us) }, ACTIVE_USER_LAST_EXIT_US[cpu].load(Ordering::Relaxed), ACTIVE_USER_ENTRIES[cpu].load(Ordering::Relaxed), trap_kind, trap_since_us, if trap_kind == 0 { 0 } else { now.saturating_sub(trap_since_us) }, ACTIVE_USER_TRAP_ENTRIES[cpu].load(Ordering::Relaxed));
    }
    for cpu in 0..CPU_SLOTS {
        let syscall_id = ACTIVE_SYSCALL_ID[cpu].load(Ordering::Acquire);
        crate::println!(
            "BUILDSTORM_DIAG syscall_active cpu={} id={} sepc={:#x} since_us={} elapsed_us={} last_exit_us={} entries={}",
            cpu,
            syscall_id,
            ACTIVE_SYSCALL_SEPC[cpu].load(Ordering::Relaxed),
            ACTIVE_SYSCALL_SINCE_US[cpu].load(Ordering::Relaxed),
            if syscall_id == usize::MAX {
                0
            } else {
                now.saturating_sub(ACTIVE_SYSCALL_SINCE_US[cpu].load(Ordering::Relaxed))
            },
            ACTIVE_SYSCALL_LAST_EXIT_US[cpu].load(Ordering::Relaxed),
            ACTIVE_SYSCALL_ENTRIES[cpu].load(Ordering::Relaxed),
        );
    }
    for cpu in 0..CPU_SLOTS {
        let stage = ACTIVE_EXEC_STAGE[cpu].load(Ordering::Acquire);
        let started_at = ACTIVE_EXEC_STARTED_AT_US[cpu].load(Ordering::Relaxed);
        let stage_since = ACTIVE_EXEC_STAGE_SINCE_US[cpu].load(Ordering::Relaxed);
        crate::println!(
            "BUILDSTORM_DIAG exec_active cpu={} pid={} stage={} name={} path_hash={:#x} path_len={} started_at_us={} total_elapsed_us={} stage_since_us={} stage_elapsed_us={} last_exit_us={} entries={}",
            cpu,
            ACTIVE_EXEC_PID[cpu].load(Ordering::Relaxed),
            stage,
            exec_stage_name(stage),
            ACTIVE_EXEC_PATH_HASH[cpu].load(Ordering::Relaxed),
            ACTIVE_EXEC_PATH_LEN[cpu].load(Ordering::Relaxed),
            started_at,
            if stage == 0 {
                0
            } else {
                now.saturating_sub(started_at)
            },
            stage_since,
            if stage == 0 {
                0
            } else {
                now.saturating_sub(stage_since)
            },
            ACTIVE_EXEC_LAST_EXIT_US[cpu].load(Ordering::Relaxed),
            ACTIVE_EXEC_ENTRIES[cpu].load(Ordering::Relaxed),
        );
    }
    for cpu in 0..CPU_SLOTS {
        crate::println!("BUILDSTORM_DIAG blocked_owner cpu={} current={} max={} enters={} exits={} loop_dispatches={} timed_loops={} timed_total_us={} timed_max_us={} empty_iterations={} wake_to_owner={} wake_to_global={}", cpu, BLOCKED_OWNER_CURRENT[cpu].load(Ordering::Relaxed), BLOCKED_OWNER_MAX[cpu].load(Ordering::Relaxed), BLOCKED_OWNER_ENTERS[cpu].load(Ordering::Relaxed), BLOCKED_OWNER_EXITS[cpu].load(Ordering::Relaxed), BLOCKED_OWNER_LOOP_DISPATCHES[cpu].load(Ordering::Relaxed), BLOCKED_OWNER_TIMED_LOOPS[cpu].load(Ordering::Relaxed), BLOCKED_OWNER_TIMED_TOTAL_US[cpu].load(Ordering::Relaxed), BLOCKED_OWNER_TIMED_MAX_US[cpu].load(Ordering::Relaxed), BLOCKED_OWNER_EMPTY_ITERATIONS[cpu].load(Ordering::Relaxed), BLOCKED_OWNER_WAKES[cpu].load(Ordering::Relaxed), BLOCKED_GLOBAL_WAKES.load(Ordering::Relaxed));
    }
    crate::println!("BUILDSTORM_DIAG mm user_root={} kernel_root={} redundant_root={} page_table_writes={} local_flush={} remote_shootdown={} remote_targets={}", USER_ROOT_ACTIVATIONS.load(Ordering::Relaxed), KERNEL_ROOT_ACTIVATIONS.load(Ordering::Relaxed), REDUNDANT_ROOT_ACTIVATIONS.load(Ordering::Relaxed), PAGE_TABLE_WRITES.load(Ordering::Relaxed), LOCAL_TLB_FLUSHES.load(Ordering::Relaxed), REMOTE_SHOOTDOWNS.load(Ordering::Relaxed), REMOTE_SHOOTDOWN_TARGETS.load(Ordering::Relaxed));
    crate::println!(
        "BUILDSTORM_DIAG page_table_batch commits={} writes={} max={} local_flushes={}",
        PAGE_TABLE_BATCHES.load(Ordering::Relaxed),
        PAGE_TABLE_BATCH_WRITES.load(Ordering::Relaxed),
        PAGE_TABLE_BATCH_MAX.load(Ordering::Relaxed),
        PAGE_TABLE_BATCH_LOCAL_FLUSHES.load(Ordering::Relaxed)
    );
    let (tracked_frames, page_table_frames, contiguous_frames, owner_transitions) =
        crate::mm::frame_allocator::diagnostic_ownership_snapshot();
    crate::println!(
        "BUILDSTORM_DIAG frame_ownership tracked={} page_table={} contiguous={} transitions={}",
        tracked_frames,
        page_table_frames,
        contiguous_frames,
        owner_transitions
    );
    crate::mm::frame_allocator::report_cache_diagnostics();
    crate::println!("BUILDSTORM_DIAG anonymous_vma installs={} left={} right={} both={} neither={} current={} max={} pages={} pages_max={}", ANONYMOUS_VMA_INSTALLS.load(Ordering::Relaxed), ANONYMOUS_VMA_LEFT_MERGEABLE.load(Ordering::Relaxed), ANONYMOUS_VMA_RIGHT_MERGEABLE.load(Ordering::Relaxed), ANONYMOUS_VMA_BOTH_MERGEABLE.load(Ordering::Relaxed), ANONYMOUS_VMA_NEITHER_MERGEABLE.load(Ordering::Relaxed), ANONYMOUS_VMA_CURRENT.load(Ordering::Relaxed), ANONYMOUS_VMA_MAX.load(Ordering::Relaxed), ANONYMOUS_VMA_PAGES.load(Ordering::Relaxed), ANONYMOUS_VMA_PAGES_MAX.load(Ordering::Relaxed));
    crate::println!("BUILDSTORM_DIAG anonymous_fault_shape calls={} samples={} left_resident={} right_resident={} both_resident={} isolated={} forward_pages={} backward_pages={} forward_max={} backward_max={} sample_shift={}", ANONYMOUS_FAULT_SHAPE_CALLS.load(Ordering::Relaxed), ANONYMOUS_FAULT_SHAPE_SAMPLES.load(Ordering::Relaxed), ANONYMOUS_FAULT_LEFT_RESIDENT.load(Ordering::Relaxed), ANONYMOUS_FAULT_RIGHT_RESIDENT.load(Ordering::Relaxed), ANONYMOUS_FAULT_BOTH_RESIDENT.load(Ordering::Relaxed), ANONYMOUS_FAULT_ISOLATED.load(Ordering::Relaxed), ANONYMOUS_FAULT_FORWARD_PAGES.load(Ordering::Relaxed), ANONYMOUS_FAULT_BACKWARD_PAGES.load(Ordering::Relaxed), ANONYMOUS_FAULT_FORWARD_MAX.load(Ordering::Relaxed), ANONYMOUS_FAULT_BACKWARD_MAX.load(Ordering::Relaxed), WORK_SAMPLE_SHIFT);
    let clean_cache = crate::fs::ext4_vol::diagnostic_clean_page_cache_stats();
    crate::println!("BUILDSTORM_DIAG clean_page_cache current={} max={} folios={} folio_touches={} run_calls={} run_pages={} run_max={} prefetch_calls={} prefetch_pages={} inserts={} evictions={} invalidations={}", clean_cache.0, clean_cache.1, clean_cache.2, clean_cache.3, clean_cache.4, clean_cache.5, clean_cache.6, clean_cache.7, clean_cache.8, clean_cache.9, clean_cache.10, clean_cache.11);
    crate::println!("BUILDSTORM_DIAG anonymous_move left_selected={} left_no_realloc={} left_shrink={} left_remove={} left_frame_relocate={} left_remove_suffix={} left_remove_suffix_max={} right_selected={} right_no_realloc={} right_shrink={} right_remove={} right_frame_move={} right_remove_suffix={} right_remove_suffix_max={} zero_metadata={}", ANONYMOUS_MOVE_LEFT_SELECTED.load(Ordering::Relaxed), ANONYMOUS_MOVE_LEFT_NO_REALLOC.load(Ordering::Relaxed), ANONYMOUS_MOVE_LEFT_SHRINK.load(Ordering::Relaxed), ANONYMOUS_MOVE_LEFT_REMOVE.load(Ordering::Relaxed), ANONYMOUS_MOVE_LEFT_FRAME_RELOCATE.load(Ordering::Relaxed), ANONYMOUS_MOVE_LEFT_REMOVE_SUFFIX.load(Ordering::Relaxed), ANONYMOUS_MOVE_LEFT_REMOVE_SUFFIX_MAX.load(Ordering::Relaxed), ANONYMOUS_MOVE_RIGHT_SELECTED.load(Ordering::Relaxed), ANONYMOUS_MOVE_RIGHT_NO_REALLOC.load(Ordering::Relaxed), ANONYMOUS_MOVE_RIGHT_SHRINK.load(Ordering::Relaxed), ANONYMOUS_MOVE_RIGHT_REMOVE.load(Ordering::Relaxed), ANONYMOUS_MOVE_RIGHT_FRAME_MOVE.load(Ordering::Relaxed), ANONYMOUS_MOVE_RIGHT_REMOVE_SUFFIX.load(Ordering::Relaxed), ANONYMOUS_MOVE_RIGHT_REMOVE_SUFFIX_MAX.load(Ordering::Relaxed), ANONYMOUS_MOVE_ZERO_METADATA.load(Ordering::Relaxed));
    crate::println!("BUILDSTORM_DIAG anonymous_coalesce calls={} areas={} areas_max={} sort_us={} scan_us={} total_us={} merges={} merged_frames={} frame_reallocs={} frame_relocate={}", ANONYMOUS_COALESCE_CALLS.load(Ordering::Relaxed), ANONYMOUS_COALESCE_AREAS.load(Ordering::Relaxed), ANONYMOUS_COALESCE_AREAS_MAX.load(Ordering::Relaxed), ANONYMOUS_COALESCE_SORT_US.load(Ordering::Relaxed), ANONYMOUS_COALESCE_SCAN_US.load(Ordering::Relaxed), ANONYMOUS_COALESCE_TOTAL_US.load(Ordering::Relaxed), ANONYMOUS_COALESCE_MERGES.load(Ordering::Relaxed), ANONYMOUS_COALESCE_MERGED_FRAMES.load(Ordering::Relaxed), ANONYMOUS_COALESCE_FRAME_REALLOCS.load(Ordering::Relaxed), ANONYMOUS_COALESCE_FRAME_RELOCATE.load(Ordering::Relaxed));
    crate::println!(
        "BUILDSTORM_DIAG vma current={} max={}",
        LOGICAL_VMA_CURRENT.load(Ordering::Relaxed),
        LOGICAL_VMA_MAX.load(Ordering::Relaxed)
    );
    crate::println!("BUILDSTORM_DIAG cache path_components={} path_hit={} path_miss={} negative_path_hit={} dir_hit={} dir_miss={} inode_metadata_hit={} inode_metadata_miss={} inode_hit={} inode_miss={} metadata_hit={} metadata_miss={} block_hit={} block_miss={} virtio_requests={} virtio_bytes={} virtio_queue_us={} virtio_complete_us={}", PATH_COMPONENT_LOOKUPS.load(Ordering::Relaxed), PATH_CACHE_HITS.load(Ordering::Relaxed), PATH_CACHE_MISSES.load(Ordering::Relaxed), NEGATIVE_PATH_CACHE_HITS.load(Ordering::Relaxed), DIR_CACHE_HITS.load(Ordering::Relaxed), DIR_CACHE_MISSES.load(Ordering::Relaxed), INODE_METADATA_CACHE_HITS.load(Ordering::Relaxed), INODE_METADATA_CACHE_MISSES.load(Ordering::Relaxed), INODE_CACHE_HITS.load(Ordering::Relaxed), INODE_CACHE_MISSES.load(Ordering::Relaxed), METADATA_CACHE_HITS.load(Ordering::Relaxed), METADATA_CACHE_MISSES.load(Ordering::Relaxed), BLOCK_CACHE_HITS.load(Ordering::Relaxed), BLOCK_CACHE_MISSES.load(Ordering::Relaxed), VIRTIO_REQUESTS.load(Ordering::Relaxed), VIRTIO_BYTES.load(Ordering::Relaxed), VIRTIO_QUEUE_US.load(Ordering::Relaxed), VIRTIO_COMPLETE_US.load(Ordering::Relaxed));
    for slot in 0..WORK_SLOTS {
        let count: usize = WORK_COUNTS_PER_CPU[slot]
            .iter()
            .map(|value| value.load(Ordering::Relaxed))
            .sum();
        if count != 0 {
            crate::println!(
                "BUILDSTORM_DIAG work={} count={} total_us={} max_us={} sample_count={} sample_shift={}",
                WORK_NAMES[slot],
                count,
                WORK_TOTAL_US[slot].load(Ordering::Relaxed),
                WORK_MAX_US[slot].load(Ordering::Relaxed),
                WORK_SAMPLES[slot].load(Ordering::Relaxed),
                WORK_SAMPLE_SHIFT,
            );
        }
    }
    for slot in 0..PHASE_SLOTS {
        let count = PHASE_COUNTS[slot].load(Ordering::Relaxed);
        if count != 0 {
            crate::println!(
                "BUILDSTORM_DIAG phase={} count={} total_us={} max_us={}",
                PHASE_NAMES[slot],
                count,
                PHASE_TOTAL_US[slot].load(Ordering::Relaxed),
                PHASE_MAX_US[slot].load(Ordering::Relaxed)
            );
        }
    }
    for slot in 0..PAGE_FAULT_SOURCE_SLOTS {
        let count = PAGE_FAULT_SOURCE_COUNTS[slot].load(Ordering::Relaxed);
        if count != 0 {
            crate::println!(
                "BUILDSTORM_DIAG fault_source={} count={} total_us={} max_us={}",
                PAGE_FAULT_SOURCE_NAMES[slot],
                count,
                PAGE_FAULT_SOURCE_TOTAL_US[slot].load(Ordering::Relaxed),
                PAGE_FAULT_SOURCE_MAX_US[slot].load(Ordering::Relaxed)
            );
        }
    }
    for slot in 0..PAGE_FAULT_RESOLUTION_SLOTS {
        let count = PAGE_FAULT_RESOLUTION_COUNTS[slot].load(Ordering::Relaxed);
        if count != 0 {
            crate::println!(
                "BUILDSTORM_DIAG fault_resolution={} count={} pages={} total_us={} max_us={}",
                PAGE_FAULT_RESOLUTION_NAMES[slot],
                count,
                PAGE_FAULT_RESOLUTION_PAGES[slot].load(Ordering::Relaxed),
                PAGE_FAULT_RESOLUTION_TOTAL_US[slot].load(Ordering::Relaxed),
                PAGE_FAULT_RESOLUTION_MAX_US[slot].load(Ordering::Relaxed)
            );
        }
    }
    for slot in 0..BLOCK_SLOTS {
        let count = BLOCK_COUNTS[slot].load(Ordering::Relaxed);
        if count != 0 {
            crate::println!("BUILDSTORM_DIAG block={} count={} total_us={} max_us={} woken={} timed_out={} interrupted={}", BLOCK_NAMES[slot], count, BLOCK_TOTAL_US[slot].load(Ordering::Relaxed), BLOCK_MAX_US[slot].load(Ordering::Relaxed), BLOCK_WOKEN[slot].load(Ordering::Relaxed), BLOCK_TIMED_OUT[slot].load(Ordering::Relaxed), BLOCK_INTERRUPTED[slot].load(Ordering::Relaxed));
        }
    }
    for slot in 0..BLOCK_SLOTS {
        let count = WAKE_TO_RUN_COUNTS[slot].load(Ordering::Relaxed);
        if count != 0 {
            crate::println!(
                "BUILDSTORM_DIAG wake_to_run block={} count={} total_us={} max_us={}",
                BLOCK_NAMES[slot],
                count,
                WAKE_TO_RUN_TOTAL_US[slot].load(Ordering::Relaxed),
                WAKE_TO_RUN_MAX_US[slot].load(Ordering::Relaxed)
            );
        }
    }
    for actor in 0..BLOCK_ACTOR_SLOTS {
        crate::println!(
            "BUILDSTORM_DIAG wait_actor={} count={} total_us={}",
            BLOCK_ACTOR_NAMES[actor],
            BLOCK_ACTOR_COUNTS[actor].load(Ordering::Relaxed),
            BLOCK_ACTOR_TOTAL_US[actor].load(Ordering::Relaxed)
        );
    }
    crate::println!(
        "BUILDSTORM_DIAG poll ppoll_calls={} keyed_sleeps={} unkeyed_sleeps={}",
        PPOLL_CALLS.load(Ordering::Relaxed),
        PPOLL_KEYED_SLEEPS.load(Ordering::Relaxed),
        PPOLL_UNKEYED_SLEEPS.load(Ordering::Relaxed)
    );
    crate::println!("BUILDSTORM_DIAG ipc pipe2_calls={} pipe2_nonblock={} pipe2_cloexec={} pipe_read_calls={} pipe_read_bytes={} pipe_read_eagain={} pipe_read_eof={} pipe_write_calls={} pipe_write_bytes={} pipe_write_eagain={} pipe_write_epipe={} pipe_read_waits={} pipe_write_waits={} reader_wake_calls={} reader_woken={} writer_wake_calls={} writer_woken={}", PIPE2_CALLS.load(Ordering::Relaxed), PIPE2_NONBLOCK.load(Ordering::Relaxed), PIPE2_CLOEXEC.load(Ordering::Relaxed), PIPE_READ_CALLS.load(Ordering::Relaxed), PIPE_READ_BYTES.load(Ordering::Relaxed), PIPE_READ_EAGAIN.load(Ordering::Relaxed), PIPE_READ_EOF.load(Ordering::Relaxed), PIPE_WRITE_CALLS.load(Ordering::Relaxed), PIPE_WRITE_BYTES.load(Ordering::Relaxed), PIPE_WRITE_EAGAIN.load(Ordering::Relaxed), PIPE_WRITE_EPIPE.load(Ordering::Relaxed), PIPE_READ_WAITS.load(Ordering::Relaxed), PIPE_WRITE_WAITS.load(Ordering::Relaxed), PIPE_READER_WAKE_CALLS.load(Ordering::Relaxed), PIPE_READER_WOKEN.load(Ordering::Relaxed), PIPE_WRITER_WAKE_CALLS.load(Ordering::Relaxed), PIPE_WRITER_WOKEN.load(Ordering::Relaxed));
    crate::println!("BUILDSTORM_DIAG fd_lifecycle dup_calls={} dup_pipe_calls={} getfd={} setfd={} getfl={} setfl={} pipe_getfl={} pipe_setfl={} pipe_alias_observations={} pipe_alias_mismatches={} pipe_external_aliases_at_setfl={} exec_calls={} exec_pipe_endpoints={} exec_pipe_cloexec={} exec_pipe_closed={} sched_getaffinity_calls={} affinity_mask_bits_total={} affinity_mask_bits_max={}", DUP_CALLS.load(Ordering::Relaxed), DUP_PIPE_CALLS.load(Ordering::Relaxed), FCNTL_GETFD.load(Ordering::Relaxed), FCNTL_SETFD.load(Ordering::Relaxed), FCNTL_GETFL.load(Ordering::Relaxed), FCNTL_SETFL.load(Ordering::Relaxed), FCNTL_PIPE_GETFL.load(Ordering::Relaxed), FCNTL_PIPE_SETFL.load(Ordering::Relaxed), PIPE_ALIAS_OBSERVATIONS.load(Ordering::Relaxed), PIPE_ALIAS_MISMATCHES.load(Ordering::Relaxed), PIPE_EXTERNAL_ALIASES_AT_SETFL.load(Ordering::Relaxed), EXEC_CALLS.load(Ordering::Relaxed), EXEC_PIPE_ENDPOINTS.load(Ordering::Relaxed), EXEC_PIPE_CLOEXEC.load(Ordering::Relaxed), EXEC_PIPE_CLOSED.load(Ordering::Relaxed), SCHED_GETAFFINITY_CALLS.load(Ordering::Relaxed), SCHED_GETAFFINITY_MASK_BITS.load(Ordering::Relaxed), SCHED_GETAFFINITY_MAX_BITS.load(Ordering::Relaxed));
    let mut selected = [false; LOCK_SLOTS];
    for rank in 0..LOCK_SLOTS.min(10) {
        let mut best = None;
        for candidate in 0..LOCK_SLOTS {
            let wait = LOCK_WAIT_TOTAL_US[candidate].load(Ordering::Relaxed);
            if !selected[candidate]
                && best
                    .map(|current: usize| {
                        wait > LOCK_WAIT_TOTAL_US[current].load(Ordering::Relaxed)
                    })
                    .unwrap_or(true)
            {
                best = Some(candidate);
            }
        }
        let Some(slot) = best else {
            break;
        };
        selected[slot] = true;
        let hot = hot_lock_slot(slot);
        crate::println!("BUILDSTORM_DIAG lock_rank={} name={} acquire_count={} contended_count={} wait_total_us={} wait_max_us={} hold_total_us={} hold_max_us={} sample_count={} sample_shift={}", rank + 1, LOCK_NAMES[slot], lock_acquire_count(slot), LOCK_CONTENDED[slot].load(Ordering::Relaxed), LOCK_WAIT_TOTAL_US[slot].load(Ordering::Relaxed), LOCK_WAIT_MAX_US[slot].load(Ordering::Relaxed), LOCK_HOLD_TOTAL_US[slot].load(Ordering::Relaxed), LOCK_HOLD_MAX_US[slot].load(Ordering::Relaxed), hot.map(|index| HOT_LOCK_SAMPLES[index].load(Ordering::Relaxed)).unwrap_or_else(|| lock_acquire_count(slot)), hot.map(|_| HOT_LOCK_SAMPLE_SHIFT).unwrap_or(0));
    }
    for site in 0..MEMORY_SET_LOCK_SITE_SLOTS {
        let acquires = memory_set_site_acquire_count(site);
        if acquires != 0 {
            crate::println!("BUILDSTORM_DIAG memory_set_site={} acquire_count={} contended_count={} wait_total_us={} wait_max_us={} hold_total_us={} hold_max_us={} sample_shift={}", MEMORY_SET_LOCK_SITE_NAMES[site], acquires, MEMORY_SET_LOCK_SITE_CONTENDED[site].load(Ordering::Relaxed), MEMORY_SET_LOCK_SITE_WAIT_TOTAL_US[site].load(Ordering::Relaxed), MEMORY_SET_LOCK_SITE_WAIT_MAX_US[site].load(Ordering::Relaxed), MEMORY_SET_LOCK_SITE_HOLD_TOTAL_US[site].load(Ordering::Relaxed), MEMORY_SET_LOCK_SITE_HOLD_MAX_US[site].load(Ordering::Relaxed), HOT_LOCK_SAMPLE_SHIFT);
        }
    }
    for waiter in 0..MEMORY_SET_LOCK_SITE_SLOTS {
        for owner in 0..MEMORY_SET_OWNER_SLOTS {
            let samples = MEMORY_SET_OWNER_WAITER_SAMPLES[waiter][owner].load(Ordering::Relaxed);
            if samples != 0 {
                crate::println!(
                    "BUILDSTORM_DIAG memory_set_pair waiter={} owner={} samples={} sample_shift={}",
                    MEMORY_SET_LOCK_SITE_NAMES[waiter],
                    if owner == MEMORY_SET_OWNER_UNKNOWN {
                        "unknown"
                    } else {
                        MEMORY_SET_LOCK_SITE_NAMES[owner]
                    },
                    samples,
                    HOT_LOCK_SAMPLE_SHIFT,
                );
            }
        }
    }
    for bucket in 0..HEAP_SIZE_BUCKETS {
        let alloc_samples: usize = HEAP_ALLOC_SAMPLES[bucket]
            .iter()
            .map(|v| v.load(Ordering::Relaxed))
            .sum();
        let free_samples: usize = HEAP_FREE_SAMPLES[bucket]
            .iter()
            .map(|v| v.load(Ordering::Relaxed))
            .sum();
        if alloc_samples != 0 || free_samples != 0 {
            let alloc_bytes: usize = HEAP_ALLOC_SAMPLE_BYTES[bucket]
                .iter()
                .map(|v| v.load(Ordering::Relaxed))
                .sum();
            let free_bytes: usize = HEAP_FREE_SAMPLE_BYTES[bucket]
                .iter()
                .map(|v| v.load(Ordering::Relaxed))
                .sum();
            crate::println!("BUILDSTORM_DIAG heap_size bucket={} alloc_samples={} alloc_bytes={} free_samples={} free_bytes={} sample_shift={}", bucket, alloc_samples, alloc_bytes, free_samples, free_bytes, HOT_LOCK_SAMPLE_SHIFT);
        }
    }
    crate::println!(
        "BUILDSTORM_DIAG mmap_shape anonymous={} file={} fixed={} hinted={} lazy={} eager={} commit_reselect={} high_arena={} low_arena={}",
        MMAP_ANONYMOUS.load(Ordering::Relaxed),
        MMAP_FILE.load(Ordering::Relaxed),
        MMAP_FIXED.load(Ordering::Relaxed),
        MMAP_HINTED.load(Ordering::Relaxed),
        MMAP_LAZY.load(Ordering::Relaxed),
        MMAP_EAGER.load(Ordering::Relaxed),
        MMAP_COMMIT_RESELECT.load(Ordering::Relaxed),
        MMAP_HIGH_ARENA_PLACEMENTS.load(Ordering::Relaxed),
        MMAP_LOW_ARENA_PLACEMENTS.load(Ordering::Relaxed),
    );
    crate::println!(
        "BUILDSTORM_DIAG madvise_dontneed calls={} requested_pages={} discarded_pages={}",
        MADVISE_DONTNEED_CALLS.load(Ordering::Relaxed),
        MADVISE_DONTNEED_REQUESTED_PAGES.load(Ordering::Relaxed),
        MADVISE_DONTNEED_DISCARDED_PAGES.load(Ordering::Relaxed),
    );
    crate::println!(
        "BUILDSTORM_DIAG mmap_result select_ok={} select_enomem={} reselect_ok={} reselect_enomem={} len_le_64k={} len_le_1m={} len_le_16m={} len_le_256m={} len_gt_256m={} failed_len_max={} failed_gap_max={} failed_vmas_max={}",
        MMAP_SELECT_OK.load(Ordering::Relaxed),
        MMAP_SELECT_ENOMEM.load(Ordering::Relaxed),
        MMAP_COMMIT_RESELECT_OK.load(Ordering::Relaxed),
        MMAP_COMMIT_RESELECT_ENOMEM.load(Ordering::Relaxed),
        MMAP_LENGTH_COUNTS[0].load(Ordering::Relaxed),
        MMAP_LENGTH_COUNTS[1].load(Ordering::Relaxed),
        MMAP_LENGTH_COUNTS[2].load(Ordering::Relaxed),
        MMAP_LENGTH_COUNTS[3].load(Ordering::Relaxed),
        MMAP_LENGTH_COUNTS[4].load(Ordering::Relaxed),
        MMAP_FAILED_LENGTH_MAX.load(Ordering::Relaxed),
        MMAP_FAILED_GAP_MAX.load(Ordering::Relaxed),
        MMAP_FAILED_VMAS_MAX.load(Ordering::Relaxed),
    );
    crate::println!(
        "BUILDSTORM_DIAG mmap_failure_shape len_le_64k={} len_le_1m={} len_le_16m={} len_le_256m={} len_gt_256m={} munmap_below_le_64k={} munmap_below_le_1m={} munmap_below_le_16m={} munmap_below_le_256m={} munmap_below_gt_256m={} munmap_overlaps_cursor={} munmap_at_or_above_cursor={}",
        MMAP_FAILED_LENGTH_COUNTS[0].load(Ordering::Relaxed),
        MMAP_FAILED_LENGTH_COUNTS[1].load(Ordering::Relaxed),
        MMAP_FAILED_LENGTH_COUNTS[2].load(Ordering::Relaxed),
        MMAP_FAILED_LENGTH_COUNTS[3].load(Ordering::Relaxed),
        MMAP_FAILED_LENGTH_COUNTS[4].load(Ordering::Relaxed),
        MUNMAP_BELOW_CURSOR_LENGTH_COUNTS[0].load(Ordering::Relaxed),
        MUNMAP_BELOW_CURSOR_LENGTH_COUNTS[1].load(Ordering::Relaxed),
        MUNMAP_BELOW_CURSOR_LENGTH_COUNTS[2].load(Ordering::Relaxed),
        MUNMAP_BELOW_CURSOR_LENGTH_COUNTS[3].load(Ordering::Relaxed),
        MUNMAP_BELOW_CURSOR_LENGTH_COUNTS[4].load(Ordering::Relaxed),
        MUNMAP_OVERLAPS_CURSOR.load(Ordering::Relaxed),
        MUNMAP_AT_OR_ABOVE_CURSOR.load(Ordering::Relaxed),
    );
    crate::println!(
        "BUILDSTORM_DIAG process_progress exits={} child_wake_calls={} child_woken={}",
        PROCESS_EXITS.load(Ordering::Relaxed),
        CHILD_WAKE_CALLS.load(Ordering::Relaxed),
        CHILD_WOKEN.load(Ordering::Relaxed)
    );
    crate::println!(
        "BUILDSTORM_DIAG process_clone calls={} vfork={} share_vm={} fork_cow={} fork_cow_total_us={} fork_cow_max_us={} vfork_fork_cow={} vfork_fork_cow_total_us={} fork_cow_areas={} fork_cow_resident_pages={}",
        CLONE_CALLS.load(Ordering::Relaxed),
        CLONE_VFORK_CALLS.load(Ordering::Relaxed),
        CLONE_SHARE_VM_CALLS.load(Ordering::Relaxed),
        CLONE_FORK_COW_CALLS.load(Ordering::Relaxed),
        CLONE_FORK_COW_TOTAL_US.load(Ordering::Relaxed),
        CLONE_FORK_COW_MAX_US.load(Ordering::Relaxed),
        CLONE_VFORK_FORK_COW_CALLS.load(Ordering::Relaxed),
        CLONE_VFORK_FORK_COW_TOTAL_US.load(Ordering::Relaxed),
        CLONE_FORK_COW_AREAS.load(Ordering::Relaxed),
        CLONE_FORK_COW_RESIDENT_PAGES.load(Ordering::Relaxed)
    );
    crate::task::manager::diagnostic_dump_user_comm();
    crate::syscall::other::diagnostic_dump_futex_waiters();
    report_exited_mmap_protocols();
    crate::println!("BUILDSTORM_DIAG snapshot_end={}", sequence);
}
