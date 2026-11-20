# M0-02 `cargo xtask` の骨組み

## 何をしたか

- ワークスペースに `xtask` クレート(ホストで動く普通のバイナリ)を足した
- `.cargo/config.toml` の alias で `cargo xtask <task>` から呼べるようにした
- `build` / `run` / `test` / `ci` の4つを用意した。今の中身は次のとおり

| タスク | 実行するもの |
| --- | --- |
| `build` | `TARGET_CRATES` を `aarch64-unknown-none` 向けにビルド |
| `run` | まだ起動するカーネルがないのでエラー(M1 で実装) |
| `test` | `cargo test --workspace`(ホストの単体テスト) |
| `ci` | fmt → clippy(ホスト)→ clippy(aarch64)→ ホストのテスト → aarch64 ビルド |

## なぜ必要か

- 手順をシェルスクリプトや Makefile ではなく Rust で書くことで、開発PCと GitHub Actions で同じものが動き、手順そのものも単体テストできる
- `cargo xtask ci` を「CI の正本」にする。GitHub Actions はこれを呼ぶだけにするので、「ローカルでは通るのに CI で落ちる」が起きにくい

## 中で何が起きているか

- `cargo xtask ci` は alias により `cargo run --quiet --package xtask -- ci` に展開される
- xtask は2つに分かれている
  - `lib.rs`:引数を `Task` に変換する `parse` と、`Task` を「実行するコマンドの列」に変換する `plan`。どちらも副作用のない関数なので、`cargo test` で順番まで検証できる
  - `main.rs`:計画を受け取り、1つずつ実行して、失敗したらそこで止める
- 実行するときは、環境変数 `CARGO`(cargo が子プロセスに渡す、自分自身のパス)を使う。PATH に古い cargo があっても、`cargo xtask` を呼んだのと同じ cargo で動く
- QEMU のテストやスナップショットのテストは、後のステップで `plan` に足していく

## 確認方法

```bash
cargo xtask ci
cargo test -p xtask
```
