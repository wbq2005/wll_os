use crate::utils::error::SysErrNo;
use super::SyscallRet;
use crate::config::{PAGE_SIZE, USER_HEAP_START};
use crate::mm::page_table::PTEFlags;
use crate::task::current_task;
use polyhal::VirtAddr;

fn align_up(value: usize) -> usize {
    value.div_ceil(PAGE_SIZE) * PAGE_SIZE
}

/// brk 系统调用
pub fn sys_brk(new_brk: usize) -> SyscallRet {
    let task = current_task().ok_or(SysErrNo::ESRCH)?;
    let mut inner = task.inner.lock();

    if new_brk == 0 {
        return Ok(inner.program_break);
    }
    if new_brk < USER_HEAP_START {
        return Err(SysErrNo::EINVAL);
    }

    let new_mapped_end = align_up(new_brk);
    if new_mapped_end > inner.mapped_break {
        let mapped_break = inner.mapped_break;
        inner.memory_set.lock().insert_framed_area(
            VirtAddr::new(mapped_break),
            VirtAddr::new(new_mapped_end),
            PTEFlags::U | PTEFlags::R | PTEFlags::W | PTEFlags::V,
        );
        inner.mapped_break = new_mapped_end;
        inner.memory_set.lock().activate();
    }

    inner.program_break = new_brk;
    Ok(new_brk)
}

/// mmap 系统调用
pub fn sys_mmap(
    addr: usize,
    length: usize,
    prot: i32,
    _flags: i32,
    fd: i32,
    offset: usize,
) -> SyscallRet {
    let task = current_task().ok_or(SysErrNo::ESRCH)?;
    let mut inner = task.inner.lock();

    if length == 0 {
        return Err(SysErrNo::EINVAL);
    }
    if fd >= 0 {
        return Err(SysErrNo::ENOSYS);
    }
    if offset != 0 {
        return Err(SysErrNo::EINVAL);
    }

    let start = if addr == 0 {
        let next = inner.next_mmap;
        inner.next_mmap = align_up(next + length);
        next
    } else {
        addr / PAGE_SIZE * PAGE_SIZE
    };
    let end = align_up(start + length);

    let mut flags = PTEFlags::U | PTEFlags::V;
    if prot & 0x1 != 0 {
        flags |= PTEFlags::R; // PROT_READ
    }
    if prot & 0x2 != 0 {
        flags |= PTEFlags::W; // PROT_WRITE
    }
    if prot & 0x4 != 0 {
        flags |= PTEFlags::X; // PROT_EXEC
    }
    if !flags.contains(PTEFlags::R) && !flags.contains(PTEFlags::W) && !flags.contains(PTEFlags::X) {
        flags |= PTEFlags::R | PTEFlags::W;
    }

    inner.memory_set.lock().insert_framed_area(
        VirtAddr::new(start),
        VirtAddr::new(end),
        flags,
    );
    inner.memory_set.lock().activate();
    Ok(start)
}

/// mprotect 系统调用
pub fn sys_mprotect(addr: usize, len: usize, _prot: i32) -> SyscallRet {
    if addr % PAGE_SIZE != 0 {
        return Err(SysErrNo::EINVAL);
    }
    if len == 0 {
        return Err(SysErrNo::EINVAL);
    }
    // 简化实现：暂不修改页表权限，返回成功
    Ok(0)
}

/// munmap 系统调用
pub fn sys_munmap(addr: usize, length: usize) -> SyscallRet {
    let task = current_task().ok_or(SysErrNo::ESRCH)?;
    let mut inner = task.inner.lock();
    if length == 0 {
        return Err(SysErrNo::EINVAL);
    }

    let start = addr / PAGE_SIZE * PAGE_SIZE;
    let end = align_up(addr + length);
    let mut current = start;
    while current < end {
        inner
            .memory_set
            .lock()
            .page_table
            .unmap_page(VirtAddr::new(current));
        current += PAGE_SIZE;
    }
    inner.memory_set.lock().remove_area(VirtAddr::new(start));
    inner.memory_set.lock().activate();
    Ok(0)
}
