//! システムコール(Linux AArch64 互換)。
//!
//! ユーザーは `svc #0` で番号を x8、引数を x0〜x5 に入れて呼ぶ。戻り値は x0(エラーは負の errno)。
//! 番号ごとのハンドラは `fs.rs`(ファイル)、`proc.rs`(プロセス)などに分けて置く。
//! 未実装の番号は `ENOSYS` を返し、最初の1回だけログに出す(次に何を作るべきかが分かる)。

mod fs;
mod proc;

use crate::process::Process;
use core::sync::atomic::{AtomicU32, Ordering};

/// システムコールの番号(Linux AArch64、`include/uapi/asm-generic/unistd.h`)。
pub mod nr {
    pub const WRITE: u64 = 64;
    pub const EXIT: u64 = 93;
    pub const EXIT_GROUP: u64 = 94;
}

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
        nr::WRITE => result(fs::write(p, a[0], a[1], a[2])),
        nr::EXIT | nr::EXIT_GROUP => Action::Exit(proc::exit_status(a[0])),
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
pub(crate) mod tests {
    use super::*;
    use crate::Errno;
    use crate::exec::Loaded;
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

    pub fn process() -> (Process, Capture) {
        let (mut mem, _) = memory();
        mem.map_zeroed(VirtAddr(0x40_0000), 0x1000, MapFlags::USER_RW)
            .unwrap();
        let out = Capture::default();
        let loaded = Loaded {
            entry: 0x40_0000,
            sp: 0x40_1000,
            brk: 0x50_0000,
        };
        (Process::new(mem, loaded, Box::new(out.clone())), out)
    }

    fn ret(errno: Errno) -> Action {
        Action::Return(errno.as_syscall_return())
    }

    #[test]
    fn write_to_stdout_copies_user_bytes_to_the_console() {
        let (mut p, out) = process();
        p.mem.copy_to_user(VirtAddr(0x40_0000), b"hello\n").unwrap();
        let r = dispatch(&mut p, nr::WRITE, [1, 0x40_0000, 6, 0, 0, 0]);
        assert_eq!(r, Action::Return(6));
        assert_eq!(&out.0.lock()[..], b"hello\n");
    }

    #[test]
    fn write_with_a_bad_pointer_is_efault() {
        let (mut p, out) = process();
        let r = dispatch(&mut p, nr::WRITE, [1, 0xdead_0000, 4, 0, 0, 0]);
        assert_eq!(r, ret(Errno::EFAULT));
        assert!(out.0.lock().is_empty());
    }

    #[test]
    fn write_to_an_unknown_fd_is_ebadf() {
        let (mut p, _) = process();
        assert_eq!(
            dispatch(&mut p, nr::WRITE, [7, 0x40_0000, 1, 0, 0, 0]),
            ret(Errno::EBADF)
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
        assert_eq!(dispatch(&mut p, 511, [0; 6]), ret(Errno::ENOSYS));
        assert_eq!(unimplemented_count(511), before + 1);
    }
}
