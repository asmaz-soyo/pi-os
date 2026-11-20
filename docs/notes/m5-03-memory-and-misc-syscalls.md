# M5-03 メモリとその他のシステムコール、init.rc

## 何をしたか

- メモリ:`brk`、`mmap`(無名と、ファイルの private マップ、`MAP_FIXED`)、`munmap`、`mprotect`、`madvise`(何もしない)、`mremap`(`ENOMEM` で断る)
- その他:`set_tid_address`、`getpid` / `gettid`(1)、`getuid` など(0)、`rt_sigaction` / `rt_sigprocmask`(受け付けるだけ)、`clock_gettime`、`sched_getaffinity`、`uname`、`getrandom`
- `kernel::time::now_ns`:ARM Generic Timer のカウンタから求めた起動後の時間。`BOOT_EPOCH`:ビルドした時刻(RTC の代わり)
- `UserMemory::protect`:中身を保ったままページの権限を変える
- `/etc/init.rc`:起動時に上から順に実行するコマンドの一覧(`kernel::init::parse`)
- musl の C プログラム(`/bin/hello_musl`)が、`printf`、`fopen` / `fgets`、1MB の `malloc` / `free` まで動いた

## なぜ必要か

QEMU で `hello_musl` を動かすと、カーネルは未実装のシステムコールを1回ずつ表示する。

```
[pi-os] unimplemented syscall 96     ← set_tid_address(スレッドの準備)
[pi-os] unimplemented syscall 123    ← sched_getaffinity(CPU の数)
[pi-os] unimplemented syscall 222    ← mmap(malloc の中身)
fopen /etc/motd: Out of memory
```

この表示を手がかりに、必要なものだけを足した(ADR-0001 の方針)。

## 中で何が起きているか

### musl の malloc とカーネル

- 小さな確保:musl の mallocng は、まとまった領域を `mmap` で取ってきて、その中を自分で切り分ける
- 大きな確保(1MB など):1 回ごとに `mmap`、解放すると `munmap`
- `brk` は古い方式で、mallocng はほとんど使わない(それでも作っておく)

### mmap の場所の選び方

`MAP_FIXED` でなければ、カーネルが `0x4000_0000_0000` から順に場所を選ぶ。隣とくっつかないよう、1 ページずつ空ける。解放した場所はまだ再利用しない(仮想アドレスは広いので、フェーズ1では困らない)。

### 時刻

- `CLOCK_MONOTONIC`:`CNTPCT_EL0`(カウンタ)× 10⁹ ÷ `CNTFRQ_EL0`(周波数)
- `CLOCK_REALTIME`:ビルドした時刻 + 起動後の時間。Pi 3 には電池で動く時計(RTC)がないため。NTP はネットワークができてから
