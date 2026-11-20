# フェーズ2-15 ブロック層の始まり(BlockDevice、RAM ディスク、MBR)

## 何をしたか

- `hal::block::BlockDevice`:512 バイト単位で読み書きするディスクのポート
- `kernel::block::ramdisk::RamDisk`:メモリの上のディスク(テストと開発用)
- `kernel::block::mbr::parse`:MBR のパーティション表を読む

## なぜ必要か

フェーズ2の「ブロック層、FAT32、ext2」。SD カード(実機)や virtio-blk(QEMU virt)のドライバは `BlockDevice` を実装し、FAT32 や ext2 は `BlockDevice` だけを使う。ドライバとファイルシステムを別々にテストできる(FAT32 や ext2 はホストで RAM ディスクを使って試せる)。

## 中で何が起きているか

### MBR

```
オフセット 0〜445   ブートローダのコード(Pi では使わない)
オフセット 446〜509 パーティション表(16 バイト × 4)
    +4  種類(0x0c = FAT32 LBA、0x83 = Linux)
    +8  最初の LBA(リトルエンディアン 32 ビット)
    +12 ブロックの数
オフセット 510〜511 印 0x55 0xAA
```

種類が 0 か、ブロックの数が 0 の項目は「空き」として飛ばす。
