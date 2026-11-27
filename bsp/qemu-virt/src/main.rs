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
use pl061::Pl061;

// アドレスは QEMU の hw/arm/virt.c より
const UART_PHYS: usize = 0x0900_0000;
const UART_CLOCK_HZ: u32 = 24_000_000;
const GICD_PHYS: usize = 0x0800_0000;
const GICC_PHYS: usize = 0x0801_0000;
/// PL061 GPIO(電源ボタンが GPIO 3)。割り込みは SPI 7 = INTID 39
const GPIO_PHYS: usize = 0x0903_0000;
const POWER_BUTTON: u32 = 3;

// SAFETY: 各アドレスは virt のデバイス。この static 以外から同じ領域を触らない
static UART: SpinLock<Pl011<MmioRegion>> =
    SpinLock::new(Pl011::new(unsafe { MmioRegion::new(device_va(UART_PHYS)) }));
static INTC: SpinLock<GicV2<MmioRegion>> = SpinLock::new(GicV2::new(
    unsafe { MmioRegion::new(device_va(GICD_PHYS)) },
    unsafe { MmioRegion::new(device_va(GICC_PHYS)) },
));

const VIRTIO_SLOTS: [usize; 32] = {
    let mut slots = [0; 32];
    let mut i = 0;
    while i < 32 {
        slots[i] = 0x0a00_0000 + i * 0x200;
        i += 1;
    }
    slots
};

static GPIO_CTRL: SpinLock<Pl061<MmioRegion>> =
    SpinLock::new(Pl061::new(unsafe { MmioRegion::new(device_va(GPIO_PHYS)) }));

struct Board;

impl boot::Board for Board {
    const NAME: &'static str = "qemu-virt";
    /// `-m 128M` の RAM
    const RAM: Range<u64> = 0x4000_0000..0x4800_0000;
    /// GIC(0x0800_0000〜)、PL011(0x0900_0000)、virtio-mmio(0x0a00_0000〜)を含む範囲
    #[allow(clippy::single_range_in_vec_init)] // 範囲の一覧(たまたま1つ)
    const DEVICES: &'static [Range<u64>] = &[0x0800_0000..0x0a00_4000];
    /// virtio-mmio の枠 32 個(hw/arm/virt.c の VIRT_MMIO、0x200 ずつ)
    const VIRTIO_MMIO: &'static [usize] = &VIRTIO_SLOTS;
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
        GPIO_CTRL.lock().enable_rising_edge(POWER_BUTTON);
    }

    const GPIO_IRQ: Option<u32> = Some(39);

    fn gpio_take_pending() -> u32 {
        GPIO_CTRL.lock().take_pending()
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

    #[test_case]
    fn df_reports_the_disk() {
        // statfs:ルート(ext3)と /boot(FAT32)の大きさ
        assert_eq!(
            boot::run_program("/bin/df", &[b"df", b"/", b"/boot"]),
            Ok(0)
        );
    }

    #[test_case]
    fn cpython_runs_from_the_disk() {
        let argv: [&[u8]; 2] = [b"python3", b"/usr/share/pi-os/hello_cpython.py"];
        assert_eq!(boot::run_program("/usr/bin/python3", &argv), Ok(7));
    }

    #[test_case]
    fn the_fat32_partition_of_the_disk_is_mounted_on_boot() {
        assert_eq!(boot::run_program("/bin/bootread", &[b"bootread"]), Ok(0));
    }

    #[test_case]
    fn the_ext2_partition_of_the_disk_is_the_root() {
        assert_eq!(boot::run_program("/bin/ext2read", &[b"ext2read"]), Ok(0));
    }

    #[test_case]
    fn the_virtio_network_card_is_found_as_eth0() {
        // QEMU の既定の MAC アドレスは 52:54:00:12:34:56
        assert_eq!(
            kernel::net::dev::mac_of("eth0"),
            Some([0x52, 0x54, 0x00, 0x12, 0x34, 0x56])
        );
    }

    /// eth0(インターフェース 1)が DHCP でアドレスをもらうまで待つ(3 秒まで)
    fn wait_for_dhcp() -> Option<kernel::net::inet::Ipv4Address> {
        for _ in 0..300 {
            if let Some(a) = kernel::net::inet::ipv4_of(1) {
                return Some(a);
            }
            kernel::sched::sleep_until(kernel::time::now_ns() + 10_000_000);
        }
        None
    }

    #[test_case]
    fn eth0_gets_its_address_gateway_and_dns_by_dhcp() {
        use kernel::net::inet::Ipv4Address;
        // QEMU のユーザーモードのネットワークが配る値
        assert_eq!(wait_for_dhcp(), Some(Ipv4Address::new(10, 0, 2, 15)));
        assert_eq!(
            kernel::net::inet::dns_of(1),
            [Ipv4Address::new(10, 0, 2, 3)]
        );
    }

    #[test_case]
    fn tcp_reaches_the_outside_through_virtio_net() {
        assert!(wait_for_dhcp().is_some());
        // QEMU の guestfwd で、10.0.2.100:7 への接続はホストの `cat` につながる(エコーサーバー)
        let script = b"test \"$(echo hello through virtio-net | nc 10.0.2.100 7)\" = \"hello through virtio-net\"";
        let argv: [&[u8]; 4] = [b"busybox", b"sh", b"-c", script];
        assert_eq!(boot::run_program("/bin/busybox", &argv), Ok(0));
    }

    #[test_case]
    fn musl_resolves_names_with_the_dns_server_from_dhcp() {
        assert!(wait_for_dhcp().is_some());
        // /etc/resolv.conf は /proc/pios/resolv.conf へのリンク(DHCP でもらった DNS サーバー)
        let script = b"test \"$(cat /etc/resolv.conf)\" = \"nameserver 10.0.2.3\" && resolve localhost | grep -qx 127.0.0.1";
        let argv: [&[u8]; 4] = [b"busybox", b"sh", b"-c", script];
        assert_eq!(boot::run_program("/bin/busybox", &argv), Ok(0));
    }

    #[test_case]
    fn virtio_disk_is_readable_and_writable_through_dev_vda() {
        assert_eq!(boot::run_program("/bin/diskread", &[b"diskread"]), Ok(0));
    }
}
