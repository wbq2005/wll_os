use super::SyscallRet;
use crate::config::{PAGE_SIZE, USER_HEAP_START, USER_STACK_TOP};
use crate::fs::fd::FileDescriptor;
use crate::mm::map_area::{MapArea, MapAreaBacking};
use crate::mm::page_table::PTEFlags;
use crate::task::current_task;
use crate::utils::error::SysErrNo;
use alloc::vec::Vec;
use polyhal::VirtAddr;

const PROT_READ: i32 = 0x1;
const PROT_WRITE: i32 = 0x2;
const PROT_EXEC: i32 = 0x4;
const PROT_MASK: i32 = PROT_READ | PROT_WRITE | PROT_EXEC;

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
const MAP_COMPAT_IGNORED: usize = MAP_DENYWRITE
    | MAP_EXECUTABLE
    | MAP_LOCKED
    | MAP_NORESERVE
    | MAP_POPULATE
    | MAP_NONBLOCK
    | MAP_STACK;
const MAP_SUPPORTED: usize =
    MAP_TYPE | MAP_FIXED | MAP_ANONYMOUS | MAP_FIXED_NOREPLACE | MAP_COMPAT_IGNORED;

fn align_down(value: usize) -> usize {
    value / PAGE_SIZE * PAGE_SIZE
}

fn align_up(value: usize) -> Result<usize, SysErrNo> {
    value
        .checked_add(PAGE_SIZE - 1)
        .map(|value| value / PAGE_SIZE * PAGE_SIZE)
        .ok_or(SysErrNo::EINVAL)
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

fn collect_shared_file_writes(
    task: &crate::task::TaskControlBlock,
    start: usize,
    end: usize,
) -> Result<Vec<(FileDescriptor, usize, Vec<u8>)>, SysErrNo> {
    let ms = task.memory_set.lock();
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

fn write_back_shared_files(writes: Vec<(FileDescriptor, usize, Vec<u8>)>) -> Result<(), SysErrNo> {
    for (mut file, offset, data) in writes {
        super::with_kernel_page_table(|| file.write_at(offset, &data))?;
    }
    Ok(())
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
        let mut ms = task.memory_set.lock();
        if new_mapped_end > mapped_break {
            if ms.range_overlaps(mapped_break, new_mapped_end) {
                return Ok(current_break);
            }
            if ms
                .insert_framed_area(
                    VirtAddr::new(mapped_break),
                    VirtAddr::new(new_mapped_end),
                    PTEFlags::U | PTEFlags::R | PTEFlags::W | PTEFlags::V,
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
        ms.activate();
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
    let start = {
        let ms = task.memory_set.lock();
        if fixed_addr {
            addr
        } else if addr != 0 {
            let hint = align_up(addr)?;
            let end = hint.checked_add(map_len).ok_or(SysErrNo::EINVAL)?;
            if end <= USER_STACK_TOP && !ms.range_overlaps(hint, end) {
                hint
            } else {
                ms.find_free_area(next_hint, map_len, USER_STACK_TOP)
                    .ok_or(SysErrNo::ENOMEM)?
            }
        } else {
            ms.find_free_area(next_hint, map_len, USER_STACK_TOP)
                .ok_or(SysErrNo::ENOMEM)?
        }
    };
    let end = start.checked_add(map_len).ok_or(SysErrNo::EINVAL)?;
    if start < PAGE_SIZE {
        return Err(SysErrNo::EPERM);
    }
    if end > USER_STACK_TOP {
        return Err(SysErrNo::ENOMEM);
    }

    let map_has_leaf = pte_has_leaf_permission(pte_flags);
    let (backing, file_data) = if anonymous {
        (MapAreaBacking::Anonymous, None)
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

        let data = if map_has_leaf {
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
        )
    };

    if map_fixed && !no_replace {
        let writes = collect_shared_file_writes(&task, start, end)?;
        write_back_shared_files(writes)?;
    }

    {
        let mut ms = task.memory_set.lock();
        if no_replace && ms.range_overlaps(start, end) {
            return Err(SysErrNo::EEXIST);
        }
        if map_fixed && !no_replace {
            ms.unmap_range(VirtAddr::new(start), VirtAddr::new(end))?;
        } else if !fixed_addr && ms.range_overlaps(start, end) {
            return Err(SysErrNo::ENOMEM);
        }

        ms.insert_framed_area_with_backing(
            VirtAddr::new(start),
            VirtAddr::new(end),
            pte_flags,
            backing,
        )?;
        if let Some(data) = file_data {
            ms.write_bytes(start, &data)?;
        }
        ms.activate();
    }

    let mut mm = task.mm.lock();
    if mm.next_mmap < end {
        mm.next_mmap = end;
    }
    Ok(start)
}

/// mprotect system call.
pub fn sys_mprotect(addr: usize, len: usize, prot: i32) -> SyscallRet {
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
        let mut ms = task.memory_set.lock();
        ms.protect_range(VirtAddr::new(start), VirtAddr::new(end), pte_flags)?;
        ms.activate();
    }
    Ok(0)
}

/// munmap system call.
pub fn sys_munmap(addr: usize, length: usize) -> SyscallRet {
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
        let mut ms = task.memory_set.lock();
        ms.unmap_range(VirtAddr::new(start), VirtAddr::new(end))?;
        ms.activate();
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
        let ms = task.memory_set.lock();
        if !ms.range_covered(start, end) {
            return Err(SysErrNo::ENOMEM);
        }
    }
    let writes = collect_shared_file_writes(&task, start, end)?;
    write_back_shared_files(writes)?;
    Ok(0)
}
