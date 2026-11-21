# musl

- `musl-1.2.6.tar.gz`:https://musl.libc.org/releases/ の公式 tarball。`.asc` は同じ場所の署名
  (手元に musl の公開鍵がないので、署名はまだ検証していない)。`.sha256` は取ってきたときの値
- `user/build.rs` が共有ライブラリ `libc.so` をビルドし、`/lib/ld-musl-aarch64.so.1` に置く。
  musl では libc.so がそのまま動的リンカも兼ねる
- 静的リンクのプログラムは、これまでどおり zig に付いてくる musl を使う
