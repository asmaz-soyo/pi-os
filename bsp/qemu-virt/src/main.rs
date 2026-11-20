//! QEMU `-M virt` 用のカーネルイメージ。
//!
//! 今はカーネル内テスト(QEMU の結合テスト)を動かす入れ物。通常起動は M1 で作る。
#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(crate::ktest::runner)]
#![reexport_test_harness_main = "test_main"]

#[cfg(test)]
mod ktest;

/// `arch_aarch64::boot` の `_start` から呼ばれる。`dtb` はデバイスツリーのアドレス。
#[unsafe(no_mangle)]
extern "C" fn kernel_main(_dtb: usize) -> ! {
    #[cfg(test)]
    test_main();

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
