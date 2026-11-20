# コンセプトと目標

Pi-OS は、Raspberry Pi 3B 向けの Rust 製の学習用カーネル。

- **軸**:OS を作りながら、OS の仕組みを学ぶ。作る過程(1ステップ=1コミット)と中身(学習ノート)を後から追えるようにする
- **観察できるカーネル**:システムコール、VFS、割り込みなどの動きをトレースして見られるようにする
- **Linux 互換**:musl で静的リンクしたプログラムを無改造で動かす
- **フェーズ1のゴール**:自作 OS 上で `qjs hello.js` を実行し、UART に「Hello from JS」と出す

詳しい計画は [plan.md](plan.md)、構造は [architecture.md](architecture.md) を参照。
