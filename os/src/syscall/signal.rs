use super::SyscallRet;
use crate::utils::error::SysErrNo;

const MAX_SIGNAL: i32 = 64;
const KERNEL_SIGACTION_SIZE: usize = 32;
const MAX_SIGSET_SIZE: usize = 128;

fn valid_signal(signum: i32) -> bool {
    signum > 0 && signum <= MAX_SIGNAL
}

/// Minimal rt_sigaction-compatible stub.
///
/// Userland such as BusyBox installs handlers during startup. The kernel does
/// not deliver signals yet, but returning a default old action keeps libc from
/// falling back into error paths.
pub fn sys_sigaction(signum: i32, _act: usize, oldact: usize) -> SyscallRet {
    if !valid_signal(signum) {
        return Err(SysErrNo::EINVAL);
    }
    if oldact != 0 {
        let empty = [0u8; KERNEL_SIGACTION_SIZE];
        super::user::copy_to_user(oldact, &empty)?;
    }
    Ok(0)
}

/// Minimal rt_sigprocmask-compatible stub.
pub fn sys_sigprocmask(_how: i32, _set: usize, oldset: usize, sigset_size: usize) -> SyscallRet {
    if oldset != 0 {
        let empty = [0u8; MAX_SIGSET_SIZE];
        let len = if sigset_size == 0 {
            core::mem::size_of::<usize>()
        } else {
            core::cmp::min(sigset_size, MAX_SIGSET_SIZE)
        };
        super::user::copy_to_user(oldset, &empty[..len])?;
    }
    Ok(0)
}

/// Signal delivery is not implemented yet. Accept benign kill/tkill/tgkill
/// requests so shell utilities can continue; invalid signal numbers still fail.
pub fn sys_kill(_pid: i32, sig: i32) -> SyscallRet {
    if sig != 0 && !valid_signal(sig) {
        return Err(SysErrNo::EINVAL);
    }
    Ok(0)
}
