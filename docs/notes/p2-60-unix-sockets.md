# フェーズ2-60 ネットワーク(2):名前でつなぐ AF_UNIX ソケット

## 何をしたか

- システムコール:`socket`(198)、`bind`(200)、`listen`(201)、`accept`(202)/`accept4`(242)、`connect`(203)、`getsockname`(204)、`getpeername`(205)、`shutdown`(210)
- `net::Socket` トレイト:bind・listen・accept・connect などの操作。`File::as_socket` で fd からたどる(ソケットでない fd は ENOTSOCK)
- `net::Namespace` トレイト:AF_UNIX がパスのソケットを作ったり探したりするための口。ネットワークのコアは VFS もプロセスも知らず、システムコールの層(`ProcessNamespace`)が実装して渡す(ポート&アダプター)
- AF_UNIX の名前は 2 種類
  - パス(`/tmp/srv`):bind でソケットのファイル(`S_IFSOCK`)を作り、(st_dev, inode 番号) で見分ける。名前付きパイプと同じ考え方。つなぐにはファイルへの書き込み権限が要る
  - 抽象名(先頭が `\0`):ファイルを作らず、名前のバイト列で見分ける(Linux 独自。systemd などが使う)
- つなぎ方

  ```text
  bind    名前 → Binding を登録表(BOUND)に入れる
  listen  Binding に待ち行列(Listener)を付ける
  connect Stream を 1 組作り、片方を相手の待ち行列に入れてすぐ戻る(accept を待たない)
  accept  待ち行列から 1 つ取り出し、新しいソケットにする
  ```

  ソケットを閉じると Binding が消え(`Drop`)、その名前にはつながらなくなる(ECONNREFUSED)
- `shutdown`:書く側(SHUT_WR)を閉じると相手は終わり(0)を読み、読む側(SHUT_RD)を閉じると相手の書き込みは EPIPE
- ファイルシステム:tmpfs と ext2 にソケットのファイル(ext2 の inode の種類 0xC000、ディレクトリ項目の種類 6)。`mknodat` の `S_IFSOCK` も作れる。ソケットのファイルを `open` すると ENXIO
- エラー番号:ENOTSOCK、EADDRINUSE、EISCONN、ENOTCONN、ECONNREFUSED

## 互換テストで分かったこと

bind 済みのソケットをもう一度 bind すると、Linux は普通 EINVAL を返すが、**パスがもうあると EADDRINUSE** になる。Linux はまずパスにファイルを作ってから「もう名前があるか」を確かめるため。抽象名ではこの順番が逆(EINVAL が先)。Pi-OS も同じ順番にした。つながっていないストリームを読むと EINVAL、書くと ENOTCONN という違いも Linux と同じ

## なぜ必要か

AF_UNIX は、関係のないプロセスどうしがパスでつながる方法。サービスマネージャ、データベース(PostgreSQL)、Python の `multiprocessing` などが使う。そして TCP と同じ「bind → listen → accept / connect」の形をしているので、次の AF_INET(ループバック)で同じ `Socket` トレイトをそのまま使える

## まだのこと

- SOCK_DGRAM、SOCK_SEQPACKET、`sendmsg`/`recvmsg`(fd の受け渡し、SCM_RIGHTS)
- 名前のないソケットの listen(Linux は自動で抽象名を付ける)
- ext2 で inode 番号が使い回されたとき:ソケットのファイルを消してから同じ番号で別のソケットを作ると、古い登録に当たりうる(Linux は bind したソケットが inode を握っているので起きない)
- `poll` で書けるかどうか(今は読めるかだけ)
