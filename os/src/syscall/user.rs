use alloc::string::String;
use alloc::vec::Vec;
use core::mem::{self, MaybeUninit};

use polyhal::VirtAddr;

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
    let memory_set = task.memory_set.lock();
    for (i, &byte) in src.iter().enumerate() {
        let pa = memory_set
            .translate(VirtAddr::new(dst + i))
            .ok_or(SysErrNo::EFAULT)?;
        unsafe {
            *(pa.raw() as *mut u8) = byte;
        }
    }
    Ok(())
}

pub fn copy_from_user(src: usize, dst: &mut [u8]) -> Result<(), SysErrNo> {
    if src == 0 && !dst.is_empty() {
        return Err(SysErrNo::EFAULT);
    }
    let task = current_task().ok_or(SysErrNo::ESRCH)?;
    let memory_set = task.memory_set.lock();
    for (i, byte) in dst.iter_mut().enumerate() {
        let pa = memory_set
            .translate(VirtAddr::new(src + i))
            .ok_or(SysErrNo::EFAULT)?;
        *byte = unsafe { *(pa.raw() as *const u8) };
    }
    Ok(())
}

pub fn copy_object_to_user<T>(dst: usize, obj: &T) -> Result<(), SysErrNo> {
    let bytes =
        unsafe { core::slice::from_raw_parts(obj as *const T as *const u8, mem::size_of::<T>()) };
    copy_to_user(dst, bytes)
}

pub fn copy_object_from_user<T: Copy>(src: usize) -> Result<T, SysErrNo> {
    let mut obj = MaybeUninit::<T>::uninit();
    let bytes =
        unsafe { core::slice::from_raw_parts_mut(obj.as_mut_ptr() as *mut u8, mem::size_of::<T>()) };
    copy_from_user(src, bytes)?;
    Ok(unsafe { obj.assume_init() })
}

pub fn read_cstr(addr: usize) -> Result<String, SysErrNo> {
    read_cstr_inner(addr, false, SysErrNo::EFAULT)
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

    let mut bytes = Vec::new();
    let task = current_task().ok_or(SysErrNo::ESRCH)?;
    let memory_set = task.memory_set.lock();
    for i in 0..MAX_CSTR_LEN {
        let pa = memory_set
            .translate(VirtAddr::new(addr + i))
            .ok_or(SysErrNo::EFAULT)?;
        let byte = unsafe { *(pa.raw() as *const u8) };
        if byte == 0 {
            return String::from_utf8(bytes).map_err(|_| SysErrNo::EINVAL);
        }
        bytes.push(byte);
    }
    Err(unterminated_err)
}
