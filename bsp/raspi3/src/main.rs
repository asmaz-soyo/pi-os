//! Raspberry Pi 3B 用のカーネルイメージ(QEMU `-M raspi3b` と実機で共通)。
//!
//! BSP は「どのドライバをどのアドレスで使うか」の配線だけを持つ。
//! 初期化の順番などの共通の処理は `boot` クレートにある。
#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(ktest::runner)]
#![reexport_test_harness_main = "test_main"]

use bcm2836_local_intc::LocalIntc;
use boot::device_va;
use core::ops::Range;
use hal::mmio::MmioRegion;
use kernel::sync::SpinLock;
use pl011::Pl011;

/// BCM2837 の UART0(PL011)。ペリフェラルはバスアドレス 0x7E00_0000 が物理 0x3F00_0000 に見える
const UART_PHYS: usize = 0x3F20_1000;
/// UART のクロック。ファームウェアの既定値(config.txt の init_uart_clock)
const UART_CLOCK_HZ: u32 = 48_000_000;
/// BCM2836 のローカルペリフェラル(コアごとの割り込みの振り分け)
const LOCAL_INTC_PHYS: usize = 0x4000_0000;

// SAFETY: 各アドレスは BCM2837 のデバイス。この static 以外から同じ領域を触らない
static UART: SpinLock<Pl011<MmioRegion>> =
    SpinLock::new(Pl011::new(unsafe { MmioRegion::new(device_va(UART_PHYS)) }));
static INTC: SpinLock<LocalIntc<MmioRegion>> = SpinLock::new(LocalIntc::new(unsafe {
    MmioRegion::new(device_va(LOCAL_INTC_PHYS))
}));

struct Board;

impl boot::Board for Board {
    const NAME: &'static str = "raspi3";
    /// RAM 1GB のうち、GPU に渡す部分(上の方)を避けた範囲
    const RAM: Range<u64> = 0x0..0x3C00_0000;
    /// ペリフェラル(0x3F00_0000〜)とローカルペリフェラル(0x4000_0000〜)
    #[allow(clippy::single_range_in_vec_init)] // 範囲の一覧(たまたま1つ)
    const DEVICES: &'static [Range<u64>] = &[0x3F00_0000..0x4100_0000];
    /// EL1 物理タイマ(CORE0_IRQ_SOURCE のビット 1)
    const TIMER_IRQ: u32 = bcm2836_local_intc::CNTPNSIRQ;

    type Console = Pl011<MmioRegion>;
    type Intc = LocalIntc<MmioRegion>;

    fn console() -> &'static SpinLock<Self::Console> {
        &UART
    }

    fn intc() -> &'static SpinLock<Self::Intc> {
        &INTC
    }

    fn init_devices() {
        UART.lock().init(UART_CLOCK_HZ, 115_200);
    }
}

/// `arch_aarch64::boot` の `_start` から呼ばれる。`dtb` はデバイスツリーの物理アドレス。
#[unsafe(no_mangle)]
extern "C" fn kernel_main(_dtb: usize) -> ! {
    boot::init::<Board>();

    #[cfg(test)]
    test_main();

    kernel::kmain(<Board as boot::Board>::NAME);
    boot::run_init();
    boot::run_demos(cfg!(feature = "panic-demo"), cfg!(feature = "fault-demo"));
    boot::shutdown(0)
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
