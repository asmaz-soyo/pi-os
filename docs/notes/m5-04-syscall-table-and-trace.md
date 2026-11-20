# M5-04 システムコール表、トレース、自動生成のドキュメント

## 何をしたか

- `kernel::syscall::TABLE`:番号・名前・引数の数・ハンドラを並べた表。`dispatch` は表を引くだけになった
- トレース:`cargo xtask run --trace syscall` で、呼ばれたシステムコールの引数と戻り値を表示する(`trace-syscalls` feature)
- `cargo xtask docs`:表から [docs/generated/syscalls.md](../generated/syscalls.md) を作る。`cargo xtask ci` は最初に `docs --check` で、生成物が最新かを確かめる

```
$ cargo xtask run --trace syscall
[trace] set_tid_address(0x1038c08) = 1
[trace] ioctl(0x1, 0x5413, 0x7ffffffedc28) = 0          ← 標準出力は端末か?
Hello from musl! argc=1 argv[0]=/bin/hello_musl
[trace] writev(0x1, 0x7ffffffedbb0, 0x2) = 48
[trace] openat(0xffffffffffffff9c, 0x1000281, 0x20000) = 3   ← AT_FDCWD, "/etc/motd", O_LARGEFILE
[trace] mmap(0x0, 0x1f000, 0x3) = 0x400000000000        ← malloc の領域
[trace] munmap(0x400000010000, 0xf000) = 0              ← 境界合わせの余りを返す
[trace] read(0x3, 0x400000000100, 0x400) = 18
```

## なぜ必要か

- 計画書の「自作ならではの要素」:カーネルの動きを観察できること。ユーザープログラムが OS に何を頼んでいるかが、そのまま見える
- 対応表を手で書くと、コードとずれる。表をコードに1つだけ持ち、ドキュメントはそこから作る(CLAUDE.md のルール)

## 中で何が起きているか

- `table!` マクロが、行ごとに小さなハンドラ関数を作って表に並べる。番号の順に並んでいることはテストで確かめている
- トレースは `TRACE` という `AtomicBool` で切り替える。ビルド時に `trace-syscalls` を付けると、起動時に BSP が有効にする
- 戻り値が -4095〜-1 ならエラーとして errno の名前を、小さな値は 10 進数、大きな値(アドレス)は 16 進数で表示する
