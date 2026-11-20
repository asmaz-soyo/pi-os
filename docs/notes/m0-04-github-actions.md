# M0-04 GitHub Actions と pre-push フック

## 何をしたか

- `.github/workflows/ci.yml`:QEMU と Rust を入れて `cargo xtask ci` を呼ぶだけのワークフロー
- `.githooks/pre-push`:push の前に `cargo xtask ci` を走らせるフック(`git config core.hooksPath .githooks` で有効になる)
- `xtask/tests/workflow.rs`:ワークフローが上の形になっていることを確かめるテスト

## なぜ必要か

- CI の中身を YAML に書くと、ローカルで再現できない。中身は `cargo xtask ci` に集め、Actions は「環境を用意して呼ぶ」だけにする
- 公開リポジトリなので、標準ランナーは無料・無制限で使える

## 中で何が起きているか

- `on`:main への push と PR で動く。`*.md` と `docs/` だけの変更では動かない
- `concurrency`:同じブランチで新しい実行が始まると、古い実行を取り消す(無駄な実行を減らす)
- `rustup toolchain install`:引数なしだと `rust-toolchain.toml` を読み、固定した nightly とターゲットを入れる
- `Swatinem/rust-cache`:`~/.cargo` と `target/` をキャッシュして、2回目以降のビルドを速くする
- 重いテスト(互換テストの全件、両BSPのマトリクス)を夜間に回す設定は、テストが増えてから足す
