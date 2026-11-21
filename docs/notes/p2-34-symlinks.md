# フェーズ2-34 VFS:シンボリックリンク(VFS と tmpfs)

## 何をしたか

- `FileKind::Symlink`、`File::readlink`(指す先のパス)、`FileSystem::symlink(path, target)`
- `Vfs::resolve_path(path, follow_last)`:パスを 1 要素ずつたどり、シンボリックリンクに当たったら指す先に置き換えて続ける
  - 絶対のリンクはルートから、相対のリンクはリンクのあるディレクトリから
  - `..` はリンクをたどった後のディレクトリで計算する(`/tmp/etc/..` は、`/tmp/etc → /etc` なら `/`)
  - 追ったリンクが 40 を超えたら `ELOOP`。途中がディレクトリでなければ `ENOTDIR`
  - `follow_last`:最後の要素もリンクなら追うか。open、stat、chdir、exec は追う。lstat、readlink、unlink、rename、mkdir は追わない(リンクそのものを扱う)
  - 作る(O_CREAT)ときに最後が「指す先のないリンク」なら、指す先に作る
- tmpfs:`TmpLink`(指す先の文字列を持つだけ)
- システムコール:`symlinkat`(36)、`readlinkat`(78。NUL を付けず、入らなければ切り詰める)、`newfstatat` の `AT_SYMLINK_NOFOLLOW`(lstat)、`openat` の `O_NOFOLLOW`(最後がリンクなら ELOOP)。stat の種類は `S_IFLNK`、getdents は `DT_LNK`
- 互換テストにリンクのケースを足した(Linux と一致)

## 中で何が起きているか

```
open("/tmp/etc/motd")、/tmp/etc → /etc のとき
  todo = [tmp, etc, motd]、cur = /
  tmp  → /tmp はディレクトリ           → cur = /tmp
  etc  → /tmp/etc はリンク("/etc")    → cur = /、todo = [etc, motd]
  etc  → /etc はディレクトリ           → cur = /etc
  motd → /etc/motd(最後、ファイル)    → cur = /etc/motd
  → マウントの表で /etc/motd を担当するファイルシステムに渡す
```

## まだのこと

- ext2 のシンボリックリンク(次)、initramfs(tar)のリンク
- 1 要素ごとにルートから探し直すので、深いパスは遅い(ext2 ではディレクトリを何度も読む)。VFS がディレクトリの実体を覚えてたどる形(dentry のキャッシュ)にすると速くなる
