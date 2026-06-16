//! 运行时 ext4：`VirtIO-BLK` + `mount_block_device` 后与 MemFS 并列作为读路径后端。
//!
//! `ext4_rs::ext4_file_open` 在 crates.io 版中有误（打开类型被写成目录），此处不用它；
//! 目录项逐级 `ext4_dir_get_entries`/`compare_name`，文件内容 [`Ext4::read_at`]。

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::format;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;

use ext4_rs::{Errno, Ext4, Ext4Error, InodeFileType};

/// ext4 标准根 inode 号（与 `ext4_rs` 内部一致，crate 根未再导出该常量）。
const ROOT_INODE: u32 = 2;
const MAX_FILE_OFFSET: usize = isize::MAX as usize;
const REGULAR_CACHE_LIMIT: usize = 8 * 1024 * 1024;
const DIRTY_RANGE_FILE_LIMIT: usize = 256 * 1024;
const CLEAN_PAGE_CACHE_LIMIT: usize = 2048;
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
use spin::Mutex;

use crate::config::PAGE_SIZE;
use crate::fs::normalize_path;
use crate::mm::frame_allocator::{self, FrameTracker};
use crate::utils::error::SysErrNo;

lazy_static! {
    /// 挂载后的 Ext4（无盘或未探测到 virtio 时为 `None`）
    pub static ref ROOT_EXT4: Mutex<Option<Arc<Ext4>>> = Mutex::new(None);
    static ref PATH_CACHE: Mutex<BTreeMap<String, (u32, Ext4NodeKind)>> =
        Mutex::new(BTreeMap::new());
    static ref DIR_CACHE: Mutex<BTreeMap<u32, Vec<(u32, String, bool)>>> =
        Mutex::new(BTreeMap::new());
    static ref DATA_TIME_OVERRIDES: Mutex<BTreeMap<u32, (u32, u32, u32, u32)>> =
        Mutex::new(BTreeMap::new());
    static ref REGULAR_FILE_CACHE: Mutex<BTreeMap<u32, CachedRegularFile>> =
        Mutex::new(BTreeMap::new());
    static ref CLEAN_PAGE_CACHE: Mutex<CleanPageCache> = Mutex::new(CleanPageCache::new());
    static ref OPEN_REGULAR_REFS: Mutex<BTreeMap<u32, usize>> = Mutex::new(BTreeMap::new());
    static ref PENDING_UNLINK_REGULAR: Mutex<BTreeSet<u32>> = Mutex::new(BTreeSet::new());
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
    Writeback,
    DirtyDuringWriteback,
}

#[derive(Clone, Debug)]
struct PageCacheDirtyModel {
    ranges: Vec<DirtyRange>,
    full_dirty: bool,
    size_dirty: bool,
    writeback: PageCacheWritebackState,
    seq: u64,
}

impl PageCacheDirtyModel {
    fn clean() -> Self {
        Self {
            ranges: Vec::new(),
            full_dirty: false,
            size_dirty: false,
            writeback: PageCacheWritebackState::Clean,
            seq: 0,
        }
    }

    fn dirty_whole_file() -> Self {
        Self {
            ranges: Vec::new(),
            full_dirty: true,
            size_dirty: true,
            writeback: PageCacheWritebackState::Dirty,
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

    fn begin_writeback(&mut self) -> u64 {
        self.writeback = PageCacheWritebackState::Writeback;
        self.seq
    }

    fn finish_writeback(&mut self, seq: u64) {
        if self.seq == seq {
            self.ranges.clear();
            self.full_dirty = false;
            self.size_dirty = false;
            self.writeback = PageCacheWritebackState::Clean;
        } else {
            self.writeback = PageCacheWritebackState::Dirty;
        }
    }

    fn fail_writeback(&mut self) {
        self.writeback = if self.data_dirty() {
            PageCacheWritebackState::Dirty
        } else {
            PageCacheWritebackState::Clean
        };
    }
}

#[derive(Clone, Debug)]
struct CachedRegularFile {
    data: Vec<u8>,
    dirty: PageCacheDirtyModel,
    mtime_sec: u32,
    mtime_extra: u32,
    ctime_sec: u32,
    ctime_extra: u32,
}

impl CachedRegularFile {
    fn is_dirty(&self) -> bool {
        self.dirty.is_dirty()
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct CleanPageKey {
    ino: u32,
    page_idx: usize,
}

struct CachedCleanPage {
    frame: FrameTracker,
    last_used: u64,
}

struct CleanPageCache {
    pages: BTreeMap<CleanPageKey, CachedCleanPage>,
    next_age: u64,
}

impl CleanPageCache {
    fn new() -> Self {
        Self {
            pages: BTreeMap::new(),
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
        page.last_used = age;
        Some(page.frame.clone())
    }

    fn insert(&mut self, key: CleanPageKey, frame: FrameTracker) {
        let age = self.bump_age();
        if let Some(page) = self.pages.get_mut(&key) {
            page.frame = frame;
            page.last_used = age;
            return;
        }
        if self.pages.len() >= CLEAN_PAGE_CACHE_LIMIT {
            self.evict_one();
        }
        self.pages.insert(
            key,
            CachedCleanPage {
                frame,
                last_used: age,
            },
        );
    }

    fn evict_one(&mut self) {
        let victim = self
            .pages
            .iter()
            .min_by_key(|(_, page)| page.last_used)
            .map(|(key, _)| *key);
        if let Some(key) = victim {
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
            self.pages.remove(&key);
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
            self.pages.remove(&key);
        }
    }

    fn clear(&mut self) {
        self.pages.clear();
    }
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

fn current_ext4_time() -> u32 {
    let (sec, _) = crate::timer::get_timeval();
    sec.min(u32::MAX as usize) as u32
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
}

fn note_data_write(ino: u32) {
    let now = current_ext4_time();
    DATA_TIME_OVERRIDES.lock().insert(ino, (now, 0, now, 0));
}

fn note_cached_data_write(cached: &mut CachedRegularFile) {
    let now = current_ext4_time();
    cached.dirty.note_write();
    cached.mtime_sec = now;
    cached.mtime_extra = 0;
    cached.ctime_sec = now;
    cached.ctime_extra = 0;
}

fn mark_dirty_range(cached: &mut CachedRegularFile, start: usize, end: usize) {
    if start >= end {
        return;
    }
    let mut new_range = DirtyRange {
        start,
        end: end.min(cached.data.len()),
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

fn load_regular_data(fs: &Ext4, ino: u32) -> Result<Vec<u8>, SysErrNo> {
    let size = fs.get_inode_ref(ino).inode.size() as usize;
    let mut data = alloc::vec![0u8; size];
    let mut off = 0usize;
    while off < size {
        let n = fs
            .read_at(ino, off, &mut data[off..])
            .map_err(map_ext4_err)?;
        if n == 0 {
            return Err(SysErrNo::EIO);
        }
        off += n;
    }
    Ok(data)
}

fn clean_page_key(ino: u32, file_offset: usize) -> CleanPageKey {
    CleanPageKey {
        ino,
        page_idx: file_offset / PAGE_SIZE,
    }
}

fn fill_clean_page_frame(ino: u32, page_idx: usize) -> Result<FrameTracker, SysErrNo> {
    let frame = frame_allocator::alloc_frame().ok_or(SysErrNo::ENOMEM)?;
    let page_start = page_idx.checked_mul(PAGE_SIZE).ok_or(SysErrNo::EFBIG)?;
    let fs = ROOT_EXT4.lock().clone().ok_or(SysErrNo::ENOENT)?;
    let inode = fs.get_inode_ref(ino).inode;
    if !inode.is_file() {
        return Err(SysErrNo::EINVAL);
    }

    let size = inode.size() as usize;
    if page_start >= size {
        return Ok(frame);
    }

    let read_len = PAGE_SIZE.min(size - page_start);
    let page = unsafe { core::slice::from_raw_parts_mut(frame.ppn().addr() as *mut u8, PAGE_SIZE) };
    let mut copied = 0usize;
    while copied < read_len {
        let n = fs
            .read_at(ino, page_start + copied, &mut page[copied..read_len])
            .map_err(map_ext4_err)?;
        if n == 0 {
            return Err(SysErrNo::EIO);
        }
        copied += n;
    }
    Ok(frame)
}

pub fn clean_page_cache_frame(ino: u32, file_offset: usize) -> Result<FrameTracker, SysErrNo> {
    let key = clean_page_key(ino, file_offset);
    if let Some(frame) = CLEAN_PAGE_CACHE.lock().get(key) {
        return Ok(frame);
    }

    let frame = fill_clean_page_frame(ino, key.page_idx)?;
    let mut cache = CLEAN_PAGE_CACHE.lock();
    if let Some(existing) = cache.get(key) {
        return Ok(existing);
    }
    cache.insert(key, frame.clone());
    Ok(frame)
}

fn clean_page_cached_read(ino: u32, offset: usize, buf: &mut [u8]) -> Result<usize, SysErrNo> {
    if buf.is_empty() {
        return Ok(0);
    }
    let fs = ROOT_EXT4.lock().clone().ok_or(SysErrNo::ENOENT)?;
    let inode = fs.get_inode_ref(ino).inode;
    if !inode.is_file() {
        return Err(SysErrNo::EINVAL);
    }
    let size = inode.size() as usize;
    if offset >= size {
        return Ok(0);
    }
    let total = buf.len().min(size - offset);
    let mut copied = 0usize;
    while copied < total {
        let current = offset + copied;
        let page_off = current % PAGE_SIZE;
        let n = (total - copied).min(PAGE_SIZE - page_off);
        let frame = clean_page_cache_frame(ino, current)?;
        let src = (frame.ppn().addr() + page_off) as *const u8;
        unsafe {
            core::ptr::copy_nonoverlapping(src, buf[copied..].as_mut_ptr(), n);
        }
        copied += n;
    }
    Ok(copied)
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

fn ensure_regular_cache(ino: u32) -> Result<bool, SysErrNo> {
    if REGULAR_FILE_CACHE.lock().contains_key(&ino) {
        return Ok(true);
    }
    let fs = ROOT_EXT4.lock().clone().ok_or(SysErrNo::ENOENT)?;
    let inode = fs.get_inode_ref(ino).inode;
    if inode.size() > REGULAR_CACHE_LIMIT as u64 {
        return Ok(false);
    }
    let data = load_regular_data(&fs, ino)?;
    REGULAR_FILE_CACHE.lock().insert(
        ino,
        CachedRegularFile {
            data,
            dirty: PageCacheDirtyModel::clean(),
            mtime_sec: inode.mtime(),
            mtime_extra: inode.i_mtime_extra(),
            ctime_sec: inode.ctime(),
            ctime_extra: inode.i_ctime_extra(),
        },
    );
    Ok(true)
}

fn cached_regular_read(ino: u32, offset: usize, buf: &mut [u8]) -> Option<usize> {
    let cache = REGULAR_FILE_CACHE.lock();
    let cached = cache.get(&ino)?;
    if offset >= cached.data.len() {
        return Some(0);
    }
    let n = buf.len().min(cached.data.len() - offset);
    buf[..n].copy_from_slice(&cached.data[offset..offset + n]);
    Some(n)
}

fn cached_regular_size(ino: u32) -> Option<usize> {
    REGULAR_FILE_CACHE
        .lock()
        .get(&ino)
        .map(|cached| cached.data.len())
}

fn cached_regular_info(ino: u32) -> Option<CachedRegularInfo> {
    REGULAR_FILE_CACHE
        .lock()
        .get(&ino)
        .map(|cached| CachedRegularInfo {
            size: cached.data.len(),
            mtime_sec: cached.mtime_sec,
            mtime_extra: cached.mtime_extra,
            ctime_sec: cached.ctime_sec,
            ctime_extra: cached.ctime_extra,
        })
}

fn cached_regular_resize(ino: u32, new_len: usize) -> Result<(), SysErrNo> {
    invalidate_clean_pages_ino(ino);
    let mut cache = REGULAR_FILE_CACHE.lock();
    let cached = cache.get_mut(&ino).ok_or(SysErrNo::ENOENT)?;
    let old_len = cached.data.len();
    cached.data.resize(new_len, 0);
    if new_len > old_len {
        cached.dirty.full_dirty = true;
    } else if new_len < old_len {
        clip_dirty_ranges(cached, new_len);
    }
    cached.dirty.size_dirty = true;
    note_cached_data_write(cached);
    Ok(())
}

fn cached_regular_write(ino: u32, offset: usize, buf: &[u8]) -> Result<usize, SysErrNo> {
    let end = offset + buf.len();
    invalidate_clean_pages_range(ino, offset, end);
    let mut cache = REGULAR_FILE_CACHE.lock();
    let cached = cache.get_mut(&ino).ok_or(SysErrNo::ENOENT)?;
    let old_len = cached.data.len();
    if end > cached.data.len() {
        cached.data.resize(end, 0);
    }
    cached.data[offset..end].copy_from_slice(buf);
    if cached.dirty.full_dirty || end > old_len || cached.data.len() > DIRTY_RANGE_FILE_LIMIT {
        cached.dirty.full_dirty = true;
        if end > old_len {
            cached.dirty.size_dirty = true;
        }
    } else {
        mark_dirty_range(cached, offset, end);
    }
    note_cached_data_write(cached);
    Ok(buf.len())
}

fn is_regular_cached(ino: u32) -> bool {
    REGULAR_FILE_CACHE.lock().contains_key(&ino)
}

pub fn can_use_clean_page_cache(ino: u32) -> bool {
    !is_regular_cached(ino)
}

fn discard_regular_cache(ino: u32) {
    REGULAR_FILE_CACHE.lock().remove(&ino);
    invalidate_clean_pages_ino(ino);
    DATA_TIME_OVERRIDES.lock().remove(&ino);
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
    if last_ref && PENDING_UNLINK_REGULAR.lock().remove(&ino) {
        let _ = finish_unlinked_regular(ino);
    }
}

fn finish_unlinked_regular(ino: u32) -> Result<(), SysErrNo> {
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
    Ok(())
}

fn cache_empty_regular(ino: u32, dirty: bool) {
    let now = current_ext4_time();
    REGULAR_FILE_CACHE.lock().insert(
        ino,
        CachedRegularFile {
            data: Vec::new(),
            dirty: if dirty {
                PageCacheDirtyModel::dirty_whole_file()
            } else {
                PageCacheDirtyModel::clean()
            },
            mtime_sec: now,
            mtime_extra: 0,
            ctime_sec: now,
            ctime_extra: 0,
        },
    );
}

fn uncached_regular_write(ino: u32, offset: usize, buf: &[u8]) -> Result<usize, SysErrNo> {
    if buf.is_empty() {
        return Ok(0);
    }
    let fs = ROOT_EXT4.lock().clone().ok_or(SysErrNo::ENOENT)?;
    let written = fs.write_at(ino, offset, buf).map_err(map_ext4_err)?;
    if written != 0 {
        invalidate_clean_pages_range(ino, offset, offset.saturating_add(written));
        note_data_write(ino);
    }
    Ok(written)
}

fn flush_time_override(ino: u32) -> Result<(), SysErrNo> {
    let Some(times) = DATA_TIME_OVERRIDES.lock().get(&ino).copied() else {
        return Ok(());
    };
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
    Ok(())
}

pub fn flush_cached_ino(ino: u32) -> Result<(), SysErrNo> {
    let (
        target_len,
        whole_data,
        dirty_ranges,
        dirty_seq,
        mtime_sec,
        mtime_extra,
        ctime_sec,
        ctime_extra,
    ) = {
        let mut cache = REGULAR_FILE_CACHE.lock();
        let Some(cached) = cache.get_mut(&ino) else {
            return flush_time_override(ino);
        };
        if !cached.is_dirty() {
            return flush_time_override(ino);
        }
        let dirty_bytes = cached.dirty.ranges.iter().fold(0usize, |total, range| {
            total.saturating_add(range.end.saturating_sub(range.start))
        });
        let write_whole =
            cached.dirty.full_dirty || dirty_bytes.saturating_mul(2) >= cached.data.len();
        let dirty_seq = cached.dirty.begin_writeback();
        (
            cached.data.len(),
            write_whole.then(|| cached.data.clone()),
            cached.dirty.ranges.clone(),
            dirty_seq,
            cached.mtime_sec,
            cached.mtime_extra,
            cached.ctime_sec,
            cached.ctime_extra,
        )
    };

    let result = (|| -> Result<(), SysErrNo> {
        let fs = ROOT_EXT4.lock().clone().ok_or(SysErrNo::ENOENT)?;
        let old_size = fs.get_inode_ref(ino).inode.size();
        if (target_len as u64) < old_size {
            let mut iref = fs.get_inode_ref(ino);
            fs.truncate_inode(&mut iref, target_len as u64)
                .map_err(map_ext4_err)?;
        }

        if let Some(data) = whole_data {
            if !data.is_empty() {
                let written = fs.write_at(ino, 0, &data).map_err(map_ext4_err)?;
                if written != data.len() {
                    return Err(SysErrNo::EIO);
                }
            }
        } else {
            for range in dirty_ranges {
                let end = range.end.min(target_len);
                if range.start >= end {
                    continue;
                }
                let data = {
                    let cache = REGULAR_FILE_CACHE.lock();
                    let Some(cached) = cache.get(&ino) else {
                        return Ok(());
                    };
                    let current_end = end.min(cached.data.len());
                    if range.start >= current_end {
                        Vec::new()
                    } else {
                        cached.data[range.start..current_end].to_vec()
                    }
                };
                if data.is_empty() {
                    continue;
                }
                let written = fs.write_at(ino, range.start, &data).map_err(map_ext4_err)?;
                if written != data.len() {
                    return Err(SysErrNo::EIO);
                }
            }
        }

        if target_len == 0 && old_size != 0 {
            let current_size = fs.get_inode_ref(ino).inode.size();
            if current_size != 0 {
                return Err(SysErrNo::EIO);
            }
        }

        let mut iref = fs.get_inode_ref(ino);
        iref.inode.set_mtime(mtime_sec);
        iref.inode.set_i_mtime_extra(mtime_extra);
        iref.inode.set_ctime(ctime_sec);
        iref.inode.set_i_ctime_extra(ctime_extra);
        fs.write_back_inode(&mut iref);
        DATA_TIME_OVERRIDES.lock().remove(&ino);
        Ok(())
    })();

    let mut cache = REGULAR_FILE_CACHE.lock();
    if let Some(cached) = cache.get_mut(&ino) {
        if result.is_ok() {
            cached.dirty.finish_writeback(dirty_seq);
        } else {
            cached.dirty.fail_writeback();
        }
    }
    result
}

pub fn flush_all_cached() -> Result<(), SysErrNo> {
    let inos: Vec<u32> = REGULAR_FILE_CACHE
        .lock()
        .iter()
        .filter_map(|(ino, cached)| cached.is_dirty().then_some(*ino))
        .collect();
    for ino in inos {
        flush_cached_ino(ino)?;
    }
    let override_inos: Vec<u32> = DATA_TIME_OVERRIDES.lock().keys().copied().collect();
    for ino in override_inos {
        flush_time_override(ino)?;
    }
    clear_clean_page_cache();
    Ok(())
}

fn metadata_for_ino(fs: &Ext4, ino: u32) -> Ext4Metadata {
    let iref = fs.get_inode_ref(ino);
    let inode = iref.inode;
    let cached_info = cached_regular_info(ino);
    let mut meta = Ext4Metadata {
        ino,
        mode: inode.mode() as u32,
        nlink: inode.links_count() as u32,
        uid: inode.uid() as u32,
        gid: inode.gid() as u32,
        size: cached_info
            .map(|info| info.size as u64)
            .unwrap_or_else(|| inode.size()),
        blocks: inode.blocks_count(),
        atime_sec: inode.atime() as isize,
        atime_nsec: ext4_extra_nsec(inode.i_atime_extra()),
        mtime_sec: inode.mtime() as isize,
        mtime_nsec: ext4_extra_nsec(inode.i_mtime_extra()),
        ctime_sec: inode.ctime() as isize,
        ctime_nsec: ext4_extra_nsec(inode.i_ctime_extra()),
    };
    if let Some(info) = cached_info {
        meta.mtime_sec = info.mtime_sec as isize;
        meta.mtime_nsec = ext4_extra_nsec(info.mtime_extra);
        meta.ctime_sec = info.ctime_sec as isize;
        meta.ctime_nsec = ext4_extra_nsec(info.ctime_extra);
    } else if let Some((mtime, mtime_extra, ctime, ctime_extra)) =
        DATA_TIME_OVERRIDES.lock().get(&ino).copied()
    {
        meta.mtime_sec = mtime as isize;
        meta.mtime_nsec = ext4_extra_nsec(mtime_extra);
        meta.ctime_sec = ctime as isize;
        meta.ctime_nsec = ext4_extra_nsec(ctime_extra);
    }
    meta.blocks = meta.blocks.max(regular_blocks(meta.size));
    meta
}

fn clear_namespace_cache() {
    // Path and directory caches only depend on namespace topology. Regular-file
    // data writes update size/time through REGULAR_FILE_CACHE, or through
    // DATA_TIME_OVERRIDES for uncached writes, so they should not churn these
    // global caches.
    PATH_CACHE.lock().clear();
    DIR_CACHE.lock().clear();
}

fn clear_all_caches() {
    clear_namespace_cache();
    DATA_TIME_OVERRIDES.lock().clear();
    REGULAR_FILE_CACHE.lock().clear();
    clear_clean_page_cache();
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

/// 删除 ext4 上的普通文件（非目录）。
pub fn unlink_non_dir(path: &str) -> Result<(), SysErrNo> {
    let mut fs = ROOT_EXT4.lock().clone().ok_or(SysErrNo::ENOENT)?;
    let norm = normalize_path(path);
    let (parent_path, name) = split_parent_name(&norm)?;
    let Some((parent_ino, parent_kind)) = resolve_existing(&fs, &parent_path) else {
        return Err(SysErrNo::ENOENT);
    };
    if parent_kind != Ext4NodeKind::Directory {
        return Err(SysErrNo::ENOTDIR);
    }
    let Some((child_ino, child_kind)) = resolve_existing(&fs, &norm) else {
        return Err(SysErrNo::ENOENT);
    };
    if child_kind == Ext4NodeKind::Directory {
        return Err(SysErrNo::EISDIR);
    }
    invalidate_clean_pages_ino(child_ino);
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
    }
    fs.write_back_inode(&mut parent_ref);
    fs.write_back_inode(&mut child_ref);
    clear_namespace_cache();
    Ok(())
}

pub fn unlink_regular_file(path: &str) -> Result<(), SysErrNo> {
    unlink_non_dir(path)
}

pub fn mkdir_ext4_with_mode(path: &str, mode: u32) -> Result<(), SysErrNo> {
    let fs = ROOT_EXT4.lock().clone().ok_or(SysErrNo::ENOENT)?;
    let norm = normalize_path(path);
    let (parent_path, name) = split_parent_name(&norm)?;
    let Some((parent_ino, parent_kind)) = resolve_existing(&fs, &parent_path) else {
        return Err(SysErrNo::ENOENT);
    };
    if parent_kind != Ext4NodeKind::Directory {
        return Err(SysErrNo::ENOTDIR);
    }
    if resolve_existing(&fs, &norm).is_some() {
        return Err(SysErrNo::EEXIST);
    }
    let perm = (mode as u16) & 0o777;
    let mut child_ref = fs
        .create(parent_ino, &name, InodeFileType::S_IFDIR.bits() | perm)
        .map_err(map_ext4_err)?;
    let now = current_ext4_time();
    child_ref
        .inode
        .set_mode(InodeFileType::S_IFDIR.bits() | perm);
    child_ref.inode.set_atime(now);
    child_ref.inode.set_mtime(now);
    child_ref.inode.set_ctime(now);
    fs.write_back_inode(&mut child_ref);
    touch_inode(&fs, parent_ino, false, true, true);
    clear_namespace_cache();
    Ok(())
}

pub fn mkdir_ext4(path: &str) -> Result<(), SysErrNo> {
    mkdir_ext4_with_mode(path, 0o755)
}

pub fn remove_empty_dir_ext4(path: &str) -> Result<(), SysErrNo> {
    let fs = ROOT_EXT4.lock().clone().ok_or(SysErrNo::ENOENT)?;
    let norm = normalize_path(path);
    if norm == "/" {
        return Err(SysErrNo::EINVAL);
    }
    let (parent_path, name) = split_parent_name(&norm)?;
    let Some((parent_ino, parent_kind)) = resolve_existing(&fs, &parent_path) else {
        return Err(SysErrNo::ENOENT);
    };
    if parent_kind != Ext4NodeKind::Directory {
        return Err(SysErrNo::ENOTDIR);
    }
    let Some((child_ino, child_kind)) = resolve_existing(&fs, &norm) else {
        return Err(SysErrNo::ENOENT);
    };
    if child_kind != Ext4NodeKind::Directory {
        return Err(SysErrNo::ENOTDIR);
    }
    if !cached_dir_entries(&fs, child_ino).is_empty() {
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
    clear_namespace_cache();
    Ok(())
}

/// 创建普通文件（已存在则由 `generic_open` 语义处理）。
pub fn create_regular_ext4_with_mode(path: &str, mode: u32) -> Result<u32, SysErrNo> {
    let fs = ROOT_EXT4.lock().clone().ok_or(SysErrNo::ENOENT)?;
    let norm = normalize_path(path);
    let (parent_path, name) = split_parent_name(&norm)?;
    let Some((parent_ino, parent_kind)) = resolve_existing(&fs, &parent_path) else {
        return Err(SysErrNo::ENOENT);
    };
    if parent_kind != Ext4NodeKind::Directory {
        return Err(SysErrNo::ENOTDIR);
    }
    if resolve_existing(&fs, &norm).is_some() {
        return Err(SysErrNo::EEXIST);
    }
    let perm = (mode as u16) & 0o777;
    let mut iref = fs
        .create(parent_ino, &name, InodeFileType::S_IFREG.bits() | perm)
        .map_err(map_ext4_err)?;
    let now = current_ext4_time();
    iref.inode.set_mode(InodeFileType::S_IFREG.bits() | perm);
    iref.inode.set_atime(now);
    iref.inode.set_mtime(now);
    iref.inode.set_ctime(now);
    fs.write_back_inode(&mut iref);
    touch_inode(&fs, parent_ino, false, true, true);
    invalidate_clean_pages_ino(iref.inode_num);
    cache_empty_regular(iref.inode_num, false);
    clear_namespace_cache();
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
    }
    if kind == Ext4NodeKind::Regular && is_regular_cached(ino) {
        if size as usize <= REGULAR_CACHE_LIMIT {
            return cached_regular_resize(ino, size as usize);
        }
        flush_cached_ino(ino)?;
        discard_regular_cache(ino);
    }
    let mut iref = fs.get_inode_ref(ino);
    let old_size = iref.inode.size();
    if size < old_size {
        fs.truncate_inode(&mut iref, size).map_err(map_ext4_err)?;
        if kind == Ext4NodeKind::Regular && size == 0 {
            cache_empty_regular(ino, true);
        }
    } else if size > old_size {
        let zeroes = alloc::vec![0u8; 4096];
        let mut off = old_size as usize;
        let target = size as usize;
        while off < target {
            let n = (target - off).min(zeroes.len());
            let written = fs.write_at(ino, off, &zeroes[..n]).map_err(map_ext4_err)?;
            if written == 0 {
                return Err(SysErrNo::EIO);
            }
            off += written;
        }
    }
    touch_inode(&fs, ino, false, true, true);
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
    if let Some(n) = cached_regular_read(ino, offset, buf) {
        return Ok(n);
    }
    match clean_page_cached_read(ino, offset, buf) {
        Ok(n) => return Ok(n),
        Err(SysErrNo::ENOMEM | SysErrNo::EINVAL) => {}
        Err(err) => return Err(err),
    }
    let fs = ROOT_EXT4.lock().clone().ok_or(SysErrNo::ENOENT)?;
    fs.read_at(ino, offset, buf).map_err(map_ext4_err)
}

pub fn ext4_write_at(ino: u32, offset: usize, buf: &[u8]) -> Result<usize, SysErrNo> {
    checked_file_end(offset, buf.len())?;
    if buf.is_empty() {
        return Ok(0);
    }
    let end = offset + buf.len();
    if end > REGULAR_CACHE_LIMIT {
        if is_regular_cached(ino) {
            flush_cached_ino(ino)?;
            discard_regular_cache(ino);
        }
        return uncached_regular_write(ino, offset, buf);
    }
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

pub fn metadata(path: &str) -> Result<Ext4Metadata, SysErrNo> {
    let fs = ROOT_EXT4.lock().clone().ok_or(SysErrNo::ENOENT)?;
    let (ino, _) = resolve_existing(&fs, path).ok_or(SysErrNo::ENOENT)?;
    Ok(metadata_for_ino(&fs, ino))
}

pub fn set_times_ino(
    ino: u32,
    atime: Option<(isize, isize)>,
    mtime: Option<(isize, isize)>,
) -> Result<(), SysErrNo> {
    flush_cached_ino(ino)?;
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
    if let Some(cached) = REGULAR_FILE_CACHE.lock().get_mut(&ino) {
        if let Some((sec, nsec)) = mtime {
            cached.mtime_sec = (sec.max(0) as usize).min(u32::MAX as usize) as u32;
            cached.mtime_extra = ext4_nsec_extra(nsec);
        }
        cached.ctime_sec = now;
        cached.ctime_extra = 0;
    }
    clear_namespace_cache();
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

fn cached_dir_entries(fs: &Ext4, ino: u32) -> Vec<(u32, String, bool)> {
    if let Some(entries) = DIR_CACHE.lock().get(&ino).cloned() {
        return entries;
    }

    let mut out = Vec::new();
    for e in fs.ext4_dir_get_entries(ino) {
        if e.unused() {
            continue;
        }
        let name = e.get_name();
        if name == "." || name == ".." {
            continue;
        }
        let child_ino = e.inode;
        let is_subdir = fs.get_inode_ref(child_ino).inode.is_dir();
        out.push((child_ino, name, is_subdir));
    }
    DIR_CACHE.lock().insert(ino, out.clone());
    out
}

fn find_child_ino(fs: &Ext4, parent_ino: u32, name: &str) -> Option<u32> {
    for (child_ino, child_name, _) in cached_dir_entries(fs, parent_ino) {
        if child_name == name {
            return Some(child_ino);
        }
    }
    None
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
    let fs = ROOT_EXT4.lock().clone().ok_or(SysErrNo::ENOENT)?;
    let norm = normalize_path(path);
    let Some((ino, kind)) = resolve_existing(&fs, &norm) else {
        return Err(SysErrNo::ENOENT);
    };
    if kind != Ext4NodeKind::Symlink {
        return Err(SysErrNo::EINVAL);
    }

    let inode_ref = fs.get_inode_ref(ino);
    let size = inode_ref.inode.size() as usize;
    let mut data = alloc::vec![0u8; size];
    let read_ok = if size == 0 {
        true
    } else {
        fs.read_at(ino, 0, &mut data)
            .map(|n| n == size)
            .unwrap_or(false)
    };
    if !read_ok && size <= 60 {
        data.clear();
        for word in inode_ref.inode.block() {
            data.extend_from_slice(&word.to_le_bytes());
        }
        data.truncate(size);
    } else if !read_ok {
        return Err(SysErrNo::EIO);
    }
    String::from_utf8(data).map_err(|_| SysErrNo::EINVAL)
}

pub fn resolve_symlinks(path: &str) -> Result<String, SysErrNo> {
    let mut current = normalize_path(path);
    for _ in 0..8 {
        let Some((_ino, kind)) = lookup_kind(&current) else {
            return Err(SysErrNo::ENOENT);
        };
        if kind != Ext4NodeKind::Symlink {
            return Ok(current);
        }
        let target = readlink_ext4(&current)?;
        current = resolve_link_target(&current, &target);
    }
    Err(SysErrNo::ELOOP)
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
    let fs = ROOT_EXT4.lock().clone().ok_or(SysErrNo::ENOENT)?;
    let norm = normalize_path(link_path);
    let (parent_path, name) = split_parent_name(&norm)?;
    let Some((parent_ino, parent_kind)) = resolve_existing(&fs, &parent_path) else {
        return Err(SysErrNo::ENOENT);
    };
    if parent_kind != Ext4NodeKind::Directory {
        return Err(SysErrNo::ENOTDIR);
    }
    if resolve_existing(&fs, &norm).is_some() {
        return Err(SysErrNo::EEXIST);
    }
    let mut iref = fs
        .create(parent_ino, &name, InodeFileType::S_IFLNK.bits() | 0o777)
        .map_err(map_ext4_err)?;
    let now = current_ext4_time();
    iref.inode.set_mode(InodeFileType::S_IFLNK.bits() | 0o777);
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
    clear_namespace_cache();
    Ok(())
}

pub fn link_ext4(old_path: &str, new_path: &str, follow_old: bool) -> Result<(), SysErrNo> {
    let fs = ROOT_EXT4.lock().clone().ok_or(SysErrNo::ENOENT)?;
    let old = if follow_old {
        resolve_symlinks(old_path)?
    } else {
        normalize_path(old_path)
    };
    let new = normalize_path(new_path);
    let (new_parent_path, new_name) = split_parent_name(&new)?;
    let Some((new_parent_ino, new_parent_kind)) = resolve_existing(&fs, &new_parent_path) else {
        return Err(SysErrNo::ENOENT);
    };
    if new_parent_kind != Ext4NodeKind::Directory {
        return Err(SysErrNo::ENOTDIR);
    }
    if resolve_existing(&fs, &new).is_some() {
        return Err(SysErrNo::EEXIST);
    }
    let Some((old_ino, old_kind)) = resolve_existing(&fs, &old) else {
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
    clear_namespace_cache();
    Ok(())
}

pub fn rename_ext4(old_path: &str, new_path: &str, no_replace: bool) -> Result<(), SysErrNo> {
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
    let Some((old_parent_ino, old_parent_kind)) = resolve_existing(&fs, &old_parent_path) else {
        return Err(SysErrNo::ENOENT);
    };
    let Some((new_parent_ino, new_parent_kind)) = resolve_existing(&fs, &new_parent_path) else {
        return Err(SysErrNo::ENOENT);
    };
    if old_parent_kind != Ext4NodeKind::Directory || new_parent_kind != Ext4NodeKind::Directory {
        return Err(SysErrNo::ENOTDIR);
    }
    let Some((old_ino, old_kind)) = resolve_existing(&fs, &old) else {
        return Err(SysErrNo::ENOENT);
    };
    if old_kind == Ext4NodeKind::Directory && path_is_descendant(&old, &new) {
        return Err(SysErrNo::EINVAL);
    }
    let new_existing = resolve_existing(&fs, &new);
    if no_replace && new_existing.is_some() {
        return Err(SysErrNo::EEXIST);
    }
    if let Some((new_ino, new_kind)) = new_existing {
        if new_ino == old_ino {
            return Ok(());
        }
        match (old_kind, new_kind) {
            (Ext4NodeKind::Directory, Ext4NodeKind::Directory) => {
                if !cached_dir_entries(&fs, new_ino).is_empty() {
                    return Err(SysErrNo::ENOTEMPTY);
                }
                drop(fs);
                remove_empty_dir_ext4(&new)?;
            }
            (Ext4NodeKind::Directory, _) => return Err(SysErrNo::ENOTDIR),
            (_, Ext4NodeKind::Directory) => return Err(SysErrNo::EISDIR),
            _ => {
                drop(fs);
                unlink_non_dir(&new)?;
            }
        }
    }

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
    clear_namespace_cache();
    Ok(())
}

/// 解析已存在的绝对路径 → (inode, 是否为目录)。不存在或非法则 `None`。
fn resolve_existing(fs: &Ext4, path: &str) -> Option<(u32, Ext4NodeKind)> {
    let n = normalize_path(path);
    if !n.starts_with('/') {
        return None;
    }
    if let Some(found) = PATH_CACHE.lock().get(&n).copied() {
        return Some(found);
    }
    let tail = n.trim_matches('/');
    let parts: Vec<&str> = if tail.is_empty() {
        Vec::new()
    } else {
        tail.split('/').filter(|p| !p.is_empty()).collect()
    };

    if parts.is_empty() {
        let found = (ROOT_INODE, inode_kind(fs, ROOT_INODE));
        PATH_CACHE.lock().insert(n, found);
        return Some(found);
    }

    let mut parent = ROOT_INODE;
    for (i, comp) in parts.iter().enumerate() {
        let ino = find_child_ino(fs, parent, comp)?;
        if i + 1 == parts.len() {
            let found = (ino, inode_kind(fs, ino));
            PATH_CACHE.lock().insert(n, found);
            return Some(found);
        }
        if !fs.get_inode_ref(ino).inode.is_dir() {
            return None;
        }
        parent = ino;
    }
    None
}

/// 整块读入普通文件（用于 `execve` / harness）。目录或不存在返回 `None`。
pub fn slurp_regular_file(path: &str) -> Option<Vec<u8>> {
    let resolved = resolve_symlinks(path).ok()?;
    let fs = ROOT_EXT4.lock().clone()?;
    let (ino, kind) = resolve_existing(&fs, &resolved)?;
    let inode_ref = fs.get_inode_ref(ino);
    if kind != Ext4NodeKind::Regular || !inode_ref.inode.is_file() {
        return None;
    }

    // Runtime exec paths must get the exact file image. Some ext4 backends do
    // not make EOF detection by repeated read_at() robust enough for large
    // static ELFs, so use the inode size as the authoritative bound.
    let size = inode_ref.inode.size() as usize;
    let mut out = alloc::vec![0u8; size];
    let mut off = 0usize;
    while off < size {
        let n = fs.read_at(ino, off, &mut out[off..]).ok()?;
        if n == 0 {
            return None;
        }
        off += n;
    }
    Some(out)
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
    let fs = ROOT_EXT4.lock().clone().ok_or(SysErrNo::ENOENT)?;
    let (ino, kind) = resolve_existing(&fs, dir_path).ok_or(SysErrNo::ENOENT)?;
    if kind != Ext4NodeKind::Directory {
        return Err(SysErrNo::ENOTDIR);
    }

    let mut out: Vec<(String, bool)> = cached_dir_entries(&fs, ino)
        .into_iter()
        .map(|(_, name, is_dir)| (name, is_dir))
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}

/// 枚举 ext4 目录项（按 inode 号），用于 `getdents64` 对 `Ext4Dir` fd 的支持。
/// 返回 `(child_ino, name, is_dir)` 元组的 `Vec`，跳过 `.` 和 `..`。
pub fn ext4_list_dir_by_ino(ino: u32) -> Result<Vec<(u32, String, bool)>, SysErrNo> {
    let fs = ROOT_EXT4.lock().clone().ok_or(SysErrNo::ENOENT)?;
    Ok(cached_dir_entries(&fs, ino))
}

fn ext4_gather_file_paths(fs: &Ext4, dir_path: &str, parent_ino: u32, out: &mut Vec<String>) {
    for (child_ino, name, is_dir) in cached_dir_entries(fs, parent_ino) {
        let full_path = if dir_path == "/" {
            format!("/{}", name)
        } else {
            format!("{}/{}", dir_path, name)
        };

        if is_dir {
            ext4_gather_file_paths(fs, &full_path, child_ino, out);
        } else {
            out.push(full_path);
        }
    }
}

/// 枚举卷上全部普通文件的绝对路径（用于 harness 发现 *_testcode.sh）。
pub fn ext4_list_all_file_paths() -> Vec<String> {
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
