//! システムコール(Linux AArch64 互換)。
//!
//! ユーザーは `svc #0` で番号を x8、引数を x0〜x5 に入れて呼ぶ。戻り値は x0(エラーは負の errno)。
//! 番号ごとのハンドラは `fs.rs`(ファイル)、`proc.rs`(プロセス)などに分けて置く。
//! 未実装の番号は `ENOSYS` を返し、最初の1回だけログに出す(次に何を作るべきかが分かる)。

mod fs;
mod misc;
mod mm;
mod poll;
mod proc;
mod time;

use crate::process::Process;
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

/// システムコールの番号(Linux AArch64、`include/uapi/asm-generic/unistd.h`)。
pub mod nr {
    pub const GETCWD: u64 = 17;
    pub const EPOLL_CREATE1: u64 = 20;
    pub const EPOLL_CTL: u64 = 21;
    pub const EPOLL_PWAIT: u64 = 22;
    pub const DUP: u64 = 23;
    pub const DUP3: u64 = 24;
    pub const FCNTL: u64 = 25;
    pub const IOCTL: u64 = 29;
    pub const MKDIRAT: u64 = 34;
    pub const UNLINKAT: u64 = 35;
    pub const FTRUNCATE: u64 = 46;
    pub const FACCESSAT: u64 = 48;
    pub const CHDIR: u64 = 49;
    pub const OPENAT: u64 = 56;
    pub const CLOSE: u64 = 57;
    pub const GETDENTS64: u64 = 61;
    pub const LSEEK: u64 = 62;
    pub const READ: u64 = 63;
    pub const WRITE: u64 = 64;
    pub const READV: u64 = 65;
    pub const WRITEV: u64 = 66;
    pub const PREAD64: u64 = 67;
    pub const PWRITE64: u64 = 68;
    pub const PSELECT6: u64 = 72;
    pub const PPOLL: u64 = 73;
    pub const READLINKAT: u64 = 78;
    pub const NEWFSTATAT: u64 = 79;
    pub const FSTAT: u64 = 80;
    pub const TIMERFD_CREATE: u64 = 85;
    pub const TIMERFD_SETTIME: u64 = 86;
    pub const TIMERFD_GETTIME: u64 = 87;
    pub const EXIT: u64 = 93;
    pub const EXIT_GROUP: u64 = 94;
    pub const SET_TID_ADDRESS: u64 = 96;
    pub const NANOSLEEP: u64 = 101;
    pub const CLOCK_GETTIME: u64 = 113;
    pub const CLOCK_NANOSLEEP: u64 = 115;
    pub const SCHED_SETSCHEDULER: u64 = 119;
    pub const SCHED_GETSCHEDULER: u64 = 120;
    pub const SCHED_GETAFFINITY: u64 = 123;
    pub const SCHED_YIELD: u64 = 124;
    pub const RT_SIGACTION: u64 = 134;
    pub const RT_SIGPROCMASK: u64 = 135;
    pub const UNAME: u64 = 160;
    pub const GETPID: u64 = 172;
    pub const GETUID: u64 = 174;
    pub const GETEUID: u64 = 175;
    pub const GETGID: u64 = 176;
    pub const GETEGID: u64 = 177;
    pub const GETPPID: u64 = 173;
    pub const GETTID: u64 = 178;
    pub const BRK: u64 = 214;
    pub const MUNMAP: u64 = 215;
    pub const MREMAP: u64 = 216;
    pub const CLONE: u64 = 220;
    pub const EXECVE: u64 = 221;
    pub const MMAP: u64 = 222;
    pub const MPROTECT: u64 = 226;
    pub const MADVISE: u64 = 233;
    pub const WAIT4: u64 = 260;
    pub const GETRANDOM: u64 = 278;
}

/// ioctl:端末の大きさを問い合わせる
pub const TIOCGWINSZ: u64 = 0x5413;

/// システムコールのあとにすること。
pub enum Action {
    /// x0 にこの値を入れてユーザーに戻る
    Return(u64),
    /// プロセスを終了する(終了ステータス)
    Exit(i32),
    /// 子プロセスを動かし始める(fork)。親には子の番号、子には 0 を返す
    Fork(alloc::boxed::Box<Process>),
    /// 新しいプログラムの入口から始める(execve)。ユーザー空間は差し替え済み
    Exec { entry: u64, sp: u64 },
}

impl core::fmt::Debug for Action {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Return(v) => write!(f, "Return({v:#x})"),
            Self::Exit(s) => write!(f, "Exit({s})"),
            Self::Fork(_) => write!(f, "Fork(..)"),
            Self::Exec { entry, sp } => write!(f, "Exec {{ entry: {entry:#x}, sp: {sp:#x} }}"),
        }
    }
}

impl PartialEq for Action {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Return(a), Self::Return(b)) => a == b,
            (Self::Exit(a), Self::Exit(b)) => a == b,
            (Self::Exec { entry: a, sp: b }, Self::Exec { entry: c, sp: d }) => a == c && b == d,
            _ => false,
        }
    }
}

/// 番号ごとの「未実装で呼ばれた回数」
const TRACKED: usize = 512;
static UNIMPLEMENTED: [AtomicU32; TRACKED] = [const { AtomicU32::new(0) }; TRACKED];

/// 呼ばれたのに未実装だったシステムコール:(番号, 回数)
pub fn unimplemented_list() -> alloc::vec::Vec<(u64, u32)> {
    UNIMPLEMENTED
        .iter()
        .enumerate()
        .map(|(nr, c)| (nr as u64, c.load(Ordering::Relaxed)))
        .filter(|&(_, c)| c > 0)
        .collect()
}

pub fn unimplemented_count(nr: u64) -> u32 {
    UNIMPLEMENTED
        .get(nr as usize)
        .map_or(0, |c| c.load(Ordering::Relaxed))
}

fn result(r: Result<u64, crate::Errno>) -> Action {
    Action::Return(match r {
        Ok(v) => v,
        Err(e) => e.as_syscall_return(),
    })
}

/// システムコール表の1行。
pub struct Syscall {
    pub nr: u64,
    pub name: &'static str,
    /// 引数の数(トレースの表示用)
    pub args: usize,
    pub handler: fn(&mut Process, [u64; 6]) -> Action,
}

/// 表を作るマクロ:`番号, "名前", 引数の数 => |プロセス, 引数| 本体;`
macro_rules! table {
    ($($nr:expr, $name:literal, $args:literal => |$p:ident, $a:ident| $body:expr;)*) => {
        /// 実装済みのシステムコールの表(番号の順)。`docs/generated/syscalls.md` はここから作る。
        pub static TABLE: &[Syscall] = &[$(Syscall {
            nr: $nr,
            name: $name,
            args: $args,
            handler: {
                #[allow(unused_variables)]
                fn handler($p: &mut Process, $a: [u64; 6]) -> Action {
                    $body
                }
                handler
            },
        }),*];
    };
}

table! {
    nr::GETCWD, "getcwd", 2 => |p, a| result(fs::getcwd(p, a[0], a[1]));
    nr::EPOLL_CREATE1, "epoll_create1", 1 => |p, a| result(time::epoll_create1(p));
    nr::EPOLL_CTL, "epoll_ctl", 4 => |p, a| result(time::epoll_ctl(p, a[0], a[1], a[2], a[3]));
    nr::EPOLL_PWAIT, "epoll_pwait", 4
        => |p, a| result(time::epoll_pwait(p, a[0], a[1], a[2], a[3] as i32));
    nr::DUP, "dup", 1 => |p, a| result(fs::dup(p, a[0]));
    nr::DUP3, "dup3", 3 => |p, a| result(fs::dup3(p, a[0], a[1], a[2]));
    nr::FCNTL, "fcntl", 3 => |p, a| result(fs::fcntl(p, a[0], a[1], a[2]));
    nr::IOCTL, "ioctl", 3 => |p, a| result(fs::ioctl(p, a[0], a[1], a[2]));
    nr::MKDIRAT, "mkdirat", 3 => |p, a| result(fs::mkdirat(p, a[0], a[1], a[2]));
    nr::UNLINKAT, "unlinkat", 3 => |p, a| result(fs::unlinkat(p, a[0], a[1], a[2]));
    nr::FTRUNCATE, "ftruncate", 2 => |p, a| result(fs::ftruncate(p, a[0], a[1]));
    nr::FACCESSAT, "faccessat", 3 => |p, a| result(fs::faccessat(p, a[0], a[1], a[2]));
    nr::CHDIR, "chdir", 1 => |p, a| result(fs::chdir(p, a[0]));
    nr::OPENAT, "openat", 4 => |p, a| result(fs::openat(p, a[0], a[1], a[2], a[3]));
    nr::CLOSE, "close", 1 => |p, a| result(p.fds.close(a[0]).map(|_| 0));
    nr::GETDENTS64, "getdents64", 3 => |p, a| result(fs::getdents64(p, a[0], a[1], a[2]));
    nr::LSEEK, "lseek", 3 => |p, a| result(fs::lseek(p, a[0], a[1] as i64, a[2]));
    nr::READ, "read", 3 => |p, a| result(fs::read(p, a[0], a[1], a[2]));
    nr::WRITE, "write", 3 => |p, a| result(fs::write(p, a[0], a[1], a[2]));
    nr::READV, "readv", 3 => |p, a| result(fs::readv(p, a[0], a[1], a[2]));
    nr::WRITEV, "writev", 3 => |p, a| result(fs::writev(p, a[0], a[1], a[2]));
    nr::PREAD64, "pread64", 4 => |p, a| result(fs::pread64(p, a[0], a[1], a[2], a[3]));
    nr::PWRITE64, "pwrite64", 4 => |p, a| result(fs::pwrite64(p, a[0], a[1], a[2], a[3]));
    nr::PSELECT6, "pselect6", 6 => |p, a| result(poll::pselect6(p, a[0], a[1], a[2], a[3], a[4]));
    nr::PPOLL, "ppoll", 5 => |p, a| result(poll::ppoll(p, a[0], a[1], a[2]));
    nr::READLINKAT, "readlinkat", 4 => |p, a| result(fs::readlinkat(p, a[0], a[1]));
    nr::NEWFSTATAT, "newfstatat", 4 => |p, a| result(fs::newfstatat(p, a[0], a[1], a[2], a[3]));
    nr::FSTAT, "fstat", 2 => |p, a| result(fs::fstat(p, a[0], a[1]));
    nr::TIMERFD_CREATE, "timerfd_create", 2 => |p, a| result(time::timerfd_create(p));
    nr::TIMERFD_SETTIME, "timerfd_settime", 4
        => |p, a| result(time::timerfd_settime(p, a[0], a[1], a[2], a[3]));
    nr::TIMERFD_GETTIME, "timerfd_gettime", 2 => |p, a| result(time::timerfd_gettime(p, a[0], a[1]));
    nr::EXIT, "exit", 1 => |p, a| Action::Exit(proc::exit_status(a[0]));
    nr::EXIT_GROUP, "exit_group", 1 => |p, a| Action::Exit(proc::exit_status(a[0]));
    // フェーズ1はプロセスもスレッドも1つ
    nr::SET_TID_ADDRESS, "set_tid_address", 1 => |p, a| Action::Return(1);
    nr::NANOSLEEP, "nanosleep", 2 => |p, a| result(time::nanosleep(p, a[0]));
    nr::CLOCK_GETTIME, "clock_gettime", 2 => |p, a| result(misc::clock_gettime(p, a[0], a[1]));
    nr::CLOCK_NANOSLEEP, "clock_nanosleep", 4
        => |p, a| result(time::clock_nanosleep(p, a[0], a[1], a[2]));
    nr::SCHED_SETSCHEDULER, "sched_setscheduler", 3
        => |p, a| result(misc::sched_setscheduler(p, a[0], a[1], a[2]));
    nr::SCHED_GETSCHEDULER, "sched_getscheduler", 1
        => |p, a| result(misc::sched_getscheduler(p, a[0]));
    nr::SCHED_GETAFFINITY, "sched_getaffinity", 3
        => |p, a| result(misc::sched_getaffinity(p, a[1], a[2]));
    nr::SCHED_YIELD, "sched_yield", 0 => |p, a| {
        crate::sched::relax();
        Action::Return(0)
    };
    // シグナルはまだ配送しないので、設定は受け付けるだけ
    nr::RT_SIGACTION, "rt_sigaction", 4 => |p, a| Action::Return(0);
    nr::RT_SIGPROCMASK, "rt_sigprocmask", 4 => |p, a| Action::Return(0);
    nr::UNAME, "uname", 1 => |p, a| result(misc::uname(p, a[0]));
    nr::GETPID, "getpid", 0 => |p, a| Action::Return(1);
    // 資格情報(ADR-0005)
    nr::GETPPID, "getppid", 0 => |p, a| Action::Return(p.ppid as u64);
    nr::GETUID, "getuid", 0 => |p, a| Action::Return(p.cred.uid as u64);
    nr::GETEUID, "geteuid", 0 => |p, a| Action::Return(p.cred.euid as u64);
    nr::GETGID, "getgid", 0 => |p, a| Action::Return(p.cred.gid as u64);
    nr::GETEGID, "getegid", 0 => |p, a| Action::Return(p.cred.egid as u64);
    nr::GETTID, "gettid", 0 => |p, a| Action::Return(1);
    nr::BRK, "brk", 1 => |p, a| Action::Return(mm::brk(p, a[0]));
    nr::MUNMAP, "munmap", 2 => |p, a| result(mm::munmap(p, a[0], a[1]));
    // 断ると、libc は「新しく確保してコピー」に切り替える
    nr::MREMAP, "mremap", 5 => |p, a| Action::Return(crate::Errno::ENOMEM.as_syscall_return());
    nr::CLONE, "clone", 5 => |p, a| proc::clone(p, a[0]);
    nr::EXECVE, "execve", 3 => |p, a| proc::execve(p, a[0], a[1], a[2]);
    nr::MMAP, "mmap", 6 => |p, a| result(mm::mmap(p, a[0], a[1], a[2], a[3], a[4], a[5]));
    nr::MPROTECT, "mprotect", 3 => |p, a| result(mm::mprotect(p, a[0], a[1], a[2]));
    // 助言は無視してよい
    nr::MADVISE, "madvise", 3 => |p, a| Action::Return(0);
    nr::WAIT4, "wait4", 4 => |p, a| result(proc::wait4(p, a[0] as i64, a[1], a[2]));
    nr::GETRANDOM, "getrandom", 3 => |p, a| result(misc::getrandom(p, a[0], a[1]));
}

/// 番号から表の行を探す。
pub fn lookup(nr: u64) -> Option<&'static Syscall> {
    TABLE.iter().find(|s| s.nr == nr)
}

/// トレースを表示するか(`trace-syscalls` feature でビルドしたとき BSP が有効にする)
pub static TRACE: AtomicBool = AtomicBool::new(false);

/// トレースの1行:`[trace] openat(0xffffffffffffff9c, 0x403000, 0x0) = 3`
pub fn trace_line(s: &Syscall, a: &[u64; 6], ret: u64) -> alloc::string::String {
    use core::fmt::Write;
    let mut line = alloc::format!("[trace] {}(", s.name);
    for (i, v) in a.iter().take(s.args.min(3)).enumerate() {
        let sep = if i == 0 { "" } else { ", " };
        let _ = write!(line, "{sep}{v:#x}");
    }
    let signed = ret as i64;
    let _ = if (-4095..0).contains(&signed) {
        let name = crate::errno::name(-signed as i32).unwrap_or("?");
        write!(line, ") = {signed} ({name})")
    } else if ret < 0x1000 {
        write!(line, ") = {ret}") // 小さな値(fd、バイト数)は 10 進数
    } else {
        write!(line, ") = {ret:#x}") // アドレスは 16 進数
    };
    line
}

/// システムコールを1つ処理する。
pub fn dispatch(p: &mut Process, nr: u64, a: [u64; 6]) -> Action {
    let Some(syscall) = lookup(nr) else {
        if let Some(count) = UNIMPLEMENTED.get(nr as usize)
            && count.fetch_add(1, Ordering::Relaxed) == 0
        {
            crate::println!("[pi-os] unimplemented syscall {nr}");
        }
        return Action::Return(crate::Errno::ENOSYS.as_syscall_return());
    };
    let action = (syscall.handler)(p, a);
    if TRACE.load(Ordering::Relaxed) {
        match &action {
            &Action::Return(ret) => crate::println!("{}", trace_line(syscall, &a, ret)),
            Action::Exit(status) => crate::println!("[trace] {}({status}) = ?", syscall.name),
            Action::Fork(_) => crate::println!("[trace] {}(…) = (child)", syscall.name),
            Action::Exec { .. } => crate::println!("[trace] {}(…) = (new program)", syscall.name),
        }
    }
    action
}

#[cfg(test)]
mod tests_mm;
#[cfg(test)]
mod tests_proc;
#[cfg(test)]
mod tests_time;
#[cfg(test)]
mod tests_tmp;
#[cfg(test)]
mod tests_tty;

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::Errno;
    use crate::exec::Loaded;
    use crate::fs::devfs::{ConsoleFile, Devfs};
    use crate::fs::initramfs::Initramfs;
    use crate::fs::tar::tests::build;
    use crate::fs::vfs::Vfs;
    use crate::mm::user::tests::memory;
    use crate::process::Process;
    use crate::sync::SpinLock;
    use alloc::sync::Arc;
    use hal::addr::VirtAddr;
    use hal::console::Console;
    use hal::mmu::MapFlags;
    use std::boxed::Box;
    use std::vec::Vec;

    /// 書かれた内容を共有のバッファに残す出力先
    #[derive(Clone, Default)]
    pub struct Capture(
        pub Arc<SpinLock<Vec<u8>>>,
        pub Arc<SpinLock<std::collections::VecDeque<u8>>>,
    );

    impl Capture {
        /// 端末から入力が届いたことにする
        pub fn type_in(&self, bytes: &[u8]) {
            self.1.lock().extend(bytes);
        }
    }

    impl Console for Capture {
        fn write_bytes(&mut self, buf: &[u8]) {
            self.0.lock().extend_from_slice(buf);
        }

        fn read_byte(&mut self) -> Option<u8> {
            self.1.lock().pop_front()
        }
    }

    /// ユーザーの作業領域(読み書き可)
    pub const SCRATCH: u64 = 0x40_0000;

    /// ホストの時計をカウンタにする(ナノ秒単位)。カーネルでは ARM Generic Timer が入る
    fn host_counter() -> u64 {
        static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
        START
            .get_or_init(std::time::Instant::now)
            .elapsed()
            .as_nanos() as u64
    }

    fn fake_space() -> Result<Box<dyn hal::mmu::AddressSpace>, crate::Errno> {
        Ok(Box::new(crate::mm::user::tests::FakeSpace::default()))
    }

    pub fn process() -> (Process, Capture) {
        crate::time::set_counter(host_counter, 1_000_000_000);
        let (mut mem, _) = memory();
        mem.map_zeroed(VirtAddr(SCRATCH), 0x4000, MapFlags::USER_RW)
            .unwrap();
        let out = Capture::default();
        let mut tar = build(&[
            ("etc/motd", b'0', b"Welcome to Pi-OS!\n"),
            ("bin/x", b'0', b"ELF"),
            ("etc/secret", b'0', b"s3cret"),
        ]);
        crate::fs::tar::tests::set_mode(&mut tar, "etc/motd", 0o644);
        crate::fs::tar::tests::set_mode(&mut tar, "etc/secret", 0o600);
        // execve のテスト用:小さな ELF と、それを使うシバンのスクリプト
        let tiny = crate::exec::test_elf::build(
            crate::exec::elf::EM_AARCH64,
            &[(crate::exec::elf::PT_LOAD, 5, 0x40_0000, b"CODE", 4)],
        );
        let extra = build(&[
            ("bin/tiny", b'0', &tiny),
            ("bin/script", b'0', b"#!/bin/tiny -x\n"),
        ]);
        tar.truncate(tar.len() - 1024); // 終わりの印を外して足す
        tar.extend_from_slice(&extra);
        crate::fs::tar::tests::set_mode(&mut tar, "bin/tiny", 0o755);
        crate::fs::tar::tests::set_mode(&mut tar, "bin/script", 0o755);
        let root = Initramfs::new(Box::leak(tar.into_boxed_slice())).unwrap();
        let console = Arc::new(ConsoleFile::new(Box::new(out.clone())));
        let mut vfs = Vfs::new(Arc::new(root));
        vfs.mount("/dev", Arc::new(Devfs::new(console.clone())));
        vfs.mount("/tmp", Arc::new(crate::fs::tmpfs::Tmpfs::new()));
        let loaded = Loaded {
            entry: SCRATCH,
            sp: SCRATCH + 0x1000,
            brk: 0x50_0000,
        };
        let mut p = Process::new(mem, loaded, Arc::new(vfs), console);
        p.new_space = fake_space;
        (p, out)
    }

    pub(crate) fn put(p: &mut Process, va: u64, bytes: &[u8]) {
        p.mem.copy_to_user(VirtAddr(va), bytes).unwrap();
    }

    pub(crate) fn get(p: &mut Process, va: u64, len: usize) -> Vec<u8> {
        let mut v = std::vec![0u8; len];
        p.mem.copy_from_user(VirtAddr(va), &mut v).unwrap();
        v
    }

    pub(crate) fn call(p: &mut Process, nr: u64, a: [u64; 6]) -> u64 {
        match dispatch(p, nr, a) {
            Action::Return(v) => v,
            other => panic!("unexpected {other:?}"),
        }
    }

    pub(crate) fn err(e: Errno) -> u64 {
        e.as_syscall_return()
    }

    pub(crate) const AT_FDCWD: u64 = -100i64 as u64;

    pub(crate) fn open(p: &mut Process, path: &[u8], flags: u64) -> u64 {
        put(p, SCRATCH + 0x3000, path);
        put(p, SCRATCH + 0x3000 + path.len() as u64, b"\0");
        call(p, nr::OPENAT, [AT_FDCWD, SCRATCH + 0x3000, flags, 0, 0, 0])
    }

    #[test]
    fn write_to_stdout_copies_user_bytes_to_the_console() {
        let (mut p, out) = process();
        put(&mut p, SCRATCH, b"hello\n");
        assert_eq!(call(&mut p, nr::WRITE, [1, SCRATCH, 6, 0, 0, 0]), 6);
        assert_eq!(&out.0.lock()[..], b"hello\n");
    }

    #[test]
    fn write_with_a_bad_pointer_is_efault() {
        let (mut p, out) = process();
        assert_eq!(
            call(&mut p, nr::WRITE, [1, 0xdead_0000, 4, 0, 0, 0]),
            err(Errno::EFAULT)
        );
        assert!(out.0.lock().is_empty());
    }

    #[test]
    fn unknown_fd_is_ebadf() {
        let (mut p, _) = process();
        assert_eq!(
            call(&mut p, nr::WRITE, [7, SCRATCH, 1, 0, 0, 0]),
            err(Errno::EBADF)
        );
        assert_eq!(
            call(&mut p, nr::CLOSE, [7, 0, 0, 0, 0, 0]),
            err(Errno::EBADF)
        );
    }

    #[test]
    fn exit_and_exit_group_end_the_process_with_the_low_byte_of_the_status() {
        let (mut p, _) = process();
        assert_eq!(
            dispatch(&mut p, nr::EXIT_GROUP, [3, 0, 0, 0, 0, 0]),
            Action::Exit(3)
        );
        assert_eq!(
            dispatch(&mut p, nr::EXIT, [0x1ff, 0, 0, 0, 0, 0]),
            Action::Exit(0xff)
        );
    }

    #[test]
    fn unknown_syscall_is_enosys_and_recorded() {
        let (mut p, _) = process();
        let before = unimplemented_count(511);
        assert_eq!(call(&mut p, 511, [0; 6]), err(Errno::ENOSYS));
        assert_eq!(unimplemented_count(511), before + 1);
    }

    #[test]
    fn open_read_and_close_a_file() {
        let (mut p, _) = process();
        let fd = open(&mut p, b"/etc/motd", 0);
        assert_eq!(fd, 3, "0, 1, 2 are stdio");
        assert_eq!(call(&mut p, nr::READ, [fd, SCRATCH, 7, 0, 0, 0]), 7);
        assert_eq!(get(&mut p, SCRATCH, 7), b"Welcome");
        assert_eq!(call(&mut p, nr::READ, [fd, SCRATCH, 100, 0, 0, 0]), 11);
        assert_eq!(
            call(&mut p, nr::READ, [fd, SCRATCH, 100, 0, 0, 0]),
            0,
            "end of file"
        );
        assert_eq!(call(&mut p, nr::CLOSE, [fd, 0, 0, 0, 0, 0]), 0);
        assert_eq!(
            call(&mut p, nr::READ, [fd, SCRATCH, 1, 0, 0, 0]),
            err(Errno::EBADF)
        );
    }

    #[test]
    fn relative_paths_use_the_working_directory() {
        let (mut p, _) = process();
        assert_eq!(open(&mut p, b"etc/../etc//motd", 0), 3);
    }

    #[test]
    fn open_errors() {
        let (mut p, _) = process();
        assert_eq!(open(&mut p, b"/nope", 0), err(Errno::ENOENT));
        assert_eq!(
            open(&mut p, b"/etc/motd", 0o1),
            err(Errno::EROFS),
            "O_WRONLY"
        );
        assert_eq!(open(&mut p, b"/new", 0o101), err(Errno::EROFS), "O_CREAT");
        assert_eq!(open(&mut p, b"/etc/motd", 0o200000), err(Errno::ENOTDIR));
    }

    #[test]
    fn lseek_moves_the_file_position() {
        let (mut p, _) = process();
        let fd = open(&mut p, b"/etc/motd", 0);
        assert_eq!(call(&mut p, nr::LSEEK, [fd, 11, 0, 0, 0, 0]), 11);
        assert_eq!(call(&mut p, nr::LSEEK, [fd, -7i64 as u64, 2, 0, 0, 0]), 11);
        assert_eq!(call(&mut p, nr::READ, [fd, SCRATCH, 5, 0, 0, 0]), 5);
        assert_eq!(get(&mut p, SCRATCH, 5), b"Pi-OS");
        assert_eq!(call(&mut p, nr::LSEEK, [fd, 0, 1, 0, 0, 0]), 16);
        assert_eq!(
            call(&mut p, nr::LSEEK, [fd, -100i64 as u64, 1, 0, 0, 0]),
            err(Errno::EINVAL)
        );
        assert_eq!(
            call(&mut p, nr::LSEEK, [1, 0, 0, 0, 0, 0]),
            err(Errno::ESPIPE)
        );
    }

    #[test]
    fn fstat_and_newfstatat_report_type_and_size() {
        let (mut p, _) = process();
        let fd = open(&mut p, b"/etc/motd", 0);
        assert_eq!(call(&mut p, nr::FSTAT, [fd, SCRATCH, 0, 0, 0, 0]), 0);
        let st = get(&mut p, SCRATCH, 128);
        let mode = u32::from_le_bytes(st[16..20].try_into().unwrap());
        let size = u64::from_le_bytes(st[48..56].try_into().unwrap());
        assert_eq!(mode & 0o170000, 0o100000, "S_IFREG");
        assert_eq!(size, 18);
        put(&mut p, SCRATCH + 0x200, b"/etc\0");
        assert_eq!(
            call(
                &mut p,
                nr::NEWFSTATAT,
                [AT_FDCWD, SCRATCH + 0x200, SCRATCH, 0, 0, 0]
            ),
            0
        );
        let mode = u32::from_le_bytes(get(&mut p, SCRATCH + 16, 4).try_into().unwrap());
        assert_eq!(mode & 0o170000, 0o040000, "S_IFDIR");
        assert_eq!(call(&mut p, nr::FSTAT, [1, SCRATCH, 0, 0, 0, 0]), 0);
        let mode = u32::from_le_bytes(get(&mut p, SCRATCH + 16, 4).try_into().unwrap());
        assert_eq!(mode & 0o170000, 0o020000, "S_IFCHR");
    }

    #[test]
    fn writev_and_readv_handle_multiple_buffers() {
        let (mut p, out) = process();
        put(&mut p, SCRATCH, b"Hello, ");
        put(&mut p, SCRATCH + 0x100, b"world\n");
        let iov: Vec<u8> = [SCRATCH, 7, SCRATCH + 0x100, 6]
            .iter()
            .flat_map(|w| w.to_le_bytes())
            .collect();
        put(&mut p, SCRATCH + 0x200, &iov);
        assert_eq!(
            call(&mut p, nr::WRITEV, [1, SCRATCH + 0x200, 2, 0, 0, 0]),
            13
        );
        assert_eq!(&out.0.lock()[..], b"Hello, world\n");

        let fd = open(&mut p, b"/etc/motd", 0);
        let iov: Vec<u8> = [SCRATCH + 0x400, 3, SCRATCH + 0x500, 4]
            .iter()
            .flat_map(|w| w.to_le_bytes())
            .collect();
        put(&mut p, SCRATCH + 0x200, &iov);
        assert_eq!(
            call(&mut p, nr::READV, [fd, SCRATCH + 0x200, 2, 0, 0, 0]),
            7
        );
        assert_eq!(get(&mut p, SCRATCH + 0x400, 3), b"Wel");
        assert_eq!(get(&mut p, SCRATCH + 0x500, 4), b"come");
    }

    #[test]
    fn ioctl_winsize_answers_only_for_terminals() {
        let (mut p, _) = process();
        assert_eq!(
            call(&mut p, nr::IOCTL, [1, TIOCGWINSZ, SCRATCH, 0, 0, 0]),
            0
        );
        assert_eq!(get(&mut p, SCRATCH, 4), [24, 0, 80, 0]);
        let fd = open(&mut p, b"/etc/motd", 0);
        assert_eq!(
            call(&mut p, nr::IOCTL, [fd, TIOCGWINSZ, SCRATCH, 0, 0, 0]),
            err(Errno::ENOTTY)
        );
    }

    #[test]
    fn getcwd_returns_the_working_directory() {
        let (mut p, _) = process();
        assert_eq!(call(&mut p, nr::GETCWD, [SCRATCH, 64, 0, 0, 0, 0]), 2);
        assert_eq!(get(&mut p, SCRATCH, 2), b"/\0");
        assert_eq!(
            call(&mut p, nr::GETCWD, [SCRATCH, 1, 0, 0, 0, 0]),
            err(Errno::ERANGE)
        );
    }

    #[test]
    fn dev_null_discards_writes_and_reads_nothing() {
        let (mut p, _) = process();
        let fd = open(&mut p, b"/dev/null", 2);
        assert_eq!(call(&mut p, nr::WRITE, [fd, SCRATCH, 10, 0, 0, 0]), 10);
        assert_eq!(call(&mut p, nr::READ, [fd, SCRATCH, 10, 0, 0, 0]), 0);
    }
}
