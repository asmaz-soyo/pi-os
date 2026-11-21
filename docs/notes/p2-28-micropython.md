# フェーズ2-28 MicroPython

## 何をしたか

- MicroPython 1.29.0 のソース(GitHub のタグの tarball、サブモジュールなし)を `user/third_party/micropython/` に置いた。sha256 は取ってきたときの値を記録
- `user/build.rs`:unix ポート(`VARIANT=standard`)を zig cc で aarch64-linux-musl の静的リンクにビルドし、`/bin/micropython` に置く(約 560KB)
  - まずホストの cc で `mpy-cross`(.py をバイトコードにする道具)を作る
  - ffi、btree、ssl、frozen モジュール(micropython-lib)はサブモジュールが要るので外した
- `/hello.py`:ファイルの読み書き、`os.listdir`、`sys.exit(3)`
- QEMU の結合テスト `micropython_runs_code_and_scripts` と、互換テストのケース(Linux と同じ出力・終了コード)

```
$ micropython /hello.py
Hello from MicroPython (1, 29)
squares: [0, 1, 4, 9, 16, 25, 36, 49, 64, 81]
read back: written by python
etc has motd: True
```

## つまずいたところ

- **zig cc -E がホストの /usr/include を探す**:MicroPython は qstr(文字列の表)を作るために、前処理(`$(CPP)`)に .c をまとめて渡したり、標準入力から渡したりする。zig cc はそうすると 2 つ目以降や標準入力の分で aarch64-linux-musl ではなくホストのヘッダを探してしまう。`zcpp.sh` で 1 ファイルずつ、標準入力は一時ファイルにしてから前処理するようにした
- **cargo の DEBUG=true**:cargo はビルドスクリプトに `DEBUG=true` を渡す。MicroPython の Makefile はこれを「デバッグビルド」と読み、6MB(デバッグ情報つき、最適化なし)のバイナリができた。それを exec したカーネルが、ファイル全体を 4MB のカーネルヒープに読もうとしてパニックした(`memory allocation of 6123144 bytes failed`)。`DEBUG` を外して渡すようにした
- **zig のリンカは `-Map` を知らない**:`LDFLAGS_ARCH` を上書きした

## まだのこと

- exec でファイル全体をカーネルのヒープに読まない(セグメントごとに読む)。CPython のような大きなプログラムに必要
- CPython、動的リンク(計画書の項目6の残り)
