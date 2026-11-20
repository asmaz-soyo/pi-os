//! BCM2836/2837 のコアローカル割り込みコントローラ(Raspberry Pi 2/3)。
//!
//! Pi 3 には GIC がない。ARM Generic Timer の割り込みは、`0x4000_0000` にある
//! 「ローカルペリフェラル」がコアごとに受けて CPU に IRQ として渡す。
//! 割り込み番号は、`CORE0_IRQ_SOURCE` のビット番号をそのまま使う。
#![no_std]

use hal::irq::IrqController;
use hal::mmio::Mmio;

/// コア0のタイマ割り込みの制御(どのタイマの割り込みを IRQ にするか)
pub const CORE0_TIMER_INT_CTRL: usize = 0x40;
/// コア0の IRQ の要因(読み出し専用)
pub const CORE0_IRQ_SOURCE: usize = 0x60;

/// 非セキュアの物理タイマ(CNTP、EL1 から使うタイマ)
pub const CNTPNSIRQ: u32 = 1;

pub struct LocalIntc<M: Mmio> {
    regs: M,
    enabled: u32,
}

impl<M: Mmio> LocalIntc<M> {
    pub const fn new(regs: M) -> Self {
        Self { regs, enabled: 0 }
    }

    pub fn regs(&self) -> &M {
        &self.regs
    }

    pub fn regs_mut(&mut self) -> &mut M {
        &mut self.regs
    }
}

impl<M: Mmio> IrqController for LocalIntc<M> {
    /// 今はタイマの割り込み(ビット 0〜3)だけに対応する。
    fn enable(&mut self, irq: u32) {
        self.enabled |= 1 << irq;
        self.regs.write(CORE0_TIMER_INT_CTRL, self.enabled & 0xf);
    }

    fn pending(&mut self) -> Option<u32> {
        let source = self.regs.read(CORE0_IRQ_SOURCE) & self.enabled;
        (source != 0).then(|| source.trailing_zeros())
    }

    /// タイマの割り込みは、タイマ側を再設定すると下がるので、ここでは何もしない。
    fn end_of_interrupt(&mut self, _irq: u32) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use hal::irq::IrqController;
    use hal::mock::FakeMmio;

    #[test]
    fn enable_cntpns_routes_the_physical_timer_of_core0() {
        let mut intc = LocalIntc::new(FakeMmio::new());
        intc.enable(CNTPNSIRQ);
        assert_eq!(
            intc.regs().writes(),
            [(CORE0_TIMER_INT_CTRL, 1 << CNTPNSIRQ)]
        );
    }

    #[test]
    fn pending_returns_enabled_sources_only() {
        let mut intc = LocalIntc::new(FakeMmio::new());
        intc.enable(CNTPNSIRQ);
        // 0b10 = CNTPNSIRQ、0b1000_0000 = 有効にしていない別の要因
        intc.regs_mut()
            .queue_reads(CORE0_IRQ_SOURCE, &[0b1000_0010, 0b1000_0000, 0]);
        assert_eq!(intc.pending(), Some(CNTPNSIRQ));
        assert_eq!(intc.pending(), None);
        assert_eq!(intc.pending(), None);
    }

    #[test]
    fn end_of_interrupt_needs_no_register_access() {
        let mut intc = LocalIntc::new(FakeMmio::new());
        intc.end_of_interrupt(CNTPNSIRQ);
        assert!(intc.regs().writes().is_empty());
    }
}
