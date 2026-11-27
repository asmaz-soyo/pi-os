# フェーズ2-67 ネットワーク(9):DNS を musl に教える

## 何をしたか

- `/proc/pios/resolv.conf`:DHCP でもらった DNS サーバーを `nameserver 10.0.2.3` の形で出す(`inet::dns_servers` が全インターフェースのものを重ねずに集める)
- `/etc/resolv.conf` は `/proc/pios/resolv.conf` へのシンボリックリンク(`user/rootfs/etc/resolv.conf`)
  - systemd-resolved が `/etc/resolv.conf` → `/run/systemd/resolve/...` にするのと同じ考え。DHCP が値を変えても、ファイルを書き直す人がいらない
  - `user/build.rs` は rootfs の中のリンクをたどらずにリンクのまま tar に入れるようにした(ディスクの ext2 にも、xtask がリンクのまま写す)
- `sendmsg`(211)と `recvmsg`(212):musl の DNS の問い合わせ(`res_msend`)は `recvmsg` で答えを受け取る
  - iovec のバッファをつないで 1 回の `send_to` にする/1 回の `recv_from` で受け取って順に分けて書く
  - 補助データ(`msg_control`。SCM_RIGHTS で fd を渡すなど)はまだ扱わない。送るときに付いていれば EOPNOTSUPP、受け取るときは `msg_controllen = 0`
- `resolve <名前>`:musl の `getaddrinfo` で名前を引くだけのプログラム

## DNS はカーネルに入れない

名前を引くのはユーザー空間(musl)の仕事。カーネルが持つのは「どのサーバーに聞けばいいか」の値だけで、それをファイルとして見せる。問い合わせは、ふつうの UDP のソケットで musl が 10.0.2.3:53 に送る。QEMU のユーザーモードのネットワーク(slirp)がそれをホストの DNS に取り次ぐ。

## 確かめたこと

- ホスト:`resolv_conf` の形、`sendmsg` で 2 つのバッファをつなぎ、`recvmsg` で 4 バイトと 16 バイトに分けて受け取る、補助データは EOPNOTSUPP
- 互換テスト:`sendmsg` / `recvmsg` の結果が Linux と同じ
- QEMU:`cat /etc/resolv.conf` が `nameserver 10.0.2.3`、`resolve localhost` が 127.0.0.1(`/etc/hosts` がないので DNS で引いている。ホストの resolver が答える)。手で試したら `resolve example.com` もインターネットのアドレスを返した

## 観察:ジッタが揺れる

QEMU のテストを何回か回すと、リアルタイムのタスクのジッタの最大が 1.6ms〜6.1ms と揺れた(限度は 5ms)。ホストの忙しさで QEMU の時間の進み方が変わるうえ、ネットワークの poll のスレッドが途中で切り替わらないため。受信の割り込みとカーネルのプリエンプションで直す。

## まだのこと

- ~~pcap での保存~~ → p2-68
- 受信の割り込み
- カーネルのプリエンプション
