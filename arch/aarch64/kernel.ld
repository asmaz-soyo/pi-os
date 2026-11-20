/*
 * AArch64 カーネル共通のリンカスクリプト(higher half 配置)。
 *
 * BSP の link.ld が次を定義してから INCLUDE する:
 *   KERNEL_BASE        カーネルを置く物理アドレス
 *   BOOT_DEVICE_START  起動用ページテーブルで、これ以上の物理アドレスをデバイスとして扱う
 *
 * カーネルは仮想アドレス KERNEL_VBASE + 物理アドレス で動くようにリンクする(VMA)。
 * ファイル上の置き場所(LMA、QEMU やファームウェアが読み込む物理アドレス)は物理アドレスのまま。
 */
KERNEL_VBASE = 0xFFFF000000000000;

/* ELF の入口は物理アドレス(MMU が無効な状態で飛んでくるため) */
ENTRY(_start_phys)

SECTIONS
{
    . = KERNEL_VBASE + KERNEL_BASE;
    __kernel_start = .;

    .text : AT(ADDR(.text) - KERNEL_VBASE) {
        KEEP(*(.text.boot))   /* _start を先頭に置く */
        *(.text .text.*)
    }

    . = ALIGN(4096);
    .rodata : AT(ADDR(.rodata) - KERNEL_VBASE) { *(.rodata .rodata.*) }

    . = ALIGN(4096);
    .data : AT(ADDR(.data) - KERNEL_VBASE) { *(.data .data.*) }

    . = ALIGN(4096);
    .bss (NOLOAD) : AT(ADDR(.bss) - KERNEL_VBASE) {
        __bss_start = .;
        *(.bss.boot_tables)   /* 起動用ページテーブル(4KiB 境界) */
        *(.bss .bss.*)
        *(COMMON)
        . = ALIGN(8);
        __bss_end = .;
    }

    /* 起動用のスタック 64KiB(下に向かって伸びる) */
    . = ALIGN(4096);
    __stack_bottom = .;
    . += 0x10000;
    __stack_top = .;

    . = ALIGN(4096);
    __kernel_end = .;

    /DISCARD/ : { *(.comment) *(.eh_frame*) }
}

_start_phys = _start - KERNEL_VBASE;
