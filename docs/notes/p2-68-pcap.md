# フェーズ2-68 ネットワーク(10):pcap でパケットを記録する

## 何をしたか

- `net::capture::Capture`:1 つのインターフェースで行き来したフレームを取っておき、pcap 形式にする
  - 形式:頭 24 バイト(目印 `0xa1b2c3d4` = マイクロ秒の時刻、版 2.4、切り詰めの長さ 65535、リンクの種類)+ フレームごとに 16 バイトの頭(秒、マイクロ秒、取った長さ、元の長さ)と中身
  - リンクの種類:eth0 は 1(Ethernet)、ループバックは 101(LINKTYPE_RAW。IP のパケットそのもの)
  - 中身の合計が 256KiB を超えたら古いものから捨てる(リング。`dropped` が捨てた数)
- 記録する場所は `DevAdapter`(smoltcp と `NetDev` のあいだ)。受け取ったフレームは `receive` で、送るフレームは `TxToken::consume` で記録する。時刻は smoltcp が渡す `Instant`
- `/proc/pios/net/<名前>.pcap`:読んだ時点の記録(`lo.pcap`、`eth0.pcap`)。インターフェースの名前は `inet::attach` で付ける
- `cargo xtask disk-get <Pi-OS のパス> <ホストのパス>`:QEMU を終えたあと、virt のディスクのルート(ext2)からファイルを取り出す(`debugfs -R dump`)

## 使い方(Wireshark で見る)

```
$ cargo xtask run
pi-os:/$ resolve localhost
pi-os:/$ cp /proc/pios/net/eth0.pcap /eth0.pcap
pi-os:/$ exit
$ cargo xtask disk-get /eth0.pcap target/eth0.pcap
$ tcpdump -nr target/eth0.pcap     # か wireshark target/eth0.pcap
```

やってみたら、DHCP の 4 往復(DISCOVER → OFFER → REQUEST → ACK)、DNS サーバーの MAC を聞く ARP、musl の DNS の問い合わせ(`A? localhost.`)と答え(`A 127.0.0.1`)が見えた。

## なぜカーネルで取るのか

QEMU にも `-object filter-dump` でホスト側から取る方法がある。カーネルで取ると、(1) ループバック(QEMU の外に出ない)も見える、(2) 実機でも同じように取れる、(3) smoltcp が本当に何を送ったか(ドライバの手前)が見える。

## つまずいたこと:procfs のファイルの大きさは 0

QEMU のテストで `od` を使おうとしたら、BusyBox の `od` は小さい版で `-A` などがなかった。`hexdump -s` に替えると、今度は何も出なかった。procfs のファイルは大きさ 0 に見せている(Linux も同じ)ので、hexdump は「飛ばす量 ≥ 大きさ」だとファイルを丸ごと飛ばす。パイプで渡すと、今度は lseek できずに止まる(これも Linux と同じ)。ファイル全体を 16 進の文字列にしてから、シェルで切り出すことにした。

## まだのこと

- 受信の割り込み
- カーネルのプリエンプション
