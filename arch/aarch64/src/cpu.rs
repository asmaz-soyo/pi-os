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

/// IRQ を受け付ける(PSTATE.I を 0 にする)。
pub fn enable_irq() {
    // SAFETY: 割り込みマスクの変更。ベクタとハンドラは登録済みであること
    unsafe { asm!("msr daifclr, #2", options(nomem, nostack)) };
}

/// IRQ を禁止する(PSTATE.I を 1 にする)。
pub fn disable_irq() {
    // SAFETY: 割り込みマスクの変更
    unsafe { asm!("msr daifset, #2", options(nomem, nostack)) };
}

/// MMU が有効か(SCTLR_EL1.M)。
pub fn mmu_enabled() -> bool {
    let sctlr: u64;
    // SAFETY: 読み出しだけ
    unsafe { asm!("mrs {}, sctlr_el1", out(reg) sctlr, options(nomem, nostack)) };
    sctlr & 1 != 0
}

/// 今のページテーブルで `va` を変換する(CPU の `at s1e1r` 命令を使う)。マップされていなければ `None`。
pub fn translate(va: hal::addr::VirtAddr) -> Option<hal::addr::PhysAddr> {
    let par: u64;
    // SAFETY: アドレス変換の問い合わせだけで、メモリには触らない
    unsafe {
        asm!("at s1e1r, {}", "isb", "mrs {}, par_el1",
             in(reg) va.0, out(reg) par, options(nostack));
    }
    if par & 1 != 0 {
        None
    } else {
        Some(hal::addr::PhysAddr(
            (par & 0x0000_ffff_ffff_f000) | (va.0 & 0xfff),
        ))
    }
}

/// カーネル(上位アドレス)のページテーブルを切り替える。
///
/// # Safety
/// `root` は今実行中のコードとスタックを同じ仮想アドレスでマップしていること。
pub unsafe fn set_kernel_table(root: hal::addr::PhysAddr) {
    // SAFETY: 呼び出し元が保証する
    unsafe {
        asm!("dsb ishst", "msr ttbr1_el1, {}", "isb", "tlbi vmalle1", "dsb ish", "isb",
             in(reg) root.0, options(nostack));
    }
}

/// ユーザー(下位アドレス)のページテーブルを切り替える。
///
/// # Safety
/// 下位アドレスを使っているコードやデータがないこと。
pub unsafe fn set_user_table(root: hal::addr::PhysAddr) {
    // SAFETY: 呼び出し元が保証する
    unsafe {
        asm!("dsb ishst", "msr ttbr0_el1, {}", "isb", "tlbi vmalle1", "dsb ish", "isb",
             in(reg) root.0, options(nostack));
    }
}

/// `va` を含むページの TLB を(全コアで)無効にする。
pub fn flush_tlb_page(va: hal::addr::VirtAddr) {
    // SAFETY: TLB の無効化は副作用としてメモリを壊さない
    unsafe {
        asm!("dsb ishst", "tlbi vaae1is, {}", "dsb ish", "isb",
             in(reg) va.0 >> 12, options(nostack));
    }
}

/// カウンタの値(乱数の種などに使う)。
pub fn counter() -> u64 {
    let t: u64;
    // SAFETY: 読み出しだけ
    unsafe { asm!("isb", "mrs {}, cntpct_el0", out(reg) t, options(nomem, nostack)) };
    t
}

/// IRQ を禁止し、禁止する前に許可されていたかを返す。
pub fn irq_save() -> bool {
    let daif: u64;
    // SAFETY: DAIF を読んで、IRQ を禁止するだけ
    unsafe { asm!("mrs {}, daif", "msr daifset, #2", out(reg) daif, options(nomem, nostack)) };
    daif & (1 << 7) == 0
}

/// `irq_save` の前の状態に戻す。
pub fn irq_restore(enabled: bool) {
    if enabled {
        enable_irq();
    }
}
