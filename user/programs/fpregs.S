// FP/SIMD のレジスタがプロセスごとに保たれるかを確かめる(タイマの切り替えとシステムコールをまたいで)。
    .global _start
_start:
    ldr     x1, [sp]                // argc
    mov     x2, #0x3333
    mul     x1, x1, x2
    fmov    d0, x1
    fmov    d8, x1
    fmov    d31, x1
    ldr     x3, =20000000
1:  subs    x3, x3, #1
    b.ne    1b
    // システムコールをはさむ(カーネルが SIMD レジスタを使っても壊れないか)
    mov     x0, #0
    mov     x8, #172                // getpid
    svc     #0
    fmov    x4, d0
    fmov    x5, d8
    fmov    x6, d31
    mov     x0, #0
    cmp     x4, x1
    cinc    x0, x0, ne
    cmp     x5, x1
    cinc    x0, x0, ne
    cmp     x6, x1
    cinc    x0, x0, ne
    mov     x8, #93
    svc     #0
