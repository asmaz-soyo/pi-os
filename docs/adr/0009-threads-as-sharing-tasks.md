# ADR-0009: スレッドは「メモリと fd の表を共有するタスク」にする

- 状態:採用
- 日付:2026-11-27

## 背景

計画書の項目8(スレッド、futex)。項目7の OS 上の uv も、tokio のスレッドを使う。musl の pthread_create は `clone(CLONE_VM | CLONE_FILES | CLONE_SIGHAND | CLONE_THREAD | CLONE_SETTLS | CLONE_PARENT_SETTID | CLONE_CHILD_CLEARTID | …)` を呼ぶ。

今の作りでは、プロセスはタスク 1 つで、システムコールの間は `Process` を表から取り出して使う(待っている間もほかのタスクが表を使えるように)。

## 決定

Linux と同じく、スレッドもプロセスの表に 1 つずつ入るタスクにし、共有するものを `Arc` で共有する。

- `UserMemory`(アドレス空間、ページ、brk、mmap の位置)と `FdTable` は、中身を `Arc<SpinLock<..>>` で持つ。`share()` で共有(CLONE_VM、CLONE_FILES)、fork は写す
- `Process` の `pid` はスレッドの番号(gettid)、`tgid` はスレッドのグループの番号(getpid。最初のスレッドの番号)
- `exit` はそのスレッドだけを終える。グループの最初のスレッドだけが終わり方(ゾンビ)を残す。`exit_group` は終わり方をすぐ残し、ほかのスレッドに強制終了の印を付けて起こす
- 強制終了の印はプロセスの表とは別に持つ(システムコールの途中で表から取り出されているスレッドにも付けられるように)
- 終わるときは `clear_child_tid`(set_tid_address、CLONE_CHILD_CLEARTID)に 0 を書き、futex で起こす(pthread_join)
- CLONE_FS(カレントディレクトリ、umask)と CLONE_SYSVSEM は受け付けるだけで、まだ共有しない

## 理由

- システムコールの道(`dispatch(&mut Process, ..)`)を変えずに済む。スレッドごとに `Process` があるので、取り出しと戻しも今のまま
- 共有の単位が Linux と同じなので、clone のフラグをそのまま当てはめられ、互換テストで Linux と比べやすい
- 他の案:プロセスの中にスレッドの一覧を持つ(`Process` を共有し、スレッドの固有の部分だけ別にする)。データの持ち方としては素直だが、取り出しと戻し(システムコールの間の表のロック)を作り直すことになり、変更が大きい

## 影響

- `UserMemory` と `FdTable` のメソッドは、呼ぶたびにロックを取る。システムコールは割り込みを止めて動くので、途中でほかのスレッドに切り替わらず(眠るときを除く)、読みと書きがずれない。マルチコアにするときは見直す
- /proc にはスレッドも番号ごとに出る(Linux は /proc/<tgid>/task/ の下に隠す)
- スレッドのうち、眠っていて起こされても条件が満たされない待ち(パイプの read など)にいるものは、exit_group の印に気づかない(futex の待ちは気づく)。シグナルの配送を作るときに、待ちを中断できるようにする
