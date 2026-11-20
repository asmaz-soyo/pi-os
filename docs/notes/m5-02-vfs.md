# M5-02 VFS とファイルのシステムコール

## 何をしたか

- `kernel::fs::vfs`:`File` トレイト、`OpenFile`(読み書きの位置とフラグ)、`FdTable`、パスの正規化、マウントの表(`Vfs`)
- `kernel::fs::initramfs`:tar の中身を読み出し専用のファイルシステムとして見せる(親ディレクトリは自動で作る)
- `kernel::fs::devfs`:`/dev/console`(UART。端末として振る舞う)と `/dev/null`
- プロセスにファイル記述子の表と作業ディレクトリを持たせた。fd 0, 1, 2 は `/dev/console`
- システムコール:`openat`、`close`、`read`、`write`、`readv`、`writev`、`lseek`、`fstat`、`newfstatat`、`ioctl`(TIOCGWINSZ)、`getcwd`
- musl の `printf` を使う C プログラムが動いた(`/bin/hello_musl`)

## なぜ必要か

- `qjs hello.js` は、`hello.js` を開いて読み、結果を標準出力に書く。ファイルと端末を同じ「fd に read / write する」形で扱えることが前提
- 計画書の差し込み口「VFS の `File` トレイト」:ファイル、パイプ、ソケット、timerfd、GPIO を、後から同じ形で足せる

## 中で何が起きているか

```
read(fd=3, buf, 100)
  → FdTable[3] → OpenFile { offset: 7, flags: O_RDONLY } → Arc<dyn File>(initramfs の RamFile)
  → file.read(offset=7, カーネルのバッファ) → copy_to_user(buf, …) → offset += 読めた量
```

- 読み書きの位置は `File` ではなく `OpenFile` が持つ。同じファイルを2回開くと、それぞれ別の位置を持つ(Linux と同じ)
- パスの解決:作業ディレクトリと合わせて絶対パスにし、`.`、`..`、重なった `/` を取り除いてから、マウントの表で「一番長く一致するマウントポイント」のファイルシステムに渡す
- musl は標準出力が端末かどうかを `ioctl(1, TIOCGWINSZ)` で調べ、端末なら行ごとにバッファを書き出す。`/dev/console` はこれに 24 行 × 80 桁と答える
- `struct stat` は Linux AArch64 の 128 バイトの並びどおりに作る(`st_mode` にファイルの種類のビット S_IFREG / S_IFDIR / S_IFCHR)

## 今の限界

- ファイルシステムはすべて読み出し専用(書けるのは端末と `/dev/null`)
- ディレクトリの中身の一覧(`getdents64`)はまだない
- 端末からの入力(UART の受信)はフェーズ2
