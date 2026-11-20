# M1-03 パニック時のレジスタとスタックトレース

## 何をしたか

- `kernel::panic`:フレームポインタをたどる `frames` と、報告を書く `write_report` / `report`
- `arch_aarch64::cpu::snapshot`:その場の SP・FP(x29)・LR(x30)・CurrentEL を読む
- `.cargo/config.toml`:`aarch64-unknown-none` 向けに `-C force-frame-pointers=yes`
- BSP のパニックハンドラから報告を出し、QEMU なら終了コード 1 で止める
- `panic-demo` feature:起動後にわざとパニックする。スナップショットテストで報告の形を固定した
- スナップショットに `{..}`(行内の任意の文字列)と `...`(任意の行数)を使えるようにし、期待する終了コードも指定できるようにした

```
!!! KERNEL PANIC !!!
panicked at bsp/qemu-virt/src/main.rs:33:5:
panic demo
registers:
  CurrentEL = 1  SP = 0x0000000040095670  FP = 0x00000000400956d0  LR = 0x00000000400838b4
backtrace:
  #0 0x00000000400838b4    ← core::panicking::panic_fmt の中
  #1 0x0000000040080860    ← kernel_main の中(panic! を呼んだところの直後)
  #2 0x00000000400800b4    ← _start の中(bl kernel_main の直後)
end of backtrace
```

## なぜ必要か

- 計画書の「最初から作る」4項目の1つ。カーネルの不具合は再現が難しいので、落ちた瞬間に「どこで・どういう経路で」を残せるかどうかで、デバッグのしやすさがまったく違う
- 後からフレームポインタを有効にすると、それまでに作った部分の挙動の前提が変わる。最初から入れておく

## 中で何が起きているか

### フレームレコード

フレームポインタを有効にすると、各関数の入口(プロローグ)は次のようにスタックへ「フレームレコード」を積む。

```asm
stp x29, x30, [sp, #-16]!   // 呼び出し元の FP と、戻りアドレス(LR)を積む
mov x29, sp                 // x29 = このフレームレコードのアドレス
```

つまり `[x29]` は呼び出し元のフレームレコード、`[x29 + 8]` は戻りアドレス。x29 から連結リストのようにたどれば、呼び出しの経路が分かる。

### 安全にたどるための止め方

- FP が 0、または 16 バイト境界でない
- 次の FP が今の FP より大きくない(スタックは下に伸びるので、呼び出し元は必ず上にある。ループの防止にもなる)
- 読もうとした番地がブート用スタックの範囲外(`__stack_bottom`〜`__stack_top`)
- 32 フレームを超えた

この判定はホストのテストで、偽のメモリ(`BTreeMap`)を使って確かめている。

### ロックの強制解除

パニックしたのが `println!` の途中だと、出力先のロックを持ったまま。そのまま報告を書こうとすると自分自身を待って固まる。パニックした処理はもう続きを実行しないので、報告の前にロックを強制的に外す(`console::break_lock`)。

### アドレスから関数を調べる

`llvm-objdump`(rustup の llvm-tools に入っている)で、戻りアドレスの直前の命令を見る。

```bash
T=$(rustc --print sysroot)/lib/rustlib/x86_64-unknown-linux-gnu/bin
$T/llvm-objdump -d --no-show-raw-insn target/aarch64-unknown-none/debug/bsp-qemu-virt \
  --start-address=0x4008085c --stop-address=0x40080864
```

## 今の限界

- 表示するレジスタは SP・FP・LR・CurrentEL だけ。汎用レジスタ全部の表示は、例外の入口でレジスタを保存するようになってから(M2)
- アドレスを関数名に変換するのは、ホストで手作業
