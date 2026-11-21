# フェーズ2-56 VFS:statfs、/proc/mounts、st_dev

## 何をしたか

- `statfs`(43)、`fstatfs`(44):ファイルシステム全体の情報(`struct statfs`)
  - `FileSystem::statfs`:種類(Linux と同じマジック番号)、ブロックの大きさ、ブロックと inode の全体・空き
  - ext2:スーパーブロックの値(全体、空き、root のために予約された分を引いた「一般ユーザーが使える空き」)
  - FAT32:クラスタの数と、FSInfo セクタに書いてある空きの数
  - tmpfs(0x01021994)、initramfs(ramfs、0x858458f6)、procfs(0x9fa0)、devfs(tmpfs と同じ番号)は種類だけ
  - `fstatfs` は、fd を開いたときのパスで調べる
- `/proc/mounts`:「デバイス マウント先 種類 オプション 0 0」。カーネルにマウントの記録(`fs::record_mount`)を置き、boot がマウントするたびに記録する
- stat の `st_dev`:マウントごとに違う番号(これまでは全部 0)。BusyBox の `df` は、パスとマウント先の `st_dev` を比べて、どのマウントかを決める
- テスト:statfs の種類、ext2 の空きが書いた分だけ減ること(e2fsck が確かめる値と同じ)、FAT32 の FSInfo、/proc/mounts の形、st_dev がマウントごとに違うこと。QEMU で `df / /boot`

```
Filesystem           1024-blocks    Used Available Use% Mounted on
/dev/vda2                65536     31940     30320  51% /
/dev/vda1                34264       …           … …   /boot
```

## つまずいたところ

`df` の 1 回目は「can't find mount point」。/proc/mounts がなかった。足したら、今度は `/boot` もルートの数を出した。`st_dev` が全部 0 で、どのマウントも同じに見えていた
