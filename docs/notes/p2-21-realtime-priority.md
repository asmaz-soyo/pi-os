# フェーズ2-21 リアルタイム優先度とジッタ計測

## 何をしたか

- スケジューラ:タスクに優先度を持たせた(0 = 通常、1〜99 = リアルタイム)。`RunQueue::next_by` が、実行待ちの中で一番優先度の高いタスク(同じなら先に並んだもの)を選ぶ。今のタスクのほうが高ければ、そのまま動き続ける
- 時刻を指定した眠り:`sched::sleep_until(時刻)`。タスクを Blocked にして起きる時刻を覚え、時刻が来たら実行待ちに戻す。`nanosleep` / `clock_nanosleep` と timerfd の `read` がこれを使う(前は「時刻が来るまで譲り続ける」だけだった)。動けるタスクが1つもなければ、起きる時刻まで待つ
- 目覚まし:`Clock::request_wake_ns`。眠っているタスクの中で一番早い起床時刻に、ティックとは別のタイマ割り込みを予約する。割り込みで `request_resched` され、ユーザーに戻るときに起きたタスクへ切り替わる
- システムコール:`sched_setscheduler`(119)、`sched_getscheduler`(120)、`sched_yield`(124)。SCHED_OTHER(優先度 0)と SCHED_FIFO(1〜99)。リアルタイムにできるのは root だけ(EPERM)。対象は自分だけ(他は ESRCH)。fork した子は優先度を受け継ぐ
- `/bin/jitter`(C):cyclictest の小さな版。低い優先度で CPU を回し続ける子を作り、自分は SCHED_FIFO 80 で 5ms ごとに起きて、予定の時刻からの遅れを 100 回測る
- QEMU の結合テスト `a_realtime_task_wakes_on_time_despite_a_busy_task`:遅れの最大が 1ms 以内

```
jitter (SCHED_FIFO): min 56 us, avg 67 us, max 257 us     ← 目覚ましあり
jitter (SCHED_FIFO): min 3687 us, avg 6723 us, max 9812 us ← ティック(10ms)でしか起こせなかったとき
```

## なぜ必要か

計画書の「アプリの実行モデル」の周期アプリ(センサーを決まった間隔で読むなど)は、起きる時刻がずれないことが大事。ほかのタスクが CPU を使っていても、優先度の高いタスクは時刻どおりに起きて先に動く必要がある。

## 中で何が起きているか

```
jitter(FIFO 80):clock_nanosleep(ABSTIME, T)
  → sched::sleep_until(T):Blocked、wake_at = T
  → schedule:眠っている中で一番早い T に目覚ましを頼む(Clock::request_wake_ns)
     → タイマは「次のティック」と「T」の早いほうで割り込むよう予約
  → 子(spinner、優先度 0)が動く
時刻 T:タイマ割り込み(ティックではない) → request_resched
  → spinner がユーザーに戻る直前:yield_now
  → wake_expired:jitter を実行待ちへ
  → next_by:優先度 80 の jitter を選ぶ → 切り替え
```

## つまずいたところ

- **待ちのループが優先度を握ったまま**:`wait4` などの待ちは、まだ「譲りながら何度も確かめる」形(`relax`)。優先度の高いタスクが待つと、譲った直後のティックでまた選ばれ、待っている相手(低い優先度の子)がほとんど動けず、テストが止まった。`relax` で譲ったタスクは、次に動くまで優先度を 0 として扱い、列の順で相手に回すようにした(`Pick::Anyone` と `yielded`)。カーネル側のプロセス待ち(`boot::wait`)も `yield_now` から `relax` に直した
- **musl の `sched_setscheduler` は ENOSYS**:musl はスレッド単位の Linux の意味を嫌い、この関数を「常に失敗」にしている。`syscall(SYS_sched_setscheduler, ...)` で直接呼ぶ
- **ティックでしか起こせない**:最初はスケジューラが動くのがティック(10ms ごと)だけで、遅れの最大がほぼ 10ms になった。目覚まし(ティックとは別のタイマ割り込み)を足して 0.3ms 以下になった

## まだのこと

- 本当の待ち行列(wait queue)。今の待ちは「譲って確かめる」なので、イベントが来たタスクがすぐ起きるとは限らない(GPIO や読み込み待ちのリアルタイム性はこれから)
- SCHED_RR、他のプロセスの優先度の変更、優先度の継承(ロックを持った低い優先度のタスク問題)
- 実機での計測(QEMU は CPU 時間が揺れるので、数字は目安)
