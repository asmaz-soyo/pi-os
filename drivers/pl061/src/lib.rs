//! ARM PL061 GPIO コントローラのドライバ(QEMU virt。電源ボタンが GPIO 3 につながっている)。
#![no_std]

use hal::mmio::Mmio;

pub const GPIODIR: usize = 0x400; // 1 = 出力、0 = 入力
pub const GPIOIS: usize = 0x404; // 1 = レベル、0 = エッジ
pub const GPIOIBE: usize = 0x408; // 1 = 両方のエッジ
pub const GPIOIEV: usize = 0x40c; // 1 = 立ち上がり(High)
pub const GPIOIE: usize = 0x410; // 1 = 割り込みを通す
pub const GPIOMIS: usize = 0x418; // 割り込みの要因(マスク後)
pub const GPIOIC: usize = 0x41c; // 1 を書くと要因を消す

pub struct Pl061<M: Mmio> {
    regs: M,
    enabled: u32,
}

impl<M: Mmio> Pl061<M> {
    pub const fn new(regs: M) -> Self {
        Self { regs, enabled: 0 }
    }

    pub fn regs(&self) -> &M {
        &self.regs
    }

    /// 線 `line` を入力にし、立ち上がりのエッジで割り込むようにする
    pub fn enable_rising_edge(&mut self, line: u32) {
        self.enabled |= 1 << line;
        let dir = self.regs.read(GPIODIR) & !(1 << line);
        self.regs.write(GPIODIR, dir);
        self.regs.write(GPIOIS, 0);
        self.regs.write(GPIOIBE, 0);
        self.regs.write(GPIOIEV, self.enabled);
        self.regs.write(GPIOIE, self.enabled);
    }

    /// 割り込みの要因(線のビット)を読み、消して返す
    pub fn take_pending(&mut self) -> u32 {
        let pending = self.regs.read(GPIOMIS);
        self.regs.write(GPIOIC, pending);
        pending
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hal::mock::FakeMmio;

    #[test]
    fn enabling_an_edge_interrupt_sets_input_rising_edge_and_unmasks() {
        let mut gpio = Pl061::new(FakeMmio::new());
        gpio.enable_rising_edge(3);
        let w = gpio.regs().writes();
        assert!(w.contains(&(GPIODIR, 0)), "input");
        assert!(w.contains(&(GPIOIS, 0)), "edge, not level");
        assert!(w.contains(&(GPIOIBE, 0)), "one edge only");
        assert!(w.contains(&(GPIOIEV, 1 << 3)), "rising");
        assert_eq!(w.last(), Some(&(GPIOIE, 1 << 3)), "unmask last");
    }

    #[test]
    fn take_pending_reads_masked_status_and_clears_it() {
        let mut mmio = FakeMmio::new();
        mmio.queue_reads(GPIOMIS, &[0b1000]);
        let mut gpio = Pl061::new(mmio);
        assert_eq!(gpio.take_pending(), 0b1000);
        assert_eq!(gpio.regs().writes().last(), Some(&(GPIOIC, 0b1000)));
    }
}
