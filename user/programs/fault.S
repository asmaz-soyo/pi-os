// アドレス 0 を読む(マップされていないのでデータアボート)
    .global _start
_start:
    mov     x1, #0
    ldr     x0, [x1]
    mov     x8, #93
    svc     #0
