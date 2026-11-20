# フェーズ2-09 小さなシェル

## 何をしたか

- `chdir` システムコール(ディレクトリでなければ `ENOTDIR`、実行の権限も確かめる)
- `/bin/sh`(C、約 90 行):プロンプトを出して 1 行読み、空白で区切る。組み込みは `cd`、`pwd`、`exit`。それ以外は fork して `/bin/<名前>`(`/` を含めばそのパス)を execve し、wait する。0 以外で終わったら `[exit N]` を出す
- `/etc/init.rc` の最後をシェルにした
- スナップショットの入力を「この文字列が出たら、これを打つ」の並び(`xtask::SHELL_SCRIPT`)にした:シェルで `hello`、`qjs` で REPL に入って `1+2`、`^D` で戻って `exit`

```
pi-os:/$ hello
Hello from user space!
pi-os:/$ qjs
QuickJS - Type "\h" for help
qjs > 1+2
3
qjs > ^D
pi-os:/$ exit
```

## なぜ自作のシェルか

計画書では BusyBox を動かす予定。BusyBox のソースを入れるにはダウンロードの許可が必要なので、先に小さなシェルで fork / exec / wait の流れを確かめた。シェルの中身が短いので、OS の上で何が起きているか(どのシステムコールをどの順で呼ぶか)を追いやすい。

## 中で何が起きているか

```
sh:printf("pi-os:/$ ") → writev
sh:fgets → read(0)            ← TTY の canonical モードで 1 行がそろうまで待つ
sh:fork → clone               ← 子は sh の写し
子:execve("/bin/hello", …)    ← 子のユーザー空間を hello に差し替える
sh:waitpid → wait4            ← 子が終わるまで待つ(待つ間はほかのタスクへ譲る)
子:write → exit_group         ← ゾンビになる
sh:wait4 が子の番号を返す → 次のプロンプト
```

このシェルを作っているときに、TPIDR_EL0 がプロセスの間で漏れる不具合が見つかった(フェーズ2-08)。
