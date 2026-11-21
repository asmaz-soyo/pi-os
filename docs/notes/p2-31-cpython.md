# フェーズ2-31 CPython

## 何をしたか

- CPython 3.15.2 の公式 tarball(`user/third_party/cpython/`、sha256 は python.org のリリースページの値と一致)
- `user/build.rs`(初回は数分):
  1. ホスト用の Python 3.15 をビルド(クロスビルドには同じ版の Python が要る)
  2. aarch64-linux-musl の静的リンクの `python` を zig cc でビルド(約 8MB)。拡張モジュール 84 個は全部本体に組み込む(`MODULE_BUILDTYPE=static`。静的リンクの python は .so を読み込めない)
  3. `cpython-root/`:`usr/bin/python3` と `usr/lib/python3.15/`(テスト、idlelib、tkinter などは除く)
- xtask がそれをディスクのルート(ext2、64MiB に広げた)に入れる。大きいので initramfs には入れない
- `sysinfo`(179):起動からの秒数とメモリの量
- QEMU の結合テスト `cpython_runs_from_the_disk`:re、collections、json、os、ファイルの読み書き、`sys.exit(7)`

```
Hello from CPython 3.15.2 on linux
most common: [('the', 3)]
{"pid_positive": true, "cwd": "/"}
written by CPython
```

## つまずいたところ

- **ホストのライブラリが混ざる**:configure が `pkg-config` でホストの zstd を見つけ、aarch64 のビルドに `zstd.h` を探しに行った。`PKG_CONFIG=false` にした
- **zig は `__DATE__` を許さない**:再現できるビルドのために `-Werror=date-time` が既定。`-Wno-error=date-time`
- **snap の zig が、cargo の下の configure からだと何も言わずに失敗する**:`zig cc --version` すら終了コード 1 で、エラーも出ない。手で同じ configure を動かすと通る。環境変数を空にしても、受け継いだ fd を閉じても変わらなかった。strace で見ると `/snap/bin/zig` は `snap-confine` を通って起動していたので、snap の中の実体(`/snap/zig/current/zig`)を直接使うようにしたら通った(`zig_program()`)。cargo も snap なので、snap の中から別の snap を起動するところで何かが起きているらしい
- **xtask の `copy_tree` が実行ビットを落としていた**:`python3` が EACCES。`std::fs::copy`(モードも写す)にした

## まだのこと

- `renameat`(38):`.pyc` のキャッシュを書くときに使う。今は失敗してもキャッシュが書けないだけで動く。VFS に rename を足す
- ssl、zlib、sqlite3 など外部ライブラリが要るモジュール、pip
- Pi の実機(ディスクのドライバができてから)
