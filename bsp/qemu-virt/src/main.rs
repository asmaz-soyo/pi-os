//! QEMU `-M virt` 用のカーネルイメージ。
//!
//! BSP は「どのドライバをどのアドレスで使うか」の配線だけを持つ。
//! 初期化の順番などの共通の処理は `boot` クレートにある。
#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(ktest::runner)]
#![reexport_test_harness_main = "test_main"]

use boot::device_va;
use core::ops::Range;
use gicv2::GicV2;
use hal::mmio::MmioRegion;
use kernel::sync::SpinLock;
use pl011::Pl011;

// アドレスは QEMU の hw/arm/virt.c より
const UART_PHYS: usize = 0x0900_0000;
const UART_CLOCK_HZ: u32 = 24_000_000;
const GICD_PHYS: usize = 0x0800_0000;
const GICC_PHYS: usize = 0x0801_0000;

// SAFETY: 各アドレスは virt のデバイス。この static 以外から同じ領域を触らない
static UART: SpinLock<Pl011<MmioRegion>> =
    SpinLock::new(Pl011::new(unsafe { MmioRegion::new(device_va(UART_PHYS)) }));
static INTC: SpinLock<GicV2<MmioRegion>> = SpinLock::new(GicV2::new(
    unsafe { MmioRegion::new(device_va(GICD_PHYS)) },
    unsafe { MmioRegion::new(device_va(GICC_PHYS)) },
));

struct Board;

impl boot::Board for Board {
    const NAME: &'static str = "qemu-virt";
    /// `-m 128M` の RAM
    const RAM: Range<u64> = 0x4000_0000..0x4800_0000;
    /// GIC(0x0800_0000〜)と PL011(0x0900_0000)を含む範囲
    #[allow(clippy::single_range_in_vec_init)] // 範囲の一覧(たまたま1つ)
    const DEVICES: &'static [Range<u64>] = &[0x0800_0000..0x0a00_0000];
    /// EL1 物理タイマ(PPI 14 = INTID 30)
    const TIMER_IRQ: u32 = 30;

    type Console = Pl011<MmioRegion>;
    type Intc = GicV2<MmioRegion>;

    fn console() -> &'static SpinLock<Self::Console> {
        &UART
    }

    fn intc() -> &'static SpinLock<Self::Intc> {
        &INTC
    }

    fn init_devices() {
        UART.lock().init(UART_CLOCK_HZ, 115_200);
        INTC.lock().init();
    }
}

/// `arch_aarch64::boot` の `_start` から呼ばれる。`dtb` はデバイスツリーの物理アドレス。
#[unsafe(no_mangle)]
extern "C" fn kernel_main(_dtb: usize) -> ! {
    boot::init::<Board>();

    #[cfg(test)]
    test_main();

    kernel::kmain(<Board as boot::Board>::NAME);
    boot::run_demos(cfg!(feature = "panic-demo"), cfg!(feature = "fault-demo"));
    boot::exit(0)
}

#[cfg(not(test))]
#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    boot::panic(info)
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
