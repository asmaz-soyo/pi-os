//! カーネルのコア。ハードウェアの知識は持たず、`hal` のトレイトにだけ依存する。
#![no_std]

extern crate alloc;

#[cfg(test)]
extern crate std;

pub mod block;
pub mod console;
pub mod cred;
pub mod errno;
pub mod exec;
pub mod fs;
pub mod gpio;
pub mod init;
pub mod irq;
pub mod mm;
pub mod net;
pub mod panic;
pub mod process;
pub mod sched;
pub mod sync;
pub mod syscall;
pub mod time;
pub mod tty;
pub mod wait;

pub use errno::Errno;

use core::fmt::Write;
use hal::console::{Console, Writer};

/// 起動メッセージを出す。
pub fn print_banner(out: &mut impl Console, board: &str) {
    let mut w = Writer(out);
    let _ = writeln!(w, "Pi-OS booting on {board}");
    let _ = writeln!(w, "Hello from Pi-OS");
}

/// BSP の初期化が終わったあとに呼ばれる、カーネルの入口。
/// 出力は [`console::set_console`] で登録された先に出る。
pub fn kmain(board: &str) {
    print_banner(&mut console::Global, board);
    let hz = time::CLOCK.hz();
    if hz > 0 {
        time::wait_ticks(10);
        println!("timer: 10 ticks at {hz} Hz");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hal::mock::MockConsole;

    #[test]
    fn boot_banner_names_the_board_and_says_hello() {
        let mut con = MockConsole::new();
        print_banner(&mut con, "qemu-virt");
        assert_eq!(
            con.output(),
            b"Pi-OS booting on qemu-virt\nHello from Pi-OS\n"
        );
    }
}
