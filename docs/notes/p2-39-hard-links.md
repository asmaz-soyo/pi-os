# フェーズ2-39 VFS:ハードリンク

## 何をしたか

- `FileSystem::link(old, new)`、`Vfs::link(old, new, follow)`、`linkat`(37。`AT_SYMLINK_FOLLOW` なら old のリンクを追う。既定では追わない)
  - 名前がない → ENOENT、ファイルシステムをまたぐ → EXDEV、ディレクトリ → EPERM、new がある → EEXIST
- `Stat::nlink`(名前の数)。stat の `st_nlink` に出す(0 は数えていないもので、1 として見せる)
- tmpfs:同じファイル(`Arc`)を、別のパスの鍵でも持つ。リンク数は `Meta` に持ち、link で増やし、unlink と rename の置き換えで減らす(ノードは `Arc<dyn File>` なので、`as_any` で tmpfs のノードに戻して数える)
- ext2:inode のリンク数を増やし、新しい名前の項目を足す。unlink はリンク数が 2 以上なら減らすだけで、inode とブロックは最後の名前が消えたときに返す
- テスト:tmpfs でのシステムコール、ext2 で `e2fsck`、互換テスト(Linux と一致)

## なぜ必要か

同じ中身に複数の名前を付ける。パッケージマネージャ(pip や npm のキャッシュ)、`git`、`rsync --link-dest` などがコピーの代わりに使う。

## まだのこと

- ディレクトリのリンク数(Linux では 2 + 子のディレクトリの数)。tmpfs は数えていない
