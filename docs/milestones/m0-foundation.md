# M0 開発基盤

## 目的

カーネルのコードより先に、テストとドキュメントの仕組みを作る。

## 完了条件

- [x] Cargo ワークスペースと `rust-toolchain.toml`
- [x] `hal::Console` とモック(最初の TDD)
- [x] `cargo xtask build / run / test / ci`
- [x] QEMU の結合テスト(終了コードで成否を返す)
- [x] GitHub Actions(`cargo xtask ci` を呼ぶだけ)
- [x] `docs/` の雛形、ADR のテンプレート、最初の ADR
- [ ] M1 の最初のテスト(UART 出力のスナップショットで「Hello」を期待する)

## 学んだこと

学習ノート `docs/notes/m0-*.md` を参照。
