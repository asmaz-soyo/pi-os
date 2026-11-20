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
        fn brk_traps_to_handler_which_can_edit_registers_and_resume() {
            use arch_aarch64::esr::ExceptionClass;
            use arch_aarch64::exception::{self, Handler};
            use arch_aarch64::trap::TrapFrame;
            use core::sync::atomic::{AtomicUsize, Ordering};

            static HITS: AtomicUsize = AtomicUsize::new(0);
            struct Test;
            impl Handler for Test {
                fn synchronous(&self, frame: &mut TrapFrame) {
                    assert_eq!(ExceptionClass::from_esr(frame.esr), ExceptionClass::Brk);
                    assert_eq!(frame.x[0], 5);
                    frame.x[0] = 99;
                    frame.elr += 4; // brk の次の命令から再開する
                    HITS.fetch_add(1, Ordering::SeqCst);
                }
                fn irq(&self, _frame: &mut TrapFrame) {}
            }
            static TEST: Test = Test;

            let previous = exception::set_handler(Some(&TEST));
            let mut x0: u64 = 5;
            // SAFETY: brk は例外を起こすだけ。ハンドラが次の命令に戻す
            unsafe { core::arch::asm!("brk #42", inout("x0") x0) };
            exception::set_handler(previous);

            assert_eq!(HITS.load(Ordering::SeqCst), 1);
            assert_eq!(x0, 99);
        }

        #[test_case]
        fn fp_simd_is_usable() {
            let x = core::hint::black_box(1.5f64);
            assert_eq!(x * 2.0, 3.0);
        }
    };
}
