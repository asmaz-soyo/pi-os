# フェーズ2-76 自作 TCP(4c):既定の切り替えと、QEMU で比べる

## 何をしたか

- `/proc/sys/net/pios/tcp`:読むと既定の TCP の実装(`smoltcp` か `own`)、書くと変わる(それ以外は EINVAL)。Linux の `/proc/sys/net/ipv4/...` と同じく「読むと今の値、書くと設定」のファイル。procfs で初めての書けるファイル(`SysFile`)。シェルの `echo own > ...` は O_TRUNC で開くので、切り詰めは何もせずに受ける
- 変わるのは、そのあとに作る protocol 0 / 6 のソケット。だから BusyBox の nc や Python も、そのまま自作の TCP で動く
- `/proc/pios/tcp`:自作の TCP の待ち受けと接続の一覧(状態は netstat と同じ名前、RTO はミリ秒)

```
pi-os:/$ cat /proc/pios/tcp
local                  remote                 state        rto_ms
0.0.0.0:7310           -                      LISTEN       -
127.0.0.1:7310         127.0.0.1:49153        ESTABLISHED  200
127.0.0.1:49153        127.0.0.1:7310         ESTABLISHED  200
```

- `tcpbulk <ポート> <バイト数> [プロトコル]`:ループバックで大きなデータを流すプログラム。fork して子がサーバー、親がクライアント。親は書き終わったら `shutdown(SHUT_WR)` し、子は EOF まで読んで数と中身(i % 251 の並び)を確かめる

## nc で大きなデータを流せなかったわけ

最初は BusyBox の `nc -l < /dev/null` と `seq 50000 | nc` で比べようとしたが、smoltcp でも途中で切れた(5000 行のうち 283 行など)。TCP の不具合ではなかった:

- サーバーの nc は標準入力(/dev/null)がすぐ終わるので、すぐ `shutdown(SHUT_WR)` で FIN を送る
- BusyBox の nc のクライアントは、ネットワークの EOF(その FIN)を読むと、送り終わっていなくても終わる

小さいデータなら FIN が届く前に書き終わるので、今までのテストは通っていた。Linux でも同じことが起きる。だから、片方向に流して閉じ方を決めた専用のプログラム(tcpbulk)にした。

## 比べた結果(QEMU virt、256KB、ループバック)

| 実装 | 時間 | 速さ |
| --- | --- | --- |
| smoltcp | 132 ms | 約 1974 KB/s |
| 自作 | 268 ms | 約 975 KB/s |

自作は半分くらい。ただし公平な比べ方ではない:

- smoltcp は dev ビルドでも最適化している(`opt-level = 2`)。カーネル(自作の TCP)は最適化なし
- 自作には遅延 ACK がなく、セグメントごとに ACK を返す。送るバッファは `VecDeque` から 1 バイトずつ集めている

## 確かめたこと

- ホスト:`/proc/sys/net/pios/tcp` の読み書きとだめな値、`/proc/pios/tcp` の形、`Engine::table`
- QEMU(両方の BSP):既定を自作にすると nc が自作の TCP を使う(待ち受けが `/proc/pios/tcp` に出る)、256KB が両方の実装で正しく届く
- QEMU virt:自作の TCP で、guestfwd のエコー(相手は QEMU の slirp。3 つ目の TCP の実装)と話せる

## まだのこと

- 速い再送、輻輳制御、遅延 ACK、並べ直し
- 待っているシステムコールが relax で回っている(割り込みで起こす待ち行列にする。項目7の残り)
