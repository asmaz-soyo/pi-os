# フェーズ2-24 ルート(/)を ext2 に

## 何をしたか

- テスト用のディスクの p2(ext2、32MiB)に、ユーザーランド全体(initramfs と同じ `bin/`、`etc/`、`hello.js`)と `user/diskroot` を入れた
  - xtask が `cargo build -p userland --message-format=json` で、build.rs が作った `initramfs.tar` の場所(`out_dir`)を聞き、展開する
  - `fakeroot` の中で持ち主を root(0:0)にしてから `mke2fs -d` で作る(手元のファイルは自分の uid なので、そのままだと全部 uid 1000 の持ち物になる)
- 起動時:ディスクの ext2 に `/bin/sh` があれば、それをルート(`/`)にする。なければ(ディスクのない Pi の実機など)、これまでどおり埋め込みの initramfs をルートにする
- `/dev`、`/tmp`、`/proc`、`/boot`(FAT32)はその上にマウント
- `/bin/ext2read` はルートのファイルを読み書きするように変えた(QEMU のテスト名も `the_ext2_partition_of_the_disk_is_the_root`)

```
ext2: /dev/vda2 mounted on /
fat32: /dev/vda1 mounted on /boot
...
pi-os:/$ ls /
README.txt
bin
boot
dev
docs
etc
hello.js
proc
tmp
```

QEMU virt では、init もシェルも qjs も、すべてディスクから読んで動いている。

## なぜ必要か

計画書:SD カードは「p1 = FAT32(`/boot`)、p2 = ext2(`/`)」。ルートを ext2 にすると、書いたファイルが再起動後も残り、シンボリックリンクやパーミッション(uv や npm が使う)も持てる。initramfs はカーネルに埋め込むので、プログラムを足すたびにカーネルを作り直す必要があった。

## 中で何が起きているか

```
起動 → virtio-blk を見つける(/dev/vda)
  → MBR:p1 = 0x0c(FAT32)、p2 = 0x83(Linux)
  → p2 を Ext2 として開き、/bin/sh を探す → ある → Vfs::new(ext2)
  → /dev、/tmp、/proc、/boot(p1 の Fat32)をマウント
  → /etc/init.rc を ext2 から読む → /bin/sh を ext2 から読んで(read_image)実行
```

## まだのこと

- 実機の SD カード(EMMC のドライバ)。それまで Pi の実機は initramfs がルート
- ルートを切り替える仕組み(Linux の `switch_root`)。今は起動時に一度だけ選ぶ
- シンボリックリンク(ext2 の読み書きはまだ通常のファイルとディレクトリだけ)
