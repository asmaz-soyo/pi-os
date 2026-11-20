# M0-01 ワークスペースと `hal::Console`

## 何をしたか

- `rust-toolchain.toml` で nightly を日付つきで固定し、`aarch64-unknown-none` ターゲットを入れた
- ルートに Cargo ワークスペースを作り、最初のメンバーとして `hal` クレートを置いた
- `hal::console::Console` トレイト(`write_bytes` だけ)と、`core::fmt::Write` に変換する `Writer` アダプターを作った
- テスト用の `hal::mock::MockConsole` を作り、3件の単体テストで振る舞いを固定した

## なぜ必要か

- **nightly の日付固定**:カーネルは nightly 専用の機能を使う。日付を決めないと、ある日突然ビルドが壊れることがある。上げるときは意図してこのファイルを書き換える
- **`Console` から始める理由**:M1 の「Hello」もパニック時のレジスタ表示も、すべて文字の出力を通る。最初のポートとして一番使われる
- **バイト単位のトレイト**:UART は1バイトずつ送るハードウェアなので、基本の操作は `&[u8]` にした。文字列や書式付き出力は `Writer` が上に被せる
- **モックの意味**:コアは `Console` トレイトしか知らないので、ホストの `cargo test` では本物の UART の代わりに `MockConsole` を渡して、出力を検証できる

## 中で何が起きているか

```
kernel(コア)  ──使う──▶  hal::Console  ◀──実装──  drivers::pl011(M1 で作る)
                                      ◀──実装──  hal::mock::MockConsole(テスト用)
```

- `hal` は `#![no_std]`。カーネルと同じく標準ライブラリなしでビルドできることを、`cargo build -p hal --target aarch64-unknown-none` で確かめた
- モックだけは `Vec` を使いたいので、`#[cfg(any(test, feature = "mock"))]` のときに限って `extern crate std;` する。本番ターゲットのビルドにはモックは含まれない
- `write!(w, "x = {}", 42)` は、`core::fmt` が書式を展開して `Writer::write_str` を細切れに何度か呼ぶ。`Writer` はそれを `write_bytes` に流すだけ

## 確認方法

```bash
cargo test
cargo build -p hal --target aarch64-unknown-none
```
