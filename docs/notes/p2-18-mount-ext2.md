# フェーズ2-18 ディスクの ext2 パーティションを /mnt にマウントする

## 何をしたか

- `kernel::block::PartitionDevice`:ディスクの一部(パーティション)を 1 つのブロックデバイスとして見せる(ブロック番号をずらし、範囲を限る)
- xtask のディスクイメージ(4MiB):MBR、目印のセクタ、LBA 2048 からの ext2 パーティション(`user/diskroot/` の中身を `mke2fs -d` で入れる)
- boot:`/dev/vda` の MBR を読み、種類 0x83 のパーティションを ext2 として `/mnt` にマウントする
- virt 専用のカーネル内テスト:`/bin/ext2read`(C)が `/mnt/README.txt` と `/mnt/docs/notes.txt` を読み、`/mnt` の一覧に `docs` があることを確かめる

```
virtio-blk: /dev/vda, 8192 sectors
ext2: /dev/vda1 mounted on /mnt
pi-os:/$ ls /
bin
dev
etc
hello.js
mnt
proc
tmp
```

## 次

- ext2 への書き込み(ブロックと inode の割り当て、ディレクトリへの項目の追加)
- ルート(`/`)を ext2 にする(計画書:SD カードの p2)
