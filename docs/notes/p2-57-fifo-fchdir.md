# フェーズ2-57 VFS:名前付きパイプ(FIFO)と fchdir

## 何をしたか

- `mknodat`(33):名前付きパイプ(`S_IFIFO`)と普通のファイルを作る。デバイスファイルはまだ作れない(EPERM)。umask と持ち主も、ほかの作るものと同じ
- `FileSystem::mknod`:tmpfs は `TmpFifo`(名前と持ち主だけ)、ext2 は種類 `S_IFIFO` の inode(ブロックなし、ディレクトリの項目の種類は 5)
- FIFO を開くと(`openat`)、同じ FIFO を開いた人どうしをパイプでつなぐ(`pipe::open_fifo`)
  - 同じ FIFO かどうかは (st_dev, inode 番号) で見分ける。tmpfs でも ext2 でも同じ仕組み
  - 読む側は書く側が来るまで、書く側は読む側が来るまで、開くところで待つ。`O_NONBLOCK` なら待たない(書く側は読む側がいなければ ENXIO)
  - パイプは「閉じたか」の印ではなく、開いている読む側・書く側の数を持つようにした(FIFO は何人も開いたり閉じたりする)
- `fchdir`(50):開いたディレクトリに移る
- テスト:tmpfs でのシステムコール、ext2 の FIFO の inode(`e2fsck`)、互換テスト(Linux と一致)

## なぜ必要か

名前付きパイプは、関係のないプロセスどうしがパスでつながる方法(シェルの `mkfifo`、ログの受け渡し、ビルドツールの jobserver など)。fchdir は `find` や `rm -r` がディレクトリをたどるときに使う

## まだのこと

- FIFO を読み書き両方(O_RDWR)で開くこと
- デバイスファイル(mknod の S_IFCHR、S_IFBLK)
