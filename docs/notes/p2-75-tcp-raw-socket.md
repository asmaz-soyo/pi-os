# フェーズ2-75 自作 TCP(4b):raw ソケットでつなぐ

## 何をしたか

自作の TCP(`Engine`)を、本物のネットワークの道につないだ。`socket(AF_INET, SOCK_STREAM, 253)` で作ったソケットは自作の TCP を、0 や 6 は今までどおり smoltcp の TCP を使う。253 は IANA が実験用に取ってあるプロトコル番号(RFC 3692)。

```text
              送る                                         受け取る
Engine::output ──(src, dst, TCP)──→ IPv4 のヘッダを付ける    Engine::input ←── IPv4 のヘッダを外す
                                    │                                          ↑
                      宛先から選んだインターフェースの raw ソケット         各インターフェースの raw ソケット
                                    ↓                                          │
                      smoltcp:ARP で相手の MAC を引いて、フレームにして送る ── 届いたフレームの IP を見る
```

- `Stack::enable_raw_tcp`:TCP の raw ソケットを付ける。smoltcp は、届いた TCP の IP パケットを raw ソケットにも渡す。raw ソケットが受けたパケットには、smoltcp は RST を返さない(自作の TCP の接続に、smoltcp が「そんな接続はない」と RST を返してしまわないように)
- IP のヘッダは自分で 20 バイト作る(バージョン 4、TTL 64、DF、プロトコル 6)。ヘッダのチェックサムは smoltcp が入れる
- ソケットの層(`InetSocket`)は、接続を `T::Smol(..)` か `T::Own(..)` で持ち、違いは `Net` の `t_recv` / `t_send` / `t_close` などで吸収する。システムコールの道(待ち方、EPIPE、ポートの表)は同じ

## smoltcp と同時に動かす

ポートの表(`ports`)は 2 つの実装で共通。届いた TCP は smoltcp と自作の両方が見るので、RST を返すのはどちらか一方にする:

- smoltcp の TCP のソケットが実際に使っているポート(`Stack::tcp_ports`):smoltcp に任せ、自作は黙る
- それ以外(自作の TCP が使っているポート、だれも使っていないポート):当てはまる接続がなければ自作が RST を返す

最初は「ポートの表に載っているか」で決めていて、不具合が 2 つ見つかった。どちらも「smoltcp のソケットはまだそのポートを使っているのに、表からは外れている」場合で、自作が RST を返して接続を切っていた:

1. close した smoltcp のソケット:表から外れるが、FIN のやりとりが終わるまで動いている。相手(自作のサーバー)が ECONNRESET になった。見つけ方:一時的に、raw ソケットに届いたセグメントを 1 行ずつ表示した。最後の RST の向きと番号から、smoltcp ではなく自作が出したものとわかった
2. 待ち受けを閉じたあとの、accept した接続:BusyBox の `nc -l` は 1 本 accept すると待ち受けを閉じる。QEMU の結合テストで、nc のクライアントが EPIPE になった。ホストのテストにこの場面がなかったので、`an_accepted_connection_outlives_its_listener` を足した(smoltcp の側で Red)

表ではなく、smoltcp のソケットそのもの(SocketSet)を見て決めるようにして、2 つとも直った。

## 確かめたこと(ホスト)

- ループバックの TCP のテスト 7 つを、smoltcp と自作の両方で(`both!` のマクロで 1 つずつの #[test] に)
- 自作のクライアント ↔ smoltcp のサーバー、smoltcp のクライアント ↔ 自作のサーバー(線の上で 2 つの実装が話せる)
- 作り物の Ethernet の向こうのホスト(smoltcp)とのエコーを、自作の TCP でも(ARP を通る)
- テストの相手側のエコーサーバーは、待ち受けを常に 4 本置くようにした(同時に 2 本つながると、1 本しかない待ち受けの隙間に来た SYN が RST で断られていた)

## まだのこと

- ふつうのプログラム(BusyBox の wget、Python など)は protocol 0 で作るので、自作の TCP を使えない。既定の実装を切り替えられるようにする(次)
