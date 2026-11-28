# フェーズ2-93 /proc/self/exe:実行しているプログラムへのリンク

## なぜ必要か

OS の上で uv を動かす準備の 2 つめ(p2-89 の下調べ)。uv は起動するとすぐに `readlinkat("/proc/self/exe")` で自分のプログラムの場所を調べる(Rust の `std::env::current_exe()` がこれを使う)。今までの Pi-OS には `/proc/<pid>/exe` がなく、`ENOENT` になっていた。

## Linux での意味

`/proc/<pid>/exe` は、そのプロセスが実行しているファイルを指すシンボリックリンク。

- 指す先は、リンクをたどり切った実体のパス。BusyBox のアプレット `/bin/ls`(`/bin/busybox` へのリンク)を実行すると、`/bin/busybox` を指す
- シバンのスクリプトなら、実行されているのはインタプリタなので、インタプリタを指す
- 動的リンクのプログラムなら、動的リンカ(ld-musl)ではなくプログラム自身を指す
- 開くと(`open`、`stat`)、リンクをたどってその実行ファイル自体に届く
- fork した子とスレッドは、親と同じものを指す

## 作り

```
execve(path)
  シバンならインタプリタに置き換えて、実行するファイルを決める
  exe = vfs.resolve_path(実行するファイル, リンクをたどる)   ← 失敗したら、今のプロセスはそのまま
  読み込みに成功したら:p.exe = exe、PROCESSES.set_exe(pid, exe)

readlinkat("/proc/self/exe")
  procfs:/self → /<今のタスクの番号>
  /<pid>/exe → ProcLink { target: PROCESSES.exe_of(pid) }   種類は Symlink
  readlink() → target
```

- `Process::exe`:実行しているファイルの絶対パス。fork とスレッドは写す。カーネルが直接作り、まだ execve していないプロセスは空(`/proc/<pid>/exe` は ENOENT)
- プロセスの表の `info` を、`(親, 名前, グループ)` の組から名前付きの構造体 `ThreadInfo` にし、`exe` を足した。procfs はシステムコールの途中で `Process` が表から取り出されているあいだも読むので、`Process` ではなくこちらを見る
- カーネルがディスクのプログラムを直接動かすとき(`boot::spawn_program`。init や結合テスト)も、リンクをたどり切ったパスを入れる。カーネルに埋め込んだ実行ファイル(`run_user`)はファイルではないので空
- `ProcLink` はシンボリックリンクとして見えるだけなので、`open("/proc/self/exe")` は VFS の `walk` が普通のリンクと同じようにたどる

## 互換テストにしなかった理由

qemu-user の `/proc/self/exe` は、ホストの上のプログラムのパス(qemu が動かしているファイル)を返す。Pi-OS の `/bin/…` とは比べられないので、QEMU の結合テストで確かめた。

## 確かめたこと

- ホストのテスト:procfs の `/<pid>/exe` がシンボリックリンクで、指す先が実行ファイル。一覧に `exe` が出る。実行ファイルのないプロセスは ENOENT。execve のあとは、直接・シバン(インタプリタ)・リンク経由のどれでも実体のパス。fork とスレッドが受け継ぐ
- QEMU(qemu-virt と raspi3):BusyBox の `ls -l /proc/self/exe` が `-> /bin/busybox`、`grep -q ELF /proc/self/exe` で開ける、シェルの `/proc/$$/exe` も `/bin/busybox`

## 残り

`/proc/self` は、今の**タスク**(スレッド)の番号に置き換えている。Linux では、スレッドから見ても `/proc/self` はプロセス(スレッドのグループ)の番号になる。スレッドもプロセスの表に自分の項目を持っているので `exe` は読めるが、番号がちがう。uv のスレッド(tokio)が `/proc/self` の下を読むときに困るようなら直す。
