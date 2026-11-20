//! QEMU `-M virt` 用のカーネルイメージ。
//!
//! BSP は「どのドライバをどのアドレスで使うか」の配線だけを持つ。
#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(ktest::runner)]
#![reexport_test_harness_main = "test_main"]

use arch_aarch64::timer::GenericTimer;
use gicv2::GicV2;
use hal::irq::IrqController;
use hal::mmio::MmioRegion;
use kernel::sync::SpinLock;
use pl011::Pl011;

/// `virt` の PL011 のアドレスとクロック(QEMU の `hw/arm/virt.c` より)
const UART_BASE: usize = 0x0900_0000;
const UART_CLOCK_HZ: u32 = 24_000_000;

// SAFETY: UART_BASE は virt の PL011。この static 以外から同じ領域を触らない
static UART: SpinLock<Pl011<MmioRegion>> =
    SpinLock::new(Pl011::new(unsafe { MmioRegion::new(UART_BASE) }));

/// GICv2(`hw/arm/virt.c` の VIRT_GIC_DIST / VIRT_GIC_CPU)
const GICD_BASE: usize = 0x0800_0000;
const GICC_BASE: usize = 0x0801_0000;
/// EL1 物理タイマの割り込み(PPI 14 = INTID 30)
const TIMER_IRQ: u32 = 30;
const TIMER_HZ: u64 = 100;

// SAFETY: GICD_BASE / GICC_BASE は virt の GIC。この static 以外から触らない
static INTC: SpinLock<GicV2<MmioRegion>> =
    SpinLock::new(GicV2::new(unsafe { MmioRegion::new(GICD_BASE) }, unsafe {
        MmioRegion::new(GICC_BASE)
    }));
const INTC_INIT: fn(&mut GicV2<MmioRegion>) = GicV2::init;

/// 例外の受け口。IRQ は割り込みコントローラから番号を読んでカーネルの表で振り分ける。
struct Exceptions;

impl arch_aarch64::exception::Handler for Exceptions {
    fn synchronous(&self, frame: &mut arch_aarch64::trap::TrapFrame) {
        arch_aarch64::exception::unhandled(arch_aarch64::exception::KIND_SYNC_CURRENT_EL, frame)
    }

    fn irq(&self, _frame: &mut arch_aarch64::trap::TrapFrame) {
        kernel::irq::IRQS.dispatch(&mut *INTC.lock());
    }
}

static EXCEPTIONS: Exceptions = Exceptions;

fn timer_tick() {
    kernel::time::CLOCK.tick(&mut GenericTimer);
}

/// 割り込みコントローラとタイマを準備し、IRQ を受け付け始める。
fn start_timer() {
    arch_aarch64::exception::set_handler(Some(&EXCEPTIONS));
    let mut intc = INTC.lock();
    INTC_INIT(&mut intc);
    kernel::irq::IRQS
        .register(TIMER_IRQ, timer_tick)
        .expect("timer irq number is valid");
    intc.enable(TIMER_IRQ);
    drop(intc);
    kernel::time::CLOCK.start(&mut GenericTimer, TIMER_HZ);
    arch_aarch64::cpu::enable_irq();
}

/// `arch_aarch64::boot` の `_start` から呼ばれる。`dtb` はデバイスツリーのアドレス。
#[unsafe(no_mangle)]
#[cfg_attr(feature = "panic-demo", allow(unreachable_code))]
extern "C" fn kernel_main(_dtb: usize) -> ! {
    arch_aarch64::exception::init();
    UART.lock().init(UART_CLOCK_HZ, 115_200);
    kernel::console::set_console(&UART);
    start_timer();

    #[cfg(test)]
    test_main();

    kernel::kmain("qemu-virt");
    #[cfg(feature = "panic-demo")]
    panic!("panic demo");
    #[cfg(feature = "fault-demo")]
    // SAFETY: 未定義命令で例外を起こすだけ(未処理なのでパニックになる)
    unsafe {
        core::arch::asm!("udf #0")
    };
    arch_aarch64::semihosting::exit(0)
}

#[cfg(not(test))]
#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    let r = arch_aarch64::cpu::snapshot();
    let regs = kernel::panic::Registers {
        current_el: r.current_el,
        sp: r.sp,
        fp: r.fp,
        lr: r.lr,
    };
    kernel::panic::report(info, &regs, arch_aarch64::boot::stack_range());
    // QEMU なら終了コード 1 で終わる。実機ではここで止まる
    arch_aarch64::semihosting::exit(1)
}

#[cfg(test)]
#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    ktest::panic(info)
}

#[cfg(test)]
mod tests {
    ktest::common_tests!();
}
