# ADR-0006: TCP/IP は最初に smoltcp を使い、あとで自作と差し替えて比べる

- 状態:採用
- 日付:2026-11-21

## 背景

計画書の項目7(ネットワーク)。実装順は「AF_UNIX+ループバック → virtio-net+smoltcp → DHCP → 自作 TCP(smoltcp 版と差し替えて比較)→ IPv6 …」。IP・ARP・TCP・UDP・DHCP を一度に自作すると、ソケットやドライバの不具合と、プロトコルの不具合の区別がつきにくい。

## 決定

- TCP/IP スタックは、まず smoltcp 0.14(`no_std`、`alloc` あり)を使う。機能は Ethernet と IP のリンク、IPv4、TCP、UDP、DHCPv4 だけ
- smoltcp はカーネルの中の 1 か所(`kernel/src/net/stack.rs`)にまとめ、外からは使わない
  - 下はポート `hal::net::NetDev`(フレームの送り受けだけ)。ドライバは smoltcp を知らない
  - 上は `net::ProtocolFamily` / `net::Socket`。システムコールは smoltcp を知らない
- 自作の TCP を作るときは、`Stack` の中身を差し替え、同じテスト(ホストのテスト、互換テスト)を通す

## 理由

- 先に動くものがあると、ソケットのシステムコール、virtio-net のドライバ、pcap での観察を、正しい TCP の上で作って確かめられる。自作の TCP はそのあと、同じ環境でパケットを見比べながら作れる
- smoltcp はヒープなしでも動く設計で、OS に依存せず、`poll` を呼ぶ側が時刻を渡す形なので、カーネルにもホストのテストにも入れやすい。ライセンスは 0BSD
- 他の案
  - 最初から自作:学習には良いが、不具合の切り分けが難しい。差し替えて比べる計画にも、比べる相手が要る
  - lwIP(C):ビルドが複雑になり、Rust の型の恩恵がない

## 影響

- 依存クレートが増える(smoltcp と、heapless、managed、byteorder、bitflags、cfg-if など小さいもの)
- smoltcp は自分では動かないので、カーネルが `poll` を呼ぶ仕組み(タイマーと受信の割り込み)が要る
- smoltcp の API(`SocketSet`、`Interface`)が Linux のソケットと違うところ(listen したソケットが 1 本の接続にしかならない、など)は、`net` の層で埋める
- 差し替え方(TCP だけを raw ソケットで横に置き、ソケットごとに選ぶ)は ADR-0008
