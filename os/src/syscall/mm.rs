use super::SyscallRet;
use crate::config::{PAGE_SIZE, USER_HEAP_START, USER_STACK_TOP};
use crate::mm::map_area::MapAreaBacking;
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
const MAP_FIXED_NOREPLACE: usize = 0x100000;

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

/// brk system call.
pub fn sys_brk(new_brk: usize) -> SyscallRet {
    let task = current_task().ok_or(SysErrNo::ESRCH)?;
    let mut inner = task.inner.lock();

    if new_brk == 0 {
        return Ok(inner.program_break);
    }
    if new_brk < USER_HEAP_START {
        return Err(SysErrNo::EINVAL);
    }

    let new_mapped_end = align_up(new_brk)?;
    if new_mapped_end > inner.mapped_break {
        let mapped_break = inner.mapped_break;
        task.memory_set.lock().insert_framed_area(
            VirtAddr::new(mapped_break),
            VirtAddr::new(new_mapped_end),
            PTEFlags::U | PTEFlags::R | PTEFlags::W | PTEFlags::V,
        )?;
        inner.mapped_break = new_mapped_end;
    }
    drop(inner);

    task.memory_set.lock().activate();

    let mut inner = task.inner.lock();
    inner.program_break = new_brk;
    Ok(new_brk)
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
    let shared = mmap_type(flags)?;
    let fixed = (flags & (MAP_FIXED | MAP_FIXED_NOREPLACE)) != 0;
    let anonymous = (flags & MAP_ANONYMOUS) != 0;
    let pte_flags = prot_to_pte_flags(prot)?;
    let map_len = align_up(length)?;

    if !anonymous && offset % PAGE_SIZE != 0 {
        return Err(SysErrNo::EINVAL);
    }
    if !anonymous && fd < 0 {
        return Err(SysErrNo::EBADF);
    }
    if fixed && addr % PAGE_SIZE != 0 {
        return Err(SysErrNo::EINVAL);
    }

    let next_hint = task.inner.lock().next_mmap;
    let start = {
        let ms = task.memory_set.lock();
        if fixed {
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
    if end > USER_STACK_TOP {
        return Err(SysErrNo::ENOMEM);
    }

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

        let mut data = Vec::new();
        data.resize(length, 0);
        let n = super::with_kernel_page_table(|| file_desc.read_at(offset, &mut data))?;
        data.truncate(n);
        (
            MapAreaBacking::File {
                file: file_desc.clone(),
                offset,
                shared,
            },
            Some(data),
        )
    };

    {
        let mut ms = task.memory_set.lock();
        if (flags & MAP_FIXED_NOREPLACE) != 0 && ms.range_overlaps(start, end) {
            return Err(SysErrNo::EEXIST);
        }
        if fixed {
            ms.unmap_range(VirtAddr::new(start), VirtAddr::new(end))?;
        } else if ms.range_overlaps(start, end) {
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

    let mut inner = task.inner.lock();
    if inner.next_mmap < end {
        inner.next_mmap = end;
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
    {
        let mut ms = task.memory_set.lock();
        ms.unmap_range(VirtAddr::new(start), VirtAddr::new(end))?;
        ms.activate();
    }
    Ok(0)
}
