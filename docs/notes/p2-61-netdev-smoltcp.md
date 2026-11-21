# フェーズ2-61 ネットワーク(3):NetDev のポート、ループバック、smoltcp

## 何をしたか

- smoltcp 0.14 を入れた(ADR-0006)。機能は IPv4、TCP、UDP、DHCPv4、Ethernet と IP のリンクだけ
- `hal::net::NetDev`:ネットワークデバイスのポート。フレームを 1 つ送る(`send`)、届いたものを 1 つ読む(`recv`)、リンクの種類(`LinkType::Ethernet` / `Ip`)、MTU。計画書の差し込み口の 3 つ目で、`link_type` で Ethernet 以外にも対応する
- `net::loopback::Loopback`:送ったフレームがそのまま自分に届くデバイス(リンクの種類は IP)
- `net::stack::Stack`:`NetDev` と smoltcp をつなぐ
  - `DevAdapter`:`NetDev` を smoltcp の `Device` に見せる。受け取りは「読んでおいたフレームを渡す」、送りは「smoltcp が書いたバッファをそのまま `send`」
  - `Stack::poll(now_ns)`:届いたフレームを処理し、送るもの(データ、ACK、再送)を送る
- テスト:ループバックの上で TCP の 3 ウェイ・ハンドシェイクをして、両方向にデータを送る。送りを止めると失敗することも確かめた

## カーネル内で何が起きているか

```text
client.send_slice("hello")
Stack::poll
  → smoltcp が TCP のセグメントを作り、IP のパケットにする
  → TxToken::consume → Loopback::send(キューに入る)
Stack::poll(次の回)
  → DevAdapter::receive → Loopback::recv(キューから出る)
  → smoltcp が IP → TCP と解き、server のバッファに入れて ACK を送る
server.recv_slice → "hello"
```

smoltcp は自分では何もしない。`poll` を呼んだときだけ動く。時刻も呼ぶ側が渡す(再送のタイマーなどに使う)

## まだのこと

- AF_INET のソケット(システムコールから smoltcp の TCP/UDP を使う)
- カーネルが `poll` を呼ぶ仕組み(今はテストが手で呼んでいる)
- virtio-net、DHCP、pcap
