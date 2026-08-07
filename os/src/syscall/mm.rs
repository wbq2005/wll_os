use super::SyscallRet;
use crate::config::{PAGE_SIZE, USER_HEAP_START, USER_STACK_TOP};
use crate::fs::{ext4_vol, fd::FileDescriptor};
use crate::mm::frame_allocator::{self, FrameTracker};
use crate::mm::map_area::{MapArea, MapAreaBacking};
use crate::mm::page_table::PTEFlags;
use crate::task::current_task;
use crate::utils::error::SysErrNo;
use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicUsize, Ordering};
use lazy_static::lazy_static;
use polyhal::VirtAddr;
use spin::Mutex;

const PROT_READ: i32 = 0x1;
const PROT_WRITE: i32 = 0x2;
const PROT_EXEC: i32 = 0x4;
const PROT_MASK: i32 = PROT_READ | PROT_WRITE | PROT_EXEC;
// Keep a guard above the traditional brk base while allowing large sparse
// reservations (thread stacks and allocator arenas) to reuse the otherwise
// empty lower half of the 2 GiB user window.
const MMAP_BASE: usize = 0x2000_0000;

const MAP_SHARED: usize = 0x01;
const MAP_PRIVATE: usize = 0x02;
const MAP_SHARED_VALIDATE: usize = 0x03;
const MAP_TYPE: usize = 0x0f;
const MAP_FIXED: usize = 0x10;
const MAP_ANONYMOUS: usize = 0x20;
const MAP_DENYWRITE: usize = 0x0800;
const MAP_EXECUTABLE: usize = 0x1000;
const MAP_LOCKED: usize = 0x2000;
const MAP_NORESERVE: usize = 0x4000;
const MAP_POPULATE: usize = 0x8000;
const MAP_NONBLOCK: usize = 0x10000;
const MAP_STACK: usize = 0x20000;
const MAP_HUGETLB: usize = 0x40000;
const MAP_SYNC: usize = 0x80000;
const MAP_FIXED_NOREPLACE: usize = 0x100000;
const MS_ASYNC: usize = 0x1;
const MS_INVALIDATE: usize = 0x2;
const MS_SYNC: usize = 0x4;
const MS_SUPPORTED: usize = MS_ASYNC | MS_INVALIDATE | MS_SYNC;
const IPC_PRIVATE: isize = 0;
const IPC_CREAT: i32 = 0o1000;
const IPC_EXCL: i32 = 0o2000;
const IPC_RMID: i32 = 0;
const IPC_SET: i32 = 1;
const IPC_STAT: i32 = 2;
const SHM_RDONLY: i32 = 0o10000;
const SHM_RND: i32 = 0o20000;
const SHM_REMAP: i32 = 0o40000;
const SHM_EXEC: i32 = 0o100000;
const SHM_SUPPORTED_FLAGS: i32 = SHM_RDONLY | SHM_RND | SHM_REMAP | SHM_EXEC;
const MAP_COMPAT_IGNORED: usize = MAP_DENYWRITE
    | MAP_EXECUTABLE
    | MAP_LOCKED
    | MAP_NORESERVE
    | MAP_POPULATE
    | MAP_NONBLOCK
    | MAP_STACK;
const MAP_SUPPORTED: usize =
    MAP_TYPE | MAP_FIXED | MAP_ANONYMOUS | MAP_FIXED_NOREPLACE | MAP_COMPAT_IGNORED;

#[derive(Clone)]
struct SharedMemorySegment {
    key: isize,
    size: usize,
    perm: u16,
    cpid: usize,
    lpid: usize,
    atime: isize,
    dtime: isize,
    ctime: isize,
    frames: Vec<FrameTracker>,
    attach_count: usize,
    marked_for_remove: bool,
}

lazy_static! {
    static ref SHM_SEGMENTS: Mutex<BTreeMap<usize, SharedMemorySegment>> =
        Mutex::new(BTreeMap::new());
}

static NEXT_SHMID: AtomicUsize = AtomicUsize::new(1);

fn align_down(value: usize) -> usize {
    value / PAGE_SIZE * PAGE_SIZE
}

fn align_up(value: usize) -> Result<usize, SysErrNo> {
    value
        .checked_add(PAGE_SIZE - 1)
        .map(|value| value / PAGE_SIZE * PAGE_SIZE)
        .ok_or(SysErrNo::EINVAL)
}

fn find_mmap_area(
    memory_set: &crate::mm::memory_set::MemorySet,
    hint: usize,
    length: usize,
) -> Option<usize> {
    let search_start = hint.max(MMAP_BASE);
    memory_set
        .find_free_area(search_start, length, USER_STACK_TOP)
        .or_else(|| {
            let wrap_limit = search_start.min(USER_STACK_TOP);
            if MMAP_BASE < wrap_limit {
                memory_set.find_free_area(MMAP_BASE, length, wrap_limit)
            } else {
                None
            }
        })
}

fn current_time_sec() -> isize {
    let (sec, _) = crate::timer::get_timeval();
    sec as isize
}

fn checked_range(start: usize, length: usize) -> Result<(usize, usize), SysErrNo> {
    let end = start.checked_add(length).ok_or(SysErrNo::EINVAL)?;
    Ok((start, align_up(end)?))
}

fn ranges_overlap(
    left_start: usize,
    left_end: usize,
    right_start: usize,
    right_end: usize,
) -> bool {
    left_start < right_end && right_start < left_end
}

fn read_area_bytes(area: &MapArea, src: usize, dst: &mut [u8]) -> Result<(), SysErrNo> {
    let mut copied = 0usize;
    while copied < dst.len() {
        let addr = src.checked_add(copied).ok_or(SysErrNo::EFAULT)?;
        if !area.contains(VirtAddr::new(addr)) {
            return Err(SysErrNo::EFAULT);
        }
        let page_idx = (align_down(addr) - area.start_va.raw()) / PAGE_SIZE;
        let page_off = addr % PAGE_SIZE;
        let copy_len = (dst.len() - copied).min(PAGE_SIZE - page_off);
        let frame = area.frames.get(page_idx).ok_or(SysErrNo::EFAULT)?;
        let src_ptr = (frame.ppn().addr() + page_off) as *const u8;
        unsafe {
            core::ptr::copy_nonoverlapping(src_ptr, dst[copied..].as_mut_ptr(), copy_len);
        }
        copied += copy_len;
    }
    Ok(())
}

fn collect_shared_file_writes_for(
    task: &crate::task::TaskControlBlock,
    start: usize,
    end: usize,
    target_file: Option<&FileDescriptor>,
) -> Result<Vec<(FileDescriptor, usize, Vec<u8>)>, SysErrNo> {
    let ms = crate::buildstorm_memory_set_lock!(&task.memory_set);
    let mut writes = Vec::new();
    for area in &ms.areas {
        if !ranges_overlap(start, end, area.start_va.raw(), area.end_va.raw()) {
            continue;
        }
        let MapAreaBacking::File {
            file,
            offset,
            shared,
        } = &area.backing
        else {
            continue;
        };
        if !*shared || !area.flags.contains(PTEFlags::W) {
            continue;
        }
        if target_file
            .map(|target| !file.same_file_identity(target))
            .unwrap_or(false)
        {
            continue;
        }
        if area.frames.is_empty() {
            continue;
        }
        let copy_start = start.max(area.start_va.raw());
        let copy_end = end.min(area.end_va.raw());
        let mut data = alloc::vec![0u8; copy_end - copy_start];
        read_area_bytes(area, copy_start, &mut data)?;
        writes.push((
            file.clone(),
            offset.saturating_add(copy_start - area.start_va.raw()),
            data,
        ));
    }
    Ok(writes)
}

fn collect_shared_file_writes(
    task: &crate::task::TaskControlBlock,
    start: usize,
    end: usize,
) -> Result<Vec<(FileDescriptor, usize, Vec<u8>)>, SysErrNo> {
    collect_shared_file_writes_for(task, start, end, None)
}

fn write_back_shared_files(writes: Vec<(FileDescriptor, usize, Vec<u8>)>) -> Result<(), SysErrNo> {
    for (mut file, offset, data) in writes {
        if data.is_empty() {
            continue;
        }
        let written = super::with_kernel_page_table(|| file.write_at(offset, &data))?;
        if written != data.len() {
            return Err(SysErrNo::EIO);
        }
        super::with_kernel_page_table(|| file.sync(false))?;
    }
    Ok(())
}

pub(crate) fn write_back_shared_mappings_for_file(file: &FileDescriptor) -> Result<(), SysErrNo> {
    for task in crate::task::manager::all_user_tasks() {
        let writes = collect_shared_file_writes_for(&task, 0, USER_STACK_TOP, Some(file))?;
        write_back_shared_files(writes)?;
    }
    Ok(())
}

pub(crate) fn write_back_all_shared_file_mappings() -> Result<(), SysErrNo> {
    for task in crate::task::manager::all_user_tasks() {
        let writes = collect_shared_file_writes(&task, 0, USER_STACK_TOP)?;
        write_back_shared_files(writes)?;
    }
    Ok(())
}

fn shm_find_by_key(segments: &BTreeMap<usize, SharedMemorySegment>, key: isize) -> Option<usize> {
    segments
        .iter()
        .find(|(_, segment)| segment.key == key && !segment.marked_for_remove)
        .map(|(shmid, _)| *shmid)
}

fn shm_alloc_frames(size: usize) -> Result<Vec<FrameTracker>, SysErrNo> {
    let len = align_up(size)?;
    let pages = len / PAGE_SIZE;
    let mut frames = Vec::new();
    for _ in 0..pages {
        frames.push(frame_allocator::alloc_frame().ok_or(SysErrNo::ENOMEM)?);
    }
    Ok(frames)
}

// A single shmat attachment can split into several VMAs after mprotect/munmap.
fn task_shared_memory_attachments(task: &crate::task::TaskControlBlock) -> Vec<(usize, usize)> {
    let ms = crate::buildstorm_memory_set_lock!(&task.memory_set);
    let mut attachments = Vec::new();
    for area in &ms.areas {
        let MapAreaBacking::SharedMemory { shmid, base, .. } = &area.backing else {
            continue;
        };
        if attachments
            .iter()
            .any(|(id, attach_base)| *id == *shmid && *attach_base == *base)
        {
            continue;
        }
        attachments.push((*shmid, *base));
    }
    attachments
}

pub(crate) fn inherit_task_shared_memory(task: &crate::task::TaskControlBlock) {
    let attachments = task_shared_memory_attachments(task);
    if attachments.is_empty() {
        return;
    }
    let mut segments = SHM_SEGMENTS.lock();
    for (shmid, _) in attachments {
        if let Some(segment) = segments.get_mut(&shmid) {
            segment.attach_count = segment.attach_count.saturating_add(1);
        }
    }
}

pub(crate) fn detach_task_shared_memory(task: &crate::task::TaskControlBlock) {
    let attachments = task_shared_memory_attachments(task);
    if attachments.is_empty() {
        return;
    }
    let now = current_time_sec();
    let mut remove_ids = Vec::new();
    let mut segments = SHM_SEGMENTS.lock();
    for (shmid, _) in attachments {
        let Some(segment) = segments.get_mut(&shmid) else {
            continue;
        };
        segment.attach_count = segment.attach_count.saturating_sub(1);
        segment.lpid = task.thread_group.tgid();
        segment.dtime = now;
        if segment.marked_for_remove && segment.attach_count == 0 {
            remove_ids.push(shmid);
        }
    }
    for shmid in remove_ids {
        segments.remove(&shmid);
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
struct UserIpcPerm {
    key: i32,
    uid: u32,
    gid: u32,
    cuid: u32,
    cgid: u32,
    mode: u32,
    seq: i32,
    pad1: isize,
    pad2: isize,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct UserShmidDs {
    shm_perm: UserIpcPerm,
    shm_segsz: usize,
    shm_atime: isize,
    shm_dtime: isize,
    shm_ctime: isize,
    shm_cpid: i32,
    shm_lpid: i32,
    shm_nattch: usize,
    pad1: usize,
    pad2: usize,
}

fn user_shmid_ds(shmid: usize, segment: &SharedMemorySegment) -> UserShmidDs {
    let mode = (segment.perm & 0o777) as u32;
    UserShmidDs {
        shm_perm: UserIpcPerm {
            key: segment.key as i32,
            uid: 0,
            gid: 0,
            cuid: 0,
            cgid: 0,
            mode,
            seq: shmid as i32,
            pad1: 0,
            pad2: 0,
        },
        shm_segsz: segment.size,
        shm_atime: segment.atime,
        shm_dtime: segment.dtime,
        shm_ctime: segment.ctime,
        shm_cpid: segment.cpid as i32,
        shm_lpid: segment.lpid as i32,
        shm_nattch: segment.attach_count,
        pad1: 0,
        pad2: 0,
    }
}

fn copy_shmid_ds_to_user(
    addr: usize,
    shmid: usize,
    segment: &SharedMemorySegment,
) -> Result<(), SysErrNo> {
    if addr == 0 {
        return Err(SysErrNo::EFAULT);
    }
    super::user::copy_object_to_user(addr, &user_shmid_ds(shmid, segment))
}

fn prot_to_pte_flags(prot: i32) -> Result<PTEFlags, SysErrNo> {
    if (prot & !PROT_MASK) != 0 {
        return Err(SysErrNo::EINVAL);
    }

    let mut flags = PTEFlags::U | PTEFlags::V;
    if (prot & PROT_READ) != 0 {
        flags |= PTEFlags::R;
    }
    if (prot & PROT_WRITE) != 0 {
        // RISC-V treats W without R as a reserved PTE encoding. Promoting W to
        // R keeps write mappings valid while preserving the requested write bit.
        flags |= PTEFlags::R | PTEFlags::W;
    }
    if (prot & PROT_EXEC) != 0 {
        flags |= PTEFlags::X;
    }
    Ok(flags)
}

fn pte_has_leaf_permission(flags: PTEFlags) -> bool {
    flags.intersects(PTEFlags::R | PTEFlags::W | PTEFlags::X)
}

/// brk system call.
pub fn sys_brk(new_brk: usize) -> SyscallRet {
    let task = current_task().ok_or(SysErrNo::ESRCH)?;
    let (current_break, mapped_break) = {
        let mm = task.mm.lock();
        (mm.program_break, mm.mapped_break)
    };

    if new_brk == 0 {
        return Ok(current_break);
    }
    if new_brk < USER_HEAP_START {
        return Ok(current_break);
    }

    let new_mapped_end = match align_up(new_brk) {
        Ok(end) => end,
        Err(_) => return Ok(current_break),
    };
    if new_mapped_end > USER_STACK_TOP {
        return Ok(current_break);
    }

    if new_mapped_end != mapped_break {
        let mut ms = crate::buildstorm_memory_set_lock!(&task.memory_set);
        if new_mapped_end > mapped_break {
            if ms.range_overlaps(mapped_break, new_mapped_end) {
                return Ok(current_break);
            }
            if ms
                .insert_lazy_area_with_backing(
                    VirtAddr::new(mapped_break),
                    VirtAddr::new(new_mapped_end),
                    PTEFlags::U | PTEFlags::R | PTEFlags::W | PTEFlags::V,
                    MapAreaBacking::Anonymous,
                )
                .is_err()
            {
                return Ok(current_break);
            }
        } else if ms
            .unmap_range(VirtAddr::new(new_mapped_end), VirtAddr::new(mapped_break))
            .is_err()
        {
            return Ok(current_break);
        }
    }

    let mut mm = task.mm.lock();
    mm.program_break = new_brk;
    mm.mapped_break = new_mapped_end;
    Ok(new_brk)
}

fn validate_mmap_flags(flags: usize) -> Result<(), SysErrNo> {
    let unsupported = flags & !MAP_SUPPORTED;
    if unsupported == 0 {
        return Ok(());
    }
    if (flags & MAP_TYPE) == MAP_SHARED_VALIDATE {
        return Err(SysErrNo::EOPNOTSUPP);
    }
    if (unsupported & (MAP_HUGETLB | MAP_SYNC)) != 0 {
        return Err(SysErrNo::EINVAL);
    }
    Err(SysErrNo::EINVAL)
}

fn mmap_type(flags: usize) -> Result<bool, SysErrNo> {
    match flags & MAP_TYPE {
        MAP_SHARED | MAP_SHARED_VALIDATE => Ok(true),
        MAP_PRIVATE => Ok(false),
        _ => Err(SysErrNo::EINVAL),
    }
}

/// mmap system call.
pub fn sys_mmap(
    addr: usize,
    length: usize,
    prot: i32,
    flags_arg: i32,
    fd: i32,
    offset: usize,
) -> SyscallRet {
    #[cfg(feature = "buildstorm-diagnostics")]
    let _diag = crate::buildstorm_diagnostics::WorkScope::new(crate::buildstorm_diagnostics::WorkClass::Mmap);
    log::info!(
        "[syscall] mmap(addr={:#x}, len={:#x}, prot={:#x}, flags={:#x}, fd={}, off={:#x})",
        addr,
        length,
        prot,
        flags_arg,
        fd,
        offset
    );

    let task = current_task().ok_or(SysErrNo::ESRCH)?;
    if length == 0 {
        return Err(SysErrNo::EINVAL);
    }

    let flags = flags_arg as usize;
    validate_mmap_flags(flags)?;
    let shared = mmap_type(flags)?;
    let map_fixed = (flags & MAP_FIXED) != 0;
    let no_replace = (flags & MAP_FIXED_NOREPLACE) != 0;
    let fixed_addr = map_fixed || no_replace;
    let anonymous = (flags & MAP_ANONYMOUS) != 0;
    let pte_flags = prot_to_pte_flags(prot)?;
    let map_len = align_up(length)?;

    if !anonymous && offset % PAGE_SIZE != 0 {
        return Err(SysErrNo::EINVAL);
    }
    if !anonymous && fd < 0 {
        return Err(SysErrNo::EBADF);
    }
    if fixed_addr && addr % PAGE_SIZE != 0 {
        return Err(SysErrNo::EINVAL);
    }

    let next_hint = task.mm.lock().next_mmap;
    let mut start = {
        let ms = crate::buildstorm_memory_set_lock!(&task.memory_set);
        if fixed_addr {
            addr
        } else if addr != 0 {
            let hint = align_up(addr)?;
            let end = hint.checked_add(map_len).ok_or(SysErrNo::EINVAL)?;
            if end <= USER_STACK_TOP && !ms.range_overlaps(hint, end) {
                hint
            } else {
                find_mmap_area(&ms, next_hint, map_len).ok_or(SysErrNo::ENOMEM)?
            }
        } else {
            find_mmap_area(&ms, next_hint, map_len).ok_or(SysErrNo::ENOMEM)?
        }
    };
    let mut end = start.checked_add(map_len).ok_or(SysErrNo::EINVAL)?;
    if start < PAGE_SIZE {
        return Err(SysErrNo::EPERM);
    }
    if end > USER_STACK_TOP {
        return Err(SysErrNo::ENOMEM);
    }

    let map_has_leaf = pte_has_leaf_permission(pte_flags);
    let (backing, file_data, lazy_clean_mmap) = if anonymous {
        (MapAreaBacking::Anonymous, None, false)
    } else {
        let inner = task.inner.lock();
        let mut fds = inner.fd_table.lock();
        let file_desc = fds.get_mut(fd as usize).ok_or(SysErrNo::EBADF)?;
        if !file_desc.readable() {
            return Err(SysErrNo::EACCES);
        }
        if shared && (prot & PROT_WRITE) != 0 && !file_desc.writable() {
            return Err(SysErrNo::EACCES);
        }

        let use_clean_page_cache = map_has_leaf
            && (prot & PROT_WRITE) == 0
            && match file_desc {
                FileDescriptor::Ext4Regular { ino, .. } => ext4_vol::can_use_clean_page_cache(*ino),
                _ => false,
            };

        let data = if map_has_leaf && !use_clean_page_cache {
            let mut data = Vec::new();
            data.resize(length, 0);
            let n = super::with_kernel_page_table(|| file_desc.read_at(offset, &mut data))?;
            data.truncate(n);
            Some(data)
        } else {
            None
        };
        (
            MapAreaBacking::File {
                file: file_desc.clone(),
                offset,
                shared,
            },
            data,
            use_clean_page_cache,
        )
    };

    if map_fixed && !no_replace {
        let writes = collect_shared_file_writes(&task, start, end)?;
        write_back_shared_files(writes)?;
    }

    {
        let mut ms = crate::buildstorm_memory_set_lock!(&task.memory_set);
        if no_replace && ms.range_overlaps(start, end) {
            return Err(SysErrNo::EEXIST);
        }
        if map_fixed && !no_replace {
            ms.unmap_range(VirtAddr::new(start), VirtAddr::new(end))?;
        } else if !fixed_addr && ms.range_overlaps(start, end) {
            start = find_mmap_area(&ms, MMAP_BASE, map_len).ok_or(SysErrNo::ENOMEM)?;
            end = start.checked_add(map_len).ok_or(SysErrNo::EINVAL)?;
            if end > USER_STACK_TOP || ms.range_overlaps(start, end) {
                return Err(SysErrNo::ENOMEM);
            }
        }

        let lazy_private_anonymous = anonymous
            && !shared
            && (flags & (MAP_POPULATE | MAP_LOCKED)) == 0;
        if lazy_clean_mmap || lazy_private_anonymous {
            ms.insert_lazy_area_with_backing(
                VirtAddr::new(start),
                VirtAddr::new(end),
                pte_flags,
                backing,
            )?;
        } else {
            ms.insert_framed_area_with_backing(
                VirtAddr::new(start),
                VirtAddr::new(end),
                pte_flags,
                backing,
            )?;
            if let Some(data) = file_data {
                ms.write_bytes(start, &data)?;
            }
        }
    }

    let mut mm = task.mm.lock();
    if mm.next_mmap < end {
        mm.next_mmap = end;
    }
    Ok(start)
}

/// mprotect system call.
pub fn sys_mprotect(addr: usize, len: usize, prot: i32) -> SyscallRet {
    #[cfg(feature = "buildstorm-diagnostics")]
    let _diag = crate::buildstorm_diagnostics::WorkScope::new(crate::buildstorm_diagnostics::WorkClass::Mprotect);
    log::debug!(
        "[syscall] mprotect(addr={:#x}, len={:#x}, prot={:#x})",
        addr,
        len,
        prot
    );
    if addr % PAGE_SIZE != 0 || len == 0 {
        return Err(SysErrNo::EINVAL);
    }

    let pte_flags = prot_to_pte_flags(prot)?;
    let (start, end) = checked_range(addr, len)?;
    let task = current_task().ok_or(SysErrNo::ESRCH)?;
    {
        let mut ms = crate::buildstorm_memory_set_lock!(&task.memory_set);
        ms.protect_range(VirtAddr::new(start), VirtAddr::new(end), pte_flags)?;
    }
    Ok(0)
}

/// munmap system call.
pub fn sys_munmap(addr: usize, length: usize) -> SyscallRet {
    #[cfg(feature = "buildstorm-diagnostics")]
    let _diag = crate::buildstorm_diagnostics::WorkScope::new(crate::buildstorm_diagnostics::WorkClass::Munmap);
    log::info!("[syscall] munmap(addr={:#x}, len={:#x})", addr, length);
    if addr % PAGE_SIZE != 0 || length == 0 {
        return Err(SysErrNo::EINVAL);
    }

    let start = align_down(addr);
    let end = align_up(addr.checked_add(length).ok_or(SysErrNo::EINVAL)?)?;
    let task = current_task().ok_or(SysErrNo::ESRCH)?;
    let writes = collect_shared_file_writes(&task, start, end)?;
    write_back_shared_files(writes)?;
    {
        let mut ms = crate::buildstorm_memory_set_lock!(&task.memory_set);
        ms.unmap_range(VirtAddr::new(start), VirtAddr::new(end))?;
    }
    Ok(0)
}

pub fn sys_msync(addr: usize, length: usize, flags: usize) -> SyscallRet {
    log::debug!(
        "[syscall] msync(addr={:#x}, len={:#x}, flags={:#x})",
        addr,
        length,
        flags
    );
    if addr % PAGE_SIZE != 0 || (flags & !MS_SUPPORTED) != 0 {
        return Err(SysErrNo::EINVAL);
    }
    if (flags & MS_ASYNC) != 0 && (flags & MS_SYNC) != 0 {
        return Err(SysErrNo::EINVAL);
    }
    if length == 0 {
        return Ok(0);
    }

    let start = align_down(addr);
    let end = align_up(addr.checked_add(length).ok_or(SysErrNo::EINVAL)?)?;
    let task = current_task().ok_or(SysErrNo::ESRCH)?;
    {
        let ms = crate::buildstorm_memory_set_lock!(&task.memory_set);
        if !ms.range_covered(start, end) {
            return Err(SysErrNo::ENOMEM);
        }
    }
    let writes = collect_shared_file_writes(&task, start, end)?;
    write_back_shared_files(writes)?;
    if (flags & MS_INVALIDATE) != 0 {
        let mut ms = crate::buildstorm_memory_set_lock!(&task.memory_set);
        ms.invalidate_file_range(VirtAddr::new(start), VirtAddr::new(end))?;
    }
    Ok(0)
}

pub fn sys_shmget(key: isize, size: usize, shmflg: i32) -> SyscallRet {
    log::info!(
        "[syscall] shmget(key={:#x}, size={:#x}, flags={:#x})",
        key,
        size,
        shmflg
    );
    let task = current_task().ok_or(SysErrNo::ESRCH)?;
    let create = (shmflg & IPC_CREAT) != 0;
    let excl = (shmflg & IPC_EXCL) != 0;
    let perm = (shmflg & 0o777) as u16;

    let mut segments = SHM_SEGMENTS.lock();
    if key != IPC_PRIVATE {
        if let Some(existing) = shm_find_by_key(&segments, key) {
            if create && excl {
                return Err(SysErrNo::EEXIST);
            }
            if size != 0
                && size
                    > segments
                        .get(&existing)
                        .map(|segment| segment.size)
                        .unwrap_or(0)
            {
                return Err(SysErrNo::EINVAL);
            }
            return Ok(existing);
        }
        if !create {
            return Err(SysErrNo::ENOENT);
        }
    }

    if size == 0 {
        return Err(SysErrNo::EINVAL);
    }
    let frames = shm_alloc_frames(size)?;
    let shmid = NEXT_SHMID.fetch_add(1, Ordering::SeqCst);
    let now = current_time_sec();
    segments.insert(
        shmid,
        SharedMemorySegment {
            key,
            size,
            perm,
            cpid: task.thread_group.tgid(),
            lpid: 0,
            atime: 0,
            dtime: 0,
            ctime: now,
            frames,
            attach_count: 0,
            marked_for_remove: false,
        },
    );
    Ok(shmid)
}

pub fn sys_shmat(shmid: usize, shmaddr: usize, shmflg: i32) -> SyscallRet {
    log::info!(
        "[syscall] shmat(shmid={}, addr={:#x}, flags={:#x})",
        shmid,
        shmaddr,
        shmflg
    );
    if (shmflg & !SHM_SUPPORTED_FLAGS) != 0 {
        return Err(SysErrNo::EINVAL);
    }
    let task = current_task().ok_or(SysErrNo::ESRCH)?;
    let read_only = (shmflg & SHM_RDONLY) != 0;
    let remap = (shmflg & SHM_REMAP) != 0;

    let (frames, map_len) = {
        let mut segments = SHM_SEGMENTS.lock();
        let segment = segments.get_mut(&shmid).ok_or(SysErrNo::EINVAL)?;
        if segment.marked_for_remove {
            return Err(SysErrNo::EINVAL);
        }
        segment.attach_count = segment.attach_count.saturating_add(1);
        segment.lpid = task.thread_group.tgid();
        segment.atime = current_time_sec();
        (segment.frames.clone(), segment.frames.len() * PAGE_SIZE)
    };

    let attach_result = (|| -> Result<usize, SysErrNo> {
        let start = if shmaddr == 0 {
            let next_hint = task.mm.lock().next_mmap;
            let ms = crate::buildstorm_memory_set_lock!(&task.memory_set);
            find_mmap_area(&ms, next_hint, map_len).ok_or(SysErrNo::ENOMEM)?
        } else if (shmflg & SHM_RND) != 0 {
            align_down(shmaddr)
        } else {
            if shmaddr % PAGE_SIZE != 0 {
                return Err(SysErrNo::EINVAL);
            }
            shmaddr
        };
        if start < PAGE_SIZE {
            return Err(SysErrNo::EINVAL);
        }
        let end = start.checked_add(map_len).ok_or(SysErrNo::EINVAL)?;
        if end > USER_STACK_TOP {
            return Err(SysErrNo::ENOMEM);
        }

        let mut pte_flags = PTEFlags::U | PTEFlags::V | PTEFlags::R;
        if !read_only {
            pte_flags |= PTEFlags::W;
        }
        if (shmflg & SHM_EXEC) != 0 {
            pte_flags |= PTEFlags::X;
        }

        if remap {
            if shmaddr == 0 {
                return Err(SysErrNo::EINVAL);
            }
            let writes = collect_shared_file_writes(&task, start, end)?;
            write_back_shared_files(writes)?;
        }

        {
            let mut ms = crate::buildstorm_memory_set_lock!(&task.memory_set);
            if remap {
                ms.unmap_range(VirtAddr::new(start), VirtAddr::new(end))?;
            } else if ms.range_overlaps(start, end) {
                return Err(SysErrNo::ENOMEM);
            }
            ms.insert_shared_framed_area(
                VirtAddr::new(start),
                VirtAddr::new(end),
                pte_flags,
                MapAreaBacking::SharedMemory {
                    shmid,
                    base: start,
                    offset: 0,
                },
                &frames,
            )?;
        }
        {
            let mut mm = task.mm.lock();
            if mm.next_mmap < end {
                mm.next_mmap = end;
            }
        }
        Ok(start)
    })();

    if attach_result.is_err() {
        let mut segments = SHM_SEGMENTS.lock();
        if let Some(segment) = segments.get_mut(&shmid) {
            segment.attach_count = segment.attach_count.saturating_sub(1);
        }
    }
    attach_result
}

pub fn sys_shmdt(shmaddr: usize) -> SyscallRet {
    log::info!("[syscall] shmdt(addr={:#x})", shmaddr);
    if shmaddr == 0 || shmaddr % PAGE_SIZE != 0 {
        return Err(SysErrNo::EINVAL);
    }
    let task = current_task().ok_or(SysErrNo::ESRCH)?;
    let (shmid, ranges) = {
        let ms = crate::buildstorm_memory_set_lock!(&task.memory_set);
        let area = ms
            .areas
            .iter()
            .find(|area| {
                matches!(
                    &area.backing,
                    MapAreaBacking::SharedMemory { base, .. } if *base == shmaddr
                )
            })
            .ok_or(SysErrNo::EINVAL)?;
        let MapAreaBacking::SharedMemory { shmid, base, .. } = &area.backing else {
            return Err(SysErrNo::EINVAL);
        };
        let shmid = *shmid;
        let base = *base;
        let ranges: Vec<(usize, usize)> = ms
            .areas
            .iter()
            .filter_map(|area| match &area.backing {
                MapAreaBacking::SharedMemory {
                    shmid: area_shmid,
                    base: area_base,
                    ..
                } if *area_shmid == shmid && *area_base == base => {
                    Some((area.start_va.raw(), area.end_va.raw()))
                }
                _ => None,
            })
            .collect();
        (shmid, ranges)
    };
    {
        let mut ms = crate::buildstorm_memory_set_lock!(&task.memory_set);
        for (start, end) in ranges {
            ms.unmap_range(VirtAddr::new(start), VirtAddr::new(end))?;
        }
    }
    {
        let mut segments = SHM_SEGMENTS.lock();
        let remove = if let Some(segment) = segments.get_mut(&shmid) {
            segment.attach_count = segment.attach_count.saturating_sub(1);
            segment.lpid = task.thread_group.tgid();
            segment.dtime = current_time_sec();
            segment.marked_for_remove && segment.attach_count == 0
        } else {
            false
        };
        if remove {
            segments.remove(&shmid);
        }
    }
    Ok(0)
}

pub fn sys_shmctl(shmid: usize, cmd: i32, buf: usize) -> SyscallRet {
    log::info!(
        "[syscall] shmctl(shmid={}, cmd={}, buf={:#x})",
        shmid,
        cmd,
        buf
    );
    let mut segments = SHM_SEGMENTS.lock();
    match cmd {
        IPC_STAT => {
            let segment = segments.get(&shmid).ok_or(SysErrNo::EINVAL)?;
            copy_shmid_ds_to_user(buf, shmid, segment)?;
            Ok(0)
        }
        IPC_SET => Err(SysErrNo::ENOSYS),
        IPC_RMID => {
            let remove_now = {
                let segment = segments.get_mut(&shmid).ok_or(SysErrNo::EINVAL)?;
                segment.marked_for_remove = true;
                segment.ctime = current_time_sec();
                segment.attach_count == 0
            };
            if remove_now {
                segments.remove(&shmid);
            }
            Ok(0)
        }
        _ => Err(SysErrNo::EINVAL),
    }
}
