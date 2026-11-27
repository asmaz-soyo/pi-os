# フェーズ2-83 スレッド(3):futex と pthread

## futex とは

ユーザー空間のミューテックスは、争いがなければカーネルに来ない(アトミックな命令で 0 → 1 にするだけ)。争いがあったときだけ、カーネルに頼む:

- `FUTEX_WAIT(addr, val)`:「`*addr` がまだ `val` なら眠らせて」。もう変わっていれば EAGAIN(ほかのスレッドが先に進めた)
- `FUTEX_WAKE(addr, n)`:「`addr` で待っている人を n 人起こして」

値を確かめることと、待ちの列に積むことのあいだに WAKE が来ると、起こしてもらえないまま眠り込む。だから、表のロックを持ったまま確かめて積む。

## 何をしたか

- `kernel/src/futex.rs`:待ちの表。鍵は (アドレス空間の目印, アドレス) で、同じアドレスでも別のプロセスなら別の futex。起こすときは列から取り除くので、起きた側は「列にもういない = 起こされた」と分かる(余分な目覚めや期限切れと区別できる)
- `futex` のシステムコール(98 番):WAIT(期限は相対)、WAIT_BITSET(絶対。CLOCK_REALTIME なら起動からの時間に直す)、WAKE(_BITSET)、REQUEUE / CMP_REQUEUE(条件変数の broadcast:1 人だけ起こし、残りはミューテックスの列へ移す。一斉に起こして奪い合わせない)。PRIVATE_FLAG は同じに扱う(鍵がもともとアドレス空間ごと)
- 待っているスレッドに exit_group の印が付いたら EINTR で戻る(戻ったところで終わる)
- スレッドが終わるとき、`clear_child_tid` に 0 を書いて 1 人起こす。pthread_join はここで待っている

## pthread_create のスタック:PROT_NONE と mprotect

musl はスレッドのスタックを `mmap(PROT_NONE)` で取り、ガードの 1 ページを残して `mprotect(PROT_READ | PROT_WRITE)` する。今まで PROT_NONE の mmap は何もせず、mprotect もマップ済みのページしか変えなかったので、スタックが一度もマップされず、スレッドが最初の書き込みでフォールトした。

Linux と同じにした:PROT_NONE の mmap は場所を「予約」として覚え、mprotect は予約したページに 0 のページを用意する。マップも予約もしていないページの mprotect は ENOMEM。

## threads(musl の pthread)

- 4 つのスレッドが、ミューテックスで守ったカウンタをそれぞれ 20000 回増やす → 80000。タイマーで切り替わるので、ロックを持ったまま切り替わったときに futex で待つ
- スレッドごとの `__thread` の変数が混ざらない(TLS。CLONE_SETTLS で TPIDR_EL0 がスレッドごとに違う)
- 条件変数で、16 個の箱に 1〜200 を流す → 合計 20100(pthread_cond_wait と broadcast)
- pthread_join でスレッドの戻り値を受け取る

QEMU(両方の BSP)と互換テストで、Linux と同じ結果になる。足りないシステムコールはなかった(`/proc/pios/unimplemented` が空)。

## まだのこと

- CLONE_FS(カレントディレクトリと umask の共有)、robust futex(持ち主が死んだミューテックス)、PI futex
- シグナルの配送(スレッドを中断する、pthread_cancel、pthread_kill)
