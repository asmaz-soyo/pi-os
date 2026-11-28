# フェーズ2-89 OS 上の uv の下調べ(ホストの qemu-user で)

## したこと

uv 0.13.10(`user/third_party/uv/`)を、ホストの `qemu-aarch64 -strace` で `uv --version` と `uv pip --help` だけ動かし、呼ぶシステムコールを集めた。

呼んだのは 19 種類:write、munmap、mmap、clock_gettime、rt_sigaction、brk、ioctl、sigaltstack、readlinkat、read、openat、madvise、close、set_tid_address、sched_getaffinity、rt_sigprocmask、ppoll、mprotect、exit_group。どれも Pi-OS の対応表にある。

## 引数を見て分かったこと(uv は jemalloc でメモリを割り当てる)

| 呼び出し | Linux での意味 | Pi-OS | すること |
| --- | --- | --- | --- |
| `madvise(…, MADV_DONTNEED)` | 無名のページを捨て、次に触ると 0 のページになる | 何もせず 0 を返す | **直す**。jemalloc は「0 に戻った」と当てにして、calloc などに使い回す。そのままだと、0 でないメモリが返ってまれに壊れる |
| `madvise(…, MADV_FREE)` | 捨ててよい(すぐでなくてよい) | 何もしない | このままでよい(中身が残っても、jemalloc は 0 を当てにしない) |
| `readlinkat("/proc/self/exe")` | 自分のプログラムの場所 | ない(ENOENT) | **足す**(uv は自分の場所を使う) |
| `openat("/proc/sys/vm/overcommit_memory")`、`/sys/kernel/mm/transparent_hugepage/enabled` | jemalloc の設定を決める | ない | なくてよい(読めなければ既定の値) |
| `readlinkat("/etc/_rjem_malloc.conf")` | jemalloc の設定(普通はない) | ない | このままでよい |
| `ioctl(TIOCGWINSZ)` | 端末の大きさ | 端末なら答える | このままでよい |

## まだ分からないこと

ネットワークを使う操作(`uv pip install`、`uv venv` の Python 探し)では、スレッド(tokio)、epoll、ソケット、getrandom などをもっと呼ぶはず。QEMU を動かせるときに Pi-OS の上で動かし、`/proc/pios/unimplemented` で確かめる。
