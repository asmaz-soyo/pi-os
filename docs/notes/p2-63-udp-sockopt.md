# フェーズ2-63 ネットワーク(5):UDP、sendto / recvfrom、setsockopt / getsockopt

## 何をしたか

- AF_INET の UDP(`SOCK_DGRAM`)。中身は smoltcp の UDP ソケット
  - bind しないで送ると、最初に送るときにポートが自動で振られる(Linux と同じ)
  - データグラムの切れ目はそのまま。バッファより長いものは切られ、残りは捨てられる
  - connect は宛先を覚えるだけ(パケットは送らない)。そのあとは read / write が使える。宛先なしの write は EDESTADDRREQ
  - TCP と UDP のポートは別々(同じ番号を両方で bind できる)
- `sendto`(206)、`recvfrom`(207):宛先を付けて送る、送り元を受け取る。`MSG_DONTWAIT` ならそのときだけ待たない。TCP では write / read と同じ
- `setsockopt`(208)、`getsockopt`(209):int の値の設定(SO_REUSEADDR、SO_KEEPALIVE、SO_SNDBUF、TCP_NODELAY など)を覚えて返す。SO_TYPE と SO_ERROR はソケットから答える。知らない名前は ENOPROTOOPT。今は設定でふるまいは変わらない
- `Socket` トレイトを `File` の上に置いた(`Socket: File`)。sendto / recvfrom の既定は、ファイルの write / read
- 互換テスト:UDP と設定のケースを足し、Linux と一致

## TCP と UDP の違い(ソケットから見て)

```text
TCP  connect で 3 ウェイ・ハンドシェイク  バイトの流れ(切れ目なし)  届くまで再送
UDP  connect は宛先を覚えるだけ          1 回の送りが 1 つのまとまり  届かなければそれまで
```

## まだのこと

- 外とつながる(virtio-net)、DHCP
- `sendmsg` / `recvmsg`、設定でふるまいを変える(SO_RCVBUF など)
