//! カーネルのコア。ハードウェアの知識は持たず、`hal` のトレイトにだけ依存する。
#![no_std]

#[cfg(test)]
extern crate std;

pub mod console;
pub mod errno;
pub mod irq;
pub mod panic;
pub mod sync;
pub mod time;

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
