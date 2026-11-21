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

            // 差し替えている間に来たタイマ割り込みを Test が無視すると、割り込みが下がらず
            // 入り直し続けて先に進めない。IRQ を止めておく(brk は同期例外なので届く)
            arch_aarch64::cpu::disable_irq();
            let previous = exception::set_handler(Some(&TEST));
            let mut x0: u64 = 5;
            // SAFETY: brk は例外を起こすだけ。ハンドラが次の命令に戻す
            unsafe { core::arch::asm!("brk #42", inout("x0") x0) };
            exception::set_handler(previous);
            arch_aarch64::cpu::enable_irq();

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
        fn heap_supports_box_vec_and_string_and_returns_memory() {
            extern crate alloc;
            let before = kernel::mm::HEAP.free_bytes();
            assert!(before > 0, "heap is initialized");
            {
                let b = alloc::boxed::Box::new(41u64);
                let v: alloc::vec::Vec<u32> = (0..1000).collect();
                let s = alloc::format!("{}-{}", *b + 1, v.len());
                assert_eq!(s, "42-1000");
            }
            assert_eq!(kernel::mm::HEAP.free_bytes(), before);
        }

        #[test_case]
        fn user_program_runs_at_el0_and_returns_its_exit_status() {
            assert_eq!(boot::run_program("/bin/exit42", &[b"exit42"]), Ok(42));
        }

        #[test_case]
        fn bad_pointer_in_a_syscall_is_efault_not_a_crash() {
            assert_eq!(boot::run_program("/bin/efault", &[b"efault"]), Ok(14));
        }

        #[test_case]
        fn user_fault_ends_only_the_process() {
            assert_eq!(boot::run_program("/bin/fault", &[b"fault"]), Ok(boot::STATUS_SEGV));
        }

        #[test_case]
        fn running_a_process_returns_all_its_frames() {
            // 終わったタスクのカーネルスタックは、次にスケジューラが動いたときに返す。
            // ほかのタスク(後ろで書き出すスレッドなど)がいつ動くかで返す時期がずれないよう、
            // 測る前にスケジューラを一度通して、返し待ちを片付ける
            boot::run_program("/bin/exit42", &[b"exit42"]).unwrap();
            kernel::sched::relax();
            let before = kernel::mm::frame_stats().0;
            boot::run_program("/bin/exit42", &[b"exit42"]).unwrap();
            kernel::sched::relax();
            assert_eq!(kernel::mm::frame_stats().0, before);
        }

        #[test_case]
        fn missing_program_is_enoent() {
            assert_eq!(boot::run_program("/bin/nope", &[b"nope"]), Err(kernel::Errno::ENOENT));
        }

        #[test_case]
        fn quickjs_evaluates_javascript() {
            let argv: [&[u8]; 4] = [b"qjs", b"--std", b"-e", b"std.exit(6 * 7)"];
            assert_eq!(boot::run_program("/bin/qjs", &argv), Ok(42));
        }

        #[test_case]
        fn files_without_exec_permission_cannot_be_run() {
            assert_eq!(
                boot::run_program("/etc/motd", &[b"motd"]),
                Err(kernel::Errno::EACCES)
            );
        }

        #[test_case]
        fn kernel_threads_take_turns_with_yield() {
            use core::sync::atomic::{AtomicUsize, Ordering};
            static LOG: kernel::sync::SpinLock<[u8; 8]> = kernel::sync::SpinLock::new([0; 8]);
            static LEN: AtomicUsize = AtomicUsize::new(0);
            fn worker(id: usize) {
                for _ in 0..3 {
                    let n = LEN.fetch_add(1, Ordering::SeqCst);
                    LOG.lock()[n] = b'A' + id as u8;
                    kernel::sched::yield_now();
                }
            }
            let before = kernel::sched::task_count();
            kernel::sched::spawn(worker, 0).unwrap();
            kernel::sched::spawn(worker, 1).unwrap();
            while LEN.load(Ordering::SeqCst) < 6 || kernel::sched::task_count() > before {
                kernel::sched::yield_now();
            }
            assert_eq!(&LOG.lock()[..6], b"ABABAB");
        }

        #[test_case]
        fn spinning_user_programs_are_preempted_by_the_timer() {
            // 終わらないプログラムを 2 つ動かしても、このタスク(カーネル)が動き続け、
            // ほかのプログラムも最後まで動く
            let a = boot::spawn_program("/bin/spin", &[b"spin"]).unwrap();
            let b = boot::spawn_program("/bin/spin", &[b"spin"]).unwrap();
            assert_eq!(boot::run_program("/bin/exit42", &[b"exit42"]), Ok(42));
            boot::kill(a, 9);
            boot::kill(b, 9);
            assert_eq!(boot::wait(a), 128 + 9);
            assert_eq!(boot::wait(b), 128 + 9);
        }

        #[test_case]
        fn fork_exec_wait_and_shebang_work_from_c() {
            assert_eq!(boot::run_program("/bin/forktest", &[b"forktest"]), Ok(0));
        }

        #[test_case]
        fn each_process_keeps_its_own_thread_pointer() {
            let a = boot::spawn_program("/bin/tls", &[b"tls"]).unwrap();
            let b = boot::spawn_program("/bin/tls", &[b"tls", b"x"]).unwrap();
            assert_eq!(boot::wait(a), 0);
            assert_eq!(boot::wait(b), 0);
        }

        #[test_case]
        fn each_process_keeps_its_own_fp_registers() {
            let a = boot::spawn_program("/bin/fpregs", &[b"fpregs"]).unwrap();
            let b = boot::spawn_program("/bin/fpregs", &[b"fpregs", b"x"]).unwrap();
            assert_eq!(boot::wait(a), 0);
            assert_eq!(boot::wait(b), 0);
        }

        #[test_case]
        fn tmpfs_supports_files_and_directories_from_c() {
            assert_eq!(boot::run_program("/bin/tmpfiles", &[b"tmpfiles"]), Ok(0));
        }

        #[test_case]
        fn timerfd_and_epoll_wake_a_program_periodically() {
            assert_eq!(boot::run_program("/bin/periodic", &[b"periodic"]), Ok(0));
        }

        #[test_case]
        fn busybox_applets_run() {
            let argv: [&[u8]; 3] = [b"busybox", b"echo", b"hello from busybox"];
            assert_eq!(boot::run_program("/bin/busybox", &argv), Ok(0));
            // ash で複数のコマンドと算術
            let argv: [&[u8]; 4] = [b"busybox", b"sh", b"-c", b"x=$((6*7)); test $x = 42 && ls /etc"];
            assert_eq!(boot::run_program("/bin/busybox", &argv), Ok(0));
        }

        #[test_case]
        fn busybox_applets_are_reachable_through_symlinks() {
            // /bin/seq → busybox:argv[0] の名前でアプレットが選ばれる
            assert_eq!(boot::run_program("/bin/seq", &[b"seq", b"3"]), Ok(0));
            assert_eq!(boot::run_program("/bin/true", &[b"true"]), Ok(0));
            assert_eq!(boot::run_program("/bin/false", &[b"false"]), Ok(1));
        }

        #[test_case]
        fn busybox_shell_pipes_and_command_substitution_work() {
            let script = b"n=$(seq 1 100 | grep 7 | wc -l); echo \"lines with 7: $n\"; test $n -eq 19";
            let argv: [&[u8]; 4] = [b"busybox", b"sh", b"-c", script];
            assert_eq!(boot::run_program("/bin/busybox", &argv), Ok(0));
        }

        #[test_case]
        fn two_processes_talk_over_tcp_on_loopback() {
            // サーバー(nc -l)を後ろで動かし、クライアントがつながるまで試し直す
            // (シェルの wait は rt_sigsuspend を使うのでまだ使えない。受け取ったファイルができるまで待つ)
            let script = b"nc -l -p 7300 < /dev/null > /tmp/got & \
                for i in $(seq 20); do echo hello over tcp | nc 127.0.0.1 7300 && break; done; \
                for i in $(seq 1000); do test -s /tmp/got && break; done; \
                test \"$(cat /tmp/got)\" = \"hello over tcp\"";
            let argv: [&[u8]; 4] = [b"busybox", b"sh", b"-c", script];
            assert_eq!(boot::run_program("/bin/busybox", &argv), Ok(0));
        }

        #[test_case]
        fn dynamically_linked_programs_run_through_ld_musl() {
            assert_eq!(boot::run_program("/bin/dynhello", &[b"dynhello"]), Ok(0));
        }

        #[test_case]
        fn micropython_runs_code_and_scripts() {
            let argv: [&[u8]; 3] = [
                b"micropython",
                b"-c",
                b"import sys; assert sum(range(10)) == 45; print('Hello from MicroPython')",
            ];
            assert_eq!(boot::run_program("/bin/micropython", &argv), Ok(0));
            // ファイルの読み書き、os.listdir、sys.exit(3)
            let argv: [&[u8]; 2] = [b"micropython", b"/hello.py"];
            assert_eq!(boot::run_program("/bin/micropython", &argv), Ok(3));
        }

        #[test_case]
        fn a_realtime_task_wakes_on_time_despite_a_busy_task() {
            // 5ms 周期で起き、遅れの最大がティックの半分(5ms)以内(低い優先度のタスクが CPU を
            // 回していても)。目覚ましがなく、ティック(10ms)でしか起こせなければ、遅れはほぼ 10ms になる。
            // 手元の QEMU ではふつう 0.3ms 以下だが、GitHub Actions の共有マシンでは 1.7ms になったことがある
            let argv: [&[u8]; 3] = [b"jitter", b"rt", b"5000"];
            assert_eq!(boot::run_program("/bin/jitter", &argv), Ok(0));
        }

        #[test_case]
        fn service_manager_runs_startup_periodic_and_restarting_services() {
            let argv: [&[u8]; 2] = [b"svcman", b"/etc/svcman-test.conf"];
            // startup 1 回 + 周期 3 回 + 失敗して起動し直し 3 回 = 7
            assert_eq!(boot::run_program("/bin/svcman", &argv), Ok(7));
        }

        #[test_case]
        fn procfs_shows_the_process_and_memory() {
            assert_eq!(boot::run_program("/bin/procread", &[b"procread"]), Ok(0));
        }

        #[test_case]
        fn fp_simd_is_usable() {
            let x = core::hint::black_box(1.5f64);
            assert_eq!(x * 2.0, 3.0);
        }
    };
}
