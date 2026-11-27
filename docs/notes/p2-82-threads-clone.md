# フェーズ2-82 スレッド(2):clone でスレッドを作る

## 何をしたか(ADR-0009)

- `clone(flags, newsp, parent_tid, tls, child_tid)`(aarch64 の引数の順)。CLONE_THREAD があればスレッド:
  - `Process::thread`:メモリと fd の表を共有(`share`)、グループの番号(tgid)と親は同じ
  - CLONE_SETTLS:子の TPIDR_EL0(TLS の場所)を `tls` にする
  - CLONE_PARENT_SETTID / CLONE_CHILD_SETTID:子の番号が決まったら、そこに書く(スレッドはメモリが同じなので、親からも見える)
  - CLONE_CHILD_CLEARTID:子が終わるときに 0 を書く場所
  - CLONE_THREAD には CLONE_SIGHAND が、CLONE_SIGHAND には CLONE_VM が要る(Linux と同じ決まり。なければ EINVAL)
- `getpid` はグループの番号、`gettid` はスレッドの番号。`set_tid_address` は終わるときに 0 を書く場所を覚える
- `exit` はそのスレッドだけ、`exit_group` はグループ全体を終える

## 確かめ方:thread0

musl の公開の `clone()` は、CLONE_THREAD と CLONE_CHILD_CLEARTID を断る(TLS が食い違うので。pthread_create は内部の `__clone` を使う)。そこで thread0 は、clone のシステムコールを直接呼ぶ小さなアセンブリを持つ:

```text
raw_clone:  stp  x5, x6, [x1, #-16]!   新しいスタックに (関数, 引数) を積む
            mov  x8, #220 ; svc #0     clone
            cbz  x0, 1f                子は 0 が返る
            ret                        親:子の番号
1:          ldp  x1, x0, [sp], #16     子:新しいスタックから (関数, 引数)
            blr  x1                    関数(引数)
            mov  x8, #93 ; svc #0      exit(このスレッドだけ)
```

子は同じメモリに書き、親は CLONE_CHILD_CLEARTID の場所が 0 になるまで譲りながら待つ(futex はまだないので)。互換テストで Linux(qemu-aarch64)と同じ結果になる。

## 確かめたこと

- ホスト:スレッドのフラグの clone(共有、tgid、TLS、tid の場所)、CLONE_VM なしの CLONE_THREAD は EINVAL、getpid / gettid / set_tid_address、exit と exit_group
- QEMU(両方の BSP)と互換テスト:thread0。フレームが全部返ることを確かめるテストもそのまま通る

## 次

- futex(待つ・起こす)、musl の pthread(ミューテックス、条件変数、join)
