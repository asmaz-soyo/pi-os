/*
 * AArch64 カーネル共通のリンカスクリプト。
 * BSP の link.ld が KERNEL_BASE(カーネルを置く物理アドレス)を定義してから INCLUDE する。
 */
ENTRY(_start)

SECTIONS
{
    . = KERNEL_BASE;

    .text : {
        KEEP(*(.text.boot))   /* _start を先頭に置く */
        *(.text .text.*)
    }
    .rodata : ALIGN(8) { *(.rodata .rodata.*) }
    .data   : ALIGN(8) { *(.data .data.*) }
    .bss (NOLOAD) : ALIGN(8) {
        __bss_start = .;
        *(.bss .bss.*)
        *(COMMON)
        . = ALIGN(8);
        __bss_end = .;
    }

    /* 起動用のスタック 64KiB(下に向かって伸びる) */
    . = ALIGN(16);
    __stack_bottom = .;
    . += 0x10000;
    __stack_top = .;

    /DISCARD/ : { *(.comment) *(.eh_frame*) }
}
