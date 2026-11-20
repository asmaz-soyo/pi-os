//! カーネル内テストの枠組み(QEMU の結合テスト)。
//!
//! BSP のクレートが `custom_test_frameworks` で `#[test_case]` を集め、[`runner`] に渡す。
//! 結果はセミホスティングで出力し、QEMU の終了コードで返す。成功は 0、失敗は 1。
#![no_std]

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

/// テスト中のパニック。BSP の `#[panic_handler]` から呼ぶ。
pub fn panic(info: &core::panic::PanicInfo) -> ! {
    let _ = writeln!(Writer(&mut SemihostingConsole), "FAILED\n{info}");
    semihosting::exit(1)
}

/// どの BSP でも成り立つべきテスト。BSP のテストモジュールの中で `ktest::common_tests!();` と書く。
#[macro_export]
macro_rules! common_tests {
    () => {
        #[test_case]
        fn kernel_runs_at_el1() {
            assert_eq!(arch_aarch64::cpu::current_el(), 1);
        }

        #[test_case]
        fn fp_simd_is_usable() {
            let x = core::hint::black_box(1.5f64);
            assert_eq!(x * 2.0, 3.0);
        }
    };
}
