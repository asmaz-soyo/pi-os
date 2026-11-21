# フェーズ2-59 ネットワーク(1):ソケットの入口と AF_UNIX の socketpair

計画書の項目7(ネットワーク)の最初。順番は「AF_UNIX+ループバック → virtio-net+smoltcp → DHCP → 自作 TCP …」。

## 何をしたか

- `kernel/src/net/`:ネットワークのコア
  - `ProtocolFamily` トレイト:AF_UNIX、AF_INET などの実装の形。`socket(AF_xxx, …)` の最初の引数で、どの実装に渡すかを決める(計画書の「差し込み口」の1つ目)
  - 登録表:最初から入っている AF_UNIX と、あとから `net::register` で足すもの。ない番号は `EAFNOSUPPORT`
  - `split_type`:`type` を種類(SOCK_STREAM など)とフラグ(SOCK_NONBLOCK、SOCK_CLOEXEC)に分ける
- `net/unix.rs`:AF_UNIX のストリーム。つながった 1 組は、向きの違うパイプ 2 本でできている

  ```text
   端 A  tx ──(パイプ 1)──> rx  端 B
         rx <──(パイプ 2)── tx
  ```

  片方を閉じると、相手は読むと 0(終わり)、書くと EPIPE。パイプの規則がそのまま使える
- `socketpair`(199)。作ったソケットは普通の `File` なので、`read`/`write`/`poll`/`close` はファイルと同じ道を通る
- `FileKind::Socket`:`fstat` で `S_IFSOCK`、`getdents64` で `DT_SOCK`
- エラー番号:EPROTONOSUPPORT、ESOCKTNOSUPPORT、EOPNOTSUPP、EAFNOSUPPORT
- テスト:システムコールのテスト(双方向、閉じたとき、S_IFSOCK、SOCK_CLOEXEC、エラー)、互換テスト(Linux と一致)

## なぜ必要か

ソケットは、ネットワークでも同じマシンの中でも使う「つなぐ」ための共通の形。socketpair は親子のプロセスが双方向でやりとりするのに使う(パイプは一方通行)。まず一番簡単な形でソケットの入口と登録表を作り、次に名前(パス)でつなぐ AF_UNIX、その次に AF_INET を同じ登録表に足していく

## カーネル内で何が起きているか

```text
socketpair(AF_UNIX, SOCK_STREAM, 0, sv)
  → net::family(AF_UNIX)       登録表から実装を引く
  → Unix::socketpair            パイプ 2 本で Stream を 2 つ作る
  → fd 表に入れて sv[0], sv[1] に書く
write(sv[0]) → Stream::write → パイプ 1 の書く側
read(sv[1])  → Stream::read  → パイプ 1 の読む側
```

## まだのこと

- `socket`/`bind`/`listen`/`accept`/`connect`(パスでつなぐ AF_UNIX)
- SOCK_DGRAM、`shutdown`、`sendmsg`/`recvmsg`(fd の受け渡し)
