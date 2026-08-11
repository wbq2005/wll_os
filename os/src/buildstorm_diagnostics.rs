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
const CPU_SLOTS: usize = crate::config::MAX_CPUS;

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
}

const WORK_NAMES: [&str; 21] = [
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
];
const WORK_SLOTS: usize = WORK_NAMES.len();

// VFS and ext4 paths already expose these stable phase boundaries.  Keep their
// numeric IDs source-compatible while recording only fixed-size aggregates:
// diagnostics must not log individual syscalls, path lookups, or block I/O.
const PHASE_NAMES: [&str; 40] = [
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
    Mmap,
    Mprotect,
    Munmap,
    FileInvalidate,
    FileWriteback,
    SharedMemory,
    ForkCow,
    ExecReplace,
    Futex,
}

const MEMORY_SET_LOCK_SITE_NAMES: [&str; 15] = [
    "other",
    "user_entry_activation",
    "hardware_page_fault",
    "user_copy_read",
    "user_copy_write",
    "brk",
    "mmap",
    "mprotect",
    "munmap",
    "file_invalidate",
    "file_writeback",
    "shared_memory",
    "fork_cow",
    "exec_replace",
    "futex",
];
const MEMORY_SET_LOCK_SITE_SLOTS: usize = MEMORY_SET_LOCK_SITE_NAMES.len();

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

static WORK_COUNTS: [AtomicUsize; WORK_SLOTS] = [const { AtomicUsize::new(0) }; WORK_SLOTS];
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
static MEMORY_SET_LOCK_SITE_ACQUIRES: [AtomicUsize; MEMORY_SET_LOCK_SITE_SLOTS] =
    [const { AtomicUsize::new(0) }; MEMORY_SET_LOCK_SITE_SLOTS];
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

#[inline]
fn now_us() -> usize {
    crate::timer::get_time_us()
}

#[inline]
fn cpu_slot() -> usize {
    crate::platform::current_cpu_index().min(CPU_SLOTS.saturating_sub(1))
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
        task.diagnostic_kernel_ticks
            .fetch_add(1, Ordering::Relaxed);
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
    task.diagnostic_block_count
        .fetch_add(1, Ordering::Relaxed);
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
}

impl WorkScope {
    #[inline]
    pub(crate) fn new(class: WorkClass) -> Self {
        Self {
            class,
            started_at: now_us(),
        }
    }
}

impl Drop for WorkScope {
    fn drop(&mut self) {
        let slot = self.class as usize;
        let elapsed = now_us().saturating_sub(self.started_at);
        WORK_COUNTS[slot].fetch_add(1, Ordering::Relaxed);
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
    guard: MutexGuard<'a, T>,
    class: LockClass,
    memory_set_site: Option<MemorySetLockSite>,
    acquired_at: usize,
}

impl<T> Deref for TimedLockGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &Self::Target {
        &self.guard
    }
}
impl<T> DerefMut for TimedLockGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.guard
    }
}
impl<T> Drop for TimedLockGuard<'_, T> {
    fn drop(&mut self) {
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
}

#[inline]
pub(crate) fn lock<'a, T>(class: LockClass, mutex: &'a Mutex<T>) -> TimedLockGuard<'a, T> {
    lock_with_memory_set_site(class, None, mutex)
}

#[inline]
pub(crate) fn lock_memory_set<'a, T>(
    site: MemorySetLockSite,
    mutex: &'a Mutex<T>,
) -> TimedLockGuard<'a, T> {
    lock_with_memory_set_site(LockClass::MemorySet, Some(site), mutex)
}

#[inline]
pub(crate) fn lock_memory_set_activation<'a, T>(
    mutex: &'a Mutex<T>,
) -> TimedLockGuard<'a, T> {
    lock_with_memory_set_site(
        LockClass::MemorySetActivation,
        Some(MemorySetLockSite::UserEntryActivation),
        mutex,
    )
}

#[inline]
fn lock_with_memory_set_site<'a, T>(
    class: LockClass,
    memory_set_site: Option<MemorySetLockSite>,
    mutex: &'a Mutex<T>,
) -> TimedLockGuard<'a, T> {
    let slot = class as usize;
    let started = now_us();
    let (guard, contended) = match mutex.try_lock() {
        Some(guard) => (guard, false),
        None => (mutex.lock(), true),
    };
    let acquired = now_us();
    LOCK_ACQUIRES[slot].fetch_add(1, Ordering::Relaxed);
    if let Some(site) = memory_set_site {
        MEMORY_SET_LOCK_SITE_ACQUIRES[site as usize].fetch_add(1, Ordering::Relaxed);
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
    TimedLockGuard {
        guard,
        class,
        memory_set_site,
        acquired_at: acquired,
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

#[inline]
pub(crate) fn note_syscall_enter(_syscall_id: usize) {}
#[inline]
pub(crate) fn note_syscall_exit(_syscall_id: usize) {}

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
    for cpu in 0..CPU_SLOTS {
        crate::println!("BUILDSTORM_DIAG cpu={} user_ticks={} kernel_ticks={} idle_ticks={} context_switches={} migrations={} runqueue_max={}", cpu, USER_TICKS[cpu].load(Ordering::Relaxed), KERNEL_TICKS[cpu].load(Ordering::Relaxed), IDLE_TICKS[cpu].load(Ordering::Relaxed), CONTEXT_SWITCHES[cpu].load(Ordering::Relaxed), TASK_MIGRATIONS[cpu].load(Ordering::Relaxed), RUNQUEUE_MAX[cpu].load(Ordering::Relaxed));
    }
    for cpu in 0..CPU_SLOTS {
        crate::println!("BUILDSTORM_DIAG user_run cpu={} count={} total_us={} max_us={} syscall={} timer={} irq={} other={}", cpu, USER_RUN_COUNT[cpu].load(Ordering::Relaxed), USER_RUN_TOTAL_US[cpu].load(Ordering::Relaxed), USER_RUN_MAX_US[cpu].load(Ordering::Relaxed), USER_RUN_SYSCALL[cpu].load(Ordering::Relaxed), USER_RUN_TIMER[cpu].load(Ordering::Relaxed), USER_RUN_IRQ[cpu].load(Ordering::Relaxed), USER_RUN_OTHER[cpu].load(Ordering::Relaxed));
    }
    for cpu in 0..CPU_SLOTS {
        crate::println!("BUILDSTORM_DIAG blocked_owner cpu={} current={} max={} enters={} exits={} loop_dispatches={} timed_loops={} timed_total_us={} timed_max_us={} empty_iterations={} wake_to_owner={} wake_to_global={}", cpu, BLOCKED_OWNER_CURRENT[cpu].load(Ordering::Relaxed), BLOCKED_OWNER_MAX[cpu].load(Ordering::Relaxed), BLOCKED_OWNER_ENTERS[cpu].load(Ordering::Relaxed), BLOCKED_OWNER_EXITS[cpu].load(Ordering::Relaxed), BLOCKED_OWNER_LOOP_DISPATCHES[cpu].load(Ordering::Relaxed), BLOCKED_OWNER_TIMED_LOOPS[cpu].load(Ordering::Relaxed), BLOCKED_OWNER_TIMED_TOTAL_US[cpu].load(Ordering::Relaxed), BLOCKED_OWNER_TIMED_MAX_US[cpu].load(Ordering::Relaxed), BLOCKED_OWNER_EMPTY_ITERATIONS[cpu].load(Ordering::Relaxed), BLOCKED_OWNER_WAKES[cpu].load(Ordering::Relaxed), BLOCKED_GLOBAL_WAKES.load(Ordering::Relaxed));
    }
    crate::println!("BUILDSTORM_DIAG mm user_root={} kernel_root={} redundant_root={} page_table_writes={} local_flush={} remote_shootdown={} remote_targets={}", USER_ROOT_ACTIVATIONS.load(Ordering::Relaxed), KERNEL_ROOT_ACTIVATIONS.load(Ordering::Relaxed), REDUNDANT_ROOT_ACTIVATIONS.load(Ordering::Relaxed), PAGE_TABLE_WRITES.load(Ordering::Relaxed), LOCAL_TLB_FLUSHES.load(Ordering::Relaxed), REMOTE_SHOOTDOWNS.load(Ordering::Relaxed), REMOTE_SHOOTDOWN_TARGETS.load(Ordering::Relaxed));
    crate::println!("BUILDSTORM_DIAG anonymous_vma installs={} left={} right={} both={} neither={} current={} max={} pages={} pages_max={}", ANONYMOUS_VMA_INSTALLS.load(Ordering::Relaxed), ANONYMOUS_VMA_LEFT_MERGEABLE.load(Ordering::Relaxed), ANONYMOUS_VMA_RIGHT_MERGEABLE.load(Ordering::Relaxed), ANONYMOUS_VMA_BOTH_MERGEABLE.load(Ordering::Relaxed), ANONYMOUS_VMA_NEITHER_MERGEABLE.load(Ordering::Relaxed), ANONYMOUS_VMA_CURRENT.load(Ordering::Relaxed), ANONYMOUS_VMA_MAX.load(Ordering::Relaxed), ANONYMOUS_VMA_PAGES.load(Ordering::Relaxed), ANONYMOUS_VMA_PAGES_MAX.load(Ordering::Relaxed));
    crate::println!("BUILDSTORM_DIAG anonymous_move left_selected={} left_no_realloc={} left_shrink={} left_remove={} left_frame_relocate={} left_remove_suffix={} left_remove_suffix_max={} right_selected={} right_no_realloc={} right_shrink={} right_remove={} right_frame_move={} right_remove_suffix={} right_remove_suffix_max={} zero_metadata={}", ANONYMOUS_MOVE_LEFT_SELECTED.load(Ordering::Relaxed), ANONYMOUS_MOVE_LEFT_NO_REALLOC.load(Ordering::Relaxed), ANONYMOUS_MOVE_LEFT_SHRINK.load(Ordering::Relaxed), ANONYMOUS_MOVE_LEFT_REMOVE.load(Ordering::Relaxed), ANONYMOUS_MOVE_LEFT_FRAME_RELOCATE.load(Ordering::Relaxed), ANONYMOUS_MOVE_LEFT_REMOVE_SUFFIX.load(Ordering::Relaxed), ANONYMOUS_MOVE_LEFT_REMOVE_SUFFIX_MAX.load(Ordering::Relaxed), ANONYMOUS_MOVE_RIGHT_SELECTED.load(Ordering::Relaxed), ANONYMOUS_MOVE_RIGHT_NO_REALLOC.load(Ordering::Relaxed), ANONYMOUS_MOVE_RIGHT_SHRINK.load(Ordering::Relaxed), ANONYMOUS_MOVE_RIGHT_REMOVE.load(Ordering::Relaxed), ANONYMOUS_MOVE_RIGHT_FRAME_MOVE.load(Ordering::Relaxed), ANONYMOUS_MOVE_RIGHT_REMOVE_SUFFIX.load(Ordering::Relaxed), ANONYMOUS_MOVE_RIGHT_REMOVE_SUFFIX_MAX.load(Ordering::Relaxed), ANONYMOUS_MOVE_ZERO_METADATA.load(Ordering::Relaxed));
    crate::println!("BUILDSTORM_DIAG anonymous_coalesce calls={} areas={} areas_max={} sort_us={} scan_us={} total_us={} merges={} merged_frames={} frame_reallocs={} frame_relocate={}", ANONYMOUS_COALESCE_CALLS.load(Ordering::Relaxed), ANONYMOUS_COALESCE_AREAS.load(Ordering::Relaxed), ANONYMOUS_COALESCE_AREAS_MAX.load(Ordering::Relaxed), ANONYMOUS_COALESCE_SORT_US.load(Ordering::Relaxed), ANONYMOUS_COALESCE_SCAN_US.load(Ordering::Relaxed), ANONYMOUS_COALESCE_TOTAL_US.load(Ordering::Relaxed), ANONYMOUS_COALESCE_MERGES.load(Ordering::Relaxed), ANONYMOUS_COALESCE_MERGED_FRAMES.load(Ordering::Relaxed), ANONYMOUS_COALESCE_FRAME_REALLOCS.load(Ordering::Relaxed), ANONYMOUS_COALESCE_FRAME_RELOCATE.load(Ordering::Relaxed));
    crate::println!("BUILDSTORM_DIAG vma current={} max={}", LOGICAL_VMA_CURRENT.load(Ordering::Relaxed), LOGICAL_VMA_MAX.load(Ordering::Relaxed));
    crate::println!("BUILDSTORM_DIAG cache path_components={} path_hit={} path_miss={} negative_path_hit={} dir_hit={} dir_miss={} inode_metadata_hit={} inode_metadata_miss={} inode_hit={} inode_miss={} metadata_hit={} metadata_miss={} block_hit={} block_miss={} virtio_requests={} virtio_bytes={} virtio_queue_us={} virtio_complete_us={}", PATH_COMPONENT_LOOKUPS.load(Ordering::Relaxed), PATH_CACHE_HITS.load(Ordering::Relaxed), PATH_CACHE_MISSES.load(Ordering::Relaxed), NEGATIVE_PATH_CACHE_HITS.load(Ordering::Relaxed), DIR_CACHE_HITS.load(Ordering::Relaxed), DIR_CACHE_MISSES.load(Ordering::Relaxed), INODE_METADATA_CACHE_HITS.load(Ordering::Relaxed), INODE_METADATA_CACHE_MISSES.load(Ordering::Relaxed), INODE_CACHE_HITS.load(Ordering::Relaxed), INODE_CACHE_MISSES.load(Ordering::Relaxed), METADATA_CACHE_HITS.load(Ordering::Relaxed), METADATA_CACHE_MISSES.load(Ordering::Relaxed), BLOCK_CACHE_HITS.load(Ordering::Relaxed), BLOCK_CACHE_MISSES.load(Ordering::Relaxed), VIRTIO_REQUESTS.load(Ordering::Relaxed), VIRTIO_BYTES.load(Ordering::Relaxed), VIRTIO_QUEUE_US.load(Ordering::Relaxed), VIRTIO_COMPLETE_US.load(Ordering::Relaxed));
    for slot in 0..WORK_SLOTS {
        let count = WORK_COUNTS[slot].load(Ordering::Relaxed);
        if count != 0 {
            crate::println!(
                "BUILDSTORM_DIAG work={} count={} total_us={} max_us={}",
                WORK_NAMES[slot],
                count,
                WORK_TOTAL_US[slot].load(Ordering::Relaxed),
                WORK_MAX_US[slot].load(Ordering::Relaxed)
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
        crate::println!("BUILDSTORM_DIAG lock_rank={} name={} acquire_count={} contended_count={} wait_total_us={} wait_max_us={} hold_total_us={} hold_max_us={}", rank + 1, LOCK_NAMES[slot], LOCK_ACQUIRES[slot].load(Ordering::Relaxed), LOCK_CONTENDED[slot].load(Ordering::Relaxed), LOCK_WAIT_TOTAL_US[slot].load(Ordering::Relaxed), LOCK_WAIT_MAX_US[slot].load(Ordering::Relaxed), LOCK_HOLD_TOTAL_US[slot].load(Ordering::Relaxed), LOCK_HOLD_MAX_US[slot].load(Ordering::Relaxed));
    }
    for site in 0..MEMORY_SET_LOCK_SITE_SLOTS {
        let acquires = MEMORY_SET_LOCK_SITE_ACQUIRES[site].load(Ordering::Relaxed);
        if acquires != 0 {
            crate::println!("BUILDSTORM_DIAG memory_set_site={} acquire_count={} contended_count={} wait_total_us={} wait_max_us={} hold_total_us={} hold_max_us={}", MEMORY_SET_LOCK_SITE_NAMES[site], acquires, MEMORY_SET_LOCK_SITE_CONTENDED[site].load(Ordering::Relaxed), MEMORY_SET_LOCK_SITE_WAIT_TOTAL_US[site].load(Ordering::Relaxed), MEMORY_SET_LOCK_SITE_WAIT_MAX_US[site].load(Ordering::Relaxed), MEMORY_SET_LOCK_SITE_HOLD_TOTAL_US[site].load(Ordering::Relaxed), MEMORY_SET_LOCK_SITE_HOLD_MAX_US[site].load(Ordering::Relaxed));
        }
    }
    crate::task::manager::diagnostic_dump_user_comm();
    crate::println!("BUILDSTORM_DIAG snapshot_end={}", sequence);
}
