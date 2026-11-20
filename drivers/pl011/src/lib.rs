//! ARM PL011 UART のドライバ。
//!
//! QEMU `virt` と Raspberry Pi 3(UART0)の両方に載っている。レジスタは
//! 「ARM PrimeCell UART (PL011) Technical Reference Manual」の名前に合わせた。
#![no_std]

use hal::console::Console;
use hal::mmio::Mmio;

// レジスタのオフセット
pub const DR: usize = 0x00; // データ(書くと送信、読むと受信)
pub const FR: usize = 0x18; // フラグ
pub const IBRD: usize = 0x24; // ボーレート分周比の整数部
pub const FBRD: usize = 0x28; // ボーレート分周比の小数部(1/64 単位)
pub const LCR_H: usize = 0x2c; // 回線制御(データ長、FIFO)
pub const CR: usize = 0x30; // 制御(有効化、送受信の許可)
pub const ICR: usize = 0x44; // 割り込みのクリア

// ビット
pub const FR_RXFE: u32 = 1 << 4; // 受信 FIFO が空
pub const FR_TXFF: u32 = 1 << 5; // 送信 FIFO が満杯
pub const LCR_H_FEN: u32 = 1 << 4; // FIFO を使う
pub const LCR_H_WLEN_8: u32 = 0b11 << 5; // 8 ビット
pub const CR_UARTEN: u32 = 1 << 0;
pub const CR_TXE: u32 = 1 << 8;
pub const CR_RXE: u32 = 1 << 9;

/// UART のクロック `clock_hz` で `baud` を出すための分周比(整数部, 小数部)。
///
/// 分周比 = clock / (16 * baud)。小数部は 1/64 単位に丸める。
pub fn baud_divisors(clock_hz: u32, baud: u32) -> (u32, u32) {
    // 64 倍した値で計算すると、整数演算だけで小数部が求まる(+ baud*8 は四捨五入のため)
    let div_x64 = (clock_hz as u64 * 4 + baud as u64 / 2) / baud as u64;
    ((div_x64 >> 6) as u32, (div_x64 & 0x3f) as u32)
}

pub struct Pl011<M: Mmio> {
    regs: M,
}

impl<M: Mmio> Pl011<M> {
    pub const fn new(regs: M) -> Self {
        Self { regs }
    }

    /// 8N1、FIFO あり、送受信を有効にする。
    pub fn init(&mut self, clock_hz: u32, baud: u32) {
        let (ibrd, fbrd) = baud_divisors(clock_hz, baud);
        // 設定を変える前に UART を止める
        self.regs.write(CR, 0);
        self.regs.write(ICR, 0x7ff);
        self.regs.write(IBRD, ibrd);
        self.regs.write(FBRD, fbrd);
        // LCR_H への書き込みで IBRD/FBRD が反映される
        self.regs.write(LCR_H, LCR_H_WLEN_8 | LCR_H_FEN);
        self.regs.write(CR, CR_UARTEN | CR_TXE | CR_RXE);
    }

    fn put(&mut self, byte: u8) {
        while self.regs.read(FR) & FR_TXFF != 0 {
            core::hint::spin_loop();
        }
        self.regs.write(DR, byte as u32);
    }

    pub fn regs(&self) -> &M {
        &self.regs
    }
}

impl<M: Mmio> Console for Pl011<M> {
    /// シリアル端末向けに `\n` を `\r\n` にして送る。
    fn write_bytes(&mut self, buf: &[u8]) {
        for &byte in buf {
            if byte == b'\n' {
                self.put(b'\r');
            }
            self.put(byte);
        }
    }

    fn read_byte(&mut self) -> Option<u8> {
        if self.regs.read(FR) & FR_RXFE != 0 {
            return None;
        }
        // DR の 8〜11 ビットは受信エラー(フレーミング、パリティなど)の印。データは下位 8 ビット
        Some(self.regs.read(DR) as u8)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hal::console::Console;
    use hal::mock::FakeMmio;

    #[test]
    fn baud_divisors_for_48mhz_115200() {
        // 48_000_000 / (16 * 115200) = 26.0416… → 整数部 26、小数部 0.0416*64 ≈ 3
        assert_eq!(baud_divisors(48_000_000, 115_200), (26, 3));
    }

    #[test]
    fn baud_divisors_for_24mhz_115200() {
        assert_eq!(baud_divisors(24_000_000, 115_200), (13, 1));
    }

    #[test]
    fn init_disables_configures_then_enables() {
        let mut uart = Pl011::new(FakeMmio::new());
        uart.init(48_000_000, 115_200);
        assert_eq!(
            uart.regs().writes(),
            [
                (CR, 0),
                (ICR, 0x7ff),
                (IBRD, 26),
                (FBRD, 3),
                (LCR_H, LCR_H_WLEN_8 | LCR_H_FEN),
                (CR, CR_UARTEN | CR_TXE | CR_RXE),
            ]
        );
    }

    #[test]
    fn write_sends_each_byte_to_data_register() {
        let mut uart = Pl011::new(FakeMmio::new());
        uart.write_bytes(b"Hi");
        assert_eq!(uart.regs().writes(), [(DR, b'H' as u32), (DR, b'i' as u32)]);
    }

    #[test]
    fn write_translates_newline_to_crlf() {
        let mut uart = Pl011::new(FakeMmio::new());
        uart.write_bytes(b"\n");
        assert_eq!(
            uart.regs().writes(),
            [(DR, b'\r' as u32), (DR, b'\n' as u32)]
        );
    }

    #[test]
    fn write_waits_while_transmit_fifo_is_full() {
        let mut mmio = FakeMmio::new();
        // 最初の2回は「送信 FIFO が満杯」、3回目で空きができる
        mmio.queue_reads(FR, &[FR_TXFF, FR_TXFF, 0]);
        let mut uart = Pl011::new(mmio);
        uart.write_bytes(b"A");
        assert_eq!(uart.regs().reads_of(FR), 3);
        assert_eq!(uart.regs().writes(), [(DR, b'A' as u32)]);
    }

    #[test]
    fn read_returns_nothing_while_the_receive_fifo_is_empty() {
        let mut mmio = FakeMmio::new();
        mmio.queue_reads(FR, &[FR_RXFE]);
        let mut uart = Pl011::new(mmio);
        assert_eq!(uart.read_byte(), None);
        assert_eq!(uart.regs().reads_of(DR), 0);
    }

    #[test]
    fn read_takes_one_byte_from_the_data_register() {
        let mut mmio = FakeMmio::new();
        mmio.queue_reads(FR, &[0]);
        // 上位のビットは受信エラーの印なので、下位 8 ビットだけを使う
        mmio.queue_reads(DR, &[0x0f00 | b'a' as u32]);
        let mut uart = Pl011::new(mmio);
        assert_eq!(uart.read_byte(), Some(b'a'));
    }
}
