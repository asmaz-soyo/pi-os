# フェーズ2-25 BusyBox とパイプ

## 何をしたか

- BusyBox 1.38.0 の公式ソース(`user/third_party/busybox/`、tarball とその sha256)を入れ、`user/build.rs` でビルドして `/bin/busybox` に置いた
  - 設定は `pios.config`:`allnoconfig` から、ash と基本のコマンド(ls、cat、grep、sed、sort、wc、find、ps …)だけを有効にしたもの。静的リンクで約 300KB
  - zig cc(`aarch64-linux-musl`)でクロスビルド。BusyBox の Makefile の strip の段は使わず(`STRIP=true`)、`busybox_unstripped` を CC の `-s` で小さくして使う
  - `FEATURE_SH_STANDALONE` / `FEATURE_PREFER_APPLETS`:ash が `seq` などを PATH から探さず、BusyBox の中のアプレットを使う。別のプロセスにするときは `/bin/busybox` を exec する(`BUSYBOX_EXEC_PATH`。既定の `/proc/self/exe` はまだない)
- `pipe2`(59):パイプ。読む側と書く側は別の `File` で、同じバッファ(64KiB)を指す。最後の fd が閉じられたとき(`Drop`)に「閉じた」と記録する
  - 書く側がすべて閉じた → 読むと残りを返したあと 0(終わり)
  - 読む側がすべて閉じた → 書くと `EPIPE`
  - 種類は `FileKind::Fifo`(`lseek` は `ESPIPE`)
- `prctl`(167)の `PR_SET_NAME`:プロセスの名前を変える(BusyBox が使う)
- QEMU のテスト:`busybox echo`、ash の算術と `ls`、`n=$(seq 1 100 | grep 7 | wc -l)` が 19 になること

```
$ busybox sh -c 'n=$(seq 1 100 | grep 7 | wc -l); echo "lines with 7: $n"'
lines with 7: 19
```

## なぜ必要か

計画書のフェーズ2の2つ目「シェル(BusyBox)」。BusyBox は Linux の普通のプログラムなので、自作 OS がどれだけ Linux と同じに振る舞えるかの試金石になる。パイプはシェルの基本(`a | b`、`$(...)`)。

## 中で何が起きているか

```
ash:n=$(seq 1 100 | grep 7 | wc -l)
  → pipe2 で [r, w] を作る → fork
     子:dup2(w, 1) → seq を実行(BusyBox の中のアプレット)→ 書く側に書く → 終わる(w が閉じる)
     子:dup2(r, 0) → grep → …
  → 親:最後のパイプの読む側から読み、書く側がすべて閉じたら 0 が返る → 結果を n に入れる
```

## まだのこと

- init のシェルを BusyBox の ash にする(ジョブ制御:`setpgid`、`TIOCSPGRP` などが要る)
- アプレットの名前で呼べるようにする(`/bin/ls` → busybox)。シンボリックリンクが要る
- パイプの待ちは「譲って確かめる」形。本当の待ち行列はまだ
- SIGPIPE などのシグナルの配送
