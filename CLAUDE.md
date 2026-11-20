# Pi-OS

Raspberry Pi 3B 向けの、Rust 製の学習用カーネル。設計と計画の全体は `docs/plan.md` を参照すること。

## プロジェクトの目的

- OS を作りながら、OS の仕組みを学ぶ。ユーザーが実装の過程と中身を追いながら作れることを最優先にする
- カーネルの動き(システムコール、VFS、割り込みなど)を観察できるようにする
- フェーズ1のゴール:自作 OS 上で `qjs hello.js` を実行し、UART に「Hello from JS」と出す。達成までは他の機能に手を出さない

## 構造のルール

- ポート&アダプター。コア(`kernel/`)は `hal/` のトレイトにだけ依存する。ハードウェアや Linux の知識をコアに持ち込まない
- ドライバ(`drivers/`)と BSP(`bsp/`)は分ける。BSP は「どのドライバをどのアドレスで使うか」の配線だけを持つ
- BSP は `raspi3` と `qemu-virt` の2つ。どちらでもビルドとテストが通る状態を保つ
- 内部のエラーは `Result<T, Errno>` で統一する
- ユーザー空間のポインタは必ず `copy_from_user` / `copy_to_user` を通す
- ロックはマルチコアを前提に書く
- ユーザー空間に戻る直前の処理は1か所にまとめる

## 作業の進め方(必ず守る)

1ステップごとに次の順で進める。一度に大量のコードを書かない。

1. 次のステップの目的と背景を、ユーザーに説明する
2. テストを先に書く(ユーザーが書きたい場合は任せる)
3. テストが失敗することを確認する(Red)
4. 最小限の実装で通し、何をしたかを解説する(Green)
5. 整理する(Refactor)
6. ドキュメントと学習ノート(`docs/notes/`)を更新する
7. ユーザーの確認を得てからコミットする(1ステップ = 1コミット)

完了の定義:テストが通る・リファクタリング済み・ドキュメント更新済み、の3つがそろっていること。

## テスト

- ホストの単体テスト:`cargo test`(コアをモックのポートで検証。TDD の主戦場)
- QEMU の結合テスト:`cargo xtask test --bsp qemu-virt`
- システムコールの互換テスト:`cargo xtask test --compat`(Linux の `qemu-aarch64` と結果を比べる)
- UART 出力のスナップショット:`cargo xtask test --snapshot`
- まとめて実行:`cargo xtask ci`(push 前に必ず通す。GitHub Actions もこれを呼ぶだけ)

## ドキュメント

- 設計判断は `docs/adr/` に ADR として残す
- システムコール対応表などの一覧は、手で書かずにコードから `docs/generated/` に自動生成する
- マイルストーンを完了したら `docs/milestones/` を更新し、git タグ(例:`m1-uart`)を打つ

## 開発環境

- 開発 PC は Ubuntu。QEMU(`qemu-system-aarch64 -M raspi3b` / `-M virt`)と `gdb-multiarch` を使う
- カーネルは nightly Rust、ターゲットは `aarch64-unknown-none`
- ユーザー空間のプログラムは musl で静的リンクする(`zig cc -target aarch64-linux-musl`、Rust は `aarch64-unknown-linux-musl`)
