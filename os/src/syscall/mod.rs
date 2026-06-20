pub mod fs;
pub mod mm;
pub mod net;
pub mod other;
pub mod process;
pub mod signal;
pub(crate) mod user;

pub use crate::utils::error::SysErrNo;

/// Linux AT_FDCWD = -100, used to indicate "use current working directory" for *at syscalls.
const AT_FDCWD: isize = -100;

/// 系统调用返回值类型
pub type SyscallRet = Result<usize, SysErrNo>;

pub(crate) fn with_kernel_page_table<T>(f: impl FnOnce() -> T) -> T {
    crate::trap::restore_kernel_page_table();
    let result = f();
    if let Some(task) = crate::task::current_task() {
        if !task.is_kernel {
            task.memory_set.lock().activate();
        }
    }
    result
}

/// 系统调用号定义
pub const SYSCALL_GETCWD: usize = 17;
pub const SYSCALL_DUP: usize = 23;
pub const SYSCALL_DUP3: usize = 24;
pub const SYSCALL_FCNTL: usize = 25;
pub const SYSCALL_IOCTL: usize = 29;
pub const SYSCALL_MKDIRAT: usize = 34;
pub const SYSCALL_UNLINKAT: usize = 35;
pub const SYSCALL_SYMLINKAT: usize = 36;
pub const SYSCALL_LINKAT: usize = 37;
pub const SYSCALL_RENAMEAT: usize = 38;
pub const SYSCALL_UMOUNT2: usize = 39;
pub const SYSCALL_MOUNT: usize = 40;
pub const SYSCALL_STATFS: usize = 43;
pub const SYSCALL_FSTATFS: usize = 44;
pub const SYSCALL_FACCESSAT: usize = 48;
pub const SYSCALL_CHDIR: usize = 49;
pub const SYSCALL_FCHMOD: usize = 52;
pub const SYSCALL_FCHMODAT: usize = 53;
pub const SYSCALL_FCHOWNAT: usize = 54;
pub const SYSCALL_FCHOWN: usize = 55;
pub const SYSCALL_OPENAT: usize = 56;
pub const SYSCALL_CLOSE: usize = 57;
pub const SYSCALL_PIPE2: usize = 59;
pub const SYSCALL_GETDENTS64: usize = 61;
pub const SYSCALL_READ: usize = 63;
pub const SYSCALL_WRITE: usize = 64;
pub const SYSCALL_READV: usize = 65;
pub const SYSCALL_WRITEV: usize = 66;
pub const SYSCALL_PREAD64: usize = 67;
pub const SYSCALL_PWRITE64: usize = 68;
pub const SYSCALL_PREADV: usize = 69;
pub const SYSCALL_PWRITEV: usize = 70;
pub const SYSCALL_LSEEK: usize = 62;
pub const SYSCALL_SENDFILE: usize = 71;
pub const SYSCALL_TRUNCATE: usize = 45;
pub const SYSCALL_FTRUNCATE: usize = 46;
pub const SYSCALL_PSELECT6: usize = 72;
pub const SYSCALL_PPOLL: usize = 73;
pub const SYSCALL_READLINKAT: usize = 78;
pub const SYSCALL_NEWFSTATAT: usize = 79;
pub const SYSCALL_FSTAT: usize = 80;
pub const SYSCALL_SYNC: usize = 81;
pub const SYSCALL_FSYNC: usize = 82;
pub const SYSCALL_FDATASYNC: usize = 83;
pub const SYSCALL_UTIMENSAT: usize = 88;
pub const SYSCALL_EXIT: usize = 93;
pub const SYSCALL_EXIT_GROUP: usize = 94;
pub const SYSCALL_SET_TID_ADDRESS: usize = 96;
pub const SYSCALL_FUTEX: usize = 98;
pub const SYSCALL_SET_ROBUST_LIST: usize = 99;
pub const SYSCALL_GET_ROBUST_LIST: usize = 100;
pub const SYSCALL_NANOSLEEP: usize = 101;
pub const SYSCALL_SETITIMER: usize = 103;
pub const SYSCALL_CLOCK_SETTIME: usize = 112;
pub const SYSCALL_CLOCK_GETTIME: usize = 113;
pub const SYSCALL_CLOCK_GETRES: usize = 114;
pub const SYSCALL_CLOCK_NANOSLEEP: usize = 115;
pub const SYSCALL_SYSLOG: usize = 116;
pub const SYSCALL_SCHED_SETPARAM: usize = 118;
pub const SYSCALL_SCHED_SETSCHEDULER: usize = 119;
pub const SYSCALL_SCHED_GETSCHEDULER: usize = 120;
pub const SYSCALL_SCHED_GETPARAM: usize = 121;
pub const SYSCALL_SCHED_SETAFFINITY: usize = 122;
pub const SYSCALL_SCHED_GETAFFINITY: usize = 123;
pub const SYSCALL_SCHED_YIELD: usize = 124;
pub const SYSCALL_SCHED_GET_PRIORITY_MAX: usize = 125;
pub const SYSCALL_SCHED_GET_PRIORITY_MIN: usize = 126;
pub const SYSCALL_SCHED_RR_GET_INTERVAL: usize = 127;
pub const SYSCALL_KILL: usize = 129;
pub const SYSCALL_TKILL: usize = 130;
pub const SYSCALL_TGKILL: usize = 131;
pub const SYSCALL_SIGSUSPEND: usize = 133;
pub const SYSCALL_SIGACTION: usize = 134;
pub const SYSCALL_SIGPROCMASK: usize = 135;
pub const SYSCALL_SIGTIMEDWAIT: usize = 137;
pub const SYSCALL_SIGRETURN: usize = 139;
pub const SYSCALL_SETREGID: usize = 143;
pub const SYSCALL_SETGID: usize = 144;
pub const SYSCALL_SETREUID: usize = 145;
pub const SYSCALL_SETUID: usize = 146;
pub const SYSCALL_SETRESUID: usize = 147;
pub const SYSCALL_GETRESUID: usize = 148;
pub const SYSCALL_SETRESGID: usize = 149;
pub const SYSCALL_GETRESGID: usize = 150;
pub const SYSCALL_SETFSUID: usize = 151;
pub const SYSCALL_SETFSGID: usize = 152;
pub const SYSCALL_TIMES: usize = 153;
pub const SYSCALL_SETPGID: usize = 154;
pub const SYSCALL_GETPGID: usize = 155;
pub const SYSCALL_SETSID: usize = 157;
pub const SYSCALL_GETGROUPS: usize = 158;
pub const SYSCALL_UNAME: usize = 160;
pub const SYSCALL_GETRLIMIT: usize = 163;
pub const SYSCALL_SETRLIMIT: usize = 164;
pub const SYSCALL_GETRUSAGE: usize = 165;
pub const SYSCALL_UMASK: usize = 166;
pub const SYSCALL_GETTIMEOFDAY: usize = 169;
pub const SYSCALL_GETPID: usize = 172;
pub const SYSCALL_GETPPID: usize = 173;
pub const SYSCALL_GETUID: usize = 174;
pub const SYSCALL_GETEUID: usize = 175;
pub const SYSCALL_GETGID: usize = 176;
pub const SYSCALL_GETEGID: usize = 177;
pub const SYSCALL_GETTID: usize = 178;
pub const SYSCALL_SYSINFO: usize = 179;
pub const SYSCALL_SHMGET: usize = 194;
pub const SYSCALL_SHMCTL: usize = 195;
pub const SYSCALL_SHMAT: usize = 196;
pub const SYSCALL_SHMDT: usize = 197;
pub const SYSCALL_SOCKET: usize = 198;
pub const SYSCALL_SOCKETPAIR: usize = 199;
pub const SYSCALL_BIND: usize = 200;
pub const SYSCALL_LISTEN: usize = 201;
pub const SYSCALL_ACCEPT: usize = 202;
pub const SYSCALL_CONNECT: usize = 203;
pub const SYSCALL_GETSOCKNAME: usize = 204;
pub const SYSCALL_GETPEERNAME: usize = 205;
pub const SYSCALL_SENDTO: usize = 206;
pub const SYSCALL_RECVFROM: usize = 207;
pub const SYSCALL_SETSOCKOPT: usize = 208;
pub const SYSCALL_GETSOCKOPT: usize = 209;
pub const SYSCALL_SHUTDOWN: usize = 210;
pub const SYSCALL_BRK: usize = 214;
pub const SYSCALL_MUNMAP: usize = 215;
pub const SYSCALL_CLONE: usize = 220;
pub const SYSCALL_EXECVE: usize = 221;
pub const SYSCALL_MMAP: usize = 222;
pub const SYSCALL_MPROTECT: usize = 226;
pub const SYSCALL_MSYNC: usize = 227;
pub const SYSCALL_MLOCK: usize = 228;
pub const SYSCALL_MUNLOCK: usize = 229;
pub const SYSCALL_MLOCKALL: usize = 230;
pub const SYSCALL_MUNLOCKALL: usize = 231;
pub const SYSCALL_MADVISE: usize = 233;
pub const SYSCALL_ACCEPT4: usize = 242;
pub const SYSCALL_WAIT4: usize = 260;
pub const SYSCALL_PRLIMIT64: usize = 261;
pub const SYSCALL_SCHED_SETATTR: usize = 274;
pub const SYSCALL_SCHED_GETATTR: usize = 275;
pub const SYSCALL_RENAMEAT2: usize = 276;
pub const SYSCALL_MEMBARRIER: usize = 283;
pub const SYSCALL_STATX: usize = 291;
pub const SYSCALL_FACCESSAT2: usize = 439;
pub const SYSCALL_COPY_FILE_RANGE: usize = 326;
pub const SYSCALL_GETRANDOM: usize = 278;

/// Old SYS_open = 1024, used by some basic test binaries via syscall(SYS_open, ...).
/// Maps to openat(AT_FDCWD, path, flags, mode).
/// Linux defines SYS_open = 1024 on RISC-V (only open/openat split happened later).
pub const SYSCALL_OPEN: usize = 1024;
pub const SYSCALL_LINK: usize = 1025;
pub const SYSCALL_UNLINK: usize = 1026;
pub const SYSCALL_RMDIR: usize = 1031;
pub const SYSCALL_ACCESS: usize = 1033;
pub const SYSCALL_RENAME: usize = 1034;
pub const SYSCALL_SYMLINK: usize = 1036;

/// 系统调用分发
///
/// 根据系统调用号分发到对应的处理函数
/// 参数：
/// - syscall_id: 系统调用号
/// - args: 参数数组 [a0, a1, a2, a3, a4, a5]
///
/// 返回值: 成功返回结果，失败返回错误码
pub fn syscall(syscall_id: usize, args: [usize; 6]) -> SyscallRet {
    log::debug!("[syscall] id: {}, args: {:?}", syscall_id, args);

    match syscall_id {
        // 文件操作
        SYSCALL_GETCWD => fs::sys_getcwd(args[0] as *mut u8, args[1]),
        SYSCALL_OPENAT => fs::sys_openat(
            args[0] as isize,
            args[1] as *const u8,
            args[2] as u32,
            args[3] as u32,
        ),
        SYSCALL_OPEN => fs::sys_openat(
            AT_FDCWD,
            args[0] as *const u8,
            args[1] as u32,
            args[2] as u32,
        ),
        SYSCALL_ACCESS => fs::sys_access(args[0] as *const u8, args[1]),
        SYSCALL_CLOSE => fs::sys_close(args[0]),
        SYSCALL_PIPE2 => fs::sys_pipe2(args[0] as *mut i32, args[1]),
        SYSCALL_FACCESSAT => fs::sys_faccessat(args[0] as isize, args[1] as *const u8, args[2], 0),
        SYSCALL_FACCESSAT2 => {
            fs::sys_faccessat(args[0] as isize, args[1] as *const u8, args[2], args[3])
        }
        SYSCALL_GETDENTS64 => fs::sys_getdents64(args[0], args[1] as *mut u8, args[2]),
        SYSCALL_READ => fs::sys_read(args[0], args[1] as *mut u8, args[2]),
        SYSCALL_WRITE => fs::sys_write(args[0], args[1] as *const u8, args[2]),
        SYSCALL_READV => fs::sys_readv(args[0], args[1] as *const u8, args[2]),
        SYSCALL_WRITEV => fs::sys_writev(args[0], args[1] as *const u8, args[2]),
        SYSCALL_PREAD64 => fs::sys_pread64(args[0], args[1] as *mut u8, args[2], args[3]),
        SYSCALL_PWRITE64 => fs::sys_pwrite64(args[0], args[1] as *const u8, args[2], args[3]),
        SYSCALL_PREADV => fs::sys_preadv(args[0], args[1] as *const u8, args[2], args[3]),
        SYSCALL_PWRITEV => fs::sys_pwritev(args[0], args[1] as *const u8, args[2], args[3]),
        SYSCALL_SENDFILE => fs::sys_sendfile(args[0], args[1], args[2], args[3]),
        SYSCALL_TRUNCATE => fs::sys_truncate(args[0] as *const u8, args[1]),
        SYSCALL_FTRUNCATE => fs::sys_ftruncate(args[0], args[1]),
        SYSCALL_FCHMOD => fs::sys_fchmod(args[0], args[1] as u32),
        SYSCALL_FCHMODAT => {
            fs::sys_fchmodat(args[0] as isize, args[1] as *const u8, args[2] as u32)
        }
        SYSCALL_FCHOWN => fs::sys_fchown(args[0], args[1], args[2]),
        SYSCALL_FCHOWNAT => fs::sys_fchownat(
            args[0] as isize,
            args[1] as *const u8,
            args[2],
            args[3],
            args[4],
        ),
        SYSCALL_PPOLL => fs::sys_ppoll(
            args[0] as *mut fs::PollFd,
            args[1],
            args[2],
            args[3],
            args[4],
        ),
        SYSCALL_PSELECT6 => fs::sys_pselect6(args[0], args[1], args[2], args[3], args[4], args[5]),
        SYSCALL_LSEEK => fs::sys_lseek(args[0], args[1] as isize, args[2]),
        SYSCALL_DUP => fs::sys_dup(args[0]),
        SYSCALL_DUP3 => fs::sys_dup3(args[0], args[1], args[2]),
        SYSCALL_FCNTL => fs::sys_fcntl(args[0], args[1], args[2]),
        SYSCALL_IOCTL => fs::sys_ioctl(args[0], args[1], args[2]),
        SYSCALL_SOCKET => net::sys_socket(args[0], args[1], args[2]),
        SYSCALL_SOCKETPAIR => net::sys_socketpair(args[0], args[1], args[2], args[3]),
        SYSCALL_BIND => net::sys_bind(args[0], args[1], args[2]),
        SYSCALL_LISTEN => net::sys_listen(args[0], args[1]),
        SYSCALL_ACCEPT => net::sys_accept(args[0], args[1], args[2]),
        SYSCALL_ACCEPT4 => net::sys_accept4(args[0], args[1], args[2], args[3]),
        SYSCALL_CONNECT => net::sys_connect(args[0], args[1], args[2]),
        SYSCALL_GETSOCKNAME => net::sys_getsockname(args[0], args[1], args[2]),
        SYSCALL_GETPEERNAME => net::sys_getpeername(args[0], args[1], args[2]),
        SYSCALL_SENDTO => net::sys_sendto(args[0], args[1], args[2], args[3], args[4], args[5]),
        SYSCALL_RECVFROM => net::sys_recvfrom(args[0], args[1], args[2], args[3], args[4], args[5]),
        SYSCALL_SETSOCKOPT => net::sys_setsockopt(args[0], args[1], args[2], args[3], args[4]),
        SYSCALL_GETSOCKOPT => net::sys_getsockopt(args[0], args[1], args[2], args[3], args[4]),
        SYSCALL_SHUTDOWN => net::sys_shutdown(args[0], args[1]),
        SYSCALL_NEWFSTATAT => fs::sys_newfstatat(
            args[0] as isize,
            args[1] as *const u8,
            args[2] as *mut u8,
            args[3],
        ),
        SYSCALL_FSTAT => fs::sys_fstat(args[0], args[1] as *mut u8),
        SYSCALL_STATX => fs::sys_statx(
            args[0] as isize,
            args[1] as *const u8,
            args[2],
            args[3],
            args[4] as *mut u8,
        ),
        SYSCALL_CHDIR => fs::sys_chdir(args[0] as *const u8),
        SYSCALL_MKDIRAT => fs::sys_mkdirat(args[0] as isize, args[1] as *const u8, args[2] as u32),
        SYSCALL_UNLINK => fs::sys_unlink(args[0] as *const u8),
        SYSCALL_UNLINKAT => fs::sys_unlinkat(args[0] as isize, args[1] as *const u8, args[2]),
        SYSCALL_RMDIR => fs::sys_rmdir(args[0] as *const u8),
        SYSCALL_LINK => fs::sys_link(args[0] as *const u8, args[1] as *const u8),
        SYSCALL_LINKAT => fs::sys_linkat(
            args[0] as isize,
            args[1] as *const u8,
            args[2] as isize,
            args[3] as *const u8,
            args[4],
        ),
        SYSCALL_SYMLINK => fs::sys_symlink(args[0] as *const u8, args[1] as *const u8),
        SYSCALL_SYMLINKAT => {
            fs::sys_symlinkat(args[0] as *const u8, args[1] as isize, args[2] as *const u8)
        }
        SYSCALL_RENAME => fs::sys_rename(args[0] as *const u8, args[1] as *const u8),
        SYSCALL_RENAMEAT => fs::sys_renameat(
            args[0] as isize,
            args[1] as *const u8,
            args[2] as isize,
            args[3] as *const u8,
        ),
        SYSCALL_RENAMEAT2 => fs::sys_renameat2(
            args[0] as isize,
            args[1] as *const u8,
            args[2] as isize,
            args[3] as *const u8,
            args[4],
        ),
        SYSCALL_MOUNT => fs::sys_mount(
            args[0] as *const u8,
            args[1] as *const u8,
            args[2] as *const u8,
            args[3],
            args[4],
        ),
        SYSCALL_UMOUNT2 => fs::sys_umount2(args[0] as *const u8, args[1]),
        SYSCALL_STATFS => fs::sys_statfs(args[0] as *const u8, args[1] as *mut u8),
        SYSCALL_FSTATFS => fs::sys_fstatfs(args[0], args[1] as *mut u8),

        // 进程管理
        SYSCALL_EXIT => process::sys_exit(args[0] as i32),
        SYSCALL_EXIT_GROUP => process::sys_exit_group(args[0] as i32),
        SYSCALL_GETPID => process::sys_getpid(),
        SYSCALL_GETPPID => process::sys_getppid(),
        SYSCALL_SCHED_YIELD => process::sys_sched_yield(),
        SYSCALL_CLONE => process::sys_clone(args[0], args[1], args[2], args[3], args[4]),
        SYSCALL_EXECVE => process::sys_execve(args[0] as *const u8, args[1], args[2]),
        SYSCALL_WAIT4 => {
            process::sys_wait4(args[0] as isize, args[1] as *mut i32, args[2], args[3])
        }

        // 内存管理
        SYSCALL_BRK => mm::sys_brk(args[0]),
        SYSCALL_MMAP => mm::sys_mmap(
            args[0],
            args[1],
            args[2] as i32,
            args[3] as i32,
            args[4] as i32,
            args[5],
        ),
        SYSCALL_MUNMAP => mm::sys_munmap(args[0], args[1]),
        SYSCALL_MSYNC => mm::sys_msync(args[0], args[1], args[2]),
        SYSCALL_SHMGET => mm::sys_shmget(args[0] as isize, args[1], args[2] as i32),
        SYSCALL_SHMCTL => mm::sys_shmctl(args[0], args[1] as i32, args[2]),
        SYSCALL_SHMAT => mm::sys_shmat(args[0], args[1], args[2] as i32),
        SYSCALL_SHMDT => mm::sys_shmdt(args[0]),

        // 时间和系统信息
        SYSCALL_NANOSLEEP => other::sys_nanosleep(args[0], args[1]),
        SYSCALL_CLOCK_GETTIME => other::sys_clock_gettime(args[0], args[1]),
        SYSCALL_CLOCK_GETRES => other::sys_clock_getres(args[0], args[1]),
        SYSCALL_TIMES => other::sys_times(args[0]),
        SYSCALL_UNAME => other::sys_uname(args[0]),
        SYSCALL_GETTIMEOFDAY => other::sys_gettimeofday(args[0], args[1]),
        SYSCALL_GETUID => other::sys_getuid(),
        SYSCALL_GETEUID => other::sys_geteuid(),
        SYSCALL_GETGID => other::sys_getgid(),
        SYSCALL_GETEGID => other::sys_getegid(),
        SYSCALL_SETUID => other::sys_setuid(args[0]),
        SYSCALL_SETGID => other::sys_setgid(args[0]),
        SYSCALL_SETREUID => other::sys_setreuid(args[0], args[1]),
        SYSCALL_SETREGID => other::sys_setregid(args[0], args[1]),
        SYSCALL_SETRESUID => other::sys_setresuid(args[0], args[1], args[2]),
        SYSCALL_SETRESGID => other::sys_setresgid(args[0], args[1], args[2]),
        SYSCALL_GETRESUID => other::sys_getresuid(args[0], args[1], args[2]),
        SYSCALL_GETRESGID => other::sys_getresgid(args[0], args[1], args[2]),
        SYSCALL_SETFSUID => other::sys_setfsuid(args[0]),
        SYSCALL_SETFSGID => other::sys_setfsgid(args[0]),
        SYSCALL_GETTID => other::sys_gettid(),
        SYSCALL_GETRLIMIT => other::sys_getrlimit(args[0], args[1]),
        SYSCALL_SETRLIMIT => other::sys_setrlimit(args[0], args[1]),
        SYSCALL_PRLIMIT64 => other::sys_prlimit64(args[0], args[1], args[2], args[3]),
        SYSCALL_SET_TID_ADDRESS => other::sys_set_tid_address(args[0]),
        SYSCALL_SET_ROBUST_LIST => other::sys_set_robust_list(args[0], args[1]),
        SYSCALL_GET_ROBUST_LIST => other::sys_get_robust_list(args[0], args[1], args[2]),
        SYSCALL_GETRANDOM => other::sys_getrandom(args[0], args[1], args[2]),
        SYSCALL_SYSINFO => other::sys_sysinfo(args[0]),
        SYSCALL_SYSLOG => other::sys_syslog(args[0], args[1], args[2]),
        SYSCALL_GETRUSAGE => other::sys_getrusage(args[0], args[1]),
        SYSCALL_UMASK => other::sys_umask(args[0]),
        SYSCALL_GETPGID => other::sys_getpgid(args[0]),
        SYSCALL_SETPGID => other::sys_setpgid(args[0], args[1]),
        SYSCALL_MEMBARRIER => other::sys_membarrier(args[0], args[1]),
        SYSCALL_MPROTECT => mm::sys_mprotect(args[0], args[1], args[2] as i32),
        SYSCALL_MADVISE => Ok(0), // madvise advisory, ignore

        // sched stubs / single-CPU policy
        SYSCALL_SCHED_GETAFFINITY => other::sys_sched_getaffinity(args[0], args[1], args[2]),
        SYSCALL_SCHED_SETAFFINITY => other::sys_sched_setaffinity(args[0], args[1], args[2]),
        SYSCALL_SCHED_GETSCHEDULER => other::sys_sched_getscheduler(args[0]),
        SYSCALL_SCHED_SETSCHEDULER => other::sys_sched_setscheduler(args[0], args[1], args[2]),
        SYSCALL_SCHED_GETPARAM => other::sys_sched_getparam(args[0], args[1]),
        SYSCALL_SCHED_SETPARAM => other::sys_sched_setparam(args[0], args[1]),
        SYSCALL_SCHED_GETATTR => other::sys_sched_getattr(args[0], args[1], args[2], args[3]),
        SYSCALL_SCHED_SETATTR => other::sys_sched_setattr(args[0], args[1], args[2]),
        SYSCALL_SCHED_GET_PRIORITY_MAX => other::sys_sched_get_priority_max(args[0]),
        SYSCALL_SCHED_GET_PRIORITY_MIN => other::sys_sched_get_priority_min(args[0]),
        SYSCALL_SCHED_RR_GET_INTERVAL => other::sys_sched_rr_get_interval(args[0], args[1]),

        // signals
        SYSCALL_KILL => signal::sys_kill(args[0] as i32, args[1] as i32),
        SYSCALL_TKILL => signal::sys_tkill(args[0] as i32, args[1] as i32),
        SYSCALL_TGKILL => signal::sys_tgkill(args[0] as i32, args[1] as i32, args[2] as i32),
        SYSCALL_SIGACTION => signal::sys_sigaction(args[0] as i32, args[1], args[2], args[3]),
        SYSCALL_SIGPROCMASK => signal::sys_sigprocmask(args[0] as i32, args[1], args[2], args[3]),
        SYSCALL_SIGSUSPEND => signal::sys_sigsuspend(args[0], args[1]),
        SYSCALL_SIGTIMEDWAIT => signal::sys_sigtimedwait(args[0], args[1], args[2], args[3]),
        SYSCALL_SIGRETURN => signal::sys_sigreturn(),

        SYSCALL_READLINKAT => fs::sys_readlinkat(
            args[0] as isize,
            args[1] as *const u8,
            args[2] as *mut u8,
            args[3],
        ),
        SYSCALL_UTIMENSAT => fs::sys_utimensat(
            args[0] as isize,
            args[1] as *const u8,
            args[2] as *const fs::TimeSpec,
            args[3],
        ),
        SYSCALL_SYNC => fs::sys_sync(),
        SYSCALL_FSYNC => fs::sys_fsync(args[0]),
        SYSCALL_FDATASYNC => fs::sys_fdatasync(args[0]),
        SYSCALL_FUTEX => {
            other::sys_futex_stub(args[0], args[1], args[2], args[3], args[4], args[5])
        }
        SYSCALL_CLOCK_NANOSLEEP => other::sys_clock_nanosleep(args[0], args[1], args[2], args[3]),
        SYSCALL_MLOCK | SYSCALL_MUNLOCK | SYSCALL_MLOCKALL | SYSCALL_MUNLOCKALL => {
            other::sys_memory_lock_noop()
        }
        SYSCALL_SETSID => other::sys_getpgid(0),

        _ => {
            log::warn!("[syscall] Unsupported syscall: {}", syscall_id);
            Err(SysErrNo::ENOSYS)
        }
    }
}
