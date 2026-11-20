//! 割り込みコントローラのポート。

/// 割り込みコントローラ。割り込み番号の意味はコントローラごとに違う(BSP が決める)。
pub trait IrqController {
    /// 割り込み `irq` を受け付けるようにする。
    fn enable(&mut self, irq: u32);
    /// 保留中の割り込みを1つ取り出す。なければ `None`。
    fn pending(&mut self) -> Option<u32>;
    /// `irq` の処理が終わったことを伝える。
    fn end_of_interrupt(&mut self, irq: u32);
}
