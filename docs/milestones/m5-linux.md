# M5 最小の Linux 互換と initramfs

## 完了条件

- [x] initramfs(tar)を埋め込み、パスでプログラムを読み込む
- [x] VFS(`File` トレイト、fd の表、マウント)、`/dev/console`、`/dev/null`
- [x] musl の C プログラムが動く(printf、fopen、malloc)
- [x] 未実装のシステムコールのログ(番号ごとに1回)とトレース(`--trace syscall`)
- [x] システムコール対応表の自動生成(`docs/generated/syscalls.md`)
- [ ] システムコールの互換テスト(`cargo xtask test --compat`):Linux の `qemu-aarch64` が必要。未実施

`docs/notes/m5-*.md` を参照。
