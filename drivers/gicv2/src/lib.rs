//! ARM GICv2(Generic Interrupt Controller)のドライバ。QEMU `virt` の割り込みコントローラ。
//!
//! GIC は2つの部分に分かれている:
//! - ディストリビュータ(GICD):どの割り込みを有効にし、どのコアへ送るかを決める(全コアで共有)
//! - CPU インターフェース(GICC):コアごとに、届いた割り込みの番号を読み、処理の終わりを伝える
#![no_std]

use hal::irq::IrqController;
use hal::mmio::Mmio;

// ディストリビュータのレジスタ
pub const GICD_CTLR: usize = 0x000;
/// 32 個ずつ、1 を書いたビットの割り込みを有効にする
pub const GICD_ISENABLER: usize = 0x100;

// CPU インターフェースのレジスタ
pub const GICC_CTLR: usize = 0x000;
/// この優先度より高い(値が小さい)割り込みだけを通す
pub const GICC_PMR: usize = 0x004;
/// 読むと、届いた割り込みの番号が返る(読んだ時点で「処理中」になる)
pub const GICC_IAR: usize = 0x00c;
/// 処理が終わった割り込みの番号を書く
pub const GICC_EOIR: usize = 0x010;

/// 「保留中の割り込みはない」を表す番号
pub const SPURIOUS: u32 = 1023;

pub struct GicV2<M: Mmio> {
    gicd: M,
    gicc: M,
}

impl<M: Mmio> GicV2<M> {
    pub const fn new(gicd: M, gicc: M) -> Self {
        Self { gicd, gicc }
    }

    pub fn init(&mut self) {
        self.gicd.write(GICD_CTLR, 0);
        self.gicc.write(GICC_PMR, 0xff); // すべての優先度を通す
        self.gicc.write(GICC_CTLR, 1);
        self.gicd.write(GICD_CTLR, 1);
    }

    pub fn distributor(&self) -> &M {
        &self.gicd
    }

    pub fn cpu_interface(&self) -> &M {
        &self.gicc
    }

    pub fn cpu_interface_mut(&mut self) -> &mut M {
        &mut self.gicc
    }
}

impl<M: Mmio> IrqController for GicV2<M> {
    fn enable(&mut self, irq: u32) {
        let reg = GICD_ISENABLER + (irq / 32) as usize * 4;
        self.gicd.write(reg, 1 << (irq % 32));
    }

    fn pending(&mut self) -> Option<u32> {
        let id = self.gicc.read(GICC_IAR) & 0x3ff;
        (id != SPURIOUS).then_some(id)
    }

    fn end_of_interrupt(&mut self, irq: u32) {
        self.gicc.write(GICC_EOIR, irq);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hal::irq::IrqController;
    use hal::mock::FakeMmio;

    fn gic() -> GicV2<FakeMmio> {
        GicV2::new(FakeMmio::new(), FakeMmio::new())
    }

    #[test]
    fn init_enables_distributor_and_cpu_interface() {
        let mut gic = gic();
        gic.init();
        assert_eq!(gic.distributor().writes(), [(GICD_CTLR, 0), (GICD_CTLR, 1)]);
        assert_eq!(
            gic.cpu_interface().writes(),
            [(GICC_PMR, 0xff), (GICC_CTLR, 1)]
        );
    }

    #[test]
    fn enable_sets_the_bit_in_the_right_isenabler_register() {
        let mut gic = gic();
        gic.enable(30);
        gic.enable(40);
        assert_eq!(
            gic.distributor().writes(),
            [(GICD_ISENABLER, 1 << 30), (GICD_ISENABLER + 4, 1 << 8)]
        );
    }

    #[test]
    fn pending_reads_the_interrupt_id_from_iar() {
        let mut gic = gic();
        gic.cpu_interface_mut()
            .queue_reads(GICC_IAR, &[30, SPURIOUS]);
        assert_eq!(gic.pending(), Some(30));
        assert_eq!(gic.pending(), None);
    }

    #[test]
    fn end_of_interrupt_writes_eoir() {
        let mut gic = gic();
        gic.end_of_interrupt(30);
        assert_eq!(gic.cpu_interface().writes(), [(GICC_EOIR, 30)]);
    }
}
