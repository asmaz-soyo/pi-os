# CPython

- `Python-3.15.2.tar.xz`:https://www.python.org/ftp/python/3.15.2/ の公式 tarball。
  sha256 は python.org のリリースページに載っている値と一致することを確かめた
  (3.15 からは GPG の署名ではなく sigstore になっている)
- `config.site`:クロスビルドでは configure が実機で確かめられない項目の答え
- `user/build.rs` が次の順にビルドする(数分かかる)
  1. ホスト用の Python 3.15(クロスビルドには同じ版の Python が要る)
  2. aarch64-linux-musl の静的リンクの `python`(zig cc)。拡張モジュールは全部本体に組み込む
     (`MODULE_BUILDTYPE=static`)。ssl、zlib、sqlite3 など外部のライブラリが要るものはなし
  3. `cpython-root/`(`usr/bin/python3` と `usr/lib/python3.15/`。テストや GUI は除く)
- `cargo xtask` がディスクのルート(ext2)に入れる。大きいので initramfs には入れない(Pi の実機ではまだ動かない)
