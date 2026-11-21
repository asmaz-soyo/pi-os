# フェーズ2-47 VFS:「ない」ことも覚える(negative dentry)

## 何をしたか

- dentry キャッシュの値を `Option` にし、`None` を「その名前はない」の記憶にした(negative dentry)
- ファイルシステムが ENOENT を返したら、それも覚える(覚えてよいファイルシステムだけ)
- 名前を作る操作(create、mkdir、symlink、link、rename の先)は、もともとそのパスの記憶を消していた(`forget`)ので、「ない」の記憶も消える
- テスト:ない名前を 2 回探すと、2 回目はディスクを読まない。作る 5 通りの操作のあとで、それぞれ見えるようになる

## なぜ必要か

ない名前は意外とよく探される:
- シェルは PATH の順(/usr/local/bin、/usr/bin、/bin …)にコマンドを探す
- Python の import は sys.path のディレクトリを順に、いくつもの拡張子(.py、.pyc、パッケージのディレクトリ …)で探す
