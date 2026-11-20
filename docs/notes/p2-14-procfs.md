# フェーズ2-14 procfs

## 何をしたか

- `kernel::fs::procfs::Procfs` を `/proc` にマウントした
  - `/proc/uptime`、`/proc/meminfo`、`/proc/version`(Linux と同じ形)
  - `/proc/<pid>/status`(名前、状態、番号、親の番号)、`/proc/self`
  - `/proc/pios/unimplemented`:呼ばれたのに未実装だったシステムコールの番号と回数(Pi-OS 独自)
- プロセスに名前(`argv[0]` の最後の部分、execve でプログラムの名前に変わる)を持たせた
- プロセスの表に (親, 名前) の一覧を別に持たせた。システムコールの途中で表から取り出されているプロセス(`/proc` を読んでいる本人など)も見える
- `/bin/procread`(C)で `/proc/self/status` と `/proc/meminfo` を読むテスト。シェルのスナップショットで `cat /proc/version`

## なぜ必要か

計画書の「自作ならではの要素:カーネルの動きを観察できる」。トレース(`--trace syscall`)は起動時に決めるが、procfs ならシェルからいつでも見られる。

```
pi-os:/$ cat /proc/meminfo
MemTotal:         126492 kB
MemFree:          104916 kB
HeapFree:           4031 kB
pi-os:/$ cat /proc/pios/unimplemented
# nr count
```

## 中で何が起きているか

- ファイルの中身は、パスを引いた(開いた)ときにその時点の値で作る。開いたあとに状態が変わっても、そのファイルの中身は変わらない
- `stat` の大きさは 0(Linux と同じ)。読んでみるまで大きさは分からない
- `/proc/self` は、読んでいるタスクの番号に置き換えてから探す
