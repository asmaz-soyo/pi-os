//! 電源投入(または QEMU の `-kernel`)後に最初に実行されるコード。
//!
//! Rust のコードを呼べる状態(スタックがある、`.bss` が 0、FP/SIMD が使える)を作ってから、
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
