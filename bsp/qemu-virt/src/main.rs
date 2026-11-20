//! QEMU `-M virt` 用のカーネルイメージ。
//!
//! BSP は「どのドライバをどのアドレスで使うか」の配線だけを持つ。
#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(crate::ktest::runner)]
#![reexport_test_harness_main = "test_main"]

#[cfg(test)]
mod ktest;

use hal::mmio::MmioRegion;
use kernel::sync::SpinLock;
use pl011::Pl011;

/// `virt` の PL011 のアドレスとクロック(QEMU の `hw/arm/virt.c` より)
const UART_BASE: usize = 0x0900_0000;
const UART_CLOCK_HZ: u32 = 24_000_000;

// SAFETY: UART_BASE は virt の PL011。この static 以外から同じ領域を触らない
static UART: SpinLock<Pl011<MmioRegion>> =
    SpinLock::new(Pl011::new(unsafe { MmioRegion::new(UART_BASE) }));

/// `arch_aarch64::boot` の `_start` から呼ばれる。`dtb` はデバイスツリーのアドレス。
#[unsafe(no_mangle)]
extern "C" fn kernel_main(_dtb: usize) -> ! {
    UART.lock().init(UART_CLOCK_HZ, 115_200);
    kernel::console::set_console(&UART);

    #[cfg(test)]
    test_main();

    kernel::kmain("qemu-virt");
    arch_aarch64::semihosting::exit(0)
}

#[cfg(not(test))]
#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    arch_aarch64::semihosting::exit(1)
}

#[cfg(test)]
mod tests {
    use arch_aarch64::semihosting::SemihostingConsole;
    use core::fmt::Write;
    use hal::console::{Console, Writer};

    #[test_case]
    fn arithmetic_works_in_kernel() {
        assert_eq!(1 + 1, 2);
    }

    #[test_case]
    fn semihosting_console_accepts_bytes_and_format() {
        let mut con = SemihostingConsole;
        con.write_bytes(b"");
        write!(Writer(&mut con), "").unwrap();
    }
}
