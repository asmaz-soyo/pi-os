# フェーズ2-77 virtio の共通化

## 何をしたか

virtio-blk と virtio-net のドライバに同じものが 3 つ重なっていたので、新しいクレート `drivers/virtio-mmio` にまとめた。

| 共通にしたもの | 中身 |
| --- | --- |
| レジスタの番地と状態のビット | MAGIC、VERSION、DEVICE_ID、QUEUE_*、STATUS_* など |
| `negotiate` | 初期化の前半。目印("virt")・版(2)・種類を確かめ、リセット → ACK → DRIVER → 機能の取り決め → FEATURES_OK を読み直して確かめる |
| `setup_queue` / `driver_ok` | キューの大きさと 3 つの番地(記述子、avail、used)を教えて READY。最後に DRIVER_OK |
| `Virtqueue` | ドライバ側の数え方。`desc`(記述子を書く)、`offer`(avail に載せる)、`take_used`(used から取り出す)、`in_flight` |
| `ack_interrupt` | 割り込みの理由を読み、同じ値を書いて消す |

ドライバに残ったのは、それぞれのデバイスに固有のことだけ:

- virtio-blk:要求のヘッダ(種類、セクタ)・データ・状態の 3 つの記述子の鎖、容量の設定
- virtio-net:受信のバッファを全部先に渡しておくこと、12 バイトの `virtio_net_hdr`、MAC アドレス

## 機能の取り決め(feature negotiation)

デバイスは「できること」をビットで見せ(DEVICE_FEATURES)、ドライバは「使うこと」をビットで返す(DRIVER_FEATURES)。64 ビットあるので、SEL で上下 32 ビットを切り替えて読み書きする。

```text
DEVICE_FEATURES_SEL = 0 → DEVICE_FEATURES を読む(下位 32 ビット)
DRIVER_FEATURES_SEL = 0 → DRIVER_FEATURES = 欲しいもの & デバイスが持つもの
DRIVER_FEATURES_SEL = 1 → DRIVER_FEATURES = 1(ビット 32 = VIRTIO_F_VERSION_1:新しい形の virtio)
STATUS |= FEATURES_OK → 読み直して FEATURES_OK が残っていれば、デバイスが受け入れた
```

virtio-blk は下位で何も求めない(0)。virtio-net は MAC(ビット 5)だけ。

## 確かめたこと

- `virtio-mmio` のホストのテスト:取り決めの書き込みの順番、違うデバイス・古い版・断られたとき、キューの番地(上位 32 ビットを含む)、avail の idx が数え続けて場所が一周すること、used からの取り出し、in_flight
- 書き換えの安全網:virtio-blk(5 つ)と virtio-net(6 つ)の今までのテストが、そのまま通る
- QEMU virt の結合テスト(ディスクとネットワーク)

## まだのこと

- virtio-blk も割り込みで完了を知る(今はポーリング。システムコールの待ちを待ち行列にするときに)
