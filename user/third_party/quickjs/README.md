# QuickJS(第三者のソース)

- 入手元:https://bellard.org/quickjs/quickjs-2026-06-04.tar.xz
- 版:2026-06-04
- SHA-256(tar.xz):`b376e839b322978313d929fd20663b11ba58b75df5a46c126dd19ea2fa70ad2a`
- ライセンス:MIT(`LICENSE`)

`qjs` のビルドに必要なファイルだけを、変更せずにそのまま置いている。
ビルドは `user/build.rs` が行う(Makefile は使わない)。

1. ホスト向けに `qjsc` をビルドし、`repl.js` から `repl.c`(バイトコード)を作る
2. `qjs.c`、`repl.c`、`quickjs.c`、`dtoa.c`、`libregexp.c`、`libunicode.c`、`cutils.c`、`quickjs-libc.c` を
   `zig cc -target aarch64-linux-musl -static` でリンクして `/bin/qjs` にする
