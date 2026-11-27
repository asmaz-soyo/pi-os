# フェーズ2-87 シグナル(2):ハンドラを呼ぶ

## 何をしたか

ユーザーに戻る直前に、ハンドラのあるシグナルを見つけたら:

1. 割り込まれたときのレジスタと元のマスクを、ユーザーのスタックに積む(シグナルフレーム)。SA_ONSTACK で sigaltstack があり、まだそのスタックの上にいなければ、別のスタックに積む
2. マスクを足す:`sa_mask` と、SA_NODEFER でなければ同じシグナル(ハンドラが入れ子にならない)。SA_RESETHAND なら設定を既定に戻す
3. ハンドラから始めるようにレジスタを変える:`x0 = 番号`、`x1 = &siginfo`、`x2 = &ucontext`、`x30(戻り先)= sa_restorer`、`sp = フレーム`、`pc = ハンドラ`

ハンドラが戻ると、restorer(musl が用意する `mov x8, #139; svc #0`)が rt_sigreturn を呼び、カーネルはフレームからレジスタとマスクを戻す。

### シグナルフレーム(`arch/aarch64/src/signal.rs`)

Linux の arm64 の `struct rt_sigframe` と同じ並びにした(ハンドラが ucontext を見るプログラムもあるので)。

```text
sp →  0     siginfo(128 バイト:signo、errno、code、pid、uid)
      128   ucontext:uc_flags、uc_link、uc_stack、uc_sigmask(+40)
      304     sigcontext:fault_address、x0〜x30、sp、pc、pstate
      592       fpsimd_context:magic 0x46508001、size 528、fpsr、fpcr、v0〜v31 → 終わりの印
      4688  フレームの記録(fp、lr)
```

- rt_sigreturn で戻す PSTATE は条件フラグ(NZCV)だけ。ハンドラがフレームを書き換えて EL1 に入ったり、割り込みを止めたりできないように
- TLS(TPIDR_EL0)はフレームに入れない(Linux も別の部品。スレッドのものなので、そのまま)
- フレームを積めない(スタックが壊れている)なら、SIGSEGV で終わる

### rt_sigreturn

レジスタの並びはアーキテクチャのものなので、カーネルの共通部分は `Action::SigReturn` を返すだけにし、boot がフレームを読んで戻す。

### sigaltstack と、待ちの中断

- sigaltstack は設定を本当に覚えるようにした(スレッドごと。fork は受け継ぐ)。MINSIGSTKSZ(5120)より小さければ ENOMEM
- poll / select / epoll_wait も、マスクされていないシグナルで EINTR(musl の pause() は ppoll(NULL, 0, NULL))

## 確かめたこと(signals。QEMU の両方の BSP と、互換テストで Linux と同じ)

ハンドラと siginfo、戻ったあともレジスタがそのまま、ハンドラの中の同じシグナルは戻ってから届く、マスクで保留になり外すと届く、別のスタックで動く、SA_RESETHAND、SIG_IGN、子に SIGTERM を送ると WTERMSIG = 15、子が終わると SIGCHLD のハンドラ

## まだのこと

- パイプ・ソケット・端末の read と wait4 の中断(EINTR)、SA_RESTART、rt_sigsuspend
