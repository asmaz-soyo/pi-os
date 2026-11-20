//! CPU の状態を読むための小さな関数。

use core::arch::asm;

/// 現在の例外レベル(0〜3)。カーネルは 1 で動く。
pub fn current_el() -> u8 {
    let el: u64;
    // SAFETY: CurrentEL の読み出しには副作用がない
    unsafe { asm!("mrs {}, CurrentEL", out(reg) el, options(nomem, nostack)) };
    ((el >> 2) & 0b11) as u8
}
