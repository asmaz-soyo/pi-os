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
        fn timer_interrupts_advance_the_tick_count() {
            let start = kernel::time::CLOCK.ticks();
            kernel::time::wait_ticks(3);
            assert!(kernel::time::CLOCK.ticks() >= start + 3);
            assert_eq!(kernel::irq::IRQS.unhandled(), 0);
        }

        #[test_case]
        fn kernel_runs_in_the_higher_half() {
            let pc: u64;
            // SAFETY: 今の命令のアドレスを読むだけ
            unsafe { core::arch::asm!("adr {}, .", out(reg) pc) };
            assert!(pc >= arch_aarch64::mmu::KERNEL_VBASE, "pc = {pc:#x}");
        }

        #[test_case]
        fn mmu_maps_ram_linearly_and_leaves_low_addresses_unmapped() {
            use arch_aarch64::mmu::{phys_to_virt, virt_to_phys};
            use hal::addr::VirtAddr;
            let image = arch_aarch64::boot::kernel_image_phys();
            assert!(arch_aarch64::cpu::mmu_enabled());
            // カーネルの先頭は線形マップの位置に見える
            assert_eq!(
                arch_aarch64::cpu::translate(phys_to_virt(image.start)),
                Some(image.start)
            );
            // 恒等マップ(低いアドレス)は消えている。ユーザー空間のために空けてある
            assert_eq!(arch_aarch64::cpu::translate(VirtAddr(image.start.0)), None);
            assert_eq!(virt_to_phys(phys_to_virt(image.start)), image.start);
        }

        #[test_case]
        fn frame_allocator_hands_out_usable_ram() {
            let frame = kernel::mm::alloc_frame().expect("free frame");
            let ptr = arch_aarch64::mmu::phys_to_virt(frame).0 as *mut u64;
            // SAFETY: 割り当てたばかりのフレームで、線形マップに見えている
            unsafe {
                ptr.write_volatile(0xdead_beef);
                assert_eq!(ptr.read_volatile(), 0xdead_beef);
            }
            kernel::mm::release_frame(frame).unwrap();
        }

        #[test_case]
        fn fp_simd_is_usable() {
            let x = core::hint::black_box(1.5f64);
            assert_eq!(x * 2.0, 3.0);
        }
    };
}
