/* QEMU -M virt 用のリンカスクリプト。RAM は 0x4000_0000 から始まる。 */
ENTRY(_start)

SECTIONS
{
    . = 0x40080000;

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
    . += 0x10000;
    __stack_top = .;

    /DISCARD/ : { *(.comment) *(.eh_frame*) }
}
