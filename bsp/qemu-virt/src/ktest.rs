//! カーネル内テストの枠組み(`#[test_case]` を集めて順に実行する)。
//!
//! 結果はセミホスティングで出力し、QEMU の終了コードで返す。成功は 0、失敗は 1。

use arch_aarch64::semihosting::{self, SemihostingConsole};
use core::fmt::Write;
use hal::console::Writer;

pub trait Testable {
    fn run(&self);
}

impl<T: Fn()> Testable for T {
    fn run(&self) {
        let mut out = Writer(&mut SemihostingConsole);
        let _ = write!(out, "test {} ... ", core::any::type_name::<T>());
        self();
        let _ = writeln!(out, "ok");
    }
}

pub fn runner(tests: &[&dyn Testable]) {
    let _ = writeln!(
        Writer(&mut SemihostingConsole),
        "running {} tests",
        tests.len()
    );
    for test in tests {
        test.run();
    }
    let _ = writeln!(Writer(&mut SemihostingConsole), "test result: ok");
    semihosting::exit(0);
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    let _ = writeln!(Writer(&mut SemihostingConsole), "FAILED\n{info}");
    semihosting::exit(1)
}
