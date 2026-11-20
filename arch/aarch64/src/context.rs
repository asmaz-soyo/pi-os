//! ユーザーモード(EL0)への出入りと、カーネルの文脈の保存・復帰。
//!
//! `enter_user` はカーネルの callee-saved レジスタと SP を `KernelContext` に保存してから、
//! トラップフレームの内容で EL0 へ `eret` する。プロセスが終わったら、例外ハンドラの中から
//! `resume` を呼ぶと、保存した文脈に戻り、`enter_user` が値を返したように続きが実行される
//! (C の setjmp / longjmp と同じ考え方)。
//!
//! EL0 へ戻る `eret` は、例外の復帰経路(`exception_return`)の1か所だけ。

use crate::trap::TrapFrame;

/// 関数呼び出しをまたいで保存されるレジスタ(x19〜x30)と SP。
#[repr(C)]
#[derive(Debug, Default)]
pub struct KernelContext {
    regs: [u64; 12],
    sp: u64,
}

impl KernelContext {
    pub const fn new() -> Self {
        Self {
            regs: [0; 12],
            sp: 0,
        }
    }
}

unsafe extern "C" {
    fn arch_enter_user(ctx: *mut KernelContext, frame: *const TrapFrame) -> u64;
    fn arch_resume_kernel(ctx: *const KernelContext, value: u64) -> !;
}

/// `frame` の状態で EL0 に入る。`resume(ctx, value)` が呼ばれると `value` を返す。
///
/// # Safety
/// `frame` は EL0 のプログラムを指し、ユーザーのアドレス空間が有効になっていること。
/// `ctx` は `resume` されるまで有効であること。
pub unsafe fn enter_user(ctx: *mut KernelContext, frame: &TrapFrame) -> u64 {
    // SAFETY: 呼び出し元が保証する
    unsafe { arch_enter_user(ctx, frame) }
}

/// `enter_user` に戻る。
///
/// # Safety
/// `ctx` は `enter_user` が保存し、まだ戻っていない文脈であること。
/// 呼び出し元のスタック(`enter_user` より深い部分)は捨てられる。
pub unsafe fn resume(ctx: *const KernelContext, value: u64) -> ! {
    // SAFETY: 呼び出し元が保証する
    unsafe { arch_resume_kernel(ctx, value) }
}

core::arch::global_asm!(
    r#"
    .section .text
    .global arch_enter_user
arch_enter_user:
    // x0 = ctx, x1 = frame
    msr     daifset, #2             // 復帰の途中で IRQ が来て ELR/SPSR を壊さないように
    stp     x19, x20, [x0, #0]
    stp     x21, x22, [x0, #16]
    stp     x23, x24, [x0, #32]
    stp     x25, x26, [x0, #48]
    stp     x27, x28, [x0, #64]
    stp     x29, x30, [x0, #80]
    mov     x2, sp
    str     x2, [x0, #96]
    // トラップフレームをスタックに写し、例外の復帰経路から EL0 へ戻る
    sub     sp, sp, #288
    mov     x2, #0
1:
    ldr     x3, [x1, x2]
    str     x3, [sp, x2]
    add     x2, x2, #8
    cmp     x2, #288
    b.lo    1b
    b       exception_return

    .global arch_resume_kernel
arch_resume_kernel:
    // x0 = ctx, x1 = value
    ldp     x19, x20, [x0, #0]
    ldp     x21, x22, [x0, #16]
    ldp     x23, x24, [x0, #32]
    ldp     x25, x26, [x0, #48]
    ldp     x27, x28, [x0, #64]
    ldp     x29, x30, [x0, #80]
    ldr     x2, [x0, #96]
    mov     sp, x2
    mov     x0, x1
    ret                             // arch_enter_user の呼び出し元へ
"#
);
