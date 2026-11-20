//! Raspberry Pi 3B 用のカーネルイメージ(QEMU `-M raspi3b` と実機で共通)。
//!
//! BSP は「どのドライバをどのアドレスで使うか」の配線だけを持つ。
#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(ktest::runner)]
#![reexport_test_harness_main = "test_main"]

use hal::mmio::MmioRegion;
use kernel::sync::SpinLock;
use pl011::Pl011;

/// BCM2837 の UART0(PL011)。ペリフェラルはバスアドレス 0x7E00_0000 が物理 0x3F00_0000 に見える。
const UART_BASE: usize = 0x3F20_1000;
/// UART のクロック。ファームウェアの既定値(config.txt の init_uart_clock)
const UART_CLOCK_HZ: u32 = 48_000_000;

// SAFETY: UART_BASE は BCM2837 の PL011。この static 以外から同じ領域を触らない
static UART: SpinLock<Pl011<MmioRegion>> =
    SpinLock::new(Pl011::new(unsafe { MmioRegion::new(UART_BASE) }));

/// `arch_aarch64::boot` の `_start` から呼ばれる。`dtb` はデバイスツリーのアドレス。
#[unsafe(no_mangle)]
extern "C" fn kernel_main(_dtb: usize) -> ! {
    UART.lock().init(UART_CLOCK_HZ, 115_200);
    kernel::console::set_console(&UART);

    #[cfg(test)]
    test_main();

    kernel::kmain("raspi3");
    #[cfg(feature = "panic-demo")]
    panic!("panic demo");
    #[cfg(not(feature = "panic-demo"))]
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
