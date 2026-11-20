# フェーズ2-13 サービスマネージャ

## 何をしたか

- `/bin/svcman`(C、約 200 行):設定ファイルのサービスを、起動のきっかけ(トリガー)に応じて動かす
  - `trigger = "startup"`:起動時に 1 回
  - `trigger = { every = "30ms" }`:timerfd と epoll で一定間隔に
  - `restart = "on-failure"`:0 以外で終わったら起動し直す
  - `max_runs`:動かす回数の上限
- `/etc/services.conf`(既定)と `/etc/svcman-test.conf`(テスト用)
- カーネル内テスト:startup 1 回 + 周期 3 回 + 失敗して起動し直し 3 回 = 合計 7 回(svcman の終了ステータス)

```
[service.tick]
exec = "/bin/hello"
trigger = { every = "30ms" }
max_runs = 3
```

## なぜ必要か

計画書の「アプリの実行モデル」:スタートアップ、周期、割り込み駆動の3種類のアプリを「起動のきっかけ」の違いとして統一し、カーネルはきっかけを fd(timerfd など)として提供し、管理はユーザー空間のサービスマネージャが行う。そのうちスタートアップと周期ができた。割り込み駆動(`/dev/gpio/N`)は GPIO のドライバができてから足す。

## 計画書との違い

- 計画書ではサービスマネージャは Rust 製。Rust のユーザープログラムをビルドするには `aarch64-unknown-linux-musl` のツールチェーンの追加(ダウンロード)が要るので、まず C で作った
- 設定の形式は TOML の一部だけ(`[service.名前]`、`キー = "値"`、`trigger = { every = "…" }`)
- init は今も「カーネルが `/etc/init.rc` を上から順に動かす」形。svcman を init から常駐させるのは次の段階

## 中で何が起きているか

```
svcman
  startup のサービス:fork → execv → waitpid(on-failure なら上限まで繰り返す)
  周期のサービス:timerfd_create → timerfd_settime(間隔)→ epoll_ctl(ADD)
  epoll_wait で待つ → 満了したタイマのサービスを fork / exec / wait → 上限に達したら epoll_ctl(DEL)
```

カーネルから見ると、svcman は「epoll で待ち、ときどき fork する」普通のプロセス。カーネルにサービスの知識はない(ADR-0002 の方針どおり)。
