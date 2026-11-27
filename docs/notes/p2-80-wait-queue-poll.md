# フェーズ2-80 待ち行列(3):poll / select / epoll、wait4、GPIO、timerfd、端末

## 何をしたか

relax で回って待っていた残りを、全部眠って待つ形にした(`sched_yield` だけは「譲る」という意味なので relax のまま)。

| 待ち | 待ち行列 | 起こす側 |
| --- | --- | --- |
| wait4、カーネルがプログラムの終わりを待つ | `CHILD_WQ` | プロセスが終わってゾンビになったとき |
| `/dev/gpio/N` の read | GPIO の出来事の列 | GPIO の割り込みのハンドラ |
| 止まっている timerfd の read | timerfd ごとの列 | timerfd_settime |
| poll / select / epoll_wait | `POLL_WQ` | ほかのどの待ち行列が起こされても |
| 端末の read | (なし) | 10ms ずつ眠っては UART を確かめる |

### poll / select / epoll:いくつものファイルを同時に待つ

Linux は、poll するファイルそれぞれの待ち行列に登録する(`poll_wait`)。ここでは簡単に、どの待ち行列の `wake_all` も、最後に `POLL_WQ` を起こすようにした。poll で待つタスクは `POLL_WQ` だけに登録し、起こされたら自分のファイルを全部見直す。関係のない目覚めは増えるが、見直すので正しい。

時間で読めるようになるファイル(timerfd の満了)は、だれも起こしてくれない。そこで `File::next_event_at`(次に変わる時刻)を足し、poll はその時刻までに目を覚ます。

条件を関数で渡す `wait_event` は poll に合わない(ファイルを見る処理が長く、プロセスを借りて書き込むため)。そこで一段下の API を作った:

```rust
loop {
    let reg = register_poll();      // 1. 登録
    ...ファイルを全部見る...          // 2. 条件
    if 何かあった || 期限 { return }
    reg.sleep(早いほうの期限);       // 3. 眠る。reg を捨てると 4. 登録が外れる
}
```

`wait_event` もこの上に書き直した。

### 端末

UART の受信割り込みは、まだ使っていない(両方の BSP で割り込みの番号と、raspi3 の割り込みコントローラの道が要る)。今は 10ms ずつ眠っては UART を確かめる。回り続けるよりずっと軽く、打った文字が見えるまでの遅れは最大 10ms。poll / select も `next_event_at` で 10ms ごとに見直す。

## 確かめたこと

- QEMU:子(`/bin/busybox sleep 2`)の終わりを待つ sh が `S (sleeping)`、ash の `read -t 5`(poll で待つ)が `S (sleeping)`
- QEMU:timerfd と epoll の周期のテスト、GPIO の割り込みのテスト、端末に打ち込むスナップショットのテストは、そのまま通る
- ジッタ(リアルタイムのタスクの遅れ):最大 0.23ms

## まだのこと

- UART の受信割り込み(端末の入力で起こす)
- virtio-blk の完了の割り込み(今は回って待つ。ロックを持ったまま回るので、そのあいだは切り替わらない)
