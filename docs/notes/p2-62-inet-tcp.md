# フェーズ2-62 ネットワーク(4):AF_INET の TCP(ループバック)

## 何をしたか

- `net::inet`:AF_INET の TCP ソケット。中身は smoltcp で、今はループバック(127.0.0.0/8)だけ
  - bind(ポート 0 なら自動で振る。49152〜65535)、listen、accept、connect、read/write、shutdown、getsockname/getpeername
  - 使っているポートを覚えておき、重なれば EADDRINUSE。このマシンのものでないアドレスへの bind は EADDRNOTAVAIL
  - 閉じたソケットは FIN を送り、やりとりが終わるまで(Closed か TIME-WAIT になるまで)面倒を見てから片付ける
- BusyBox の `nc` を有効にし、QEMU の上で「`nc -l` のサーバーと `nc` のクライアントが、別々のプロセスで TCP でつながる」結合テストを足した
- 互換テスト:TCP のケースを足し、Linux と一致

## Linux と smoltcp の違いを埋める

Linux では listen した 1 本のソケットが何本もの接続を受けるが、smoltcp では listen したソケットが、接続が来るとそのままその接続になる。そこで

```text
listen(backlog=4)   smoltcp のソケットを 4 本 listen させておく   [L][L][L][L]
SYN が来る          そのうち 1 本がつながる                        [E][L][L][L]
accept              つながった 1 本を新しい fd にし、代わりを足す  [L][L][L][L]  → fd
```

## 誰が smoltcp を動かすか

smoltcp は `poll` を呼んだときだけ動く。今は、ソケットのシステムコールが待つあいだに自分で `poll` を呼ぶ(`inet::wait`)。ループバックでは、送る側か受ける側のどちらかが必ずシステムコールの中で待っているので、これで回る。外とつながる(virtio-net)と、だれも待っていないときにもパケットが届くので、受信の割り込みかタイマーで `poll` を呼ぶ仕組みが要る

## 互換テストで分かったこと

つながっていない TCP のソケットに書くと、Linux は EPIPE を返す(AF_UNIX は ENOTCONN)。Pi-OS も同じにした

## まだのこと

- UDP、`setsockopt`/`getsockopt`(nc が SO_REUSEADDR を設定しようとする)、`sendto`/`recvfrom`
- O_NONBLOCK の connect(EINPROGRESS)、`poll` で書けるかどうか
- シェルの `wait`(rt_sigsuspend。シグナルの配送と一緒に)
