# M4 ユーザープロセス

## 完了条件

- [x] EL0 でユーザープログラムを動かす
- [x] SVC の入口(システムコール)と、ユーザーのフォールトの処理
- [x] 静的 ELF ローダと初期スタック(argc、argv、envp、補助ベクタ)
- [x] ユーザー空間への戻り口を1か所にまとめる(`exception_return` と `before_return_to_user`)
- [x] `copy_from_user` / `copy_to_user`

`docs/notes/m4-*.md` を参照。
