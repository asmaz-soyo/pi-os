//! 電源投入(または QEMU の `-kernel`)後に最初に実行されるコード。
//!
//! EL1 まで降り、Rust のコードを呼べる状態(スタックがある、`.bss` が 0、FP/SIMD が使える)を作ってから、
//! BSP が定義する `kernel_main` に飛ぶ。リンカスクリプトが次のシンボルを定義すること:
//! `__stack_top`、`__bss_start`、`__bss_end`(どちらも 8 バイト境界)。

core::arch::global_asm!(
    r#"
    .section .text.boot, "ax"
    .global _start
_start:
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

    // スタックを用意する
    ldr     x1, =__stack_top
    mov     sp, x1

    // .bss を 0 で埋める(初期値なしの static 変数が 0 であることを保証する)
    ldr     x1, =__bss_start
    ldr     x2, =__bss_end
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

    // x0 にはブートローダが渡した DTB のアドレスが入っている。そのまま渡す
    bl      kernel_main

.Lpark:
    wfe
    b       .Lpark
"#
);
