# M1-02 raspi3 BSP と EL1 への降下

## 何をしたか

- `bsp/raspi3`(`bsp-raspi3`)を作った。PL011 は `0x3F20_1000`、クロック 48MHz、カーネルは `0x8_0000` に置く
- リンカスクリプトの本体を `arch/aarch64/kernel.ld` にまとめ、BSP の `link.ld` は `KERNEL_BASE` を決めて `INCLUDE` するだけにした
- カーネル内テストの枠組みを `ktest` クレートに切り出した。どの BSP でも成り立つテストは `ktest::common_tests!()` で両方に入る
- `_start` に「EL3 / EL2 で始まったら EL1 まで降りる」処理を足した
- `cargo xtask build` で実機用の `kernel8.img`(生のバイナリ)も作るようにした
- xtask の CI は両方の BSP で QEMU のテストとスナップショットを回す

## なぜ必要か

- CLAUDE.md のルール「BSP は raspi3 と qemu-virt の2つ。どちらでもビルドとテストが通る状態を保つ」を、早いうちから CI で守らせるため
- カーネルは EL1(OS の権限)で動く前提で作る。ボードや起動方法によって始まる例外レベルが違うので、`_start` でそろえる

## 中で何が起きているか

### 例外レベル(EL)

| EL | 役割 | Pi-OS での扱い |
| --- | --- | --- |
| EL3 | セキュアモニタ | 降りるだけ |
| EL2 | ハイパーバイザ | 降りるだけ(EL1 がタイマを使えるよう設定してから) |
| EL1 | OS カーネル | カーネルはここで動く |
| EL0 | ユーザープログラム | M4 で使う |

実際に始まる EL は起動方法で違う(テストで確かめた):

| 起動方法 | 始まる EL |
| --- | --- |
| QEMU `-M virt -kernel <ELF>` | EL1 |
| QEMU `-M raspi3b -kernel <ELF>` | EL3 |
| QEMU `-M raspi3b -kernel kernel8.img` | EL2(QEMU のブートスタブ経由) |
| 実機 Pi 3B(ファームウェア) | EL2 |

`kernel_runs_at_el1` テストを先に書き、raspi3 で `left: 3`(EL3)で失敗することを確かめてから、降りる処理を足した。

### 降り方

- 「上の EL の `SPSR_ELx`(戻り先の状態)と `ELR_ELx`(戻り先のアドレス)を設定して `eret`」で1段ずつ降りる。例外から戻るときと同じ命令を、わざと使う
- EL3 → EL2:`SCR_EL3` で「下は非セキュア・AArch64」と設定し、`CPTR_EL3` で FP/SIMD をトラップしないようにする
- EL2 → EL1:`HCR_EL2.RW` で「EL1 は AArch64」と設定する。`CNTHCTL_EL2` で EL1 から物理タイマを使えるようにし(M2 で使う)、`CPTR_EL2` で FP/SIMD を許し、`SCTLR_EL1` を既知の値(MMU・キャッシュはオフ)にする

### 実機で動かすとき

`kernel8.img` を microSD の FAT パーティションにファームウェアと一緒に置き、`config.txt` に次を書く:

```
arm_64bit=1
enable_uart=1
dtoverlay=disable-bt
```

`disable-bt` で PL011 を Bluetooth から取り戻し、GPIO14/15 につなぐ。実機での確認はまだしていない。
