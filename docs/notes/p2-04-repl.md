# フェーズ2-04 端末からの入力と JS の REPL

## 何をしたか

- `/dev/console` を TTY(行規律)経由にした。`read` は 1 行(raw モードなら届いた分)が読めるまで待つ
- `ioctl` の `TCGETS` / `TCSETS` / `TCSETSW` / `TCSETSF`:termios の読み書き
- `pselect6` / `ppoll`:複数の fd のうち、どれかが読める(書ける)まで待つ。タイムアウト付き
- `kernel::console::Global` が UART の入力も中継するようにした(`SharedConsole::read_byte`)
- `/etc/init.rc` の最後で `qjs`(引数なし = REPL)を動かす
- スナップショットテストで、QEMU の標準入力(= UART)に入力を送れるようにした。REPL で `1+2` を計算し、`^D` で終える

```
QuickJS - Type "\h" for help
qjs > 1+2
3
qjs > console.log("hi " + [1,2,3].map(x => x*x))
hi 1,4,9
undefined
qjs > ^D
init: /bin/qjs exited with status 0
```

## なぜ必要か

計画書のフェーズ2の最初の項目「UART 入力、TTY、JS の REPL」。プログラムに入力を渡せると、OS を「動かして見る」ものから「触って試す」ものにできる。

## 中で何が起きているか

QuickJS の REPL を `--trace syscall` で見ると、次のように端末を準備してから入力を待つ。

```
ioctl(0, TIOCGWINSZ, …)   端末の大きさ
ioctl(0, TCGETS, …)       今の termios を保存
ioctl(0, TCSETS, …)       raw モードにする(行の編集は REPL が自分でする)
rt_sigaction(SIGINT, …)   ^C の処理(まだ届かない)
writev(1, "qjs > ")
pselect6(1, {0}, …)       ← 入力が届くまで待つ
read(0, …)                届いた文字を読み、行を描き直す
```

- 待つ間は、UART に文字が届いていないかを繰り返し確かめている(ビジーウェイト)。システムコールの中では割り込みが止まっているので、タイマのティックも進まない。スケジューラを作るときに、UART の受信割り込みでタスクを起こす形に変える
- REPL は色付けのためにエスケープシーケンス(`ESC[32m` など)を出す。スナップショットでは `{..}` で受けている

## テストの工夫と、分かったこと

- UART の初期化より前に届いた入力は、PL011 の FIFO の設定で捨てられる。そのため xtask は、出力に REPL のプロンプト「qjs > 」が現れてから入力を送る
- `1+2` の Enter と `^D` を1回の書き込みで送ると、REPL は `^D` を取りこぼした。raw モードの read は届いた分をまとめて返す(Linux と同じ)ので、REPL 側の扱いによるもの。人が打つときと同じく、プロンプトごとに1つずつ送る
