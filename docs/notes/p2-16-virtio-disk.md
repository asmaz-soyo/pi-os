# フェーズ2-16 virtio-blk のディスクを QEMU virt で使う

## 何をしたか

- `drivers/virtio-blk`:virtio-mmio(version 2)の virtio-blk ドライバ。ポーリングで 512 バイトずつ読み書きする。ホストでは、デバイスの代わりに DMA のメモリへ完了を書き込んでテストする
- `Board::VIRTIO_MMIO`:virtio-mmio の枠の一覧。boot が起動時に調べ、virtio-blk を `vda` として登録する
- `kernel::block::{register, find, BlockFile}`:登録したブロックデバイスを、devfs が `/dev/vda` として見せる(バイト単位の位置で読み書きできる)
- `pread64` / `pwrite64`
- xtask:テスト用のディスクイメージ(`target/virt-disk.img`、1MiB、MBR と目印)を作って QEMU に渡す
- virt 専用のカーネル内テスト:C のプログラムが `/dev/vda` から目印と MBR の印を読み、最後のセクタに書いて読み戻す

```
virtio-blk: /dev/vda, 2048 sectors
```

## なぜ必要か

FAT32 や ext2 を「本物のディスク」の上で動かすため。実機の SD カードのドライバ(SDHCI / EMMC)は raspi3 の BSP で別に作るが、ファイルシステムは `BlockDevice` しか使わないので、QEMU virt の virtio-blk で先に作って確かめられる。

## 中で何が起きているか

### virtqueue での読み出し(1 セクタ)

```
1. ヘッダ(種類 = 読む、セクタ番号)、データ(512 バイト)、状態(1 バイト)を DMA 領域に用意する
2. 記述子 3 つを鎖にする:ヘッダ → データ(デバイスが書く)→ 状態(デバイスが書く)
3. avail リングに鎖の先頭(記述子 0)を載せ、idx を 1 進める
4. QueueNotify に 0 を書いてデバイスに知らせる
5. used リングの idx が進むまで待つ → 状態が 0 なら成功、データを取り出す
```

ドライバとデバイスの間でメモリの書き込みの順番が入れ替わらないよう、リングの idx を書く前後にメモリの壁(`fence`)を置いている。

### QEMU の設定

- `-global virtio-mmio.force-legacy=false`:virtio-mmio の新しい形(version 2)を使う(既定は古い形)
- `-drive file=…,if=none,id=hd0 -device virtio-blk-device,drive=hd0`:ディスクイメージを virtio-blk として渡す
- cargo の runner は BSP のクレートのディレクトリで QEMU を動かすので、ディスクのパスは `../../target/virt-disk.img`
