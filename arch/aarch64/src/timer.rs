//! ARM Generic Timer(CPU に内蔵されたタイマ)。
//!
//! システムレジスタで操作するので MMIO のドライバではなく、arch に置く。
//! EL1 の非セキュア物理タイマ(CNTP)を使う。割り込みは virt では GIC の PPI 30、
//! Pi 3 ではローカル割り込みコントローラの CNTPNSIRQ として届く。

use core::arch::asm;
use hal::timer::Timer;

/// レジスタだけで状態を持たないので、大きさ 0 の型。
pub struct GenericTimer;

impl Timer for GenericTimer {
    fn frequency(&self) -> u64 {
        let f: u64;
        // SAFETY: CNTFRQ_EL0 の読み出しには副作用がない
        unsafe { asm!("mrs {}, cntfrq_el0", out(reg) f, options(nomem, nostack)) };
        f
    }

    fn now(&self) -> u64 {
        let t: u64;
        // SAFETY: isb で前の命令の完了を待ってからカウンタを読む
        unsafe { asm!("isb", "mrs {}, cntpct_el0", out(reg) t, options(nomem, nostack)) };
        t
    }

    fn set_timeout(&mut self, ticks: u64) {
        // SAFETY: TVAL に「あと何カウントで割り込むか」を書き、CTL で有効化(ENABLE=1, IMASK=0)
        unsafe {
            asm!(
                "msr cntp_tval_el0, {}",
                "msr cntp_ctl_el0, {}",
                "isb",
                in(reg) ticks,
                in(reg) 1u64,
                options(nomem, nostack)
            )
        };
    }
}
