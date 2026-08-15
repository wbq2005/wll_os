//! 运行时 ext4：`VirtIO-BLK` + `mount_block_device` 后与 MemFS 并列作为读路径后端。
//!
//! `ext4_rs::ext4_file_open` 在 crates.io 版中有误（打开类型被写成目录），此处不用它；
//! 目录项逐级 `ext4_dir_get_entries`/`compare_name`，文件内容 [`Ext4::read_at`]。

use alloc::collections::{BTreeMap, BTreeSet, VecDeque};
use alloc::format;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use ext4_rs::{Errno, Ext4, Ext4Error, InodeFileType, BLOCK_SIZE};

/// ext4 标准根 inode 号（与 `ext4_rs` 内部一致，crate 根未再导出该常量）。
const ROOT_INODE: u32 = 2;
const EXT4_DIRENT_UNKNOWN: u8 = 0;
const EXT4_DIRENT_DIR: u8 = 2;
const MAX_FILE_OFFSET: usize = isize::MAX as usize;
const DENSE_REGULAR_CACHE_LIMIT: usize = 8 * 1024 * 1024;
const EXECUTABLE_IMAGE_CACHE_LIMIT: usize = 256 * 1024 * 1024;
const ASYNC_WRITEBACK_MIN_FILE: usize = 256 * 1024;
const WRITEBACK_CLUSTER_BYTES: usize = 256 * 1024;
const CLEAN_PAGE_CACHE_MIN_PAGES: usize = 2048;
const CLEAN_PAGE_CACHE_MAX_PAGES: usize = 65536;
const CLEAN_PAGE_CACHE_MEMORY_DIVISOR: usize = 32;
const PBLOCK_RUN_CACHE_LIMIT: usize = 2048;
const PBLOCK_RUN_LOOKAHEAD: u32 = 16;
// The official compiler workload touches more than 50K distinct inodes in a
// single run.  Clearing the whole cache at 32K turns that working set into a
// repeated ext4 inode/block scan.  Keep enough entries for the workload while
// retaining the existing explicit invalidation on namespace and inode writes.
const INODE_METADATA_CACHE_LIMIT: usize = 128 * 1024;
const NEGATIVE_PATH_CACHE_LIMIT: usize = 64 * 1024;
const DEFAULT_WRITEBACK_WORKER_ENABLED: bool = false;
static WRITEBACK_WORKER_STARTED: AtomicBool = AtomicBool::new(false);
#[cfg(feature = "buildstorm-diagnostics")]
static EXECUTABLE_IMAGE_CACHE_HITS: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "buildstorm-diagnostics")]
static EXECUTABLE_IMAGE_CACHE_MISSES: AtomicUsize = AtomicUsize::new(0);
static NAMESPACE_GENERATION: AtomicUsize = AtomicUsize::new(0);
fn checked_file_end(offset: usize, len: usize) -> Result<usize, SysErrNo> {
    let end = offset.checked_add(len).ok_or(SysErrNo::EFBIG)?;
    if end > MAX_FILE_OFFSET {
        return Err(SysErrNo::EFBIG);
    }
    Ok(end)
}

pub(crate) fn map_ext4_err(e: Ext4Error) -> SysErrNo {
    match e.error() {
        Errno::ENOENT => SysErrNo::ENOENT,
        Errno::EEXIST => SysErrNo::EEXIST,
        Errno::ENOTDIR => SysErrNo::ENOTDIR,
        Errno::EISDIR => SysErrNo::EISDIR,
        Errno::EINVAL => SysErrNo::EINVAL,
        Errno::ENOSPC => SysErrNo::ENOSPC,
        Errno::EROFS => SysErrNo::EROFS,
        Errno::EBADF => SysErrNo::EBADF,
        Errno::EPERM => SysErrNo::EPERM,
        Errno::EACCES => SysErrNo::EACCES,
        Errno::EFBIG => SysErrNo::EFBIG,
        Errno::EMLINK => SysErrNo::EMLINK,
        _ => SysErrNo::EIO,
    }
}

use lazy_static::lazy_static;
use spin::{Mutex, RwLock};

use crate::config::PAGE_SIZE;
use crate::fs::normalize_path;
use crate::mm::frame_allocator::{self, FrameTracker};
use crate::utils::error::SysErrNo;

lazy_static! {
    /// 挂载后的 Ext4（无盘或未探测到 virtio 时为 `None`）
    pub static ref ROOT_EXT4: Mutex<Option<Arc<Ext4>>> = Mutex::new(None);
    static ref PATH_CACHE: RwLock<BTreeMap<String, (u32, Ext4NodeKind)>> =
        RwLock::new(BTreeMap::new());
    static ref NEGATIVE_PATH_CACHE: RwLock<BTreeSet<String>> = RwLock::new(BTreeSet::new());
    static ref DIR_CACHE: RwLock<BTreeMap<u32, Arc<BTreeMap<String, (u32, bool)>>>> =
        RwLock::new(BTreeMap::new());
    static ref INODE_METADATA_CACHE: RwLock<BTreeMap<u32, (Ext4Metadata, Ext4NodeKind)>> =
        RwLock::new(BTreeMap::new());
    static ref DATA_TIME_OVERRIDES: Mutex<BTreeMap<u32, (u32, u32, u32, u32)>> =
        Mutex::new(BTreeMap::new());
    static ref REGULAR_FILE_CACHE: Mutex<BTreeMap<u32, RegularCacheEntry>> =
        Mutex::new(BTreeMap::new());
    static ref EXECUTABLE_IMAGE_CACHE: Mutex<BTreeMap<u32, Arc<Vec<u8>>>> =
        Mutex::new(BTreeMap::new());
    static ref CLEAN_PAGE_CACHE: Mutex<CleanPageCache> = Mutex::new(CleanPageCache::new());
    static ref PBLOCK_RUN_CACHE: Mutex<PblockRunCache> = Mutex::new(PblockRunCache::new());
    static ref OPEN_REGULAR_REFS: Mutex<BTreeMap<u32, usize>> = Mutex::new(BTreeMap::new());
    static ref PENDING_UNLINK_REGULAR: Mutex<BTreeSet<u32>> = Mutex::new(BTreeSet::new());
    static ref WRITEBACK_QUEUE: Mutex<WritebackQueue> = Mutex::new(WritebackQueue::new());
    static ref REGULAR_CACHE_CLOSE_LOCK: Mutex<()> = Mutex::new(());
    /// ext4_rs does not serialize block bitmap/inode transactions internally.
    /// Keep cache updates parallel, but commit each filesystem mutation as one
    /// transaction so concurrent writers cannot allocate the same blocks.
    static ref EXT4_MUTATION_LOCK: Mutex<()> = Mutex::new(());
    /// Readers hold a shared transaction across lookup and cache publication;
    /// writers hold an exclusive transaction from their first path resolution
    /// through directory/inode updates and cache invalidation. Regular file
    /// data and writeback stay outside this lock.
    static ref NAMESPACE_LOCK: RwLock<()> = RwLock::new(());
}

type RegularCacheEntry = Arc<Mutex<CachedRegularFile>>;

// spin::RwLock::write() does not advertise a waiting writer. Taking the
// upgradeable slot first blocks new readers while existing readers drain.
macro_rules! namespace_write_lock {
    () => {
        NAMESPACE_LOCK.upgradeable_read().upgrade()
    };
}

#[cfg(feature = "buildstorm-diagnostics")]
macro_rules! ext4_mutation_lock {
    () => {
        crate::buildstorm_diagnostics::lock(
            crate::buildstorm_diagnostics::LockClass::Ext4,
            &EXT4_MUTATION_LOCK,
        )
    };
}

#[cfg(not(feature = "buildstorm-diagnostics"))]
macro_rules! ext4_mutation_lock {
    () => {
        EXT4_MUTATION_LOCK.lock()
    };
}

struct WritebackQueue {
    pending: VecDeque<u32>,
    queued: BTreeSet<u32>,
}

impl WritebackQueue {
    fn new() -> Self {
        Self {
            pending: VecDeque::new(),
            queued: BTreeSet::new(),
        }
    }

    fn push(&mut self, ino: u32) {
        if self.queued.insert(ino) {
            self.pending.push_back(ino);
        }
    }

    fn pop(&mut self) -> Option<u32> {
        while let Some(ino) = self.pending.pop_front() {
            if self.queued.remove(&ino) {
                return Some(ino);
            }
        }
        None
    }

    fn remove(&mut self, ino: u32) {
        self.queued.remove(&ino);
    }

    fn has_work(&self) -> bool {
        !self.queued.is_empty()
    }

    fn clear(&mut self) {
        self.pending.clear();
        self.queued.clear();
    }
}

#[derive(Clone, Copy, Debug)]
struct DirtyRange {
    start: usize,
    end: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PageCacheWritebackState {
    Clean,
    Dirty,
    Queued,
    Writeback,
    DirtyDuringWriteback,
}

#[derive(Clone, Debug)]
struct PageCacheDirtyModel {
    ranges: Vec<DirtyRange>,
    full_dirty: bool,
    size_dirty: bool,
    writeback: PageCacheWritebackState,
    last_error: Option<SysErrNo>,
    seq: u64,
}

impl PageCacheDirtyModel {
    fn clean() -> Self {
        Self {
            ranges: Vec::new(),
            full_dirty: false,
            size_dirty: false,
            writeback: PageCacheWritebackState::Clean,
            last_error: None,
            seq: 0,
        }
    }

    fn dirty_whole_file() -> Self {
        Self {
            ranges: Vec::new(),
            full_dirty: true,
            size_dirty: true,
            writeback: PageCacheWritebackState::Dirty,
            last_error: None,
            seq: 1,
        }
    }

    fn data_dirty(&self) -> bool {
        self.full_dirty || self.size_dirty || !self.ranges.is_empty()
    }

    fn is_dirty(&self) -> bool {
        self.data_dirty()
    }

    fn refresh_writeback_state(&mut self) {
        if matches!(
            self.writeback,
            PageCacheWritebackState::Writeback | PageCacheWritebackState::DirtyDuringWriteback
        ) {
            return;
        }
        self.writeback = if self.data_dirty() {
            PageCacheWritebackState::Dirty
        } else {
            PageCacheWritebackState::Clean
        };
    }

    fn note_write(&mut self) {
        self.seq = self.seq.wrapping_add(1);
        self.writeback = match self.writeback {
            PageCacheWritebackState::Writeback => PageCacheWritebackState::DirtyDuringWriteback,
            PageCacheWritebackState::DirtyDuringWriteback => {
                PageCacheWritebackState::DirtyDuringWriteback
            }
            _ => PageCacheWritebackState::Dirty,
        };
    }

    fn note_queued(&mut self) {
        if self.data_dirty()
            && matches!(
                self.writeback,
                PageCacheWritebackState::Dirty | PageCacheWritebackState::Queued
            )
        {
            self.writeback = PageCacheWritebackState::Queued;
        }
    }

    fn writeback_active(&self) -> bool {
        matches!(
            self.writeback,
            PageCacheWritebackState::Writeback | PageCacheWritebackState::DirtyDuringWriteback
        )
    }

    fn begin_writeback(&mut self) -> Option<u64> {
        if self.writeback_active() || !self.data_dirty() {
            return None;
        }
        self.writeback = PageCacheWritebackState::Writeback;
        Some(self.seq)
    }

    fn finish_writeback(
        &mut self,
        seq: u64,
        completed: Option<DirtyRange>,
        committed_size: bool,
    ) -> bool {
        if self.seq == seq {
            if let Some(completed) = completed {
                let mut remaining = Vec::new();
                for range in self.ranges.iter().copied() {
                    if range.end <= completed.start || range.start >= completed.end {
                        remaining.push(range);
                        continue;
                    }
                    if range.start < completed.start {
                        remaining.push(DirtyRange {
                            start: range.start,
                            end: completed.start,
                        });
                    }
                    if completed.end < range.end {
                        remaining.push(DirtyRange {
                            start: completed.end,
                            end: range.end,
                        });
                    }
                }
                self.ranges = remaining;
            }
            self.full_dirty = false;
            if committed_size {
                self.size_dirty = false;
            }
            self.writeback = if self.data_dirty() {
                PageCacheWritebackState::Dirty
            } else {
                PageCacheWritebackState::Clean
            };
            self.last_error = None;
        } else {
            self.writeback = PageCacheWritebackState::Dirty;
        }
        self.data_dirty()
    }

    fn fail_writeback(&mut self, err: SysErrNo) {
        self.last_error = Some(err);
        self.writeback = if self.data_dirty() {
            PageCacheWritebackState::Dirty
        } else {
            PageCacheWritebackState::Clean
        };
    }
}

struct CachedRegularFile {
    dense: Option<Vec<u8>>,
    pages: BTreeMap<usize, FrameTracker>,
    size: usize,
    persisted_size: usize,
    truncate_to: Option<usize>,
    dirty: PageCacheDirtyModel,
    mtime_sec: u32,
    mtime_extra: u32,
    ctime_sec: u32,
    ctime_extra: u32,
    evicted: bool,
}

impl CachedRegularFile {
    fn new(
        size: usize,
        persisted_size: usize,
        dense: Option<Vec<u8>>,
        dirty: PageCacheDirtyModel,
        mtime_sec: u32,
        mtime_extra: u32,
        ctime_sec: u32,
        ctime_extra: u32,
    ) -> Self {
        Self {
            dense,
            pages: BTreeMap::new(),
            size,
            persisted_size,
            truncate_to: None,
            dirty,
            mtime_sec,
            mtime_extra,
            ctime_sec,
            ctime_extra,
            evicted: false,
        }
    }

    fn is_dirty(&self) -> bool {
        !self.evicted && self.dirty.is_dirty()
    }
}

#[derive(Clone, Copy)]
struct CachedRegularInfo {
    size: usize,
    mtime_sec: u32,
    mtime_extra: u32,
    ctime_sec: u32,
    ctime_extra: u32,
}

struct WritebackSnapshot {
    ino: u32,
    target_len: usize,
    truncate_to: Option<usize>,
    range: Option<DirtyRange>,
    data: Vec<u8>,
    commit_size: bool,
    dirty_seq: u64,
    mtime_sec: u32,
    mtime_extra: u32,
    ctime_sec: u32,
    ctime_extra: u32,
}

enum WritebackSnapshotResult {
    NoCache,
    Clean,
    Failed(SysErrNo),
    Busy,
    Snapshot(WritebackSnapshot),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct CleanPageKey {
    ino: u32,
    page_idx: usize,
}

struct CachedCleanPage {
    frame: FrameTracker,
    last_used: u64,
}

#[derive(Clone, Copy)]
struct CleanPageReadMetadata {
    file_size: usize,
}

#[derive(Clone, Copy)]
struct PblockRun {
    start_lblock: u32,
    len: u32,
    start_pblock: u64,
    last_used: u64,
}

struct PblockRunCache {
    runs: BTreeMap<(u32, u32), PblockRun>,
    next_age: u64,
}

impl PblockRunCache {
    fn new() -> Self {
        Self {
            runs: BTreeMap::new(),
            next_age: 1,
        }
    }

    fn bump_age(&mut self) -> u64 {
        let age = self.next_age;
        self.next_age = self.next_age.wrapping_add(1).max(1);
        age
    }

    fn get(&mut self, ino: u32, lblock: u32) -> Option<u64> {
        let (key, run) = self
            .runs
            .range(..=(ino, lblock))
            .next_back()
            .map(|(key, run)| (*key, *run))?;
        if key.0 != ino || lblock < run.start_lblock {
            return None;
        }
        let run_offset = lblock - run.start_lblock;
        if run_offset >= run.len {
            return None;
        }
        let age = self.bump_age();
        if let Some(run) = self.runs.get_mut(&key) {
            run.last_used = age;
        }
        Some(run.start_pblock + run_offset as u64)
    }

    fn insert(&mut self, ino: u32, start_lblock: u32, len: u32, start_pblock: u64) {
        if len == 0 {
            return;
        }
        let key = (ino, start_lblock);
        let age = self.bump_age();
        if let Some(run) = self.runs.get_mut(&key) {
            *run = PblockRun {
                start_lblock,
                len,
                start_pblock,
                last_used: age,
            };
            return;
        }
        if self.runs.len() >= PBLOCK_RUN_CACHE_LIMIT {
            self.evict_one();
        }
        self.runs.insert(
            key,
            PblockRun {
                start_lblock,
                len,
                start_pblock,
                last_used: age,
            },
        );
    }

    fn evict_one(&mut self) {
        let victim = self
            .runs
            .iter()
            .min_by_key(|(_, run)| run.last_used)
            .map(|(key, _)| *key);
        if let Some(key) = victim {
            self.runs.remove(&key);
        }
    }

    fn invalidate_ino(&mut self, ino: u32) {
        let victims: Vec<(u32, u32)> = self
            .runs
            .keys()
            .copied()
            .filter(|(run_ino, _)| *run_ino == ino)
            .collect();
        for key in victims {
            self.runs.remove(&key);
        }
    }

    fn clear(&mut self) {
        self.runs.clear();
    }
}

struct CleanPageCache {
    pages: BTreeMap<CleanPageKey, CachedCleanPage>,
    ages: BTreeSet<(u64, CleanPageKey)>,
    next_age: u64,
}

impl CleanPageCache {
    fn new() -> Self {
        Self {
            pages: BTreeMap::new(),
            ages: BTreeSet::new(),
            next_age: 1,
        }
    }

    fn bump_age(&mut self) -> u64 {
        let age = self.next_age;
        self.next_age = self.next_age.wrapping_add(1).max(1);
        age
    }

    fn get(&mut self, key: CleanPageKey) -> Option<FrameTracker> {
        let age = self.bump_age();
        let page = self.pages.get_mut(&key)?;
        self.ages.remove(&(page.last_used, key));
        page.last_used = age;
        self.ages.insert((age, key));
        Some(page.frame.clone())
    }

    fn uncached_prefix_len(&self, ino: u32, start_page_idx: usize, max_pages: usize) -> usize {
        let mut pages = 0usize;
        while pages < max_pages {
            let Some(page_idx) = start_page_idx.checked_add(pages) else {
                break;
            };
            if self.pages.contains_key(&CleanPageKey { ino, page_idx }) {
                break;
            }
            pages += 1;
        }
        pages
    }

    fn get_run(&mut self, ino: u32, start_page_idx: usize, max_pages: usize) -> Vec<FrameTracker> {
        let mut frames = Vec::new();
        for page in 0..max_pages {
            let Some(page_idx) = start_page_idx.checked_add(page) else {
                break;
            };
            let key = CleanPageKey { ino, page_idx };
            let age = self.bump_age();
            let Some(cached) = self.pages.get_mut(&key) else {
                break;
            };
            self.ages.remove(&(cached.last_used, key));
            cached.last_used = age;
            self.ages.insert((age, key));
            frames.push(cached.frame.clone());
        }
        frames
    }

    fn insert(&mut self, key: CleanPageKey, frame: FrameTracker) {
        let age = self.bump_age();
        if let Some(page) = self.pages.get_mut(&key) {
            self.ages.remove(&(page.last_used, key));
            page.frame = frame;
            page.last_used = age;
            self.ages.insert((age, key));
            return;
        }
        if self.pages.len() >= clean_page_cache_limit() {
            self.evict_one();
        }
        self.pages.insert(
            key,
            CachedCleanPage {
                frame,
                last_used: age,
            },
        );
        self.ages.insert((age, key));
    }

    fn evict_one(&mut self) {
        if let Some((age, key)) = self.ages.iter().next().copied() {
            self.ages.remove(&(age, key));
            self.pages.remove(&key);
        }
    }

    fn invalidate_ino(&mut self, ino: u32) {
        let victims: Vec<CleanPageKey> = self
            .pages
            .keys()
            .copied()
            .filter(|key| key.ino == ino)
            .collect();
        for key in victims {
            if let Some(page) = self.pages.remove(&key) {
                self.ages.remove(&(page.last_used, key));
            }
        }
    }

    fn invalidate_range(&mut self, ino: u32, start: usize, end: usize) {
        if start >= end {
            return;
        }
        let first = start / PAGE_SIZE;
        let last = (end - 1) / PAGE_SIZE;
        let victims: Vec<CleanPageKey> = self
            .pages
            .keys()
            .copied()
            .filter(|key| key.ino == ino && key.page_idx >= first && key.page_idx <= last)
            .collect();
        for key in victims {
            if let Some(page) = self.pages.remove(&key) {
                self.ages.remove(&(page.last_used, key));
            }
        }
    }

    fn clear(&mut self) {
        self.pages.clear();
        self.ages.clear();
    }
}

fn clean_page_cache_limit() -> usize {
    let proportional =
        frame_allocator::total_frames().saturating_div(CLEAN_PAGE_CACHE_MEMORY_DIVISOR);
    proportional
        .max(CLEAN_PAGE_CACHE_MIN_PAGES)
        .min(CLEAN_PAGE_CACHE_MAX_PAGES)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ext4NodeKind {
    Regular,
    Directory,
    Symlink,
    Other,
}

#[derive(Clone, Copy, Debug)]
pub struct Ext4Metadata {
    pub ino: u32,
    pub mode: u32,
    pub flags: u32,
    pub nlink: u32,
    pub uid: u32,
    pub gid: u32,
    pub size: u64,
    pub blocks: u64,
    pub atime_sec: isize,
    pub atime_nsec: isize,
    pub mtime_sec: isize,
    pub mtime_nsec: isize,
    pub ctime_sec: isize,
    pub ctime_nsec: isize,
}

#[derive(Clone, Copy, Debug)]
pub struct Ext4StatFs {
    pub block_size: usize,
    pub blocks: usize,
    pub free_blocks: usize,
    pub files: usize,
    pub free_files: usize,
    pub max_name_len: usize,
}

fn inode_kind(fs: &Ext4, ino: u32) -> Ext4NodeKind {
    if let Some((_meta, kind)) = INODE_METADATA_CACHE.read().get(&ino).copied() {
        #[cfg(feature = "buildstorm-diagnostics")]
        crate::buildstorm_diagnostics::note_inode_metadata_cache(true);
        return kind;
    }
    #[cfg(feature = "buildstorm-diagnostics")]
    crate::buildstorm_diagnostics::note_inode_metadata_cache(false);
    let inode = fs.get_inode_ref(ino).inode;
    if inode.is_dir() {
        Ext4NodeKind::Directory
    } else if inode.is_file() {
        Ext4NodeKind::Regular
    } else if inode.is_link() {
        Ext4NodeKind::Symlink
    } else {
        Ext4NodeKind::Other
    }
}

fn overlay_dynamic_metadata(mut meta: Ext4Metadata) -> Ext4Metadata {
    let cached_info = cached_regular_info(meta.ino);
    if let Some(info) = cached_info {
        meta.size = info.size as u64;
        meta.mtime_sec = info.mtime_sec as isize;
        meta.mtime_nsec = ext4_extra_nsec(info.mtime_extra);
        meta.ctime_sec = info.ctime_sec as isize;
        meta.ctime_nsec = ext4_extra_nsec(info.ctime_extra);
    } else if let Some((mtime, mtime_extra, ctime, ctime_extra)) =
        DATA_TIME_OVERRIDES.lock().get(&meta.ino).copied()
    {
        meta.mtime_sec = mtime as isize;
        meta.mtime_nsec = ext4_extra_nsec(mtime_extra);
        meta.ctime_sec = ctime as isize;
        meta.ctime_nsec = ext4_extra_nsec(ctime_extra);
    }
    meta.blocks = meta.blocks.max(regular_blocks(meta.size));
    meta
}

fn invalidate_metadata_ino(ino: u32) {
    INODE_METADATA_CACHE.write().remove(&ino);
}

fn clear_metadata_cache() {
    INODE_METADATA_CACHE.write().clear();
}

fn cache_metadata_ino(ino: u32, meta: Ext4Metadata, kind: Ext4NodeKind) {
    let mut cache = INODE_METADATA_CACHE.write();
    if cache.len() >= INODE_METADATA_CACHE_LIMIT {
        cache.clear();
    }
    cache.insert(ino, (meta, kind));
}

fn current_ext4_time() -> u32 {
    let (sec, _) = crate::timer::get_timeval();
    sec.min(u32::MAX as usize) as u32
}

fn current_fs_ids() -> (u16, u16) {
    let credentials = crate::task::current_task()
        .map(|task| task.credentials.lock().clone())
        .unwrap_or_else(crate::task::Credentials::root);
    let uid = credentials.fsuid.min(u16::MAX as u32) as u16;
    let gid = credentials.fsgid.min(u16::MAX as u32) as u16;
    (uid, gid)
}

fn new_child_ids_and_mode(fs: &Ext4, parent_ino: u32, mode: u32, is_dir: bool) -> (u16, u16, u16) {
    let (uid, fsgid) = current_fs_ids();
    let parent_inode = fs.get_inode_ref(parent_ino).inode;
    let parent_setgid = (parent_inode.mode() & 0o2000) != 0;
    let mut perm = (mode as u16) & 0o7777;
    if is_dir && parent_setgid {
        perm |= 0o2000;
    }
    let gid = if parent_setgid {
        parent_inode.gid()
    } else {
        fsgid
    };
    (uid, gid, perm)
}

fn ext4_extra_nsec(extra: u32) -> isize {
    (extra >> 2) as isize
}

fn ext4_nsec_extra(nsec: isize) -> u32 {
    (nsec.max(0) as u32).min(999_999_999) << 2
}

fn touch_inode(fs: &Ext4, ino: u32, atime: bool, mtime: bool, ctime: bool) {
    let now = current_ext4_time();
    let mut iref = fs.get_inode_ref(ino);
    if atime {
        iref.inode.set_atime(now);
        iref.inode.set_i_atime_extra(0);
    }
    if mtime {
        iref.inode.set_mtime(now);
        iref.inode.set_i_mtime_extra(0);
    }
    if ctime {
        iref.inode.set_ctime(now);
        iref.inode.set_i_ctime_extra(0);
    }
    fs.write_back_inode(&mut iref);
    if mtime || ctime {
        DATA_TIME_OVERRIDES.lock().remove(&ino);
    }
    if atime || mtime || ctime {
        invalidate_metadata_ino(ino);
    }
}

fn note_data_write(ino: u32) {
    let now = current_ext4_time();
    DATA_TIME_OVERRIDES.lock().insert(ino, (now, 0, now, 0));
    invalidate_metadata_ino(ino);
}

fn note_cached_data_write(cached: &mut CachedRegularFile) {
    let now = current_ext4_time();
    cached.dirty.note_write();
    cached.mtime_sec = now;
    cached.mtime_extra = 0;
    cached.ctime_sec = now;
    cached.ctime_extra = 0;
}

fn cached_file_async_writeback_ok(cached: &CachedRegularFile) -> bool {
    cached.size >= ASYNC_WRITEBACK_MIN_FILE
}

fn schedule_cached_writeback_if_ready(ino: u32) {
    let should_queue = {
        let Some(entry) = regular_cache_entry(ino) else {
            return;
        };
        let mut cached = entry.lock();
        if !cached.is_dirty()
            || cached.dirty.writeback_active()
            || !cached_file_async_writeback_ok(&cached)
        {
            false
        } else {
            cached.dirty.note_queued();
            true
        }
    };
    if should_queue {
        queue_writeback_ino(ino);
    }
}

fn wake_writeback_waiters() {
    crate::task::wait_queue::wake_io_waiters();
}

pub fn start_writeback_worker_explicit() -> Result<(), SysErrNo> {
    if DEFAULT_WRITEBACK_WORKER_ENABLED {
        log::warn!("[fs] writeback worker default startup flag is enabled unexpectedly");
    }
    if WRITEBACK_WORKER_STARTED.swap(true, Ordering::AcqRel) {
        return Ok(());
    }

    let worker = crate::task::TaskControlBlock::new_kernel_task(writeback_worker_main);
    debug_assert!(worker.is_kernel);
    let worker_pid = worker.pid.0;
    crate::task::manager::add_task(worker);
    wake_writeback_waiters();
    log::info!(
        "[fs] writeback worker started explicitly pid={}",
        worker_pid
    );
    Ok(())
}

fn queue_writeback_ino(ino: u32) {
    WRITEBACK_QUEUE.lock().push(ino);
    wake_writeback_waiters();
}

fn cancel_queued_writeback(ino: u32) {
    WRITEBACK_QUEUE.lock().remove(ino);
}

fn writeback_worker_main() -> ! {
    loop {
        if let Some(ino) = WRITEBACK_QUEUE.lock().pop() {
            if let Err(err) = drain_queued_writeback_ino(ino) {
                log::debug!("[fs] async writeback ino={} failed: {:?}", ino, err);
            }
            crate::task::suspend_current_and_run_next();
            continue;
        }

        let _ = crate::task::wait_queue::sleep_on_io_if(None, || {
            Ok(!WRITEBACK_QUEUE.lock().has_work())
        });
    }
}

fn opportunistic_drain_one_queued_writeback() {
    let Some(ino) = WRITEBACK_QUEUE.lock().pop() else {
        return;
    };
    let _ = drain_queued_writeback_ino(ino);
}

fn opportunistic_drain_queued_writeback_ino(ino: u32) {
    cancel_queued_writeback(ino);
    let _ = drain_queued_writeback_ino(ino);
}

fn mark_dirty_range(cached: &mut CachedRegularFile, start: usize, end: usize) {
    if start >= end {
        return;
    }
    let mut new_range = DirtyRange {
        start,
        end: end.min(cached.size),
    };
    if new_range.start >= new_range.end {
        return;
    }

    if let Some(last) = cached.dirty.ranges.last_mut() {
        if new_range.start >= last.start && new_range.end <= last.end {
            return;
        }
        if new_range.start >= last.start && new_range.start <= last.end {
            last.end = last.end.max(new_range.end);
            return;
        }
    }

    let mut index = 0;
    while index < cached.dirty.ranges.len() {
        let range = cached.dirty.ranges[index];
        if new_range.end < range.start {
            break;
        }
        if new_range.start > range.end {
            index += 1;
            continue;
        }
        new_range.start = new_range.start.min(range.start);
        new_range.end = new_range.end.max(range.end);
        cached.dirty.ranges.remove(index);
    }
    cached.dirty.ranges.insert(index, new_range);
    cached.dirty.refresh_writeback_state();
}

fn clip_dirty_ranges(cached: &mut CachedRegularFile, len: usize) {
    let mut clipped = Vec::new();
    for range in cached.dirty.ranges.iter().copied() {
        let end = range.end.min(len);
        if range.start < end {
            clipped.push(DirtyRange {
                start: range.start,
                end,
            });
        }
    }
    cached.dirty.ranges = clipped;
    cached.dirty.refresh_writeback_state();
}

fn regular_blocks(size: u64) -> u64 {
    size.div_ceil(512)
}

fn regular_cache_entry(ino: u32) -> Option<RegularCacheEntry> {
    REGULAR_FILE_CACHE.lock().get(&ino).cloned()
}

fn insert_regular_cache_entry(ino: u32, cached: CachedRegularFile) {
    let old = REGULAR_FILE_CACHE
        .lock()
        .insert(ino, Arc::new(Mutex::new(cached)));
    if let Some(entry) = old {
        entry.lock().evicted = true;
    }
}

fn clear_regular_file_cache() {
    let entries: Vec<RegularCacheEntry> = REGULAR_FILE_CACHE.lock().values().cloned().collect();
    for entry in entries {
        entry.lock().evicted = true;
    }
    REGULAR_FILE_CACHE.lock().clear();
}

fn clean_page_key(ino: u32, file_offset: usize) -> CleanPageKey {
    CleanPageKey {
        ino,
        page_idx: file_offset / PAGE_SIZE,
    }
}

fn clean_page_read_metadata(fs: &Ext4, ino: u32) -> Result<CleanPageReadMetadata, SysErrNo> {
    let inode = fs.get_inode_ref(ino).inode;
    if !inode.is_file() {
        return Err(SysErrNo::EINVAL);
    }
    Ok(CleanPageReadMetadata {
        file_size: inode.size() as usize,
    })
}

fn extent_pblock_for_read(fs: &Ext4, ino: u32, lblock: u32) -> Option<u64> {
    let inode_ref = fs.get_inode_ref(ino);
    if !inode_ref.inode.is_file() {
        return None;
    }
    let path = fs.find_extent(&inode_ref, lblock).ok()?;
    let node = path.path.last()?;
    let extent = node.extent?;
    let first = extent.get_first_block();
    let len = extent.get_actual_len() as u32;
    if len == 0 || lblock < first || lblock >= first.saturating_add(len) {
        return None;
    }
    Some((lblock - first) as u64 + extent.get_pblock())
}

fn cached_pblock_for_clean_read(fs: &Ext4, ino: u32, lblock: u32, max_run_len: u32) -> Option<u64> {
    if max_run_len == 0 {
        return None;
    }
    if let Some(pblock) = PBLOCK_RUN_CACHE.lock().get(ino, lblock) {
        return Some(pblock);
    }

    let first_pblock = extent_pblock_for_read(fs, ino, lblock)?;
    let mut len = 1u32;
    let mut prev_pblock = first_pblock;
    while len < max_run_len {
        let Some(next_lblock) = lblock.checked_add(len) else {
            break;
        };
        let Some(next_pblock) = extent_pblock_for_read(fs, ino, next_lblock) else {
            break;
        };
        if prev_pblock.checked_add(1) != Some(next_pblock) {
            break;
        }
        len += 1;
        prev_pblock = next_pblock;
    }
    PBLOCK_RUN_CACHE
        .lock()
        .insert(ino, lblock, len, first_pblock);
    Some(first_pblock)
}

fn try_fill_clean_page_frame_fast(
    fs: &Ext4,
    ino: u32,
    page_idx: usize,
    file_size: usize,
    read_len: usize,
    page: &mut [u8],
) -> bool {
    if PAGE_SIZE != BLOCK_SIZE || fs.super_block.block_size() as usize != BLOCK_SIZE {
        return false;
    }
    if page_idx > u32::MAX as usize || read_len > PAGE_SIZE {
        return false;
    }
    let total_blocks = file_size.div_ceil(BLOCK_SIZE);
    let remaining_blocks = total_blocks.saturating_sub(page_idx);
    let max_run_len = remaining_blocks.min(PBLOCK_RUN_LOOKAHEAD as usize) as u32;
    let Some(pblock) = cached_pblock_for_clean_read(fs, ino, page_idx as u32, max_run_len) else {
        return false;
    };
    if pblock > (usize::MAX / BLOCK_SIZE) as u64 {
        return false;
    }
    let data = fs.block_device.read_offset(pblock as usize * BLOCK_SIZE);
    if data.len() < PAGE_SIZE {
        return false;
    }
    page[..read_len].copy_from_slice(&data[..read_len]);
    true
}

fn fill_clean_page_frame(
    fs: &Ext4,
    ino: u32,
    page_idx: usize,
    metadata: Option<CleanPageReadMetadata>,
) -> Result<FrameTracker, SysErrNo> {
    let frame = frame_allocator::alloc_frame().ok_or(SysErrNo::ENOMEM)?;
    let page_start = page_idx.checked_mul(PAGE_SIZE).ok_or(SysErrNo::EFBIG)?;
    let metadata = match metadata {
        Some(metadata) => metadata,
        None => clean_page_read_metadata(fs, ino)?,
    };
    let size = metadata.file_size;
    if page_start >= size {
        return Ok(frame);
    }

    let read_len = PAGE_SIZE.min(size - page_start);
    let page = unsafe { core::slice::from_raw_parts_mut(frame.ppn().addr() as *mut u8, PAGE_SIZE) };
    if try_fill_clean_page_frame_fast(fs, ino, page_idx, size, read_len, page) {
        return Ok(frame);
    }
    let mut copied = 0usize;
    while copied < read_len {
        let n = extent_aware_read_at(ino, page_start + copied, &mut page[copied..read_len])?;
        if n == 0 {
            return Err(SysErrNo::EIO);
        }
        copied += n;
    }
    Ok(frame)
}

fn clean_page_cache_frame_with_metadata(
    fs: &Ext4,
    ino: u32,
    file_offset: usize,
    metadata: Option<CleanPageReadMetadata>,
) -> Result<FrameTracker, SysErrNo> {
    let key = clean_page_key(ino, file_offset);
    clean_page_cache_frame_with_key(fs, key, metadata)
}

fn clean_page_cache_frame_with_key(
    fs: &Ext4,
    key: CleanPageKey,
    metadata: Option<CleanPageReadMetadata>,
) -> Result<FrameTracker, SysErrNo> {
    if let Some(frame) = CLEAN_PAGE_CACHE.lock().get(key) {
        return Ok(frame);
    }

    let frame = fill_clean_page_frame(fs, key.ino, key.page_idx, metadata)?;
    let mut cache = CLEAN_PAGE_CACHE.lock();
    if let Some(existing) = cache.get(key) {
        return Ok(existing);
    }
    cache.insert(key, frame.clone());
    Ok(frame)
}

fn prefetch_clean_page_frames(
    fs: &Ext4,
    ino: u32,
    start_page_idx: usize,
    page_count: usize,
    metadata: CleanPageReadMetadata,
) -> Result<(), SysErrNo> {
    if page_count < 2
        || PAGE_SIZE != BLOCK_SIZE
        || fs.super_block.block_size() as usize != BLOCK_SIZE
    {
        return Ok(());
    }

    let page_count = CLEAN_PAGE_CACHE
        .lock()
        .uncached_prefix_len(ino, start_page_idx, page_count);
    if page_count < 2 {
        return Ok(());
    }

    let mut keys = Vec::new();
    let mut offsets = Vec::new();
    let mut read_lens = Vec::new();
    for page in 0..page_count {
        let Some(page_idx) = start_page_idx.checked_add(page) else {
            break;
        };
        let Some(page_start) = page_idx.checked_mul(PAGE_SIZE) else {
            break;
        };
        if page_start >= metadata.file_size || page_idx > u32::MAX as usize {
            break;
        }
        let key = CleanPageKey { ino, page_idx };
        let Some(pblock) = extent_pblock_for_read(fs, ino, page_idx as u32) else {
            break;
        };
        if pblock > (usize::MAX / BLOCK_SIZE) as u64 {
            break;
        }
        keys.push(key);
        offsets.push(pblock as usize * BLOCK_SIZE);
        read_lens.push(PAGE_SIZE.min(metadata.file_size - page_start));
    }
    if keys.len() < 2 {
        return Ok(());
    }

    let blocks = crate::fs::block_dev::read_root_blocks(&offsets)?;
    if blocks.len() != keys.len() {
        return Err(SysErrNo::EIO);
    }
    let mut frames = Vec::with_capacity(keys.len());
    for (block, read_len) in blocks.iter().zip(read_lens.iter().copied()) {
        if block.len() < read_len {
            return Err(SysErrNo::EIO);
        }
        let frame = frame_allocator::alloc_frame().ok_or(SysErrNo::ENOMEM)?;
        unsafe {
            core::ptr::copy_nonoverlapping(block.as_ptr(), frame.ppn().addr() as *mut u8, read_len);
        }
        frames.push(frame);
    }

    let mut cache = CLEAN_PAGE_CACHE.lock();
    for (key, frame) in keys.into_iter().zip(frames) {
        if cache.get(key).is_none() {
            cache.insert(key, frame);
        }
    }
    Ok(())
}

pub fn clean_page_cache_frame(ino: u32, file_offset: usize) -> Result<FrameTracker, SysErrNo> {
    let key = clean_page_key(ino, file_offset);
    if let Some(frame) = CLEAN_PAGE_CACHE.lock().get(key) {
        return Ok(frame);
    }
    let fs = ROOT_EXT4.lock().clone().ok_or(SysErrNo::ENOENT)?;
    clean_page_cache_frame_with_key(&fs, key, None)
}

pub fn clean_page_cache_frames(
    ino: u32,
    file_offset: usize,
    max_pages: usize,
    read_ahead_pages: usize,
) -> Result<Vec<FrameTracker>, SysErrNo> {
    if max_pages == 0 {
        return Ok(Vec::new());
    }

    let read_ahead_pages = read_ahead_pages.max(1).min(max_pages);
    let fs = ROOT_EXT4.lock().clone().ok_or(SysErrNo::ENOENT)?;
    let metadata = clean_page_read_metadata(&fs, ino)?;
    let start_page_idx = file_offset / PAGE_SIZE;
    let _ = prefetch_clean_page_frames(&fs, ino, start_page_idx, read_ahead_pages, metadata);
    let mut source = None;
    let mut frames = CLEAN_PAGE_CACHE
        .lock()
        .get_run(ino, start_page_idx, max_pages);
    for page in frames.len()..max_pages {
        let Some(delta) = page.checked_mul(PAGE_SIZE) else {
            break;
        };
        let Some(offset) = file_offset.checked_add(delta) else {
            break;
        };
        let key = clean_page_key(ino, offset);
        if page >= read_ahead_pages {
            break;
        }
        if source.is_none() {
            source = Some((fs.clone(), metadata));
        }
        let (fs, metadata) = source.as_ref().ok_or(SysErrNo::EIO)?;
        match clean_page_cache_frame_with_key(fs, key, Some(*metadata)) {
            Ok(frame) => frames.push(frame),
            Err(error) if page == 0 => return Err(error),
            Err(_) => break,
        }
    }
    Ok(frames)
}

fn clean_page_cached_read(ino: u32, offset: usize, buf: &mut [u8]) -> Result<usize, SysErrNo> {
    if buf.is_empty() {
        return Ok(0);
    }
    let fs = ROOT_EXT4.lock().clone().ok_or(SysErrNo::ENOENT)?;
    let metadata = clean_page_read_metadata(&fs, ino)?;
    let size = metadata.file_size;
    if offset >= size {
        return Ok(0);
    }
    let total = buf.len().min(size - offset);
    let mut copied = 0usize;
    while copied < total {
        let current = offset + copied;
        let page_off = current % PAGE_SIZE;
        let n = (total - copied).min(PAGE_SIZE - page_off);
        let frame = clean_page_cache_frame_with_metadata(&fs, ino, current, Some(metadata))?;
        let src = (frame.ppn().addr() + page_off) as *const u8;
        unsafe {
            core::ptr::copy_nonoverlapping(src, buf[copied..].as_mut_ptr(), n);
        }
        copied += n;
    }
    Ok(copied)
}

fn extent_aware_read_at(ino: u32, offset: usize, buf: &mut [u8]) -> Result<usize, SysErrNo> {
    if buf.is_empty() {
        return Ok(0);
    }
    let fs = ROOT_EXT4.lock().clone().ok_or(SysErrNo::ENOENT)?;
    let metadata = clean_page_read_metadata(&fs, ino)?;
    let size = metadata.file_size;
    if offset >= size {
        return Ok(0);
    }

    let total = buf.len().min(size - offset);
    buf[..total].fill(0);
    let mut copied = 0usize;
    while copied < total {
        let current = offset + copied;
        let block_off = current % BLOCK_SIZE;
        let lblock = current / BLOCK_SIZE;
        let n = (total - copied).min(BLOCK_SIZE - block_off);
        if lblock > u32::MAX as usize {
            return Err(SysErrNo::EFBIG);
        }
        if let Some(pblock) = extent_pblock_for_read(&fs, ino, lblock as u32) {
            if pblock > (usize::MAX / BLOCK_SIZE) as u64 {
                return Err(SysErrNo::EFBIG);
            }
            let data = fs.block_device.read_offset(pblock as usize * BLOCK_SIZE);
            if data.len() < block_off + n {
                return Err(SysErrNo::EIO);
            }
            buf[copied..copied + n].copy_from_slice(&data[block_off..block_off + n]);
        }
        copied += n;
    }
    Ok(total)
}

fn invalidate_clean_pages_ino(ino: u32) {
    CLEAN_PAGE_CACHE.lock().invalidate_ino(ino);
}

fn invalidate_clean_pages_range(ino: u32, start: usize, end: usize) {
    CLEAN_PAGE_CACHE.lock().invalidate_range(ino, start, end);
}

fn clear_clean_page_cache() {
    CLEAN_PAGE_CACHE.lock().clear();
}

fn invalidate_pblock_runs_ino(ino: u32) {
    PBLOCK_RUN_CACHE.lock().invalidate_ino(ino);
}

fn clear_pblock_run_cache() {
    PBLOCK_RUN_CACHE.lock().clear();
}

fn ensure_regular_cache(ino: u32) -> Result<bool, SysErrNo> {
    if regular_cache_entry(ino).is_some() {
        return Ok(true);
    }
    let fs = ROOT_EXT4.lock().clone().ok_or(SysErrNo::ENOENT)?;
    let inode = fs.get_inode_ref(ino).inode;
    let size = inode.size() as usize;
    let dense = if size <= DENSE_REGULAR_CACHE_LIMIT {
        let mut data = alloc::vec![0u8; size];
        let mut offset = 0usize;
        while offset < size {
            let read = extent_aware_read_at(ino, offset, &mut data[offset..])?;
            if read == 0 {
                return Err(SysErrNo::EIO);
            }
            offset += read;
        }
        Some(data)
    } else {
        None
    };
    let cached = CachedRegularFile::new(
        size,
        size,
        dense,
        PageCacheDirtyModel::clean(),
        inode.mtime(),
        inode.i_mtime_extra(),
        inode.ctime(),
        inode.i_ctime_extra(),
    );
    let mut cache = REGULAR_FILE_CACHE.lock();
    if !cache.contains_key(&ino) {
        cache.insert(ino, Arc::new(Mutex::new(cached)));
    }
    Ok(true)
}

fn copy_cached_range(
    ino: u32,
    cached: &CachedRegularFile,
    start: usize,
    out: &mut [u8],
) -> Result<(), SysErrNo> {
    if let Some(dense) = cached.dense.as_ref() {
        let end = start.checked_add(out.len()).ok_or(SysErrNo::EFBIG)?;
        let source = dense.get(start..end).ok_or(SysErrNo::EIO)?;
        out.copy_from_slice(source);
        return Ok(());
    }
    let mut copied = 0usize;
    while copied < out.len() {
        let current = start + copied;
        let page_idx = current / PAGE_SIZE;
        let page_off = current % PAGE_SIZE;
        let count = (out.len() - copied).min(PAGE_SIZE - page_off);
        if let Some(frame) = cached.pages.get(&page_idx) {
            unsafe {
                core::ptr::copy_nonoverlapping(
                    (frame.ppn().addr() + page_off) as *const u8,
                    out[copied..].as_mut_ptr(),
                    count,
                );
            }
        } else if current < cached.persisted_size {
            let disk_count = count.min(cached.persisted_size - current);
            let n = extent_aware_read_at(ino, current, &mut out[copied..copied + disk_count])?;
            if n != disk_count {
                return Err(SysErrNo::EIO);
            }
            out[copied + disk_count..copied + count].fill(0);
        } else {
            out[copied..copied + count].fill(0);
        }
        copied += count;
    }
    Ok(())
}

fn promote_dense_regular_cache(cached: &mut CachedRegularFile) -> Result<(), SysErrNo> {
    let Some(dense) = cached.dense.as_ref() else {
        return Ok(());
    };
    let mut pages = BTreeMap::new();
    for (page_idx, chunk) in dense.chunks(PAGE_SIZE).enumerate() {
        let frame = frame_allocator::alloc_frame().ok_or(SysErrNo::ENOMEM)?;
        unsafe {
            core::ptr::copy_nonoverlapping(
                chunk.as_ptr(),
                frame.ppn().addr() as *mut u8,
                chunk.len(),
            );
        }
        pages.insert(page_idx, frame);
    }
    cached.pages = pages;
    cached.dense = None;
    Ok(())
}

fn cached_page_for_write(
    ino: u32,
    cached: &mut CachedRegularFile,
    page_idx: usize,
    preserve_contents: bool,
) -> Result<FrameTracker, SysErrNo> {
    if let Some(frame) = cached.pages.get(&page_idx) {
        return Ok(frame.clone());
    }
    let frame = frame_allocator::alloc_frame().ok_or(SysErrNo::ENOMEM)?;
    if preserve_contents {
        let page_start = page_idx.checked_mul(PAGE_SIZE).ok_or(SysErrNo::EFBIG)?;
        if page_start < cached.persisted_size {
            let read_len = PAGE_SIZE.min(cached.persisted_size - page_start);
            let page = unsafe {
                core::slice::from_raw_parts_mut(frame.ppn().addr() as *mut u8, PAGE_SIZE)
            };
            let n = extent_aware_read_at(ino, page_start, &mut page[..read_len])?;
            if n != read_len {
                return Err(SysErrNo::EIO);
            }
        }
    }
    cached.pages.insert(page_idx, frame.clone());
    Ok(frame)
}

fn cached_regular_read(ino: u32, offset: usize, buf: &mut [u8]) -> Option<Result<usize, SysErrNo>> {
    let entry = regular_cache_entry(ino)?;
    let cached = entry.lock();
    if cached.evicted {
        return None;
    }
    if offset >= cached.size {
        return Some(Ok(0));
    }
    let n = buf.len().min(cached.size - offset);
    Some(copy_cached_range(ino, &cached, offset, &mut buf[..n]).map(|_| n))
}

fn cached_regular_size(ino: u32) -> Option<usize> {
    let entry = regular_cache_entry(ino)?;
    let cached = entry.lock();
    (!cached.evicted).then_some(cached.size)
}

fn cached_regular_snapshot(ino: u32) -> Option<Vec<u8>> {
    let entry = regular_cache_entry(ino)?;
    let cached = entry.lock();
    if cached.evicted {
        None
    } else {
        let mut data = alloc::vec![0u8; cached.size];
        copy_cached_range(ino, &cached, 0, &mut data).ok()?;
        Some(data)
    }
}

fn cached_regular_info(ino: u32) -> Option<CachedRegularInfo> {
    let entry = regular_cache_entry(ino)?;
    let cached = entry.lock();
    if cached.evicted {
        return None;
    }
    Some(CachedRegularInfo {
        size: cached.size,
        mtime_sec: cached.mtime_sec,
        mtime_extra: cached.mtime_extra,
        ctime_sec: cached.ctime_sec,
        ctime_extra: cached.ctime_extra,
    })
}

fn cached_regular_resize(ino: u32, new_len: usize) -> Result<(), SysErrNo> {
    invalidate_executable_image(ino);
    cancel_queued_writeback(ino);
    invalidate_clean_pages_ino(ino);
    invalidate_pblock_runs_ino(ino);
    let entry = regular_cache_entry(ino).ok_or(SysErrNo::ENOENT)?;
    let mut cached = entry.lock();
    if cached.evicted {
        return Err(SysErrNo::ENOENT);
    }
    let old_len = cached.size;
    if new_len > DENSE_REGULAR_CACHE_LIMIT && cached.dense.is_some() {
        promote_dense_regular_cache(&mut cached)?;
    }
    if new_len < old_len {
        cached.persisted_size = cached.persisted_size.min(new_len);
        cached.truncate_to = Some(
            cached
                .truncate_to
                .map_or(new_len, |pending| pending.min(new_len)),
        );
        if let Some(dense) = cached.dense.as_mut() {
            dense.truncate(new_len);
        } else {
            let first_removed_page = new_len.div_ceil(PAGE_SIZE);
            drop(cached.pages.split_off(&first_removed_page));
            if new_len % PAGE_SIZE != 0 {
                if let Some(frame) = cached.pages.get(&(new_len / PAGE_SIZE)) {
                    unsafe {
                        core::ptr::write_bytes(
                            (frame.ppn().addr() + new_len % PAGE_SIZE) as *mut u8,
                            0,
                            PAGE_SIZE - new_len % PAGE_SIZE,
                        );
                    }
                }
            }
        }
        clip_dirty_ranges(&mut cached, new_len);
    } else if new_len > old_len {
        if let Some(dense) = cached.dense.as_mut() {
            dense.resize(new_len, 0);
        }
    }
    cached.size = new_len;
    if new_len > old_len {
        // ext4_rs treats logical blocks below i_size as already allocated.
        // Materialize a grown range before committing the final size so a
        // later write into that range cannot silently target an extent hole.
        mark_dirty_range(&mut cached, old_len, new_len);
    }
    cached.dirty.size_dirty = true;
    note_cached_data_write(&mut cached);
    drop(cached);
    schedule_cached_writeback_if_ready(ino);
    Ok(())
}

fn cached_regular_write(ino: u32, offset: usize, buf: &[u8]) -> Result<usize, SysErrNo> {
    invalidate_executable_image(ino);
    let end = offset + buf.len();
    invalidate_clean_pages_range(ino, offset, end);
    invalidate_pblock_runs_ino(ino);
    let entry = regular_cache_entry(ino).ok_or(SysErrNo::ENOENT)?;
    let mut cached = entry.lock();
    if cached.evicted {
        return Err(SysErrNo::ENOENT);
    }
    let old_len = cached.size;
    if end > DENSE_REGULAR_CACHE_LIMIT && cached.dense.is_some() {
        promote_dense_regular_cache(&mut cached)?;
    }
    let copied = if let Some(dense) = cached.dense.as_mut() {
        if end > dense.len() {
            dense.resize(end, 0);
        }
        dense[offset..end].copy_from_slice(buf);
        buf.len()
    } else {
        let mut copied = 0usize;
        while copied < buf.len() {
            let current = offset + copied;
            let page_idx = current / PAGE_SIZE;
            let page_off = current % PAGE_SIZE;
            let count = (buf.len() - copied).min(PAGE_SIZE - page_off);
            let preserve_contents = page_off != 0 || count != PAGE_SIZE;
            let frame = match cached_page_for_write(ino, &mut cached, page_idx, preserve_contents) {
                Ok(frame) => frame,
                Err(err) if copied == 0 => return Err(err),
                Err(_) => break,
            };
            unsafe {
                core::ptr::copy_nonoverlapping(
                    buf[copied..].as_ptr(),
                    (frame.ppn().addr() + page_off) as *mut u8,
                    count,
                );
            }
            copied += count;
        }
        copied
    };
    let written_end = offset + copied;
    cached.size = cached.size.max(written_end);
    mark_dirty_range(&mut cached, offset, written_end);
    if written_end > old_len {
        cached.dirty.size_dirty = true;
    }
    note_cached_data_write(&mut cached);
    drop(cached);
    schedule_cached_writeback_if_ready(ino);
    Ok(copied)
}

fn is_regular_cached(ino: u32) -> bool {
    REGULAR_FILE_CACHE.lock().contains_key(&ino)
}

fn is_regular_cache_dirty(ino: u32) -> bool {
    regular_cache_entry(ino)
        .map(|entry| entry.lock().is_dirty())
        .unwrap_or(false)
}

fn invalidate_executable_image(ino: u32) {
    EXECUTABLE_IMAGE_CACHE.lock().remove(&ino);
}

fn cached_executable_image(ino: u32) -> Option<Arc<Vec<u8>>> {
    let image = EXECUTABLE_IMAGE_CACHE.lock().get(&ino).cloned();
    #[cfg(feature = "buildstorm-diagnostics")]
    if image.is_some() {
        EXECUTABLE_IMAGE_CACHE_HITS.fetch_add(1, Ordering::Relaxed);
    } else {
        EXECUTABLE_IMAGE_CACHE_MISSES.fetch_add(1, Ordering::Relaxed);
    }
    image
}

fn cache_executable_image(ino: u32, data: Arc<Vec<u8>>) {
    if data.len() > EXECUTABLE_IMAGE_CACHE_LIMIT {
        return;
    }
    let mut cache = EXECUTABLE_IMAGE_CACHE.lock();
    let current_bytes = cache
        .values()
        .fold(0usize, |total, image| total.saturating_add(image.len()));
    if current_bytes.saturating_add(data.len()) > EXECUTABLE_IMAGE_CACHE_LIMIT {
        cache.clear();
    }
    cache.insert(ino, data);
}

#[cfg(feature = "buildstorm-diagnostics")]
pub fn diagnostic_regular_cache_stats() -> (usize, usize, usize) {
    let entries: Vec<RegularCacheEntry> = REGULAR_FILE_CACHE.lock().values().cloned().collect();
    let mut dirty = 0usize;
    let mut bytes = 0usize;
    for entry in &entries {
        let cached = entry.lock();
        if !cached.evicted {
            dirty += usize::from(cached.is_dirty());
            bytes = bytes.saturating_add(
                cached
                    .dense
                    .as_ref()
                    .map_or_else(|| cached.pages.len().saturating_mul(PAGE_SIZE), Vec::len),
            );
        }
    }
    (entries.len(), dirty, bytes)
}

#[cfg(feature = "buildstorm-diagnostics")]
pub fn diagnostic_executable_cache_stats() -> (usize, usize, usize, usize) {
    let cache = EXECUTABLE_IMAGE_CACHE.lock();
    let bytes = cache
        .values()
        .fold(0usize, |total, image| total.saturating_add(image.len()));
    (
        cache.len(),
        bytes,
        EXECUTABLE_IMAGE_CACHE_HITS.load(Ordering::Relaxed),
        EXECUTABLE_IMAGE_CACHE_MISSES.load(Ordering::Relaxed),
    )
}

pub fn can_use_clean_page_cache(ino: u32) -> bool {
    !is_regular_cached(ino)
}

fn discard_regular_cache(ino: u32) {
    invalidate_executable_image(ino);
    cancel_queued_writeback(ino);
    if let Some(entry) = regular_cache_entry(ino) {
        entry.lock().evicted = true;
        let mut cache = REGULAR_FILE_CACHE.lock();
        let should_remove = cache
            .get(&ino)
            .map(|current| Arc::ptr_eq(current, &entry))
            .unwrap_or(false);
        if should_remove {
            cache.remove(&ino);
        }
    }
    invalidate_clean_pages_ino(ino);
    invalidate_pblock_runs_ino(ino);
    DATA_TIME_OVERRIDES.lock().remove(&ino);
    invalidate_metadata_ino(ino);
}

fn has_open_regular_ref(ino: u32) -> bool {
    OPEN_REGULAR_REFS.lock().get(&ino).copied().unwrap_or(0) > 0
}

pub fn open_regular_ino(ino: u32) {
    let mut refs = OPEN_REGULAR_REFS.lock();
    *refs.entry(ino).or_insert(0) += 1;
}

pub fn close_regular_ino(ino: u32) {
    let last_ref = {
        let mut refs = OPEN_REGULAR_REFS.lock();
        let Some(count) = refs.get_mut(&ino) else {
            return;
        };
        if *count > 1 {
            *count -= 1;
            false
        } else {
            refs.remove(&ino);
            true
        }
    };
    if !last_ref {
        return;
    }
    if PENDING_UNLINK_REGULAR.lock().remove(&ino) {
        cancel_queued_writeback(ino);
        let _ = finish_unlinked_regular(ino);
        return;
    }

    // Whole-file caching keeps linker output coherent until close, but a
    // compiler workload creates many short-lived files. Flush and evict the
    // last closed instance so clean/dirty Vec buffers cannot accumulate for
    // the lifetime of the mounted filesystem. Serialize reclamation to bound
    // the temporary writeback snapshot peak under parallel closes.
    //
    // Most descriptors use the extent/page cache rather than whole-file
    // caching. Do not queue those closes behind an unrelated dirty-file
    // writeback. Recheck after taking the lock because another close may have
    // evicted the entry in the meantime.
    if !is_regular_cached(ino) {
        #[cfg(feature = "buildstorm-diagnostics")]
        crate::buildstorm_diagnostics::note_phase(21, 0);
        return;
    }
    if is_regular_cache_dirty(ino) {
        schedule_cached_writeback_if_ready(ino);
        #[cfg(feature = "buildstorm-diagnostics")]
        crate::buildstorm_diagnostics::note_phase(22, 0);
        return;
    }
    #[cfg(feature = "buildstorm-diagnostics")]
    let started_at = crate::timer::get_time_us();
    let _reclaim = REGULAR_CACHE_CLOSE_LOCK.lock();
    #[cfg(feature = "buildstorm-diagnostics")]
    crate::buildstorm_diagnostics::note_phase(
        17,
        crate::timer::get_time_us().saturating_sub(started_at),
    );
    if has_open_regular_ref(ino) {
        return;
    }
    if !is_regular_cached(ino) {
        #[cfg(feature = "buildstorm-diagnostics")]
        crate::buildstorm_diagnostics::note_phase(21, 0);
        return;
    }
    // A read-only cache has no persistence work. In particular, shell scripts
    // and shared objects are opened and closed heavily during process startup;
    // sending every clean last-close through the writeback path creates lock
    // traffic and can couple close() to filesystem mutation ordering.
    if is_regular_cache_dirty(ino) {
        schedule_cached_writeback_if_ready(ino);
        #[cfg(feature = "buildstorm-diagnostics")]
        crate::buildstorm_diagnostics::note_phase(22, 0);
        return;
    }
    let refs = OPEN_REGULAR_REFS.lock();
    if refs.get(&ino).copied().unwrap_or(0) == 0 {
        #[cfg(feature = "buildstorm-diagnostics")]
        let started_at = crate::timer::get_time_us();
        discard_regular_cache(ino);
        #[cfg(feature = "buildstorm-diagnostics")]
        crate::buildstorm_diagnostics::note_phase(
            19,
            crate::timer::get_time_us().saturating_sub(started_at),
        );
    }
}

fn finish_unlinked_regular(ino: u32) -> Result<(), SysErrNo> {
    let _mutation = ext4_mutation_lock!();
    cancel_queued_writeback(ino);
    discard_regular_cache(ino);
    let fs = ROOT_EXT4.lock().clone().ok_or(SysErrNo::ENOENT)?;
    let mut iref = fs.get_inode_ref(ino);
    if iref.inode.size() > 0 {
        fs.truncate_inode(&mut iref, 0).map_err(map_ext4_err)?;
    }
    let now = current_ext4_time();
    iref.inode.set_links_count(0);
    iref.inode.set_ctime(now);
    iref.inode.set_dtime(now);
    fs.write_back_inode(&mut iref);
    fs.ialloc_free_inode(ino, false);
    Ok(())
}

fn cache_empty_regular(ino: u32, dirty: bool) {
    invalidate_executable_image(ino);
    invalidate_pblock_runs_ino(ino);
    let now = current_ext4_time();
    insert_regular_cache_entry(
        ino,
        CachedRegularFile::new(
            0,
            0,
            Some(Vec::new()),
            if dirty {
                PageCacheDirtyModel::dirty_whole_file()
            } else {
                PageCacheDirtyModel::clean()
            },
            now,
            0,
            now,
            0,
        ),
    );
}

fn uncached_regular_write(ino: u32, offset: usize, buf: &[u8]) -> Result<usize, SysErrNo> {
    if buf.is_empty() {
        return Ok(0);
    }
    invalidate_executable_image(ino);
    let _mutation = ext4_mutation_lock!();
    invalidate_pblock_runs_ino(ino);
    let fs = ROOT_EXT4.lock().clone().ok_or(SysErrNo::ENOENT)?;
    let written = fs.write_at(ino, offset, buf).map_err(map_ext4_err)?;
    if written != 0 {
        invalidate_clean_pages_range(ino, offset, offset.saturating_add(written));
        invalidate_pblock_runs_ino(ino);
        note_data_write(ino);
    }
    Ok(written)
}

fn flush_time_override(ino: u32) -> Result<(), SysErrNo> {
    let Some(times) = DATA_TIME_OVERRIDES.lock().get(&ino).copied() else {
        return Ok(());
    };
    let _mutation = ext4_mutation_lock!();
    let (mtime_sec, mtime_extra, ctime_sec, ctime_extra) = times;
    let fs = ROOT_EXT4.lock().clone().ok_or(SysErrNo::ENOENT)?;
    let mut iref = fs.get_inode_ref(ino);
    iref.inode.set_mtime(mtime_sec);
    iref.inode.set_i_mtime_extra(mtime_extra);
    iref.inode.set_ctime(ctime_sec);
    iref.inode.set_i_ctime_extra(ctime_extra);
    fs.write_back_inode(&mut iref);
    let mut overrides = DATA_TIME_OVERRIDES.lock();
    if overrides.get(&ino).copied() == Some(times) {
        overrides.remove(&ino);
    }
    invalidate_metadata_ino(ino);
    Ok(())
}

fn take_writeback_snapshot(ino: u32) -> WritebackSnapshotResult {
    let Some(entry) = regular_cache_entry(ino) else {
        return WritebackSnapshotResult::NoCache;
    };
    let mut cached = entry.lock();
    if cached.evicted {
        return WritebackSnapshotResult::NoCache;
    }
    if !cached.is_dirty() {
        return WritebackSnapshotResult::Clean;
    }
    if let Some(err) = cached.dirty.last_error {
        return WritebackSnapshotResult::Failed(err);
    }
    if cached.dirty.full_dirty {
        cached.dirty.ranges.clear();
        let size = cached.size;
        if size != 0 {
            cached.dirty.ranges.push(DirtyRange {
                start: 0,
                end: size,
            });
        }
        cached.dirty.full_dirty = false;
    }
    let Some(dirty_seq) = cached.dirty.begin_writeback() else {
        return WritebackSnapshotResult::Busy;
    };

    let range = cached
        .dirty
        .ranges
        .first()
        .copied()
        .map(|range| DirtyRange {
            start: range.start,
            end: range
                .end
                .min(range.start.saturating_add(WRITEBACK_CLUSTER_BYTES)),
        });
    let commit_size = match (range, cached.dirty.ranges.as_slice()) {
        (None, []) => true,
        (Some(completed), [only]) => completed.start == only.start && completed.end == only.end,
        _ => false,
    };
    let mut data = if let Some(range) = range {
        alloc::vec![0u8; range.end - range.start]
    } else {
        Vec::new()
    };
    if let Some(range) = range {
        if let Err(err) = copy_cached_range(ino, &cached, range.start, &mut data) {
            cached.dirty.fail_writeback(err);
            return WritebackSnapshotResult::Failed(err);
        }
    }

    WritebackSnapshotResult::Snapshot(WritebackSnapshot {
        ino,
        target_len: cached.size,
        truncate_to: cached.truncate_to,
        range,
        data,
        commit_size,
        dirty_seq,
        mtime_sec: cached.mtime_sec,
        mtime_extra: cached.mtime_extra,
        ctime_sec: cached.ctime_sec,
        ctime_extra: cached.ctime_extra,
    })
}

fn apply_writeback_snapshot(snapshot: &WritebackSnapshot) -> Result<(), SysErrNo> {
    let _mutation = ext4_mutation_lock!();
    invalidate_pblock_runs_ino(snapshot.ino);
    let fs = ROOT_EXT4.lock().clone().ok_or(SysErrNo::ENOENT)?;
    let old_size = fs.get_inode_ref(snapshot.ino).inode.size();
    let truncate_to = snapshot
        .truncate_to
        .unwrap_or(snapshot.target_len)
        .min(snapshot.target_len);
    if (truncate_to as u64) < old_size {
        let mut iref = fs.get_inode_ref(snapshot.ino);
        fs.truncate_inode(&mut iref, truncate_to as u64)
            .map_err(map_ext4_err)?;
    }

    if let Some(range) = snapshot.range {
        let written = fs
            .write_at(snapshot.ino, range.start, &snapshot.data)
            .map_err(map_ext4_err)?;
        if written != snapshot.data.len() {
            return Err(SysErrNo::EIO);
        }
    }

    if snapshot.commit_size {
        let current_size = fs.get_inode_ref(snapshot.ino).inode.size();
        if snapshot.target_len as u64 > current_size {
            let mut iref = fs.get_inode_ref(snapshot.ino);
            iref.inode.set_size(snapshot.target_len as u64);
            fs.write_back_inode(&mut iref);
        }
    }

    if snapshot.target_len == 0 && old_size != 0 {
        let current_size = fs.get_inode_ref(snapshot.ino).inode.size();
        if current_size != 0 {
            return Err(SysErrNo::EIO);
        }
    }

    let mut iref = fs.get_inode_ref(snapshot.ino);
    iref.inode.set_mtime(snapshot.mtime_sec);
    iref.inode.set_i_mtime_extra(snapshot.mtime_extra);
    iref.inode.set_ctime(snapshot.ctime_sec);
    iref.inode.set_i_ctime_extra(snapshot.ctime_extra);
    fs.write_back_inode(&mut iref);
    DATA_TIME_OVERRIDES.lock().remove(&snapshot.ino);
    invalidate_pblock_runs_ino(snapshot.ino);
    invalidate_metadata_ino(snapshot.ino);
    Ok(())
}

fn finish_writeback_snapshot(
    snapshot: &WritebackSnapshot,
    result: Result<(), SysErrNo>,
) -> Result<(), SysErrNo> {
    let mut requeue = false;
    {
        if let Some(entry) = regular_cache_entry(snapshot.ino) {
            let mut cached = entry.lock();
            if cached.evicted {
                return result;
            }
            if result.is_ok() {
                if cached.dirty.seq == snapshot.dirty_seq
                    && cached.truncate_to == snapshot.truncate_to
                {
                    cached.truncate_to = None;
                }
                requeue = cached.dirty.finish_writeback(
                    snapshot.dirty_seq,
                    snapshot.range,
                    snapshot.commit_size,
                );
                if !requeue {
                    cached.persisted_size = snapshot.target_len;
                } else {
                    cached.persisted_size = cached.persisted_size.min(snapshot.target_len);
                }
            } else {
                cached
                    .dirty
                    .fail_writeback(result.as_ref().err().copied().unwrap_or(SysErrNo::EIO));
            }
        }
    }
    if requeue {
        schedule_cached_writeback_if_ready(snapshot.ino);
    }
    result
}

fn acknowledge_writeback_error(ino: u32, err: SysErrNo) -> bool {
    if let Some(entry) = regular_cache_entry(ino) {
        let mut cached = entry.lock();
        if cached.evicted {
            return false;
        }
        if cached.dirty.last_error == Some(err) {
            cached.dirty.last_error = None;
        }
        cached.dirty.is_dirty()
    } else {
        false
    }
}

fn drain_queued_writeback_ino(ino: u32) -> Result<(), SysErrNo> {
    match take_writeback_snapshot(ino) {
        WritebackSnapshotResult::NoCache | WritebackSnapshotResult::Clean => Ok(()),
        WritebackSnapshotResult::Failed(err) => Err(err),
        WritebackSnapshotResult::Busy => {
            queue_writeback_ino(ino);
            Ok(())
        }
        WritebackSnapshotResult::Snapshot(snapshot) => {
            let result = apply_writeback_snapshot(&snapshot);
            finish_writeback_snapshot(&snapshot, result)
        }
    }
}

fn wait_for_writeback_progress() -> Result<(), SysErrNo> {
    if crate::trap::foreground_driver_active() {
        return Err(SysErrNo::EBUSY);
    }
    if crate::task::yield_current_once() {
        Ok(())
    } else {
        Err(SysErrNo::EBUSY)
    }
}

pub fn flush_cached_ino(ino: u32) -> Result<(), SysErrNo> {
    cancel_queued_writeback(ino);
    loop {
        cancel_queued_writeback(ino);
        match take_writeback_snapshot(ino) {
            WritebackSnapshotResult::NoCache | WritebackSnapshotResult::Clean => {
                return flush_time_override(ino);
            }
            WritebackSnapshotResult::Failed(err) => {
                if acknowledge_writeback_error(ino, err) {
                    continue;
                }
                return Err(err);
            }
            WritebackSnapshotResult::Busy => {
                wait_for_writeback_progress()?;
                continue;
            }
            WritebackSnapshotResult::Snapshot(snapshot) => {
                let result = apply_writeback_snapshot(&snapshot);
                finish_writeback_snapshot(&snapshot, result)?;
            }
        }
    }
}

pub fn flush_all_cached() -> Result<(), SysErrNo> {
    let mut first_error = None;
    let entries: Vec<(u32, RegularCacheEntry)> = REGULAR_FILE_CACHE
        .lock()
        .iter()
        .map(|(ino, entry)| (*ino, entry.clone()))
        .collect();
    let inos: Vec<u32> = entries
        .into_iter()
        .filter_map(|(ino, entry)| {
            let cached = entry.lock();
            cached.is_dirty().then_some(ino)
        })
        .collect();
    for ino in inos {
        if let Err(err) = flush_cached_ino(ino) {
            first_error.get_or_insert(err);
        }
    }
    let override_inos: Vec<u32> = DATA_TIME_OVERRIDES.lock().keys().copied().collect();
    for ino in override_inos {
        if let Err(err) = flush_time_override(ino) {
            first_error.get_or_insert(err);
        }
    }
    clear_clean_page_cache();
    clear_pblock_run_cache();
    if let Some(err) = first_error {
        Err(err)
    } else {
        Ok(())
    }
}

fn metadata_for_ino(fs: &Ext4, ino: u32) -> Ext4Metadata {
    if let Some((meta, _kind)) = INODE_METADATA_CACHE.read().get(&ino).copied() {
        #[cfg(feature = "buildstorm-diagnostics")]
        crate::buildstorm_diagnostics::note_inode_metadata_cache(true);
        return overlay_dynamic_metadata(meta);
    }
    #[cfg(feature = "buildstorm-diagnostics")]
    crate::buildstorm_diagnostics::note_inode_metadata_cache(false);
    let iref = fs.get_inode_ref(ino);
    let inode = iref.inode;
    let kind = if inode.is_dir() {
        Ext4NodeKind::Directory
    } else if inode.is_file() {
        Ext4NodeKind::Regular
    } else if inode.is_link() {
        Ext4NodeKind::Symlink
    } else {
        Ext4NodeKind::Other
    };
    let meta = Ext4Metadata {
        ino,
        mode: inode.mode() as u32,
        flags: inode.flags(),
        nlink: inode.links_count() as u32,
        uid: inode.uid() as u32,
        gid: inode.gid() as u32,
        size: inode.size(),
        blocks: inode.blocks_count(),
        atime_sec: inode.atime() as isize,
        atime_nsec: ext4_extra_nsec(inode.i_atime_extra()),
        mtime_sec: inode.mtime() as isize,
        mtime_nsec: ext4_extra_nsec(inode.i_mtime_extra()),
        ctime_sec: inode.ctime() as isize,
        ctime_nsec: ext4_extra_nsec(inode.i_ctime_extra()),
    };
    cache_metadata_ino(ino, meta, kind);
    overlay_dynamic_metadata(meta)
}

fn clear_namespace_cache() {
    // Path and directory caches only depend on namespace topology. Regular-file
    // data writes update size/time through REGULAR_FILE_CACHE, or through
    // DATA_TIME_OVERRIDES for uncached writes, so they should not churn these
    // global caches.
    PATH_CACHE.write().clear();
    NEGATIVE_PATH_CACHE.write().clear();
    DIR_CACHE.write().clear();
    clear_metadata_cache();
    NAMESPACE_GENERATION.fetch_add(1, Ordering::Release);
}

fn invalidate_path_cache(path: &str) {
    let path = normalize_path(path);
    PATH_CACHE.write().retain(|cached, _| {
        if path == "/" || cached == &path {
            return false;
        }
        !cached
            .as_str()
            .strip_prefix(path.as_str())
            .is_some_and(|suffix| suffix.starts_with('/'))
    });
    NEGATIVE_PATH_CACHE.write().retain(|cached| {
        if path == "/" || cached == &path {
            return false;
        }
        !cached
            .as_str()
            .strip_prefix(path.as_str())
            .is_some_and(|suffix| suffix.starts_with('/'))
    });
}

fn cache_negative_path(path: &str) {
    let mut cache = NEGATIVE_PATH_CACHE.write();
    if cache.len() >= NEGATIVE_PATH_CACHE_LIMIT {
        cache.clear();
    }
    cache.insert(String::from(path));
}

fn invalidate_namespace_entry(parent_ino: u32, path: &str) {
    DIR_CACHE.write().remove(&parent_ino);
    invalidate_metadata_ino(parent_ino);
    invalidate_path_cache(path);
    NAMESPACE_GENERATION.fetch_add(1, Ordering::Release);
}

fn invalidate_namespace_rename(
    old_parent_ino: u32,
    new_parent_ino: u32,
    old_path: &str,
    new_path: &str,
) {
    {
        let mut dirs = DIR_CACHE.write();
        dirs.remove(&old_parent_ino);
        dirs.remove(&new_parent_ino);
    }
    invalidate_metadata_ino(old_parent_ino);
    invalidate_metadata_ino(new_parent_ino);
    invalidate_path_cache(old_path);
    invalidate_path_cache(new_path);
    NAMESPACE_GENERATION.fetch_add(1, Ordering::Release);
}

fn clear_all_caches() {
    clear_namespace_cache();
    WRITEBACK_QUEUE.lock().clear();
    DATA_TIME_OVERRIDES.lock().clear();
    clear_metadata_cache();
    clear_regular_file_cache();
    clear_clean_page_cache();
    clear_pblock_run_cache();
    OPEN_REGULAR_REFS.lock().clear();
    PENDING_UNLINK_REGULAR.lock().clear();
}

pub fn is_ext4_mounted() -> bool {
    ROOT_EXT4.lock().is_some()
}

pub fn statfs_info() -> Option<Ext4StatFs> {
    let fs = ROOT_EXT4.lock().clone()?;
    let sb = &fs.super_block;
    Some(Ext4StatFs {
        block_size: sb.block_size() as usize,
        blocks: sb.blocks_count() as usize,
        free_blocks: sb.free_blocks_count() as usize,
        files: sb.total_inodes() as usize,
        free_files: sb.free_inodes_count() as usize,
        max_name_len: 255,
    })
}

pub fn mount_block_device(device: Arc<dyn ext4_rs::BlockDevice>) {
    let _namespace = namespace_write_lock!();
    log::info!("[fs] Attempting to mount ext4 from block device...");

    // Read first block. For 4KB block ext4: block0 = boot(1024) + superblock(1024) + bgd(2048).
    // Superblock magic (0xEF53 LE) is at superblock offset 0x38 = absolute byte 1024+0x38 = 0x438.
    // In the returned block buffer, it's at index 0x438.
    let block0 = device.read_offset(0);
    log::info!("[fs] read_offset(0) returned {} bytes", block0.len());

    if block0.len() < 0x438 + 2 {
        log::error!(
            "[fs] ext4: buffer too small ({} < {})",
            block0.len(),
            0x438 + 2
        );
        return;
    }

    let magic = u16::from_le_bytes([block0[0x438], block0[0x439]]);
    log::info!("[fs] ext4: superblock magic = {:#x} (expect 0xef53)", magic);
    if magic != 0xef53 {
        log::error!(
            "[fs] ext4: INVALID superblock magic {:#x} != 0xef53. Image may not be a valid ext4 filesystem.",
            magic
        );
        return;
    }

    // block_size: stored as log2 at superblock offset 0x18 = block0[1024+0x18] = block0[0x418]
    let log_block_size =
        u32::from_le_bytes([block0[0x418], block0[0x419], block0[0x41a], block0[0x41b]]) as usize;
    let block_size = 1024usize << log_block_size;
    log::info!(
        "[fs] ext4: log_block_size={}, block_size={}",
        log_block_size,
        block_size
    );

    let fs = Ext4::open(device);
    let root_count = fs.ext4_dir_get_entries(ROOT_INODE).len();
    match root_count {
        0 => log::warn!("[fs] EXT4 root dir appears empty — expected test scripts here"),
        n => log::info!("[fs] EXT4 root dir has {} entries", n),
    }
    clear_all_caches();
    *ROOT_EXT4.lock() = Some(Arc::new(fs));
    log::info!("[fs] EXT4 mounted as runtime root backing");
}

/// 卸载运行时 ext4 根（`umount2` / 回退 MemFS）。
pub fn unmount_root() {
    let _namespace = namespace_write_lock!();
    let _ = flush_all_cached();
    clear_all_caches();
    *ROOT_EXT4.lock() = None;
    log::info!("[fs] EXT4 unmounted");
}

fn split_parent_name(norm: &str) -> Result<(String, String), SysErrNo> {
    let n = norm.trim_end_matches('/');
    if n.is_empty() || n == "/" {
        return Err(SysErrNo::EINVAL);
    }
    if let Some(pos) = n.rfind('/') {
        if pos == 0 {
            Ok((String::from("/"), n[1..].into()))
        } else {
            Ok((n[..pos].into(), n[pos + 1..].into()))
        }
    } else {
        Err(SysErrNo::EINVAL)
    }
}

pub fn unlink_non_dir(path: &str) -> Result<(), SysErrNo> {
    let _namespace = namespace_write_lock!();
    unlink_non_dir_locked(path)
}

/// The caller serializes the complete namespace transaction.
fn unlink_non_dir_locked(path: &str) -> Result<(), SysErrNo> {
    let mut fs = ROOT_EXT4.lock().clone().ok_or(SysErrNo::ENOENT)?;
    let norm = normalize_path(path);
    let (parent_path, name) = split_parent_name(&norm)?;
    let Some((parent_ino, parent_kind)) = resolve_existing_locked(&fs, &parent_path) else {
        #[cfg(feature = "buildstorm-diagnostics")]
        crate::buildstorm_diagnostics::note_phase(55, 0);
        return Err(SysErrNo::ENOENT);
    };
    if parent_kind != Ext4NodeKind::Directory {
        return Err(SysErrNo::ENOTDIR);
    }
    let Some((child_ino, child_kind)) = resolve_existing_locked(&fs, &norm) else {
        #[cfg(feature = "buildstorm-diagnostics")]
        crate::buildstorm_diagnostics::note_phase(56, 0);
        return Err(SysErrNo::ENOENT);
    };
    if child_kind == Ext4NodeKind::Directory {
        return Err(SysErrNo::EISDIR);
    }
    invalidate_clean_pages_ino(child_ino);
    invalidate_pblock_runs_ino(child_ino);
    let mut parent_ref = fs.get_inode_ref(parent_ino);
    let mut child_ref = fs.get_inode_ref(child_ino);
    let old_links = child_ref.inode.links_count();
    let delay_delete = old_links <= 1 && has_open_regular_ref(child_ino);
    if old_links > 1 {
        drop(child_ref);
        drop(parent_ref);
        flush_cached_ino(child_ino)?;
        fs = ROOT_EXT4.lock().clone().ok_or(SysErrNo::ENOENT)?;
        parent_ref = fs.get_inode_ref(parent_ino);
        child_ref = fs.get_inode_ref(child_ino);
    } else if !delay_delete {
        discard_regular_cache(child_ino);
    }
    let _mutation = ext4_mutation_lock!();
    fs.dir_remove_entry(&mut parent_ref, &name)
        .map_err(map_ext4_err)?;
    if old_links > 0 {
        child_ref.inode.set_links_count(old_links - 1);
    }
    let now = current_ext4_time();
    parent_ref.inode.set_mtime(now);
    parent_ref.inode.set_ctime(now);
    child_ref.inode.set_ctime(now);
    if delay_delete {
        PENDING_UNLINK_REGULAR.lock().insert(child_ino);
    } else if old_links <= 1 {
        let old_size = child_ref.inode.size();
        if old_size > 0 {
            fs.truncate_inode(&mut child_ref, 0).map_err(map_ext4_err)?;
        }
        child_ref.inode.set_dtime(now);
        child_ref.inode.set_links_count(0);
    }
    fs.write_back_inode(&mut parent_ref);
    fs.write_back_inode(&mut child_ref);
    if old_links <= 1 && !delay_delete {
        fs.ialloc_free_inode(child_ino, child_kind == Ext4NodeKind::Directory);
    }
    invalidate_metadata_ino(child_ino);
    invalidate_namespace_entry(parent_ino, &norm);
    Ok(())
}

pub fn unlink_regular_file(path: &str) -> Result<(), SysErrNo> {
    unlink_non_dir(path)
}

pub fn mkdir_ext4_with_mode(path: &str, mode: u32) -> Result<(), SysErrNo> {
    let _namespace = namespace_write_lock!();
    let _mutation = ext4_mutation_lock!();
    let fs = ROOT_EXT4.lock().clone().ok_or(SysErrNo::ENOENT)?;
    let norm = normalize_path(path);
    let (parent_path, name) = split_parent_name(&norm)?;
    let Some((parent_ino, parent_kind)) = resolve_existing_locked(&fs, &parent_path) else {
        return Err(SysErrNo::ENOENT);
    };
    if parent_kind != Ext4NodeKind::Directory {
        return Err(SysErrNo::ENOTDIR);
    }
    if resolve_existing_locked(&fs, &norm).is_some() {
        return Err(SysErrNo::EEXIST);
    }
    let (uid, gid, perm) = new_child_ids_and_mode(&fs, parent_ino, mode, true);
    let mut child_ref = fs
        .create(parent_ino, &name, InodeFileType::S_IFDIR.bits() | perm)
        .map_err(map_ext4_err)?;
    let now = current_ext4_time();
    child_ref
        .inode
        .set_mode(InodeFileType::S_IFDIR.bits() | perm);
    child_ref.inode.set_uid(uid);
    child_ref.inode.set_gid(gid);
    child_ref.inode.set_atime(now);
    child_ref.inode.set_mtime(now);
    child_ref.inode.set_ctime(now);
    fs.write_back_inode(&mut child_ref);
    touch_inode(&fs, parent_ino, false, true, true);
    invalidate_metadata_ino(child_ref.inode_num);
    invalidate_namespace_entry(parent_ino, &norm);
    Ok(())
}

pub fn mkdir_ext4(path: &str) -> Result<(), SysErrNo> {
    mkdir_ext4_with_mode(path, 0o755)
}

pub fn remove_empty_dir_ext4(path: &str) -> Result<(), SysErrNo> {
    let _namespace = namespace_write_lock!();
    remove_empty_dir_ext4_locked(path)
}

/// The caller serializes the complete namespace transaction.
fn remove_empty_dir_ext4_locked(path: &str) -> Result<(), SysErrNo> {
    let _mutation = ext4_mutation_lock!();
    let fs = ROOT_EXT4.lock().clone().ok_or(SysErrNo::ENOENT)?;
    let norm = normalize_path(path);
    if norm == "/" {
        return Err(SysErrNo::EINVAL);
    }
    let (parent_path, name) = split_parent_name(&norm)?;
    let Some((parent_ino, parent_kind)) = resolve_existing_locked(&fs, &parent_path) else {
        return Err(SysErrNo::ENOENT);
    };
    if parent_kind != Ext4NodeKind::Directory {
        return Err(SysErrNo::ENOTDIR);
    }
    let Some((child_ino, child_kind)) = resolve_existing_locked(&fs, &norm) else {
        return Err(SysErrNo::ENOENT);
    };
    if child_kind != Ext4NodeKind::Directory {
        return Err(SysErrNo::ENOTDIR);
    }
    if !cached_dir_entries_locked(&fs, child_ino).is_empty() {
        return Err(SysErrNo::ENOTEMPTY);
    }
    let mut parent_ref = fs.get_inode_ref(parent_ino);
    let mut child_ref = fs.get_inode_ref(child_ino);
    fs.dir_remove_entry(&mut parent_ref, &name)
        .map_err(map_ext4_err)?;
    let now = current_ext4_time();
    if child_ref.inode.size() > 0 {
        fs.truncate_inode(&mut child_ref, 0).map_err(map_ext4_err)?;
    }
    child_ref.inode.set_links_count(0);
    child_ref.inode.set_dtime(now);
    child_ref.inode.set_ctime(now);
    let parent_links = parent_ref.inode.links_count();
    if parent_links > 0 {
        parent_ref.inode.set_links_count(parent_links - 1);
    }
    parent_ref.inode.set_mtime(now);
    parent_ref.inode.set_ctime(now);
    fs.write_back_inode(&mut child_ref);
    fs.write_back_inode(&mut parent_ref);
    fs.ialloc_free_inode(child_ino, true);
    invalidate_metadata_ino(child_ino);
    invalidate_namespace_entry(parent_ino, &norm);
    Ok(())
}

/// 创建普通文件（已存在则由 `generic_open` 语义处理）。
pub fn create_regular_ext4_with_mode(path: &str, mode: u32) -> Result<u32, SysErrNo> {
    let _namespace = namespace_write_lock!();
    let _mutation = ext4_mutation_lock!();
    let fs = ROOT_EXT4.lock().clone().ok_or(SysErrNo::ENOENT)?;
    let norm = normalize_path(path);
    let (parent_path, name) = split_parent_name(&norm)?;
    let Some((parent_ino, parent_kind)) = resolve_existing_locked(&fs, &parent_path) else {
        #[cfg(feature = "buildstorm-diagnostics")]
        crate::buildstorm_diagnostics::note_phase(50, 0);
        return Err(SysErrNo::ENOENT);
    };
    if parent_kind != Ext4NodeKind::Directory {
        return Err(SysErrNo::ENOTDIR);
    }
    if resolve_existing_locked(&fs, &norm).is_some() {
        return Err(SysErrNo::EEXIST);
    }
    let (uid, gid, perm) = new_child_ids_and_mode(&fs, parent_ino, mode, false);
    let mut iref = fs
        .create(parent_ino, &name, InodeFileType::S_IFREG.bits() | perm)
        .map_err(map_ext4_err)?;
    let now = current_ext4_time();
    iref.inode.set_mode(InodeFileType::S_IFREG.bits() | perm);
    iref.inode.set_uid(uid);
    iref.inode.set_gid(gid);
    iref.inode.set_atime(now);
    iref.inode.set_mtime(now);
    iref.inode.set_ctime(now);
    fs.write_back_inode(&mut iref);
    touch_inode(&fs, parent_ino, false, true, true);
    invalidate_clean_pages_ino(iref.inode_num);
    invalidate_pblock_runs_ino(iref.inode_num);
    cache_empty_regular(iref.inode_num, false);
    invalidate_metadata_ino(iref.inode_num);
    invalidate_namespace_entry(parent_ino, &norm);
    Ok(iref.inode_num)
}

pub fn create_regular_ext4(path: &str) -> Result<u32, SysErrNo> {
    create_regular_ext4_with_mode(path, 0o666)
}

pub fn truncate_regular_ino(ino: u32, size: u64) -> Result<(), SysErrNo> {
    if size > MAX_FILE_OFFSET as u64 {
        return Err(SysErrNo::EFBIG);
    }
    let fs = ROOT_EXT4.lock().clone().ok_or(SysErrNo::ENOENT)?;
    let kind = inode_kind(&fs, ino);
    if kind == Ext4NodeKind::Directory {
        return Err(SysErrNo::EISDIR);
    }
    if kind != Ext4NodeKind::Regular && kind != Ext4NodeKind::Symlink {
        return Err(SysErrNo::EINVAL);
    }
    if kind == Ext4NodeKind::Regular {
        invalidate_clean_pages_ino(ino);
        invalidate_pblock_runs_ino(ino);
    }
    if kind == Ext4NodeKind::Regular && is_regular_cached(ino) {
        return cached_regular_resize(ino, size as usize);
    }
    let _mutation = ext4_mutation_lock!();
    let mut iref = fs.get_inode_ref(ino);
    let old_size = iref.inode.size();
    if size < old_size {
        fs.truncate_inode(&mut iref, size).map_err(map_ext4_err)?;
        if kind == Ext4NodeKind::Regular && size == 0 {
            cache_empty_regular(ino, true);
        }
    } else if size > old_size {
        iref.inode.set_size(size);
        fs.write_back_inode(&mut iref);
    }
    touch_inode(&fs, ino, false, true, true);
    if kind == Ext4NodeKind::Regular {
        invalidate_pblock_runs_ino(ino);
    }
    invalidate_metadata_ino(ino);
    Ok(())
}

pub fn truncate_regular_ext4(path: &str, size: u64) -> Result<(), SysErrNo> {
    let fs = ROOT_EXT4.lock().clone().ok_or(SysErrNo::ENOENT)?;
    let Some((ino, kind)) = resolve_existing(&fs, path) else {
        return Err(SysErrNo::ENOENT);
    };
    if kind == Ext4NodeKind::Directory {
        return Err(SysErrNo::EISDIR);
    }
    drop(fs);
    truncate_regular_ino(ino, size)
}

pub fn ext4_read_at(ino: u32, offset: usize, buf: &mut [u8]) -> Result<usize, SysErrNo> {
    if let Some(result) = cached_regular_read(ino, offset, buf) {
        return result;
    }
    match clean_page_cached_read(ino, offset, buf) {
        Ok(n) => return Ok(n),
        Err(SysErrNo::ENOMEM | SysErrNo::EINVAL) => {}
        Err(err) => return Err(err),
    }
    extent_aware_read_at(ino, offset, buf)
}

pub fn ext4_write_at(ino: u32, offset: usize, buf: &[u8]) -> Result<usize, SysErrNo> {
    checked_file_end(offset, buf.len())?;
    if buf.is_empty() {
        return Ok(0);
    }
    invalidate_pblock_runs_ino(ino);
    if ensure_regular_cache(ino)? {
        cached_regular_write(ino, offset, buf)
    } else {
        uncached_regular_write(ino, offset, buf)
    }
}

pub fn lookup_path(path: &str) -> Option<(u32, bool)> {
    let fs = ROOT_EXT4.lock().clone()?;
    resolve_existing(&fs, path).map(|(ino, kind)| (ino, kind == Ext4NodeKind::Directory))
}

pub fn lookup_kind(path: &str) -> Option<(u32, Ext4NodeKind)> {
    let fs = ROOT_EXT4.lock().clone()?;
    resolve_existing(&fs, path)
}

pub fn metadata_by_ino(ino: u32) -> Result<Ext4Metadata, SysErrNo> {
    let fs = ROOT_EXT4.lock().clone().ok_or(SysErrNo::ENOENT)?;
    Ok(metadata_for_ino(&fs, ino))
}

pub fn metadata_with_kind(path: &str) -> Result<(Ext4Metadata, Ext4NodeKind), SysErrNo> {
    let fs = ROOT_EXT4.lock().clone().ok_or(SysErrNo::ENOENT)?;
    let (ino, kind) = resolve_existing(&fs, path).ok_or(SysErrNo::ENOENT)?;
    Ok((metadata_for_ino(&fs, ino), kind))
}

pub fn metadata(path: &str) -> Result<Ext4Metadata, SysErrNo> {
    metadata_with_kind(path).map(|(metadata, _)| metadata)
}

pub fn file_flags_by_ino(ino: u32) -> Result<u32, SysErrNo> {
    let fs = ROOT_EXT4.lock().clone().ok_or(SysErrNo::ENOENT)?;
    Ok(fs.get_inode_ref(ino).inode.flags())
}

pub fn set_file_flags_ino(ino: u32, flags: u32) -> Result<(), SysErrNo> {
    let _mutation = ext4_mutation_lock!();
    let fs = ROOT_EXT4.lock().clone().ok_or(SysErrNo::ENOENT)?;
    let mut iref = fs.get_inode_ref(ino);
    iref.inode.set_flags(flags);
    let now = current_ext4_time();
    iref.inode.set_ctime(now);
    iref.inode.set_i_ctime_extra(0);
    fs.write_back_inode(&mut iref);
    invalidate_metadata_ino(ino);
    Ok(())
}

pub fn set_mode_ino(ino: u32, mode: u32) -> Result<(), SysErrNo> {
    let _mutation = ext4_mutation_lock!();
    let fs = ROOT_EXT4.lock().clone().ok_or(SysErrNo::ENOENT)?;
    let mut iref = fs.get_inode_ref(ino);
    let file_type = iref.inode.mode() & 0o170000;
    let perm = (mode as u16) & 0o7777;
    iref.inode.set_mode(file_type | perm);
    let now = current_ext4_time();
    iref.inode.set_ctime(now);
    iref.inode.set_i_ctime_extra(0);
    fs.write_back_inode(&mut iref);
    invalidate_metadata_ino(ino);
    Ok(())
}

pub fn set_mode_path(path: &str, mode: u32) -> Result<(), SysErrNo> {
    let fs = ROOT_EXT4.lock().clone().ok_or(SysErrNo::ENOENT)?;
    let (ino, _) = resolve_existing(&fs, path).ok_or(SysErrNo::ENOENT)?;
    drop(fs);
    set_mode_ino(ino, mode)
}

pub fn set_owner_ino(ino: u32, uid: Option<u32>, gid: Option<u32>) -> Result<(), SysErrNo> {
    let _mutation = ext4_mutation_lock!();
    let fs = ROOT_EXT4.lock().clone().ok_or(SysErrNo::ENOENT)?;
    let mut iref = fs.get_inode_ref(ino);
    let is_regular = iref.inode.is_file();
    if let Some(uid) = uid {
        if uid > u16::MAX as u32 {
            return Err(SysErrNo::EINVAL);
        }
        iref.inode.set_uid(uid as u16);
    }
    if let Some(gid) = gid {
        if gid > u16::MAX as u32 {
            return Err(SysErrNo::EINVAL);
        }
        iref.inode.set_gid(gid as u16);
    }
    let mode = super::chown_mode_after_owner_update(
        iref.inode.mode() as u32,
        is_regular,
        uid.is_some() || gid.is_some(),
    );
    iref.inode.set_mode(mode as u16);
    let now = current_ext4_time();
    iref.inode.set_ctime(now);
    iref.inode.set_i_ctime_extra(0);
    fs.write_back_inode(&mut iref);
    invalidate_metadata_ino(ino);
    Ok(())
}

pub fn set_owner_path(path: &str, uid: Option<u32>, gid: Option<u32>) -> Result<(), SysErrNo> {
    let fs = ROOT_EXT4.lock().clone().ok_or(SysErrNo::ENOENT)?;
    let (ino, _) = resolve_existing(&fs, path).ok_or(SysErrNo::ENOENT)?;
    drop(fs);
    set_owner_ino(ino, uid, gid)
}

pub fn set_times_ino(
    ino: u32,
    atime: Option<(isize, isize)>,
    mtime: Option<(isize, isize)>,
) -> Result<(), SysErrNo> {
    flush_cached_ino(ino)?;
    let _mutation = ext4_mutation_lock!();
    let fs = ROOT_EXT4.lock().clone().ok_or(SysErrNo::ENOENT)?;
    let mut iref = fs.get_inode_ref(ino);
    if let Some((sec, nsec)) = atime {
        iref.inode
            .set_atime((sec.max(0) as usize).min(u32::MAX as usize) as u32);
        iref.inode.set_i_atime_extra(ext4_nsec_extra(nsec));
    }
    if let Some((sec, nsec)) = mtime {
        iref.inode
            .set_mtime((sec.max(0) as usize).min(u32::MAX as usize) as u32);
        iref.inode.set_i_mtime_extra(ext4_nsec_extra(nsec));
    }
    let now = current_ext4_time();
    iref.inode.set_ctime(now);
    iref.inode.set_i_ctime_extra(0);
    fs.write_back_inode(&mut iref);
    DATA_TIME_OVERRIDES.lock().remove(&ino);
    invalidate_metadata_ino(ino);
    if let Some(entry) = regular_cache_entry(ino) {
        let mut cached = entry.lock();
        if cached.evicted {
            return Ok(());
        }
        if let Some((sec, nsec)) = mtime {
            cached.mtime_sec = (sec.max(0) as usize).min(u32::MAX as usize) as u32;
            cached.mtime_extra = ext4_nsec_extra(nsec);
        }
        cached.ctime_sec = now;
        cached.ctime_extra = 0;
    }
    Ok(())
}

pub fn set_times_path(
    path: &str,
    atime: Option<(isize, isize)>,
    mtime: Option<(isize, isize)>,
) -> Result<(), SysErrNo> {
    let fs = ROOT_EXT4.lock().clone().ok_or(SysErrNo::ENOENT)?;
    let (ino, _) = resolve_existing(&fs, path).ok_or(SysErrNo::ENOENT)?;
    drop(fs);
    set_times_ino(ino, atime, mtime)
}

pub fn regular_file_size(ino: u32) -> Result<usize, SysErrNo> {
    if let Some(size) = cached_regular_size(ino) {
        return Ok(size);
    }
    let fs = ROOT_EXT4.lock().clone().ok_or(SysErrNo::ENOENT)?;
    Ok(fs.get_inode_ref(ino).inode.size() as usize)
}

/// The caller holds either the shared or exclusive namespace transaction.
fn cached_dir_entries_locked(fs: &Ext4, ino: u32) -> Arc<BTreeMap<String, (u32, bool)>> {
    if let Some(entries) = DIR_CACHE.read().get(&ino).cloned() {
        #[cfg(feature = "buildstorm-diagnostics")]
        crate::buildstorm_diagnostics::note_dir_cache(true);
        return entries;
    }
    #[cfg(feature = "buildstorm-diagnostics")]
    crate::buildstorm_diagnostics::note_dir_cache(false);

    let mut out = BTreeMap::new();
    for e in fs.ext4_dir_get_entries(ino) {
        if e.unused() {
            continue;
        }
        let name = e.get_name();
        if name == "." || name == ".." {
            continue;
        }
        let child_ino = e.inode;
        let is_subdir = match e.get_de_type() {
            EXT4_DIRENT_DIR => true,
            EXT4_DIRENT_UNKNOWN => fs.get_inode_ref(child_ino).inode.is_dir(),
            _ => false,
        };
        out.insert(name, (child_ino, is_subdir));
    }
    let out = Arc::new(out);
    DIR_CACHE.write().insert(ino, out.clone());
    out
}

fn find_child_ino_locked(fs: &Ext4, parent_ino: u32, name: &str) -> Option<u32> {
    cached_dir_entries_locked(fs, parent_ino)
        .get(name)
        .map(|(child_ino, _)| *child_ino)
}

fn parent_path_of(path: &str) -> String {
    let norm = normalize_path(path);
    let trimmed = norm.trim_end_matches('/');
    match trimmed.rfind('/') {
        Some(0) | None => String::from("/"),
        Some(pos) => String::from(&trimmed[..pos]),
    }
}

fn resolve_link_target(link_path: &str, target: &str) -> String {
    if target.starts_with('/') {
        normalize_path(target)
    } else {
        normalize_path(&format!("{}/{}", parent_path_of(link_path), target))
    }
}

pub fn readlink_ext4(path: &str) -> Result<String, SysErrNo> {
    let _namespace = NAMESPACE_LOCK.read();
    readlink_ext4_locked(path)
}

/// The caller holds either the shared or exclusive namespace transaction.
fn readlink_ext4_locked(path: &str) -> Result<String, SysErrNo> {
    let fs = ROOT_EXT4.lock().clone().ok_or(SysErrNo::ENOENT)?;
    let norm = normalize_path(path);
    let Some((ino, kind)) = resolve_existing_locked(&fs, &norm) else {
        return Err(SysErrNo::ENOENT);
    };
    if kind != Ext4NodeKind::Symlink {
        return Err(SysErrNo::EINVAL);
    }

    let inode_ref = fs.get_inode_ref(ino);
    let size = inode_ref.inode.size() as usize;
    let mut data = Vec::new();
    if size <= 60 && inode_ref.inode.blocks_count() == 0 {
        for word in inode_ref.inode.block() {
            data.extend_from_slice(&word.to_le_bytes());
        }
        data.truncate(size);
    } else {
        data.resize(size, 0);
        if size != 0
            && !fs
                .read_at(ino, 0, &mut data)
                .map(|n| n == size)
                .unwrap_or(false)
        {
            return Err(SysErrNo::EIO);
        }
    }
    String::from_utf8(data).map_err(|_| SysErrNo::EINVAL)
}

pub fn resolve_symlinks(path: &str) -> Result<String, SysErrNo> {
    let _namespace = NAMESPACE_LOCK.read();
    resolve_symlinks_locked(path)
}

/// The caller holds either the shared or exclusive namespace transaction.
fn resolve_symlinks_locked(path: &str) -> Result<String, SysErrNo> {
    let mut pending: VecDeque<String> = normalize_path(path)
        .trim_matches('/')
        .split('/')
        .filter(|part| !part.is_empty())
        .map(String::from)
        .collect();
    let mut resolved: Vec<String> = Vec::new();
    let mut followed = 0usize;

    while let Some(component) = pending.pop_front() {
        let mut candidate = String::from("/");
        for part in &resolved {
            if candidate.len() > 1 {
                candidate.push('/');
            }
            candidate.push_str(part);
        }
        if candidate.len() > 1 {
            candidate.push('/');
        }
        candidate.push_str(&component);

        let Some(fs) = ROOT_EXT4.lock().clone() else {
            return Err(SysErrNo::ENOENT);
        };
        let Some((_ino, kind)) = resolve_existing_locked(&fs, &candidate) else {
            return Err(SysErrNo::ENOENT);
        };
        if kind == Ext4NodeKind::Symlink {
            followed += 1;
            if followed > 40 {
                return Err(SysErrNo::ELOOP);
            }
            let target = readlink_ext4_locked(&candidate)?;
            let target_path = resolve_link_target(&candidate, &target);
            let mut target_components: VecDeque<String> = target_path
                .trim_matches('/')
                .split('/')
                .filter(|part| !part.is_empty())
                .map(String::from)
                .collect();
            target_components.append(&mut pending);
            pending = target_components;
            resolved.clear();
            continue;
        }
        if !pending.is_empty() && kind != Ext4NodeKind::Directory {
            return Err(SysErrNo::ENOTDIR);
        }
        resolved.push(component);
    }

    if resolved.is_empty() {
        Ok(String::from("/"))
    } else {
        Ok(format!("/{}", resolved.join("/")))
    }
}

fn path_is_descendant(parent: &str, child: &str) -> bool {
    let parent = normalize_path(parent);
    let child = normalize_path(child);
    child
        .strip_prefix(&parent)
        .and_then(|rest| rest.strip_prefix('/'))
        .is_some()
}

pub fn create_symlink_ext4(target: &str, link_path: &str) -> Result<(), SysErrNo> {
    let _namespace = namespace_write_lock!();
    let _mutation = ext4_mutation_lock!();
    let fs = ROOT_EXT4.lock().clone().ok_or(SysErrNo::ENOENT)?;
    let norm = normalize_path(link_path);
    let (parent_path, name) = split_parent_name(&norm)?;
    let Some((parent_ino, parent_kind)) = resolve_existing_locked(&fs, &parent_path) else {
        return Err(SysErrNo::ENOENT);
    };
    if parent_kind != Ext4NodeKind::Directory {
        return Err(SysErrNo::ENOTDIR);
    }
    if resolve_existing_locked(&fs, &norm).is_some() {
        return Err(SysErrNo::EEXIST);
    }
    let (uid, gid, perm) = new_child_ids_and_mode(&fs, parent_ino, 0o777, false);
    let mut iref = fs
        .create(parent_ino, &name, InodeFileType::S_IFLNK.bits() | 0o777)
        .map_err(map_ext4_err)?;
    let now = current_ext4_time();
    iref.inode.set_mode(InodeFileType::S_IFLNK.bits() | perm);
    iref.inode.set_uid(uid);
    iref.inode.set_gid(gid);
    iref.inode.set_atime(now);
    iref.inode.set_mtime(now);
    iref.inode.set_ctime(now);
    fs.write_back_inode(&mut iref);
    let bytes = target.as_bytes();
    if !bytes.is_empty() {
        let written = fs
            .write_at(iref.inode_num, 0, bytes)
            .map_err(map_ext4_err)?;
        if written != bytes.len() {
            return Err(SysErrNo::EIO);
        }
    }
    touch_inode(&fs, iref.inode_num, false, true, true);
    touch_inode(&fs, parent_ino, false, true, true);
    invalidate_metadata_ino(iref.inode_num);
    invalidate_namespace_entry(parent_ino, &norm);
    Ok(())
}

pub fn link_ext4(old_path: &str, new_path: &str, follow_old: bool) -> Result<(), SysErrNo> {
    let _namespace = namespace_write_lock!();
    let _mutation = ext4_mutation_lock!();
    let fs = ROOT_EXT4.lock().clone().ok_or(SysErrNo::ENOENT)?;
    let old = if follow_old {
        resolve_symlinks_locked(old_path)?
    } else {
        normalize_path(old_path)
    };
    let new = normalize_path(new_path);
    let (new_parent_path, new_name) = split_parent_name(&new)?;
    let Some((new_parent_ino, new_parent_kind)) = resolve_existing_locked(&fs, &new_parent_path)
    else {
        return Err(SysErrNo::ENOENT);
    };
    if new_parent_kind != Ext4NodeKind::Directory {
        return Err(SysErrNo::ENOTDIR);
    }
    if resolve_existing_locked(&fs, &new).is_some() {
        return Err(SysErrNo::EEXIST);
    }
    let Some((old_ino, old_kind)) = resolve_existing_locked(&fs, &old) else {
        return Err(SysErrNo::ENOENT);
    };
    if old_kind == Ext4NodeKind::Directory {
        return Err(SysErrNo::EPERM);
    }

    let mut parent_ref = fs.get_inode_ref(new_parent_ino);
    let mut child_ref = fs.get_inode_ref(old_ino);
    fs.link(&mut parent_ref, &mut child_ref, &new_name)
        .map_err(map_ext4_err)?;
    let now = current_ext4_time();
    parent_ref.inode.set_mtime(now);
    parent_ref.inode.set_ctime(now);
    child_ref.inode.set_ctime(now);
    fs.write_back_inode(&mut parent_ref);
    fs.write_back_inode(&mut child_ref);
    invalidate_metadata_ino(old_ino);
    invalidate_namespace_entry(new_parent_ino, &new);
    Ok(())
}

pub fn rename_ext4(old_path: &str, new_path: &str, no_replace: bool) -> Result<(), SysErrNo> {
    let _namespace = namespace_write_lock!();
    rename_ext4_locked(old_path, new_path, no_replace)
}

/// The caller holds the exclusive namespace transaction. This permits rename
/// replacement and exchange to compose without recursively acquiring the lock.
fn rename_ext4_locked(old_path: &str, new_path: &str, no_replace: bool) -> Result<(), SysErrNo> {
    let fs = ROOT_EXT4.lock().clone().ok_or(SysErrNo::ENOENT)?;
    let old = normalize_path(old_path);
    let new = normalize_path(new_path);
    if old == new {
        return Ok(());
    }
    if old == "/" {
        return Err(SysErrNo::EINVAL);
    }

    let (old_parent_path, old_name) = split_parent_name(&old)?;
    let (new_parent_path, new_name) = split_parent_name(&new)?;
    let Some((old_parent_ino, old_parent_kind)) = resolve_existing_locked(&fs, &old_parent_path)
    else {
        #[cfg(feature = "buildstorm-diagnostics")]
        crate::buildstorm_diagnostics::note_phase(52, 0);
        return Err(SysErrNo::ENOENT);
    };
    let Some((new_parent_ino, new_parent_kind)) = resolve_existing_locked(&fs, &new_parent_path)
    else {
        #[cfg(feature = "buildstorm-diagnostics")]
        crate::buildstorm_diagnostics::note_phase(53, 0);
        return Err(SysErrNo::ENOENT);
    };
    if old_parent_kind != Ext4NodeKind::Directory || new_parent_kind != Ext4NodeKind::Directory {
        return Err(SysErrNo::ENOTDIR);
    }
    let Some((old_ino, old_kind)) = resolve_existing_locked(&fs, &old) else {
        #[cfg(feature = "buildstorm-diagnostics")]
        crate::buildstorm_diagnostics::note_phase(54, 0);
        return Err(SysErrNo::ENOENT);
    };
    if old_kind == Ext4NodeKind::Directory && path_is_descendant(&old, &new) {
        return Err(SysErrNo::EINVAL);
    }
    let new_existing = resolve_existing_locked(&fs, &new);
    if no_replace && new_existing.is_some() {
        return Err(SysErrNo::EEXIST);
    }
    if let Some((new_ino, new_kind)) = new_existing {
        if new_ino == old_ino {
            return Ok(());
        }
        match (old_kind, new_kind) {
            (Ext4NodeKind::Directory, Ext4NodeKind::Directory) => {
                if !cached_dir_entries_locked(&fs, new_ino).is_empty() {
                    return Err(SysErrNo::ENOTEMPTY);
                }
                drop(fs);
                remove_empty_dir_ext4_locked(&new)?;
            }
            (Ext4NodeKind::Directory, _) => return Err(SysErrNo::ENOTDIR),
            (_, Ext4NodeKind::Directory) => return Err(SysErrNo::EISDIR),
            _ => {
                drop(fs);
                unlink_non_dir_locked(&new)?;
            }
        }
    }

    let _mutation = ext4_mutation_lock!();
    let fs = ROOT_EXT4.lock().clone().ok_or(SysErrNo::ENOENT)?;
    let mut child_ref = fs.get_inode_ref(old_ino);
    let now = current_ext4_time();
    if old_parent_ino == new_parent_ino {
        let mut parent_ref = fs.get_inode_ref(old_parent_ino);
        fs.dir_add_entry(&mut parent_ref, &child_ref, &new_name)
            .map_err(map_ext4_err)?;
        fs.dir_remove_entry(&mut parent_ref, &old_name)
            .map_err(map_ext4_err)?;
        parent_ref.inode.set_mtime(now);
        parent_ref.inode.set_ctime(now);
        fs.write_back_inode(&mut parent_ref);
    } else {
        let mut new_parent_ref = fs.get_inode_ref(new_parent_ino);
        fs.dir_add_entry(&mut new_parent_ref, &child_ref, &new_name)
            .map_err(map_ext4_err)?;
        let mut old_parent_ref = fs.get_inode_ref(old_parent_ino);
        fs.dir_remove_entry(&mut old_parent_ref, &old_name)
            .map_err(map_ext4_err)?;
        if old_kind == Ext4NodeKind::Directory {
            let old_parent_links = old_parent_ref.inode.links_count();
            if old_parent_links > 0 {
                old_parent_ref.inode.set_links_count(old_parent_links - 1);
            }
            let new_parent_links = new_parent_ref.inode.links_count();
            new_parent_ref.inode.set_links_count(new_parent_links + 1);
            fs.dir_remove_entry(&mut child_ref, "..")
                .map_err(map_ext4_err)?;
            fs.dir_add_entry(&mut child_ref, &new_parent_ref, "..")
                .map_err(map_ext4_err)?;
        }
        old_parent_ref.inode.set_mtime(now);
        old_parent_ref.inode.set_ctime(now);
        new_parent_ref.inode.set_mtime(now);
        new_parent_ref.inode.set_ctime(now);
        fs.write_back_inode(&mut old_parent_ref);
        fs.write_back_inode(&mut new_parent_ref);
    }
    child_ref.inode.set_ctime(now);
    fs.write_back_inode(&mut child_ref);
    invalidate_metadata_ino(old_ino);
    invalidate_namespace_rename(old_parent_ino, new_parent_ino, &old, &new);
    Ok(())
}

/// 解析已存在的绝对路径 → (inode, 是否为目录)。不存在或非法则 `None`。
fn resolve_existing(fs: &Ext4, path: &str) -> Option<(u32, Ext4NodeKind)> {
    let _namespace = NAMESPACE_LOCK.read();
    resolve_existing_locked(fs, path)
}

/// The caller holds either the shared or exclusive namespace transaction, so
/// cache acceptance, backend traversal, and cache publication are one lookup.
fn resolve_existing_locked(fs: &Ext4, path: &str) -> Option<(u32, Ext4NodeKind)> {
    let generation = NAMESPACE_GENERATION.load(Ordering::Acquire);
    let n = normalize_path(path);
    if !n.starts_with('/') {
        return None;
    }
    if let Some(found) = PATH_CACHE.read().get(&n).copied() {
        #[cfg(feature = "buildstorm-diagnostics")]
        crate::buildstorm_diagnostics::note_path_cache(true, false);
        if NAMESPACE_GENERATION.load(Ordering::Acquire) != generation {
            #[cfg(feature = "buildstorm-diagnostics")]
            crate::buildstorm_diagnostics::note_phase(57, 0);
            return resolve_existing_locked(fs, path);
        }
        return Some(found);
    }
    if NEGATIVE_PATH_CACHE.read().contains(&n) {
        #[cfg(feature = "buildstorm-diagnostics")]
        crate::buildstorm_diagnostics::note_path_cache(false, true);
        if NAMESPACE_GENERATION.load(Ordering::Acquire) != generation {
            #[cfg(feature = "buildstorm-diagnostics")]
            crate::buildstorm_diagnostics::note_phase(58, 0);
            return resolve_existing_locked(fs, path);
        }
        return None;
    }
    #[cfg(feature = "buildstorm-diagnostics")]
    crate::buildstorm_diagnostics::note_path_cache(false, false);
    let tail = n.trim_matches('/');
    let parts: Vec<&str> = if tail.is_empty() {
        Vec::new()
    } else {
        tail.split('/').filter(|p| !p.is_empty()).collect()
    };

    if parts.is_empty() {
        let found = (ROOT_INODE, inode_kind(fs, ROOT_INODE));
        if NAMESPACE_GENERATION.load(Ordering::Acquire) != generation {
            #[cfg(feature = "buildstorm-diagnostics")]
            crate::buildstorm_diagnostics::note_phase(57, 0);
            return resolve_existing_locked(fs, path);
        }
        PATH_CACHE.write().insert(n, found);
        return Some(found);
    }

    let mut parent = ROOT_INODE;
    for (i, comp) in parts.iter().enumerate() {
        let Some(ino) = find_child_ino_locked(fs, parent, comp) else {
            if NAMESPACE_GENERATION.load(Ordering::Acquire) != generation {
                #[cfg(feature = "buildstorm-diagnostics")]
                crate::buildstorm_diagnostics::note_phase(58, 0);
                return resolve_existing_locked(fs, path);
            }
            cache_negative_path(&n);
            return None;
        };
        if i + 1 == parts.len() {
            let found = (ino, inode_kind(fs, ino));
            if NAMESPACE_GENERATION.load(Ordering::Acquire) != generation {
                #[cfg(feature = "buildstorm-diagnostics")]
                crate::buildstorm_diagnostics::note_phase(57, 0);
                return resolve_existing_locked(fs, path);
            }
            PATH_CACHE.write().insert(n, found);
            return Some(found);
        }
        if !fs.get_inode_ref(ino).inode.is_dir() {
            cache_negative_path(&n);
            return None;
        }
        parent = ino;
    }
    None
}

/// 整块读入普通文件（用于 `execve` / harness）。目录或不存在返回 `None`。
pub fn exchange_ext4(old_path: &str, new_path: &str) -> Result<(), SysErrNo> {
    let _namespace = namespace_write_lock!();
    let fs = ROOT_EXT4.lock().clone().ok_or(SysErrNo::ENOENT)?;
    let old = normalize_path(old_path);
    let new = normalize_path(new_path);
    if old == new {
        return Ok(());
    }
    if old == "/" || new == "/" {
        return Err(SysErrNo::EINVAL);
    }

    let (old_parent_path, _) = split_parent_name(&old)?;
    let Some((old_ino, old_kind)) = resolve_existing_locked(&fs, &old) else {
        return Err(SysErrNo::ENOENT);
    };
    let Some((new_ino, new_kind)) = resolve_existing_locked(&fs, &new) else {
        return Err(SysErrNo::ENOENT);
    };
    if old_kind == Ext4NodeKind::Directory && path_is_descendant(&old, &new) {
        return Err(SysErrNo::EINVAL);
    }
    if new_kind == Ext4NodeKind::Directory && path_is_descendant(&new, &old) {
        return Err(SysErrNo::EINVAL);
    }

    let mut temp = String::new();
    for attempt in 0..32usize {
        temp = if old_parent_path == "/" {
            format!("/.wll_rename_exchange_{}_{}_{}", old_ino, new_ino, attempt)
        } else {
            format!(
                "{}/.wll_rename_exchange_{}_{}_{}",
                old_parent_path, old_ino, new_ino, attempt
            )
        };
        if resolve_existing_locked(&fs, &temp).is_none() {
            break;
        }
        temp.clear();
    }
    if temp.is_empty() {
        return Err(SysErrNo::EEXIST);
    }
    drop(fs);

    rename_ext4_locked(&old, &temp, true)?;
    if let Err(e) = rename_ext4_locked(&new, &old, true) {
        let _ = rename_ext4_locked(&temp, &old, true);
        return Err(e);
    }
    if let Err(e) = rename_ext4_locked(&temp, &new, true) {
        return Err(e);
    }
    Ok(())
}

pub fn slurp_regular_file_shared(path: &str) -> Option<Arc<Vec<u8>>> {
    let namespace = NAMESPACE_LOCK.read();
    let resolved = resolve_symlinks_locked(path).ok()?;
    let fs = ROOT_EXT4.lock().clone()?;
    let (ino, kind) = resolve_existing_locked(&fs, &resolved)?;
    if kind != Ext4NodeKind::Regular {
        return None;
    }
    drop(namespace);

    if let Some(data) = cached_executable_image(ino) {
        return Some(data);
    }
    // Executables are allowed to be run immediately after the linker closes
    // them.  The regular-file cache is authoritative until writeback; reading
    // the ext4 inode here would otherwise expose a stale or empty image and
    // make the child-side exec helper exit with status 2.
    if let Some(data) = cached_regular_snapshot(ino) {
        let data = Arc::new(data);
        cache_executable_image(ino, data.clone());
        return Some(data);
    }

    let inode_ref = fs.get_inode_ref(ino);
    if !inode_ref.inode.is_file() {
        return None;
    }

    // Runtime exec paths must get the exact file image. Use the inode size as
    // the authoritative bound and the same extent-aware path as regular file
    // I/O. The ext4_rs convenience reader repeatedly resolves each logical
    // block through its legacy mapping helper, which can return incorrect data
    // for files whose extent tree spans multiple nodes (large PIE executables
    // commonly put their dynamic table near the end of the file).
    let size = inode_ref.inode.size() as usize;
    let mut out = alloc::vec![0u8; size];
    let mut off = 0usize;
    while off < size {
        let n = extent_aware_read_at(ino, off, &mut out[off..]).ok()?;
        if n == 0 {
            return None;
        }
        off += n;
    }
    let out = Arc::new(out);
    cache_executable_image(ino, out.clone());
    Some(out)
}

pub fn slurp_regular_file(path: &str) -> Option<Vec<u8>> {
    slurp_regular_file_shared(path).map(|data| data.as_ref().clone())
}

pub fn ext4_regular_file_exists(path: &str) -> bool {
    let Some(fs) = ROOT_EXT4.lock().clone() else {
        return false;
    };
    resolve_existing(&fs, path)
        .map(|(_ino, kind)| kind == Ext4NodeKind::Regular)
        .unwrap_or(false)
}

pub fn ext4_file_path_exists(path: &str) -> bool {
    let Some(fs) = ROOT_EXT4.lock().clone() else {
        return false;
    };
    resolve_existing(&fs, path)
        .map(|(_ino, kind)| kind == Ext4NodeKind::Regular || kind == Ext4NodeKind::Symlink)
        .unwrap_or(false)
}

pub fn ext4_dir_path_exists(path: &str) -> bool {
    let Some(fs) = ROOT_EXT4.lock().clone() else {
        return false;
    };
    resolve_existing(&fs, path)
        .map(|(_, kind)| kind == Ext4NodeKind::Directory)
        .unwrap_or(false)
}

/// 枚举目录单层子项（文件名 + 是否为目录）。
pub fn ext4_list_dir(dir_path: &str) -> Result<Vec<(String, bool)>, SysErrNo> {
    let _namespace = NAMESPACE_LOCK.read();
    let fs = ROOT_EXT4.lock().clone().ok_or(SysErrNo::ENOENT)?;
    let (ino, kind) = resolve_existing_locked(&fs, dir_path).ok_or(SysErrNo::ENOENT)?;
    if kind != Ext4NodeKind::Directory {
        return Err(SysErrNo::ENOTDIR);
    }

    let out: Vec<(String, bool)> = cached_dir_entries_locked(&fs, ino)
        .iter()
        .map(|(name, (_, is_dir))| (name.clone(), *is_dir))
        .collect();
    Ok(out)
}

/// 枚举 ext4 目录项（按 inode 号），用于 `getdents64` 对 `Ext4Dir` fd 的支持。
/// 返回 `(child_ino, name, is_dir)` 元组的 `Vec`，跳过 `.` 和 `..`。
pub fn ext4_list_dir_by_ino(ino: u32) -> Result<Vec<(u32, String, bool)>, SysErrNo> {
    let _namespace = NAMESPACE_LOCK.read();
    let fs = ROOT_EXT4.lock().clone().ok_or(SysErrNo::ENOENT)?;
    Ok(cached_dir_entries_locked(&fs, ino)
        .iter()
        .map(|(name, (child_ino, is_dir))| (*child_ino, name.clone(), *is_dir))
        .collect())
}

fn ext4_gather_file_paths(fs: &Ext4, dir_path: &str, parent_ino: u32, out: &mut Vec<String>) {
    for (name, (child_ino, is_dir)) in cached_dir_entries_locked(fs, parent_ino).iter() {
        let full_path = if dir_path == "/" {
            format!("/{}", name)
        } else {
            format!("{}/{}", dir_path, name)
        };

        if *is_dir {
            ext4_gather_file_paths(fs, &full_path, *child_ino, out);
        } else {
            out.push(full_path);
        }
    }
}

/// 枚举卷上全部普通文件的绝对路径（用于 harness 发现测试脚本）。
pub fn ext4_list_all_file_paths() -> Vec<String> {
    let _namespace = NAMESPACE_LOCK.read();
    let Some(fs) = ROOT_EXT4.lock().clone() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    ext4_gather_file_paths(&fs, "/", ROOT_INODE, &mut out);
    out.sort();
    out
}

/// MemFS + ext4：合并列出文件路径，`MEM_FS` 中的路径优先，避免重复。
pub fn merged_list_all_file_paths(mem_paths: Vec<String>) -> Vec<String> {
    let mut set: BTreeSet<String> = mem_paths.into_iter().collect();
    let mut v: Vec<String> = set.iter().cloned().collect();
    if is_ext4_mounted() {
        for p in ext4_list_all_file_paths() {
            if set.insert(p.clone()) {
                v.push(p);
            }
        }
    }
    v.sort();
    v
}
