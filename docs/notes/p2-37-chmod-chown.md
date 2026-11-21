# フェーズ2-37 VFS:chmod、chown と、作ったファイルの持ち主

## 何をしたか

- `File::set_mode`、`File::set_owner`(読み取り専用のファイルシステムでは EROFS)
- tmpfs:各ノードの mode・uid・gid を `SpinLock<Meta>` にまとめ、後から変えられるようにした(ディレクトリの mode が `& 0o777` でスティッキービットを落としていたのも直した)
- ext2:inode の mode(種類のビットは残す)と uid・gid を書き換える。uid と gid の上の 16 ビット(inode の 120 バイト目と 122 バイト目、Linux の拡張)も読み書きする
- システムコール:`fchmod`(52)、`fchmodat`(53)、`fchownat`(54、`AT_SYMLINK_NOFOLLOW` で lchown)、`fchown`(55)
  - chmod は持ち主か root だけ(EPERM)。setuid などの上のビットも持てる
  - chown は root だけ(EPERM)。-1 はその値を変えない
- 作ったファイル・ディレクトリ・シンボリックリンクは、作ったプロセスの実効 uid と gid の持ち物にする(これまでは誰が作っても root の持ち物だった)
- テスト:tmpfs でのシステムコール、ext2 で再マウント後も残ることと `e2fsck`、互換テストの chmod / fchmod

## なぜ必要か

ADR-0005(複数のユーザー)の権限の仕組みは、ファイルの持ち主とパーミッションがあって初めて意味を持つ。一般ユーザーが作ったファイルが root の持ち物になっていると、作った本人が chmod できない。

## まだのこと

- umask(作るときの mode から消すビット)
- 一般ユーザーが、自分の属するグループへ chgrp すること
- setuid のビットを exec で効かせること
- 時刻(stat の atime / mtime / ctime、utimensat)
