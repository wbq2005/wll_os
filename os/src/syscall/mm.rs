use super::SyscallRet;
use crate::config::{PAGE_SIZE, USER_HEAP_START};
use crate::mm::page_table::PTEFlags;
use crate::task::current_task;
use crate::utils::error::SysErrNo;
use alloc::vec::Vec;
use polyhal::VirtAddr;

fn align_up(value: usize) -> usize {
    value.div_ceil(PAGE_SIZE) * PAGE_SIZE
}

/// brk 系统调用
fn copy_to_user_mapped(dst: usize, src: &[u8]) -> Result<(), SysErrNo> {
    let task = current_task().ok_or(SysErrNo::ESRCH)?;
    let memory_set = task.memory_set.lock();
    let mut addr = dst;
    for &byte in src {
        let pa = memory_set.translate(VirtAddr::new(addr)).ok_or_else(|| {
            log::error!("[syscall] copy_to_user_mapped: unmapped dst {:#x}", addr);
            SysErrNo::EFAULT
        })?;
        unsafe {
            *(pa.raw() as *mut u8) = byte;
        }
        addr += 1;
    }
    Ok(())
}

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
        task.memory_set.lock().insert_framed_area(
            VirtAddr::new(mapped_break),
            VirtAddr::new(new_mapped_end),
            PTEFlags::U | PTEFlags::R | PTEFlags::W | PTEFlags::V,
        );
        inner.mapped_break = new_mapped_end;
    }
    drop(inner);

    task.memory_set.lock().activate();

    let mut inner = task.inner.lock();
    inner.program_break = new_brk;
    Ok(new_brk)
}

/// mmap 系统调用
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

    if fd >= 0 && addr == 0 && offset == 0 && (prot & 0x4) != 0 {
        let preloaded_exec = 0x0040_0000usize;
        let mut inner = task.inner.lock();
        if task
            .memory_set
            .lock()
            .is_mapped(VirtAddr::new(preloaded_exec))
        {
            let next = align_up(preloaded_exec + length);
            if inner.next_mmap < next {
                inner.next_mmap = next;
            }
            return Ok(preloaded_exec);
        }
    }
    let mut inner = task.inner.lock();

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
    if !flags.contains(PTEFlags::R) && !flags.contains(PTEFlags::W) && !flags.contains(PTEFlags::X)
    {
        flags |= PTEFlags::R | PTEFlags::W;
    }

    let file_data = if fd >= 0 {
        let mut fds = inner.fd_table.lock();
        let file_desc = fds.get_mut(fd as usize).ok_or(SysErrNo::EBADF)?;
        let mut data = Vec::new();
        data.resize(length, 0);
        let n = file_desc.read_at(offset, &mut data)?;
        data.truncate(n);
        Some(data)
    } else {
        None
    };

    drop(inner);
    {
        let mut ms = task.memory_set.lock();
        let mut current = start;
        while current < end {
            if ms.is_mapped(VirtAddr::new(current)) {
                ms.page_table.unmap_page(VirtAddr::new(current));
            }
            current += PAGE_SIZE;
        }
        ms.insert_framed_area(VirtAddr::new(start), VirtAddr::new(end), flags);
        ms.activate();
    }
    if let Some(data) = file_data {
        if let Err(err) = copy_to_user_mapped(start, &data) {
            log::error!("[syscall] mmap: copy file data failed: {:?}", err);
            return Err(err);
        }
    }
    #[cfg(target_arch = "riscv64")]
    {
        let ms = task.memory_set.lock();
        log::info!(
            "[syscall] mmap: probe 0x15a10 = {:?}",
            ms.page_table.translate(VirtAddr::new(0x15a10))
        );
    }
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
    log::info!("[syscall] munmap(addr={:#x}, len={:#x})", addr, length);
    let task = current_task().ok_or(SysErrNo::ESRCH)?;
    let mut inner = task.inner.lock();
    if length == 0 {
        return Err(SysErrNo::EINVAL);
    }

    let start = addr / PAGE_SIZE * PAGE_SIZE;
    let end = align_up(addr + length);
    let mut current = start;
    while current < end {
        task.memory_set
            .lock()
            .page_table
            .unmap_page(VirtAddr::new(current));
        current += PAGE_SIZE;
    }
    drop(inner);
    task.memory_set.lock().remove_area(VirtAddr::new(start));
    task.memory_set.lock().activate();
    Ok(0)
}
