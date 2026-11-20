//! CPU の状態を読むための小さな関数。

use core::arch::asm;

/// 現在の例外レベル(0〜3)。カーネルは 1 で動く。
pub fn current_el() -> u8 {
    let el: u64;
    // SAFETY: CurrentEL の読み出しには副作用がない
    unsafe { asm!("mrs {}, CurrentEL", out(reg) el, options(nomem, nostack)) };
    ((el >> 2) & 0b11) as u8
}

/// ある時点のレジスタ(パニックの報告用)。
#[derive(Debug, Clone, Copy)]
pub struct Snapshot {
    pub current_el: u8,
    pub sp: u64,
    pub fp: u64,
    pub lr: u64,
}

/// 呼び出した関数の中での SP・FP・LR を読む。呼び出し元に埋め込まれるよう `inline(always)`。
#[inline(always)]
pub fn snapshot() -> Snapshot {
    let (sp, fp, lr): (u64, u64, u64);
    // SAFETY: レジスタを読むだけ
    unsafe {
        asm!("mov {}, sp", "mov {}, x29", "mov {}, x30",
             out(reg) sp, out(reg) fp, out(reg) lr, options(nomem, nostack));
    }
    Snapshot {
        current_el: current_el(),
        sp,
        fp,
        lr,
    }
}

/// 何もせずに止まる(割り込みなどのイベントを待ち続ける)。
pub fn halt() -> ! {
    loop {
        // SAFETY: wfe はイベントを待つだけの命令
        unsafe { asm!("wfe", options(nomem, nostack)) };
    }
}
