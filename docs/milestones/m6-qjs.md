# M6 QuickJS(フェーズ1のゴール)

## 完了条件

- [x] QuickJS(公式 2026-06-04)を aarch64-linux-musl の静的リンクでビルドし、initramfs に入れる
- [x] `qjs hello.js` が UART に「Hello from JS」を出す(qemu-virt、qemu raspi3b、スナップショットで確認)
- [ ] 実機の Pi 3B でも確認する(手動。未実施)

`docs/notes/m6-01-quickjs.md` を参照。
