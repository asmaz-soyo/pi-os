# フェーズ2-69 ネットワーク(11):受信の割り込み

## 何をしたか

- virtio-net の割り込みを使う。フレームが届くと割り込みが来て、眠っている poll のスレッドをすぐ起こす(今までは、次に起きる時刻(最大 50ms 後)まで、だれも気づかなかった)
  - `virtio_net::ack_interrupt`:理由(InterruptStatus、0x060。1 = キューに使い終わったものがある)を読み、同じ値を InterruptACK(0x064)に書いて消す
  - `boot::net_irq`:応えてから `sched::wake(poll のスレッド)`。smoltcp の poll は重いので、割り込みの中ではしない(割り込みの中は短く。重い仕事はスレッドに任せる。Linux の「上半分/下半分」と同じ考え)
  - 割り込みの番号:QEMU virt の virtio-mmio の枠 i は SPI 16 + i、GIC の番号では 48 + i(`Board::virtio_irq`)。QEMU は eth0 を枠 30 に置くので 78 番
- `sched::wake(tid)`:眠っているタスクを、起きる時刻の前に起こす
  - 眠る前に起こされた(割り込みが、スレッドが poll している最中に来た)ときは、印(`woken`)を付けておき、次の眠りをすぐ終わらせる。印を見るのはスケジューラのロックの中なので、「見た直後に割り込みが来て、そのまま眠り込む」ことがない(起こしたことを失わない。lost wake-up)
- `/proc/interrupts`:割り込みごとの番号・届いた回数・名前(Linux と同じ並び)。`IrqTable::register` に名前を渡すようにした

```
pi-os:/$ cat /proc/interrupts
           CPU0
  30:       3025  timer
  39:          0  gpio
  78:          5  virtio-net
```

`resolve example.com` の前後で、virtio-net の回数が 3 増えた(ARP の答え、DNS の答えなど)。

## 確かめたこと

- ホスト:割り込みの回数と名前、`/proc/interrupts` の形、virtio-net の応え方(FakeMmio)
- QEMU(両方の BSP):10 秒眠るタスクが `wake` ですぐ起き、眠る前の `wake` も失われない
- QEMU virt:DHCP のあと、`/proc/interrupts` の virtio-net の回数が 0 より大きい

## まだのこと

- システムコールで待っているプロセス(`recv` など)は、まだ `sched::relax` で回りながら待っている。割り込みで起こす待ち行列にすると、CPU を使わずに待てる
- 割り込みを使うのは eth0 だけ
- ~~カーネルのプリエンプション~~ → p2-70
