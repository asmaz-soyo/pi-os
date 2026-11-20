// マップされていないアドレスを write に渡す。カーネルは -EFAULT(-14)を返すはず
    .global _start
_start:
    mov     x0, #1
    mov     x1, #0xdead0000
    mov     x2, #4
    mov     x8, #64             // write
    svc     #0
    neg     x0, x0              // -(-14) = 14 を終了ステータスにする
    mov     x8, #93
    svc     #0
