//! 例外ベクタと、Rust のハンドラへの橋渡し。
//!
//! AArch64 では、例外が起きると CPU は `VBAR_EL1` が指す表(ベクタテーブル)の決まった位置に飛ぶ。
//! 表は「どこから来たか(4通り)× 何が起きたか(4通り)」の 16 個の入口からなり、各 0x80 バイト。
//!
//! | オフセット | 来た場所 |
//! | --- | --- |
//! | 0x000 | 同じ EL、SP_EL0 を使用中 |
//! | 0x200 | 同じ EL、SP_ELx を使用中(カーネル実行中の例外はここ) |
//! | 0x400 | 下の EL(AArch64)から(ユーザープログラムの例外・システムコール) |
//! | 0x600 | 下の EL(AArch32)から(使わない) |
//!
//! それぞれの中で +0x000 同期例外、+0x080 IRQ、+0x100 FIQ、+0x180 SError。

use crate::trap::TrapFrame;
use spin::Mutex;

/// 例外を受け取る側(カーネル)。BSP が [`set_handler`] で登録する。
pub trait Handler: Sync {
    /// 同期例外(システムコール、ブレークポイント、アボートなど)
    fn synchronous(&self, frame: &mut TrapFrame);
    /// 割り込み
    fn irq(&self, frame: &mut TrapFrame);
}

static HANDLER: Mutex<Option<&'static dyn Handler>> = Mutex::new(None);

/// ハンドラを差し替え、前のハンドラを返す。
pub fn set_handler(handler: Option<&'static dyn Handler>) -> Option<&'static dyn Handler> {
    core::mem::replace(&mut *HANDLER.lock(), handler)
}

/// ベクタテーブルを CPU に登録する。
pub fn init() {
    unsafe extern "C" {
        static exception_vectors: u8;
    }
    // SAFETY: exception_vectors は下のアセンブリで 2KiB 境界に置いた正しい表
    unsafe {
        core::arch::asm!(
            "msr vbar_el1, {}",
            "isb",
            in(reg) &raw const exception_vectors,
            options(nostack)
        );
    }
}

const KIND_NAMES: [&str; 4] = ["synchronous", "IRQ", "FIQ", "SError"];
const ORIGIN_NAMES: [&str; 4] = [
    "current EL with SP_EL0",
    "current EL",
    "lower EL (AArch64)",
    "lower EL (AArch32)",
];

/// アセンブリから呼ばれる。`kind` はベクタの番号(0〜15)。
#[unsafe(no_mangle)]
extern "C" fn arch_exception_entry(kind: u64, frame: &mut TrapFrame) {
    // ロックを持ったままハンドラを呼ばないよう、取り出してからすぐに離す
    let handler = *HANDLER.lock();
    match (kind % 4, handler) {
        (0, Some(handler)) => handler.synchronous(frame),
        (1, Some(handler)) => handler.irq(frame),
        _ => panic!(
            "unhandled {} exception from {}\n{frame}",
            KIND_NAMES[(kind % 4) as usize],
            ORIGIN_NAMES[(kind / 4) as usize % 4]
        ),
    }
}

// TrapFrame::SIZE = 288。並びは trap.rs の TrapFrame と同じ(ホストのテストで確かめている)。
core::arch::global_asm!(
    r#"
.macro VECTOR kind
    .balign 0x80
    sub     sp, sp, #288
    stp     x0, x1, [sp, #0]
    mov     x0, #\kind
    b       exception_common
.endm

    .section .text.vectors, "ax"
    .balign 0x800
    .global exception_vectors
exception_vectors:
    VECTOR 0
    VECTOR 1
    VECTOR 2
    VECTOR 3
    VECTOR 4
    VECTOR 5
    VECTOR 6
    VECTOR 7
    VECTOR 8
    VECTOR 9
    VECTOR 10
    VECTOR 11
    VECTOR 12
    VECTOR 13
    VECTOR 14
    VECTOR 15

exception_common:
    // x0, x1 は入口で保存済み(x0 には kind が入っている)
    stp     x2, x3, [sp, #16]
    stp     x4, x5, [sp, #32]
    stp     x6, x7, [sp, #48]
    stp     x8, x9, [sp, #64]
    stp     x10, x11, [sp, #80]
    stp     x12, x13, [sp, #96]
    stp     x14, x15, [sp, #112]
    stp     x16, x17, [sp, #128]
    stp     x18, x19, [sp, #144]
    stp     x20, x21, [sp, #160]
    stp     x22, x23, [sp, #176]
    stp     x24, x25, [sp, #192]
    stp     x26, x27, [sp, #208]
    stp     x28, x29, [sp, #224]
    str     x30, [sp, #240]
    mrs     x2, sp_el0
    mrs     x3, elr_el1
    stp     x2, x3, [sp, #248]
    mrs     x2, spsr_el1
    mrs     x3, esr_el1
    stp     x2, x3, [sp, #264]
    mrs     x2, far_el1
    str     x2, [sp, #280]

    mov     x1, sp
    bl      arch_exception_entry

    // ハンドラが書き換えたかもしれない値を CPU に戻す
    ldp     x2, x3, [sp, #248]
    msr     sp_el0, x2
    msr     elr_el1, x3
    ldr     x2, [sp, #264]
    msr     spsr_el1, x2
    ldp     x0, x1, [sp, #0]
    ldp     x2, x3, [sp, #16]
    ldp     x4, x5, [sp, #32]
    ldp     x6, x7, [sp, #48]
    ldp     x8, x9, [sp, #64]
    ldp     x10, x11, [sp, #80]
    ldp     x12, x13, [sp, #96]
    ldp     x14, x15, [sp, #112]
    ldp     x16, x17, [sp, #128]
    ldp     x18, x19, [sp, #144]
    ldp     x20, x21, [sp, #160]
    ldp     x22, x23, [sp, #176]
    ldp     x24, x25, [sp, #192]
    ldp     x26, x27, [sp, #208]
    ldp     x28, x29, [sp, #224]
    ldr     x30, [sp, #240]
    add     sp, sp, #288
    eret
"#
);
