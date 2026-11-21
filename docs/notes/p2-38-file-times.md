# フェーズ2-38 VFS:ファイルの時刻

## 何をしたか

- `Stat` に `atime`(最後に読んだ)、`mtime`(中身を変えた)、`ctime`(属性を変えた)を足した(`Timespec`:1970 年からの秒とナノ秒)。`Stat` に `Default` を付け、今あるところは `..Default::default()` にした
- `stat` の結果(`struct stat`)の 72・88・104 バイト目に書く
- いつ変わるか(Linux と同じ):
  - 作ったとき:3 つとも今
  - 書き込み、truncate:mtime と ctime を今に
  - chmod、chown:ctime だけを今に
  - utimensat:atime と mtime を指定どおりに(ctime は今)
  - 読んだときの atime は変えない(Linux の `noatime` と同じ。読むたびにディスクに書かずに済む)
- tmpfs:`Meta` に時刻を持たせた。ext2:inode の 8・12・16 バイト目(32 ビットの秒)を読み書きする
- `utimensat`(88):times が NULL なら両方とも今。ナノ秒が `UTIME_NOW` なら今、`UTIME_OMIT` なら変えない。path が NULL なら fd のファイル(futimens)。時刻を指定できるのは持ち主か root、今にするだけなら書き込める人もよい
- テスト:tmpfs でのシステムコール、ext2 で再マウント後も残ることと `e2fsck`

## なぜ必要か

`make` は mtime を比べて、作り直すものを決める。`ls -l`、`find -newer`、Python の `.pyc`(元の .py の mtime を覚えていて、変わったら作り直す)も時刻を使う。

## まだのこと

- ext2 の時刻は秒まで(2038 年で溢れる。256 バイトの inode の拡張の欄でナノ秒と上位のビットを持てる)
- ディレクトリの中身が変わったときに、ディレクトリの mtime を進めること
- RTC がないので、起動時の時刻はビルドした時刻
