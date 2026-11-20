//! ユーザーモード(EL0)への出入りと、カーネルの文脈の保存・復帰。
//!
//! `enter_user` はカーネルの callee-saved レジスタと SP を `KernelContext` に保存してから、
//! トラップフレームの内容で EL0 へ `eret` する。プロセスが終わったら、例外ハンドラの中から
//! `resume` を呼ぶと、保存した文脈に戻り、`enter_user` が値を返したように続きが実行される
//! (C の setjmp / longjmp と同じ考え方)。
//!
//! EL0 へ戻る `eret` は、例外の復帰経路(`exception_return`)の1か所だけ。
//!
//! スケジューラのための、カーネルのタスクどうしの切り替え(`switch`)もここに置く。

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
    fn arch_switch_context(from: *mut u64, to: *const u64);
    fn arch_enter_user_frame(frame: *const TrapFrame) -> !;
    fn arch_thread_start();
}

/// 今のスタックの上で `frame` の状態から EL0 に入る(戻らない)。
///
/// # Safety
/// `frame` は EL0 のプログラムを指し、ユーザーのアドレス空間が有効になっていること。
pub unsafe fn enter_user_noreturn(frame: &TrapFrame) -> ! {
    // SAFETY: 呼び出し元が保証する
    unsafe { arch_enter_user_frame(frame) }
}

/// スケジューラ用の文脈の並び(`kernel::sched::SavedContext::regs` の中身)。
/// [0..12] = x19〜x30、[12] = SP、[13..21] = d8〜d15(呼び出し規約で保存すべき FP レジスタ)。
const CTX_X19: usize = 0;
const CTX_X20: usize = 1;
const CTX_X21: usize = 2;
const CTX_LR: usize = 11;
const CTX_SP: usize = 12;

/// 今の文脈を `from` に保存し、`to` に切り替える(関数呼び出しをまたいで保存されるレジスタだけ)。
///
/// # Safety
/// `to` は `switch` で保存したか、`new_thread_context` で作った文脈であること。
pub unsafe fn switch(from: *mut [u64; 24], to: *const [u64; 24]) {
    // SAFETY: 呼び出し元が保証する
    unsafe { arch_switch_context(from.cast(), to.cast()) }
}

/// `stack_top` のスタックで `entry(arg)` から始まる文脈。`entry` が戻ったら `exit()` を呼ぶ。
/// 始まるときに IRQ を許可する。
pub fn new_thread_context(stack_top: usize, entry: fn(usize), arg: usize, exit: fn()) -> [u64; 24] {
    let mut regs = [0u64; 24];
    regs[CTX_X19] = entry as usize as u64;
    regs[CTX_X20] = arg as u64;
    regs[CTX_X21] = exit as usize as u64;
    regs[CTX_LR] = arch_thread_start as *const () as usize as u64;
    regs[CTX_SP] = (stack_top & !15) as u64;
    regs
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
    sub     sp, sp, #832
    mov     x2, #0
1:
    ldr     x3, [x1, x2]
    str     x3, [sp, x2]
    add     x2, x2, #8
    cmp     x2, #832
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

    .global arch_enter_user_frame
arch_enter_user_frame:
    // x0 = frame。保存はしない(タスクのスタックの上で EL0 に入り、戻らない)
    msr     daifset, #2
    mov     x1, x0
    sub     sp, sp, #832
    mov     x2, #0
2:
    ldr     x3, [x1, x2]
    str     x3, [sp, x2]
    add     x2, x2, #8
    cmp     x2, #832
    b.lo    2b
    b       exception_return

    .global arch_switch_context
arch_switch_context:
    // x0 = 保存先、x1 = 切り替え先
    stp     x19, x20, [x0, #0]
    stp     x21, x22, [x0, #16]
    stp     x23, x24, [x0, #32]
    stp     x25, x26, [x0, #48]
    stp     x27, x28, [x0, #64]
    stp     x29, x30, [x0, #80]
    mov     x9, sp
    str     x9, [x0, #96]
    stp     d8, d9, [x0, #104]
    stp     d10, d11, [x0, #120]
    stp     d12, d13, [x0, #136]
    stp     d14, d15, [x0, #152]
    ldp     d8, d9, [x1, #104]
    ldp     d10, d11, [x1, #120]
    ldp     d12, d13, [x1, #136]
    ldp     d14, d15, [x1, #152]
    ldp     x19, x20, [x1, #0]
    ldp     x21, x22, [x1, #16]
    ldp     x23, x24, [x1, #32]
    ldp     x25, x26, [x1, #48]
    ldp     x27, x28, [x1, #64]
    ldp     x29, x30, [x1, #80]
    ldr     x9, [x1, #96]
    mov     sp, x9
    ret                             // 切り替え先の x30 へ(新しいスレッドなら arch_thread_start)

    .global arch_thread_start
arch_thread_start:
    // new_thread_context が x19 = entry、x20 = arg、x21 = exit を入れておく
    msr     daifclr, #2
    mov     x0, x20
    blr     x19
    blr     x21                     // 戻ってこない
1:  wfe
    b       1b
"#
);
