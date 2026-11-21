# フェーズ2-32 VFS:rename

## 何をしたか

- `FileSystem::rename(old, new)`:名前を変える、別のディレクトリに移す。`new` があれば置き換える
  - 置き換えてよいかは共通の `vfs::check_replace`:ファイルはファイルを、ディレクトリは空のディレクトリだけを置き換えられる(EISDIR、ENOTDIR、ENOTEMPTY)
- `Vfs::rename(old, new, noreplace)`:ファイルシステムに渡す前に、VFS の層で確かめる
  - `old` がない → ENOENT
  - マウントをまたぐ、マウントポイント自体を動かす → EXDEV
  - 同じパス → 何もしない
  - 自分の下へ(`/a` → `/a/b`)→ EINVAL
  - `noreplace`(`RENAME_NOREPLACE`)で `new` がある → EEXIST
- tmpfs:パスが鍵の表なので、自分と中身すべての鍵を付け替える
- ext2:新しい名前の項目を足してから、古い名前の項目を消す。置き換えられる側は unlink と同じ手順で消す(`unlink_locked`)。ディレクトリを別の親に移したら、その「..」を新しい親に向け、両方の親のリンク数を直す(`set_dotdot`)。テストのたびに `e2fsck -fn` で確かめる
- システムコール:`renameat`(38)、`renameat2`(276、`RENAME_NOREPLACE` だけ)。両方の親ディレクトリに書き込みとたどる権限が要る
- 互換テストに rename のケースを足した(Linux と一致)

## なぜ必要か

ファイルを安全に書き換える定番の形は「一時ファイルに書いて、本来の名前に rename する」(途中で止まっても、古いか新しいかのどちらかが必ず残る)。CPython の `.pyc` のキャッシュもこの形で書く。

## ext2 での rename(ディレクトリを別の親へ)

```
/a/sub を /b/sub へ
  1. /b に「sub → sub の inode」の項目を足す
  2. /a から「sub」の項目を消す
  3. sub の「..」を /a から /b に書き換える
  4. /a のリンク数 − 1(sub の「..」が指さなくなった)、/b のリンク数 + 1
```

## まだのこと

- `RENAME_EXCHANGE`(2 つを入れ替える)、スティッキービット(/tmp で他人のファイルを動かせない)
- 書き込みの途中で電源が切れたときの一貫性(ジャーナルがない)
