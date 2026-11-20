# フェーズ2-11 tmpfs とファイルの作成・削除

## 何をしたか

- `kernel::fs::tmpfs::Tmpfs`:メモリの上の書けるファイルシステム。`/tmp` にマウントした
- VFS(`FileSystem` トレイト)に `create`、`mkdir`、`unlink`、`readdir` を足した(既定は読み出し専用で `EROFS`)。initramfs と devfs も `readdir` に答える
- `Vfs::readdir` はマウントポイント(`/dev`、`/tmp`)も、その名前のディレクトリとして見せる
- システムコール:`openat` の `O_CREAT` / `O_EXCL` / `O_TRUNC`、`mkdirat`、`unlinkat`(`AT_REMOVEDIR`)、`ftruncate`、`getdents64`、`fcntl`、`faccessat`、`dup`、`dup3`
- ファイル記述子の close-on-exec(`execve` の成功時に閉じる)
- `/bin/ls`、`/bin/cat`、`/bin/tmpfiles`(C)。シェルのスナップショットで `ls /` を確かめる

```
pi-os:/$ ls /
bin
dev
etc
hello.js
tmp
```

## なぜ必要か

フェーズ2の「tmpfs、timerfd、epoll、init とサービスマネージャ」の最初。書けるファイルシステムができたので、ADR-0005 の「ユーザーを作る」(`/etc/passwd` を書き換える)にも一歩近づいた(今は `/etc` が initramfs で読み出し専用なので、まだ書けない)。

## 中で何が起きているか

### tmpfs の作り

パスをキーにした表(`BTreeMap<String, Arc<dyn File>>`)に、ファイルとディレクトリを入れる。ディレクトリの中身は「親のパスが同じ項目」を探して作る。ファイルの中身は `Vec<u8>`。消したファイルも、開いている人がいる間は `Arc` で中身が残る(Unix と同じ)。

### getdents64

ディレクトリの一覧は `struct linux_dirent64` の並びで返す。

| 欄 | 大きさ | 中身 |
| --- | --- | --- |
| d_ino | 8 | inode の番号 |
| d_off | 8 | 次の項目の位置 |
| d_reclen | 2 | この項目の大きさ(8 バイト境界) |
| d_type | 1 | 種類(4 = ディレクトリ、8 = 普通のファイル、2 = 文字デバイス) |
| d_name | 可変 | 名前と NUL |

読む位置(`OpenFile::offset`)を「何番目の項目か」として使う。0 と 1 は `.` と `..`。musl の `readdir` はこれを呼んで1つずつ取り出す。

### 見つかったこと

`tmpfiles` を動かすと、未実装の `fcntl`(25、`opendir` が close-on-exec を設定する)と `faccessat`(48、`access()`)のログが出た。`access` が `ENOSYS` を返しても、テストの書き方によっては通ってしまう。未実装のログを見ることが大事だと分かった。
