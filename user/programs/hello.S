// 「Hello from user space!」を標準出力に書いて終わる。libc を使わず、システムコールを直接呼ぶ。
    .global _start
_start:
    mov     x0, #1              // fd = 1(標準出力)
    adr     x1, msg             // buf
    mov     x2, #(msg_end - msg) // count
    mov     x8, #64             // write
    svc     #0
    mov     x0, #0              // 終了ステータス
    mov     x8, #94             // exit_group
    svc     #0
msg:
    .ascii  "Hello from user space!\n"
msg_end:
