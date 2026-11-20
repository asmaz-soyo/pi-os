//! 電源投入(または QEMU の `-kernel`)後に最初に実行されるコード。
//!
//! 1. コア 0 以外を眠らせ、EL1 まで降りる
//! 2. `.bss` を 0 にし、FP/SIMD を使えるようにする
//! 3. 起動用のページテーブルを作って MMU を有効にし、上位アドレス(higher half)へ飛ぶ
//! 4. スタックを用意して BSP の `kernel_main` を呼ぶ
//!
//! MMU を有効にするまでは物理アドレスで動いているのに、リンクされているのは上位の仮想アドレス。
//! そのため 3 までは、番地を必ず PC 相対(`adr` / `adrp`)で求める(`ldr x, =シンボル` は上位の
//! アドレスになってしまうので使えない)。
//!
//! 起動用のページテーブルは、カーネルのある 1GiB を 2MiB ブロックで覆うだけの大まかなもの。
//! 同じ表を TTBR0(恒等マップ:仮想 = 物理)と TTBR1(上位:仮想 = KERNEL_VBASE + 物理)の
//! 両方に入れる。L0 と L1 の添字は、上位アドレスでも物理アドレスと同じになるため。
//! Rust に入ったら、きちんとしたテーブルを作り直す(`boot` クレートの `init_memory`)。

core::arch::global_asm!(
    r#"
    .section .bss.boot_tables, "aw", %nobits
    .balign 4096
boot_l0: .space 4096
boot_l1: .space 4096
boot_l2: .space 4096

    .section .text.boot, "ax"
    .global _start
_start:
    // x0 にはブートローダが渡した DTB のアドレスが入っている。最後まで触らない

    // コア 0 以外は眠らせる(マルチコアの起動は後のマイルストーンで)
    mrs     x1, mpidr_el1
    and     x1, x1, #0xff
    cbnz    x1, .Lpark

    // EL3 や EL2 で始まったら EL1 まで降りる。
    // QEMU -M raspi3b に ELF を渡すと EL3、実機のファームウェアは EL2、QEMU -M virt は EL1 で始まる。
    mrs     x1, CurrentEL
    lsr     x1, x1, #2
    cmp     x1, #3
    b.ne    .Lnot_el3
    msr     cptr_el3, xzr           // FP/SIMD を EL3 でトラップしない
    mov     x1, #0x5b1              // SCR_EL3: NS | RES1(4,5) | SMD | HCE | RW(下位は AArch64)
    msr     scr_el3, x1
    mov     x1, #0x3c9              // SPSR: 戻り先は EL2h、割り込みはすべてマスク
    msr     spsr_el3, x1
    adr     x1, .Lnot_el3
    msr     elr_el3, x1
    eret
.Lnot_el3:
    mrs     x1, CurrentEL
    lsr     x1, x1, #2
    cmp     x1, #2
    b.ne    .Lin_el1
    mov     x1, #(1 << 31)          // HCR_EL2.RW: EL1 は AArch64
    msr     hcr_el2, x1
    mov     x1, #0x33ff             // CPTR_EL2: RES1 ビットのみ(FP/SIMD をトラップしない)
    msr     cptr_el2, x1
    mov     x1, #3                  // CNTHCTL_EL2: EL1 から物理タイマとカウンタを使える
    msr     cnthctl_el2, x1
    msr     cntvoff_el2, xzr        // 仮想カウンタ = 物理カウンタ
    ldr     x1, =0x30d00800         // SCTLR_EL1: RES1 ビットのみ(MMU とキャッシュはオフ)
    msr     sctlr_el1, x1
    mov     x1, #0x3c5              // SPSR: 戻り先は EL1h、割り込みはすべてマスク
    msr     spsr_el2, x1
    adr     x1, .Lin_el1
    msr     elr_el2, x1
    eret
.Lin_el1:

    // .bss を 0 で埋める(初期値なしの static 変数が 0 であることを保証する)
    adrp    x1, __bss_start
    add     x1, x1, :lo12:__bss_start
    adrp    x2, __bss_end
    add     x2, x2, :lo12:__bss_end
.Lzero_bss:
    cmp     x1, x2
    b.hs    .Lbss_done
    str     xzr, [x1], #8
    b       .Lzero_bss
.Lbss_done:

    // FP/SIMD 命令をトラップしないようにする(CPACR_EL1.FPEN = 0b11)。
    // aarch64-unknown-none は SIMD レジスタを使うコードを生成することがある。
    mov     x1, #(3 << 20)
    msr     cpacr_el1, x1
    isb

    // ---- 起動用ページテーブル ----
    adrp    x2, boot_l0
    adrp    x3, boot_l1
    adrp    x4, boot_l2
    orr     x5, x3, #3              // テーブル記述子
    str     x5, [x2]                // L0[0] → L1
    adrp    x6, _start              // カーネルの物理アドレス
    lsr     x7, x6, #30             // それを含む 1GiB の番号
    orr     x5, x4, #3
    str     x5, [x3, x7, lsl #3]    // L1[番号] → L2
    lsl     x8, x7, #30             // その 1GiB の先頭の物理アドレス
    ldr     x9, =BOOT_DEVICE_START
    mov     x10, #0
.Lfill_l2:
    lsl     x11, x10, #21
    add     x11, x11, x8            // この 2MiB ブロックの物理アドレス
    cmp     x11, x9
    b.hs    .Ldevice_block
    ldr     x12, =0x705             // 通常メモリ:ブロック | Attr1 | 内部共有 | AF
    b       .Lstore_block
.Ldevice_block:
    ldr     x12, =0x0060000000000401 // デバイス:ブロック | Attr0 | AF | PXN | UXN
.Lstore_block:
    orr     x12, x12, x11
    str     x12, [x4, x10, lsl #3]
    add     x10, x10, #1
    cmp     x10, #512
    b.lo    .Lfill_l2

    // ---- MMU の設定と有効化 ----
    ldr     x5, =0xff04             // MAIR: Attr0 = Device-nGnRE、Attr1 = Normal Write-Back
    msr     mair_el1, x5
    // TCR: T0SZ=T1SZ=16(48 ビット)、4KiB グラニュール、Write-Back キャッシュ、内部共有
    ldr     x5, =0xB5103510
    mrs     x6, id_aa64mmfr0_el1    // 物理アドレスの幅(PARange)を IPS にコピーする
    and     x6, x6, #7
    bfi     x5, x6, #32, #3
    msr     tcr_el1, x5
    msr     ttbr0_el1, x2
    msr     ttbr1_el1, x2
    isb
    tlbi    vmalle1
    dsb     ish
    isb
    mrs     x5, sctlr_el1
    orr     x5, x5, #(1 << 0)       // M: MMU
    orr     x5, x5, #(1 << 2)       // C: データキャッシュ
    orr     x5, x5, #(1 << 12)      // I: 命令キャッシュ
    msr     sctlr_el1, x5
    isb

    // 上位アドレスへ飛ぶ(ここからはリンクされた仮想アドレスで動く)
    ldr     x5, =.Lhigh
    br      x5
.Lhigh:
    ldr     x1, =__stack_top
    mov     sp, x1
    bl      kernel_main

.Lpark:
    wfe
    b       .Lpark
"#
);

unsafe extern "C" {
    static __stack_bottom: u8;
    static __stack_top: u8;
    static __kernel_start: u8;
    static __kernel_end: u8;
}

/// ブート用スタックのアドレス範囲(リンカスクリプトで確保した 64KiB)。
pub fn stack_range() -> core::ops::Range<usize> {
    // アドレスを取るだけなので unsafe は要らない(中身は読まない)
    (&raw const __stack_bottom) as usize..(&raw const __stack_top) as usize
}

/// カーネルのイメージ(コード、データ、`.bss`、起動用スタック)が置かれている物理アドレスの範囲。
pub fn kernel_image_phys() -> core::ops::Range<hal::addr::PhysAddr> {
    use crate::mmu::virt_to_phys;
    use hal::addr::VirtAddr;
    let start = VirtAddr((&raw const __kernel_start) as u64);
    let end = VirtAddr((&raw const __kernel_end) as u64);
    virt_to_phys(start)..virt_to_phys(end)
}
