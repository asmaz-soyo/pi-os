# フェーズ2-33 VFS:ディレクトリの fd を基準にした相対パス

## 何をしたか

- `*at` 系のシステムコール(openat、mkdirat、unlinkat、newfstatat、renameat、faccessat、readlinkat)のパスの解決(`path_arg`)で、`dirfd` がディレクトリの fd なら、そのディレクトリを基準に相対パスを解決する(これまでは `EBADF`)
  - 基準は、その fd を開いたときの絶対パス(`OpenFile::path`)
  - 絶対パスなら `dirfd` は見ない。`AT_FDCWD`(-100)なら今のディレクトリ
  - 開いていない fd は `EBADF`、ディレクトリでない fd は `ENOTDIR`

## なぜ必要か

`find` や `rm -r`、Python の `os.walk(..., dir_fd=)`、`shutil.rmtree` などは、ディレクトリを開いてその fd を基準に中を操作する(途中でパスの上のほうが入れ替えられても、開いたディレクトリの中だけを触れるので安全)。

## まだのこと

- 開いたあとでディレクトリが rename されると、fd が覚えているパスは古いままになる。本当は fd がディレクトリそのもの(inode)を指すべき。VFS をパスではなく「ディレクトリの実体」でたどる作りにするときに直す
