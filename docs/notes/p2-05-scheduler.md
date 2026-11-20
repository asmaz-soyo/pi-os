# フェーズ2-05 コンテキストスイッチと協調的なスケジューラ

## 何をしたか

- `kernel::sched`:タスク(カーネルスタック + 保存した文脈)、実行待ちの列(ラウンドロビン)、`spawn`、`yield_now`、`exit_current`
- `kernel::sched::Platform`:CPU 依存の操作(文脈の切り替え、新しい文脈、スタックの確保、割り込みの禁止)を BSP から受け取る
- `arch_aarch64::context::switch`:x19〜x30 と SP を保存し、切り替え先のものを戻して `ret`
- `arch_aarch64::context::new_thread_context`:新しいスレッドの文脈。最初の `ret` で `arch_thread_start` に入り、`entry(arg)` を呼ぶ
- 起動処理(`boot::init`)がそのままタスク 0 になる
- カーネル内テスト:2 つのカーネルスレッドが `yield` で交互に動き(`ABABAB`)、終わったらタスクが片付く

## なぜ必要か

fork したプロセスや、入力を待つプロセスがいると、CPU を別のタスクに渡す仕組みが要る。フェーズ2の「スケジューラ、fork、シェル」の土台。

## 中で何が起きているか

### 文脈の切り替え

```
タスク A が yield_now()
  → schedule():次のタスク B を選ぶ
  → switch(&A.ctx, &B.ctx)
       x19〜x30 と SP を A.ctx に保存
       B.ctx から x19〜x30 と SP を戻す
       ret → B の x30(B が前に switch を呼んだ場所の続き)へ
  → B の yield_now() が戻る(B から見ると、ただの関数呼び出し)
```

保存するのは「関数呼び出しをまたいで保存される」レジスタ(callee-saved)だけでよい。switch を呼ぶ側にとって、それ以外のレジスタは呼び出しで壊れてよいものだから。

### 新しいスレッドの始まり方

新しいスレッドの文脈は「x30 = `arch_thread_start`、SP = 新しいスタックの一番上」にしておく。最初に切り替えられたとき、switch の `ret` が `arch_thread_start` に飛び、そこで IRQ を許可して `entry(arg)` を呼ぶ。`entry` が戻ったら `exit_current` を呼ぶ。

### 終わったタスクの片付け

終わったタスクは、自分のスタックの上で動いているので、自分のスタックを返せない。次に誰かが `schedule` を呼んだときに返す。
