# フェーズ2-20 GPIO の割り込みを fd で待つ

## 何をしたか

- `drivers/pl061`:ARM PL061 GPIO のドライバ(入力・立ち上がりエッジの割り込み、要因の読み出しと消去)
- `kernel::gpio`:線ごとに割り込みの回数を数える `GPIO` と、`/dev/gpio/N`(`read` で前回からの回数を 8 バイトで返す。まだなければ待つ。epoll にも使える)
- GICv2:32 番以上の割り込み(SPI)は、届け先の CPU(`GICD_ITARGETSR`)を CPU0 にしてから有効にする
- `Board::GPIO_IRQ` / `gpio_take_pending`:virt は PL061(`0x0903_0000`、INTID 39)の GPIO 3(電源ボタン)
- `/bin/gpiowait`(C):`/dev/gpio/3` で割り込みを 1 回待つ
- スナップショット `qemu-virt-gpio`:シェルで `gpiowait` を動かし、QEMU のモニタ(`Ctrl-A c`)で `system_powerdown` を打って電源ボタンを押す

```
pi-os:/$ gpiowait
waiting for GPIO 3
(qemu) system_powerdown
GPIO 3: 1 event(s)
```

## なぜ必要か

計画書の「アプリの実行モデル」の3つ目、割り込み駆動のアプリ(`trigger = { gpio = 17, edge = "falling" }`)。カーネルは GPIO の割り込みを fd として提供し、サービスマネージャがそれを epoll で待つ形にできる(サービスマネージャ側の対応は次)。

## 中で何が起きているか

```
QEMU のモニタ:system_powerdown
  → virt の gpio-keys(電源ボタン)が PL061 の GPIO 3 を上げる
  → PL061:立ち上がりエッジで割り込みの要因(MIS のビット 3)が立つ
  → GIC:SPI 7(INTID 39)を CPU0 へ
  → IRQ ハンドラ:gpio_take_pending() で MIS を読み、IC に書いて消す → GPIO.record(ビット)
  → gpiowait の read が回数(1)を受け取って戻る
```

実機の Pi 3B の GPIO(BCM2835)のドライバは、実機で確かめるときに作る(QEMU の raspi3b では外から GPIO の入力を変えられないため)。

## 今の限界

- リアルタイム優先度とジッタの計測は、まだ

## 追記:サービスマネージャの GPIO トリガー

`svcman` に `trigger = { gpio = N }` を足した。`/dev/gpio/N` を epoll で待ち、割り込みのたびにサービスを動かす。スナップショット `qemu-virt-gpio` は、`/etc/svcman-gpio.conf`(電源ボタンで `hello` を 1 回動かす)を使うようにした。

```
pi-os:/$ svcman /etc/svcman-gpio.conf
svcman: watching 1 service(s)
(qemu) system_powerdown
Hello from user space!
svcman: button run 1 exited with 0
```

これで計画書の3種類のトリガー(スタートアップ、周期、割り込み駆動)がすべて揃った。
