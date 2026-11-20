# M2-02 割り込みコントローラと ARM Generic Timer

## 何をしたか

- `hal::irq::IrqController` と `hal::timer::Timer` のポートを足した
- `drivers/gicv2`:QEMU `virt` の GICv2 ドライバ
- `drivers/bcm2836-local-intc`:Pi 3 のコアローカル割り込みコントローラ(`0x4000_0000`)のドライバ
- `arch_aarch64::timer::GenericTimer`:CPU 内蔵のタイマ(システムレジスタで操作)
- `kernel::irq`:割り込み番号ごとのハンドラ表と振り分け(`IRQS.dispatch`)
- `kernel::time`:ティックを数える時計(`CLOCK`、100Hz)と `wait_ticks`
- `kernel::Errno`:Linux と同じ値のエラー番号
- BSP:割り込みコントローラとタイマの配線。IRQ を受け付け始める
- 起動時に「timer: 10 ticks at 100 Hz」を出す(スナップショットで確認)

## なぜ必要か

- タイマ割り込みは、スケジューラ(時間で処理を切り替える)、スリープ、`timerfd`、周期起動のアプリ(計画書の「周期」トリガー)の土台
- 割り込みの流れ「デバイス → コントローラ → CPU の例外 → カーネルの振り分け → ハンドラ」を一度通しておくと、GPIO や UART の受信割り込みも同じ形で足せる

## 中で何が起きているか

### タイマ割り込みが届くまで

```
CNTP_TVAL_EL0 に「あと N カウント」を書く
  ↓ N カウント経つ
タイマが割り込み線を上げる
  ↓ virt: GIC の PPI 30 / Pi 3: ローカル割り込みコントローラのビット 1
CPU が IRQ 例外を受ける(PSTATE.I = 0 のとき)
  ↓ VBAR_EL1 + 0x280(カーネル実行中の IRQ)
arch_exception_entry → Exceptions::irq
  ↓
kernel::irq::IRQS.dispatch(&mut INTC)
  ├ intc.pending() で番号を読む(GIC なら GICC_IAR)
  ├ 登録されたハンドラ timer_tick() を呼ぶ → CLOCK.tick() → 次の TVAL を書く
  └ intc.end_of_interrupt()(GIC なら GICC_EOIR)
```

### 2つのボードの違いと、それをどこに閉じ込めたか

| | QEMU virt | Raspberry Pi 3 |
| --- | --- | --- |
| 割り込みコントローラ | GICv2 | GIC なし。ローカル割り込みコントローラ |
| タイマの割り込み番号 | 30(PPI 14) | 1(CNTPNSIRQ のビット) |
| 終了の通知 | GICC_EOIR に書く | 不要(タイマを再設定すると下がる) |

違いはすべてドライバと BSP の配線にあり、カーネル(`kernel::irq`、`kernel::time`)は `IrqController` と `Timer` のトレイトしか知らない。カーネル側はホストで偽のコントローラとタイマを使ってテストしている。

### ロックと割り込み

- 割り込みハンドラの中では `println!` を使わない。本体が出力のロックを持っている最中に割り込みが来て、ハンドラが同じロックを取ろうとすると固まる
- 割り込み中に取るロック(`INTC`)は、割り込みハンドラと初期化でしか使っていない
- 本格的には「ロックを持っている間は割り込みを禁止する」ロックが要る。スケジューラを作るとき(フェーズ2)に足す

## 実機について

M2 の計画にある「UART 経由のブートローダ」(実機に毎回 microSD を差し替えずにカーネルを送る仕組み)は、実機で確かめられないので後回しにした。
