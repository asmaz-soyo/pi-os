# ADR-0008: 自作の TCP は raw ソケットで smoltcp の横に置き、ソケットごとに選ぶ

- 状態:採用
- 日付:2026-11-27

## 背景

ADR-0006 で「smoltcp の TCP を自作と差し替えて比べる」と決めた。差し替え方は 3 つ考えられる。

1. smoltcp を全部やめて、IP・ARP・DHCP・UDP・TCP を一度に自作する
2. ビルドの設定(cargo の feature)で、どちらかの TCP を選ぶ
3. smoltcp は IP より下と UDP・DHCP に使い続け、TCP だけを自作にして、横に並べる

## 決定

3 を選ぶ。

- 自作の TCP(`kernel/src/net/tcp/`)は IP を知らない(セグメント、接続、接続の表)
- 各インターフェースに TCP の raw ソケット(smoltcp の `socket-raw`)を付け、届いた TCP の IP パケットを自作の `Engine` に渡す。送るものには IPv4 のヘッダを付けて raw ソケットに入れる(ARP と経路は smoltcp)
- smoltcp の TCP と同時に動く。ソケットごとに選ぶ:
  - `socket(AF_INET, SOCK_STREAM, 253)`:自作(253 は IANA の実験用のプロトコル番号、RFC 3692)
  - protocol 0 / 6:既定の実装。既定は `/proc/sys/net/pios/tcp` に `smoltcp` か `own` を書いて変える(Linux の sysctl と同じ形。変わるのは、そのあとに作るソケット)
- 当てはまる接続がない TCP に RST を返すのは、そのポートを smoltcp の TCP のソケットが使っていないときだけ(使っていれば smoltcp に任せる)

## 理由

- 1 は不具合の切り分けが難しい(ADR-0006 の理由と同じ)。TCP だけなら、下は動いているもののまま比べられる
- 2 はビルドを 2 通り持つことになり、CI が倍になる。同じカーネルの中で同時に動けば、同じテストを 2 つの実装で並べて回せ、自作のクライアントと smoltcp のサーバー(とその逆)を線の上で話させることもできる
- ソケットごとに選べるので、並んで動くホストのテストでも、テストごとに実装を決められる。既定の切り替え(sysctl)は、BusyBox や Python のような、protocol 0 で作るふつうのプログラムを自作の TCP で動かすため

## 影響

- raw ソケットが TCP を受けると、smoltcp は RST を返さなくなる。だれも使っていないポートへの SYN に RST を返すのは自作の `Engine` になる(smoltcp の TCP だけを使っていても)
- 「smoltcp が使っているポート」はポートの表ではなく、smoltcp の TCP のソケットそのものから数える(close して表から外れても FIN のやりとりが残っているもの、待ち受けを閉じたあとの accept した接続があるため)
- 253 は Linux にはない。Linux で動かすプログラムは使わないこと(互換テストでは使わない)
