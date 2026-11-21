# フェーズ2-36 VFS:initramfs のリンクと BusyBox のアプレット

## 何をしたか

- initramfs:tar の種類 `'2'`(シンボリックリンク)を `RamLink` にする。指す先は tar の見出しの中(埋め込みデータなので `'static` のまま指せる)
- `user/build.rs`:
  - BusyBox のビルドで `busybox.links`(アプレットの一覧)も作る
  - 一覧のアプレットごとに `/bin/<名前> → busybox` のリンクを tar に入れる(BusyBox の一覧では /usr/bin や /sbin のものもあるが、シェルの PATH が /bin だけなので /bin にまとめる)。自作のプログラムと同じ名前(sh、ls、cat など)は自作のほうを残す
  - tar を作る関数がリンクを書けるようにした
- xtask:tar を展開するときにリンクを作るので、`mke2fs -d` がそのまま ext2 のリンクにする(QEMU virt のディスクのルートにも入る)
- QEMU の結合テスト(raspi3 と virt の両方):`/bin/seq 3`、`/bin/true`、`/bin/false`

## 中で何が起きているか

```
run_program("/bin/seq", ["seq", "3"])
  → VFS:/bin/seq はリンク("busybox")→ /bin/busybox を開く
  → argv[0] は "seq" のまま
  → BusyBox の main:argv[0] の名前(seq)でアプレットを選ぶ
```

## まだのこと

- init のシェルを BusyBox の ash にする(ジョブ制御が要る)
