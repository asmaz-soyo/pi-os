//! システムコール(Linux AArch64 互換)。
//!
//! ユーザーは `svc #0` で番号を x8、引数を x0〜x5 に入れて呼ぶ。戻り値は x0(エラーは負の errno)。
//! 番号ごとのハンドラは `fs.rs`(ファイル)、`proc.rs`(プロセス)などに分けて置く。
//! 未実装の番号は `ENOSYS` を返し、最初の1回だけログに出す(次に何を作るべきかが分かる)。

mod fs;
mod misc;
mod mm;
mod proc;

use crate::process::Process;
use core::sync::atomic::{AtomicU32, Ordering};

/// システムコールの番号(Linux AArch64、`include/uapi/asm-generic/unistd.h`)。
pub mod nr {
    pub const GETCWD: u64 = 17;
    pub const IOCTL: u64 = 29;
    pub const OPENAT: u64 = 56;
    pub const CLOSE: u64 = 57;
    pub const LSEEK: u64 = 62;
    pub const READ: u64 = 63;
    pub const WRITE: u64 = 64;
    pub const READV: u64 = 65;
    pub const WRITEV: u64 = 66;
    pub const NEWFSTATAT: u64 = 79;
    pub const FSTAT: u64 = 80;
    pub const EXIT: u64 = 93;
    pub const EXIT_GROUP: u64 = 94;
    pub const SET_TID_ADDRESS: u64 = 96;
    pub const CLOCK_GETTIME: u64 = 113;
    pub const SCHED_GETAFFINITY: u64 = 123;
    pub const RT_SIGACTION: u64 = 134;
    pub const RT_SIGPROCMASK: u64 = 135;
    pub const UNAME: u64 = 160;
    pub const GETPID: u64 = 172;
    pub const GETUID: u64 = 174;
    pub const GETEUID: u64 = 175;
    pub const GETGID: u64 = 176;
    pub const GETEGID: u64 = 177;
    pub const GETTID: u64 = 178;
    pub const BRK: u64 = 214;
    pub const MUNMAP: u64 = 215;
    pub const MREMAP: u64 = 216;
    pub const MMAP: u64 = 222;
    pub const MPROTECT: u64 = 226;
    pub const MADVISE: u64 = 233;
    pub const GETRANDOM: u64 = 278;
}

/// ioctl:端末の大きさを問い合わせる
pub const TIOCGWINSZ: u64 = 0x5413;

/// システムコールのあとにすること。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// x0 にこの値を入れてユーザーに戻る
    Return(u64),
    /// プロセスを終了する(終了ステータス)
    Exit(i32),
}

/// 番号ごとの「未実装で呼ばれた回数」
const TRACKED: usize = 512;
static UNIMPLEMENTED: [AtomicU32; TRACKED] = [const { AtomicU32::new(0) }; TRACKED];

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

/// システムコールを1つ処理する。
pub fn dispatch(p: &mut Process, nr: u64, a: [u64; 6]) -> Action {
    match nr {
        nr::GETCWD => result(fs::getcwd(p, a[0], a[1])),
        nr::IOCTL => result(fs::ioctl(p, a[0], a[1], a[2])),
        nr::OPENAT => result(fs::openat(p, a[0], a[1], a[2])),
        nr::CLOSE => result(p.fds.close(a[0]).map(|_| 0)),
        nr::LSEEK => result(fs::lseek(p, a[0], a[1] as i64, a[2])),
        nr::READ => result(fs::read(p, a[0], a[1], a[2])),
        nr::WRITE => result(fs::write(p, a[0], a[1], a[2])),
        nr::READV => result(fs::readv(p, a[0], a[1], a[2])),
        nr::WRITEV => result(fs::writev(p, a[0], a[1], a[2])),
        nr::NEWFSTATAT => result(fs::newfstatat(p, a[0], a[1], a[2], a[3])),
        nr::FSTAT => result(fs::fstat(p, a[0], a[1])),
        nr::EXIT | nr::EXIT_GROUP => Action::Exit(proc::exit_status(a[0])),
        // フェーズ1はプロセスもスレッドも1つ、ユーザーは root だけ
        nr::SET_TID_ADDRESS | nr::GETPID | nr::GETTID => Action::Return(1),
        nr::GETUID | nr::GETEUID | nr::GETGID | nr::GETEGID => Action::Return(0),
        // シグナルはまだ配送しないので、設定は受け付けるだけ(入れ物は後で作る)
        nr::RT_SIGACTION | nr::RT_SIGPROCMASK => Action::Return(0),
        nr::CLOCK_GETTIME => result(misc::clock_gettime(p, a[0], a[1])),
        nr::SCHED_GETAFFINITY => result(misc::sched_getaffinity(p, a[1], a[2])),
        nr::UNAME => result(misc::uname(p, a[0])),
        nr::GETRANDOM => result(misc::getrandom(p, a[0], a[1])),
        nr::BRK => Action::Return(mm::brk(p, a[0])),
        nr::MMAP => result(mm::mmap(p, a[0], a[1], a[2], a[3], a[4], a[5])),
        nr::MUNMAP => result(mm::munmap(p, a[0], a[1])),
        nr::MPROTECT => result(mm::mprotect(p, a[0], a[1], a[2])),
        // 助言は無視してよい。mremap を断ると、libc は「新しく確保してコピー」に切り替える
        nr::MADVISE => Action::Return(0),
        nr::MREMAP => Action::Return(crate::Errno::ENOMEM.as_syscall_return()),
        _ => {
            if let Some(count) = UNIMPLEMENTED.get(nr as usize)
                && count.fetch_add(1, Ordering::Relaxed) == 0
            {
                crate::println!("[pi-os] unimplemented syscall {nr}");
            }
            Action::Return(crate::Errno::ENOSYS.as_syscall_return())
        }
    }
}

#[cfg(test)]
mod tests_mm;

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
    pub struct Capture(pub Arc<SpinLock<Vec<u8>>>);

    impl Console for Capture {
        fn write_bytes(&mut self, buf: &[u8]) {
            self.0.lock().extend_from_slice(buf);
        }
    }

    /// ユーザーの作業領域(読み書き可)
    pub const SCRATCH: u64 = 0x40_0000;

    pub fn process() -> (Process, Capture) {
        let (mut mem, _) = memory();
        mem.map_zeroed(VirtAddr(SCRATCH), 0x4000, MapFlags::USER_RW)
            .unwrap();
        let out = Capture::default();
        let tar = build(&[
            ("etc/motd", b'0', b"Welcome to Pi-OS!\n"),
            ("bin/x", b'0', b"ELF"),
        ]);
        let root = Initramfs::new(Box::leak(tar.into_boxed_slice())).unwrap();
        let console = Arc::new(ConsoleFile::new(Box::new(out.clone())));
        let mut vfs = Vfs::new(Arc::new(root));
        vfs.mount("/dev", Arc::new(Devfs::new(console.clone())));
        let loaded = Loaded {
            entry: SCRATCH,
            sp: SCRATCH + 0x1000,
            brk: 0x50_0000,
        };
        (Process::new(mem, loaded, Arc::new(vfs), console), out)
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
            Action::Exit(s) => panic!("unexpected exit {s}"),
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
