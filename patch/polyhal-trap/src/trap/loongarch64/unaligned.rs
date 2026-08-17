use crate::trapframe::TrapFrame;
use loongArch64::register::{badi, badv};
use polyhal::{pagetable::{MappingFlags, PageTable}, VirtAddr};

pub const LDH_OP: u32 = 0xa1;
pub const LDHU_OP: u32 = 0xa9;
pub const LDW_OP: u32 = 0xa2;
pub const LDWU_OP: u32 = 0xaa;
pub const LDD_OP: u32 = 0xa3;
pub const STH_OP: u32 = 0xa5;
pub const STW_OP: u32 = 0xa6;
pub const STD_OP: u32 = 0xa7;

pub const LDPTRW_OP: u32 = 0x24;
pub const LDPTRD_OP: u32 = 0x26;
pub const STPTRW_OP: u32 = 0x25;
pub const STPTRD_OP: u32 = 0x27;

pub const LDXH_OP: u32 = 0x7048;
pub const LDXHU_OP: u32 = 0x7008;
pub const LDXW_OP: u32 = 0x7010;
pub const LDXWU_OP: u32 = 0x7050;
pub const LDXD_OP: u32 = 0x7018;
pub const STXH_OP: u32 = 0x7028;
pub const STXW_OP: u32 = 0x7030;
pub const STXD_OP: u32 = 0x7038;

pub const FLDS_OP: u32 = 0xac;
pub const FLDD_OP: u32 = 0xae;
pub const FSTS_OP: u32 = 0xad;
pub const FSTD_OP: u32 = 0xaf;

pub const FSTXS_OP: u32 = 0x7070;
pub const FSTXD_OP: u32 = 0x7078;
pub const FLDXS_OP: u32 = 0x7060;
pub const FLDXD_OP: u32 = 0x7068;

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum UnalignedError {
    UnsupportedInstruction(u32),
    InvalidUserMemory,
}

#[derive(Clone, Copy)]
enum UserAccess {
    Read,
    Write,
    Execute,
}

#[inline]
fn access_allowed(flags: MappingFlags, access: UserAccess) -> bool {
    if !flags.contains(MappingFlags::P | MappingFlags::U) {
        return false;
    }
    match access {
        UserAccess::Read | UserAccess::Execute => true,
        UserAccess::Write => flags.contains(MappingFlags::W),
    }
}

#[inline]
fn read_user_byte(addr: usize, access: UserAccess) -> Result<u8, UnalignedError> {
    let (paddr, flags) = PageTable::current()
        .translate(VirtAddr::new(addr))
        .ok_or(UnalignedError::InvalidUserMemory)?;
    if !access_allowed(flags, access) {
        return Err(UnalignedError::InvalidUserMemory);
    }
    // LoongArch's DMW1 alias is the only kernel-safe way to access a physical
    // frame after the trap has switched privilege, and the access is byte-sized.
    Ok(unsafe { paddr.get_ptr::<u8>().read_volatile() })
}

#[inline]
fn write_user_byte(addr: usize, value: u8) -> Result<(), UnalignedError> {
    let (paddr, flags) = PageTable::current()
        .translate(VirtAddr::new(addr))
        .ok_or(UnalignedError::InvalidUserMemory)?;
    if !access_allowed(flags, UserAccess::Write) {
        return Err(UnalignedError::InvalidUserMemory);
    }
    unsafe { paddr.get_mut_ptr::<u8>().write_volatile(value) };
    Ok(())
}

fn read_user_value(addr: usize, size: usize) -> Result<u64, UnalignedError> {
    let mut value = 0u64;
    for offset in 0..size {
        let byte_addr = addr
            .checked_add(offset)
            .ok_or(UnalignedError::InvalidUserMemory)?;
        value |= (read_user_byte(byte_addr, UserAccess::Read)? as u64) << (offset * 8);
    }
    Ok(value)
}

fn write_user_value(addr: usize, value: u64, size: usize) -> Result<(), UnalignedError> {
    for offset in 0..size {
        let byte_addr = addr
            .checked_add(offset)
            .ok_or(UnalignedError::InvalidUserMemory)?;
        write_user_byte(byte_addr, (value >> (offset * 8)) as u8)?;
    }
    Ok(())
}

#[inline]
fn sign_extend(value: u64, bits: usize) -> usize {
    let shift = 64 - bits;
    ((value << shift) as i64 >> shift) as usize
}

fn faulting_user_instruction(tf: &TrapFrame) -> Result<u32, UnalignedError> {
    // BADI is architecturally latched for address/alignment exceptions.  The
    // fallback is needed on older QEMU versions that leave it zero.
    let instruction = badi::read().inst();
    if instruction != 0 {
        return Ok(instruction);
    }
    let mut bytes = [0u8; 4];
    for (offset, byte) in bytes.iter_mut().enumerate() {
        *byte = read_user_byte(
            tf.era
                .checked_add(offset)
                .ok_or(UnalignedError::InvalidUserMemory)?,
            UserAccess::Execute,
        )?;
    }
    Ok(u32::from_le_bytes(bytes))
}

/// Emulate a LoongArch user-mode scalar access that trapped for alignment.
///
/// The caller invokes this while the faulting task's page table is still
/// active. Every byte is translated independently, so an access crossing a
/// page boundary is safe and never dereferences a user VA from the kernel
/// root. On success the trap frame is advanced exactly once.
pub fn emulate_load_store_insn(tf: &mut TrapFrame) -> Result<(), UnalignedError> {
    let instruction = faulting_user_instruction(tf)?;
    let rd = (instruction & 0x1f) as usize;
    let op22 = instruction >> 22;
    let op24 = instruction >> 24;
    let op15 = instruction >> 15;
    let addr = badv::read().vaddr();

    let integer_load = if op22 == LDD_OP || op24 == LDPTRD_OP || op15 == LDXD_OP {
        Some((8, false))
    } else if op22 == LDW_OP || op24 == LDPTRW_OP || op15 == LDXW_OP {
        Some((4, true))
    } else if op22 == LDWU_OP || op15 == LDXWU_OP {
        Some((4, false))
    } else if op22 == LDH_OP || op15 == LDXH_OP {
        Some((2, true))
    } else if op22 == LDHU_OP || op15 == LDXHU_OP {
        Some((2, false))
    } else {
        None
    };

    if let Some((size, signed)) = integer_load {
        let value = read_user_value(addr, size)?;
        tf.regs[rd] = if signed {
            sign_extend(value, size * 8)
        } else {
            value as usize
        };
        if rd == 0 {
            tf.regs[rd] = 0;
        }
        tf.era = tf
            .era
            .checked_add(4)
            .ok_or(UnalignedError::InvalidUserMemory)?;
        return Ok(());
    }

    let integer_store = if op22 == STD_OP || op24 == STPTRD_OP || op15 == STXD_OP {
        Some(8)
    } else if op22 == STW_OP || op24 == STPTRW_OP || op15 == STXW_OP {
        Some(4)
    } else if op22 == STH_OP || op15 == STXH_OP {
        Some(2)
    } else {
        None
    };

    if let Some(size) = integer_store {
        write_user_value(addr, tf.regs[rd] as u64, size)?;
        tf.era = tf
            .era
            .checked_add(4)
            .ok_or(UnalignedError::InvalidUserMemory)?;
        return Ok(());
    }

    let fp_load = if op22 == FLDD_OP || op15 == FLDXD_OP {
        Some(8)
    } else if op22 == FLDS_OP || op15 == FLDXS_OP {
        Some(4)
    } else {
        None
    };

    if let Some(size) = fp_load {
        tf.f[rd] = read_user_value(addr, size)?;
        tf.lsx[rd][0] = tf.f[rd];
        tf.era = tf
            .era
            .checked_add(4)
            .ok_or(UnalignedError::InvalidUserMemory)?;
        return Ok(());
    }

    let fp_store = if op22 == FSTD_OP || op15 == FSTXD_OP {
        Some(8)
    } else if op22 == FSTS_OP || op15 == FSTXS_OP {
        Some(4)
    } else {
        None
    };

    if let Some(size) = fp_store {
        write_user_value(addr, tf.f[rd], size)?;
        tf.era = tf
            .era
            .checked_add(4)
            .ok_or(UnalignedError::InvalidUserMemory)?;
        return Ok(());
    }

    Err(UnalignedError::UnsupportedInstruction(instruction))
}
