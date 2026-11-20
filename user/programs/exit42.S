    .global _start
_start:
    mov     x0, #42
    mov     x8, #93             // exit
    svc     #0
