use crate::utils::error::SysErrNo;
use super::SyscallRet;

/// sigaction 系统调用
pub fn sys_sigaction(signum: i32, act: usize, oldact: usize) -> SyscallRet {
    // 稍后实现
    Err(SysErrNo::ENOSYS)
}

/// sigprocmask 系统调用
pub fn sys_sigprocmask(how: i32, set: usize, oldset: usize) -> SyscallRet {
    // 稍后实现
    Err(SysErrNo::ENOSYS)
}

/// kill 系统调用
pub fn sys_kill(pid: i32, sig: i32) -> SyscallRet {
    // 稍后实现
    Err(SysErrNo::ENOSYS)
}
