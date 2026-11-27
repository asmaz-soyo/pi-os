# フェーズ2-86 シグナル(1):送る、保留する、既定の動作

## 何をしたか

今まで rt_sigaction と rt_sigprocmask は受け付けるだけで、シグナルは配っていなかった(強制終了の印だけがあった)。Node.js や txiki.js、シェルのジョブ(`wait`、Ctrl-C)の前提として、シグナルを作り始めた。

```text
kill / tgkill / 子の終わり(SIGCHLD)── 保留中のビットを立てる(スレッドごと)── 眠っていれば起こす
                                                 ↓
           ユーザーに戻る直前:マスクされていない、いちばん小さい番号を取り出す
                    SIG_IGN・既定が無視 → 捨てる / 既定が終了 → グループごと終わる(128 + 番号)
```

- `kernel/src/signal.rs`:番号、既定の動作(SIGCHLD・SIGWINCH・SIGURG など「知らせるだけ」のものは無視、ほかは終了。止める・続けるはジョブ制御がないので無視)、ハンドラの設定の表(`Actions`)、次に配るものを選ぶ `next`(SIGKILL と SIGSTOP はマスクできない)
- 持ち方:
  - ハンドラの設定:プロセス(スレッドのグループ)で共有(CLONE_SIGHAND)。fork は写し、execve は SIG_IGN 以外を既定に戻す
  - マスク:スレッドごと(`Process::sig_blocked`)
  - 保留中:スレッドごとのビット。プロセスの表とは別に持つ(システムコールの途中で表から取り出されているスレッドにも送れるように)。前の「強制終了の印」はこれに置き換えた(exit_group はほかのスレッドに SIGKILL を送る)
- システムコール:rt_sigaction、rt_sigprocmask(BLOCK / UNBLOCK / SETMASK)、rt_sigpending、kill(プロセスへ。受け取るのは最初のスレッド。シグナル 0 は「いるか」の確かめ)、tkill / tgkill(スレッドへ)
- 子が終わると親に SIGCHLD
- nanosleep / clock_nanosleep は、マスクされていないシグナルが来たら EINTR で戻る(30 秒眠るプロセスに kill を送ると、すぐ終わる)。futex の待ちも同じ

## まだのこと

- ハンドラを呼ぶ(シグナルフレーム、rt_sigreturn)。それまでは、ハンドラを設定したシグナルは無視として扱う(シェルは SIGCHLD などにハンドラを置くので、終了にするとシェルが子の終わりのたびに終わってしまう)
- ほかの待ち(パイプ、ソケット、poll、wait4)を EINTR で中断する、SA_RESTART、rt_sigsuspend
