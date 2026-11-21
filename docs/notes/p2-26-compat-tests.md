# フェーズ2-26 システムコールの互換テスト(Linux と比べる)

## 何をしたか

- `cargo xtask test --compat`:同じプログラムを Linux(`qemu-aarch64`、qemu-user)と Pi-OS(QEMU virt)で動かし、標準出力と終了コードを比べる。`cargo xtask ci` にも入れた(GitHub Actions では qemu-user を入れる)
  - ケースは `xtask::COMPAT_CASES`:`/bin/compat`(下記)と、BusyBox のアプレット(echo、seq、expr、printf、sh の算術、test、ls)
  - Pi-OS 側:ディスクの `/etc/init.rc` を互換テスト用のスクリプトに差し替えて起動する(`make_disk` の overlay)。スクリプトはケースごとに `@@case N` と `@@exit $?` を出すので、UART の出力をケースごとに切り分けられる。標準エラー出力は捨てる
  - Linux 側:build.rs が作った同じバイナリを qemu-aarch64 で直接動かす
- `/bin/compat`(C):ファイル、fd、パイプ、プロセス、メモリ、時刻のシステムコールを呼び、返り値と errno を出す(uid やパスなど環境で変わるものは出さない)

```
ok   compat one
ok   busybox echo hello world
...
compat: all 8 case(s) match Linux
```

## 見つかった違い(すべて直した)

| 違い | 原因 |
| --- | --- |
| ファイルを `O_DIRECTORY` で開けてしまう | `O_DIRECTORY` の値が x86 の `0o200000` だった。AArch64 では `0o40000`(musl の `opendir` もこの値を使う) |
| 相対パスの `unlink`/`rmdir` が `EBADF` | musl の `syscall()` 経由だと、int の `AT_FDCWD`(-100)が上位 32 ビット 0 のまま届く(`0x0000_0000_ffff_ff9c`)。Linux と同じく dirfd は下位 32 ビットだけ見るようにした |
| 子を `waitpid` している間に、シェルの `wait4(-1)` が `ECHILD` | システムコールの間はプロセスを表から取り出しているので、「子がいない」と数えていた。取り出している間も残る情報(番号 → 親)で数えるようにした |

単体テストでは自分の思い込みどおりのテストしか書けないが、本物の Linux と比べると思い込み(x86 の値、レジスタの上位ビット)が見つかる。

## まだのこと

- ケースを増やす(シグナル、`getdents64` の順番、`/proc`、時刻の形式など)
- int の引数(fd など)を全部 32 ビットとして読むようにそろえる
