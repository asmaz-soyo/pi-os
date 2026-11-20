// TPIDR_EL0(ユーザーのスレッドポインタ)がプロセスごとに保たれるかを確かめる。
// argc に応じた値を入れ、タイマで何度も切り替えられるくらい回してから、値が変わっていないかを見る。
    .global _start
_start:
    ldr     x1, [sp]                // argc
    mov     x2, #0x1111
    mul     x1, x1, x2
    msr     tpidr_el0, x1
    ldr     x3, =30000000
1:  subs    x3, x3, #1
    b.ne    1b
    mrs     x4, tpidr_el0
    cmp     x4, x1
    cset    x0, ne                  // 同じなら 0、違えば 1
    mov     x8, #93
    svc     #0
