# フェーズ2-27 サービスマネージャを Rust で

## 何をしたか

- `user/rust/svcman`:サービスマネージャを C から Rust に書き直した(C 版は削除)。依存クレートなし
  - `lib.rs`:設定ファイルを読む `parse`。ホストの `cargo test` で検証する(間違いは「何行目の何が悪いか」を返す)
  - `main.rs`:epoll と timerfd は標準ライブラリにないので musl の関数を `extern "C"` で直接呼ぶ。プログラムの起動は `std::process::Command`
- `user/build.rs`:入れ子の cargo で `aarch64-unknown-linux-musl` 向けに `--release` でビルドし、`/bin/svcman` に置く。リンカは Rust に付いてくる `rust-lld`(musl の C ツールチェーンは要らない)
- カーネルの直し:
  - `clone` の `newsp`:新しいスタックが渡されたら、子はそのスタックで動き出す(`Action::Fork { child, sp }`)。musl の `posix_spawn`(Rust の `Command` も使う)は `clone(CLONE_VM | CLONE_VFORK, 新しいスタック)` で子を作り、子は新しいスタックの上の関数に飛ぶ
  - `sigaltstack`(132):Rust の標準ライブラリが起動時に呼ぶ(スタックあふれを知らせるシグナル用のスタック)。シグナルはまだ配送しないので、受け付けて「なし」を返す
- 結合テストは C 版と同じもの(startup + 周期 + 失敗して起動し直しで合計 7 回、GPIO の割り込みで起動)がそのまま通る

## なぜ必要か

計画書:サービスマネージャは Rust で書く。設定の読み込みのような純粋な部分をホストでテストでき、ユーザー空間のプログラムを Rust で書く道筋(ツールチェーン、リンカ、標準ライブラリが要るシステムコール)ができた。

## 中で何が起きているか(`Command::new("/bin/hello").status()`)

```
Rust std → musl の posix_spawn
  → pipe2(O_CLOEXEC)        子の exec が失敗したら、ここに errno を書いてもらう
  → clone(CLONE_VM | CLONE_VFORK | SIGCHLD, 新しいスタック)
       子:新しいスタックで起き、execve("/bin/hello")(成功するとパイプは閉じる)
  → 親:パイプを読む → 0 バイト(書く側が閉じた)= exec 成功
  → wait4 で終わるのを待つ
```

`newsp` を無視していたころは、子が親のスタックの写しの上で `ldp x1, x0, [sp]` をして、でたらめな番地に飛んでいたはず。

## まだのこと

- vfork の本当の意味(子が exec するまで親を止める、メモリを共有する)。今は写す
- シグナルの配送
