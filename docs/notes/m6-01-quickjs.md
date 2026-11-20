# M6-01 QuickJS:`qjs hello.js`

## 何をしたか

- QuickJS 2026-06-04 の公式リリースから、`qjs` のビルドに必要なファイルだけを `user/third_party/quickjs/` に置いた(変更なし。入手元と SHA-256 は README)
- `user/build.rs`:
  1. ホスト向けの `qjsc` を zig でビルドし、`repl.js` をバイトコード(`repl.c`)にする
  2. `qjs.c` + `repl.c` + QuickJS 本体を `zig cc -target aarch64-linux-musl -static` でリンクして `/bin/qjs`(約 1MB)にする
- `/hello.js`(`console.log("Hello from JS");`)と、`/etc/init.rc` の `/bin/qjs /hello.js` の行
- `readlinkat`(シンボリックリンクがないので、あるパスなら `EINVAL`)
- カーネル内テスト:`qjs --std -e "std.exit(6 * 7)"` の終了ステータスが 42
- `brk` のテストの不具合を直した(テスト用のハンドラに差し替えている間は IRQ を止める)

```
$ cargo xtask run
…
Hello from JS
init: /bin/qjs exited with status 0
```

**フェーズ1のゴールに届いた。** qemu-virt と qemu raspi3b の両方で動く(実機は未確認)。

## なぜ QuickJS はほとんどそのまま動いたか

M5 で musl の C プログラムのために作ったシステムコール(ファイル、mmap、時刻、端末の問い合わせ)が、そのまま QuickJS の必要なものだった。追加が要ったのは `readlinkat` だけ(`--std` を付けたときに呼ばれる)。

## 中で何が起きているか

```
カーネル:/etc/init.rc を読む → /bin/qjs を initramfs から ELF として読み込む → EL0 へ
qjs(musl):
  set_tid_address / ioctl(TIOCGWINSZ)       ← musl の初期化、標準出力は端末か
  mmap …                                    ← JS のランタイム(ヒープ)の確保
  openat("/hello.js") / fstat / read / close ← スクリプトを読む
  (JS の実行:カーネルは関わらない)
  writev(1, "Hello from JS\n")              ← console.log
  exit_group(0)
```

`cargo xtask run --trace syscall` で、この流れを実際に見られる。

## 見つかった不具合

`brk` のテストは、M2 でタイマ割り込みを入れてから、たまに固まる状態になっていた。テストのハンドラは IRQ を無視するが、Pi 3 のタイマの割り込みは「タイマを再設定するまで上がりっぱなし」なので、差し替えている間に割り込みが来ると、例外の入口に戻り続けて先に進めない。QuickJS のテストを足して QEMU のテスト全体の時間が延びたことで、表に出た。
