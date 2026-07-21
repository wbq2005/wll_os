use alloc::string::String;
use alloc::vec::Vec;
use core::mem::{self, MaybeUninit};

use polyhal::VirtAddr;

use crate::config::PAGE_SIZE;
use crate::mm::memory_set::MemorySet;
use crate::task::current_task;
use crate::utils::error::SysErrNo;

pub const MAX_CSTR_LEN: usize = 4096;

pub fn read_usize(addr: usize) -> Result<usize, SysErrNo> {
    let mut bytes = [0u8; mem::size_of::<usize>()];
    copy_from_user(addr, &mut bytes)?;
    Ok(usize::from_le_bytes(bytes))
}

pub fn write_i32(addr: usize, value: i32) -> Result<(), SysErrNo> {
    copy_to_user(addr, &value.to_le_bytes())
}

pub fn copy_to_user(dst: usize, src: &[u8]) -> Result<(), SysErrNo> {
    if dst == 0 && !src.is_empty() {
        return Err(SysErrNo::EFAULT);
    }
    let task = current_task().ok_or(SysErrNo::ESRCH)?;
    super::with_kernel_page_table(|| {
        let mut memory_set = task.memory_set.lock();
        copy_to_user_in_memory_set(&mut memory_set, dst, src)
    })
}

pub fn copy_to_user_in_memory_set(
    memory_set: &mut MemorySet,
    dst: usize,
    src: &[u8],
) -> Result<(), SysErrNo> {
    if dst == 0 && !src.is_empty() {
        return Err(SysErrNo::EFAULT);
    }
    memory_set.prepare_write(dst, src.len())?;
    let mut copied = 0usize;
    while copied < src.len() {
        let va = dst.checked_add(copied).ok_or(SysErrNo::EFAULT)?;
        let pa = memory_set
            .translate(VirtAddr::new(va))
            .ok_or(SysErrNo::EFAULT)?;
        let page_left = PAGE_SIZE - va % PAGE_SIZE;
        let n = page_left.min(src.len() - copied);
        unsafe {
            core::ptr::copy_nonoverlapping(src[copied..].as_ptr(), pa.raw() as *mut u8, n);
        }
        copied += n;
    }
    Ok(())
}

pub fn copy_from_user_in_memory_set(
    memory_set: &mut MemorySet,
    src: usize,
    dst: &mut [u8],
) -> Result<(), SysErrNo> {
    if src == 0 && !dst.is_empty() {
        return Err(SysErrNo::EFAULT);
    }
    memory_set.prepare_read(src, dst.len())?;
    let mut copied = 0usize;
    while copied < dst.len() {
        let va = src.checked_add(copied).ok_or(SysErrNo::EFAULT)?;
        let pa = memory_set
            .translate(VirtAddr::new(va))
            .ok_or(SysErrNo::EFAULT)?;
        let page_left = PAGE_SIZE - va % PAGE_SIZE;
        let n = page_left.min(dst.len() - copied);
        unsafe {
            core::ptr::copy_nonoverlapping(pa.raw() as *const u8, dst[copied..].as_mut_ptr(), n);
        }
        copied += n;
    }
    Ok(())
}

pub fn copy_from_user(src: usize, dst: &mut [u8]) -> Result<(), SysErrNo> {
    if src == 0 && !dst.is_empty() {
        return Err(SysErrNo::EFAULT);
    }
    let task = current_task().ok_or(SysErrNo::ESRCH)?;
    super::with_kernel_page_table(|| {
        let mut memory_set = task.memory_set.lock();
        copy_from_user_in_memory_set(&mut memory_set, src, dst)
    })
}

pub fn check_user_readable(src: usize, len: usize) -> Result<(), SysErrNo> {
    if src == 0 && len != 0 {
        return Err(SysErrNo::EFAULT);
    }
    let task = current_task().ok_or(SysErrNo::ESRCH)?;
    let mut memory_set = task.memory_set.lock();
    memory_set.prepare_read(src, len)
}

pub fn clear_user(dst: usize, len: usize) -> Result<(), SysErrNo> {
    if dst == 0 && len != 0 {
        return Err(SysErrNo::EFAULT);
    }
    let task = current_task().ok_or(SysErrNo::ESRCH)?;
    super::with_kernel_page_table(|| {
        let mut memory_set = task.memory_set.lock();
        memory_set.prepare_write(dst, len)?;
        let mut copied = 0usize;
        while copied < len {
            let va = dst.checked_add(copied).ok_or(SysErrNo::EFAULT)?;
            let pa = memory_set
                .translate(VirtAddr::new(va))
                .ok_or(SysErrNo::EFAULT)?;
            let page_left = PAGE_SIZE - va % PAGE_SIZE;
            let n = page_left.min(len - copied);
            unsafe {
                core::ptr::write_bytes(pa.raw() as *mut u8, 0, n);
            }
            copied += n;
        }
        Ok(())
    })
}

pub fn copy_object_to_user<T>(dst: usize, obj: &T) -> Result<(), SysErrNo> {
    let bytes =
        unsafe { core::slice::from_raw_parts(obj as *const T as *const u8, mem::size_of::<T>()) };
    copy_to_user(dst, bytes)
}

pub fn copy_object_from_user<T: Copy>(src: usize) -> Result<T, SysErrNo> {
    let mut obj = MaybeUninit::<T>::uninit();
    let bytes = unsafe {
        core::slice::from_raw_parts_mut(obj.as_mut_ptr() as *mut u8, mem::size_of::<T>())
    };
    copy_from_user(src, bytes)?;
    Ok(unsafe { obj.assume_init() })
}

pub fn read_cstr(addr: usize) -> Result<String, SysErrNo> {
    read_cstr_inner(addr, false, SysErrNo::EFAULT)
}

pub fn read_path_cstr(addr: usize) -> Result<String, SysErrNo> {
    if addr == 0 {
        return Err(SysErrNo::EFAULT);
    }
    read_cstr_inner(addr, false, SysErrNo::ENAMETOOLONG)
}

pub fn read_cstr_null_empty(addr: usize) -> Result<String, SysErrNo> {
    read_cstr_inner(addr, true, SysErrNo::EINVAL)
}

fn read_cstr_inner(
    addr: usize,
    null_as_empty: bool,
    unterminated_err: SysErrNo,
) -> Result<String, SysErrNo> {
    if addr == 0 {
        return if null_as_empty {
            Ok(String::new())
        } else {
            Err(SysErrNo::EFAULT)
        };
    }

    let task = current_task().ok_or(SysErrNo::ESRCH)?;
    super::with_kernel_page_table(|| {
        let mut bytes = Vec::new();
        let mut memory_set = task.memory_set.lock();
        let mut offset = 0usize;
        while offset < MAX_CSTR_LEN {
            let va = addr.checked_add(offset).ok_or(SysErrNo::EFAULT)?;
            let page_left = PAGE_SIZE - va % PAGE_SIZE;
            let span = page_left.min(MAX_CSTR_LEN - offset);
            memory_set.prepare_read(va, span)?;
            let pa = memory_set
                .translate(VirtAddr::new(va))
                .ok_or(SysErrNo::EFAULT)?;
            let src = unsafe { core::slice::from_raw_parts(pa.raw() as *const u8, span) };
            for &byte in src {
                if byte == 0 {
                    return String::from_utf8(bytes).map_err(|_| SysErrNo::EINVAL);
                }
                bytes.push(byte);
            }
            offset += span;
        }
        Err(unterminated_err)
    })
}
