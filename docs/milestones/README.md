# マイルストーン

| マイルストーン | 状態 | タグ |
| --- | --- | --- |
| [M0 開発基盤](m0-foundation.md) | 完了 | `m0-foundation` |
| [M1 起動とUART](m1-uart.md) | 完了(実機確認は未) | `m1-uart` |
| [M2 例外と割り込み](m2-irq.md) | 完了(ブートローダは後回し) | `m2-irq` |
| [M3 メモリ管理](m3-mm.md) | 完了 | `m3-mm` |
| [M4 ユーザープロセス](m4-user.md) | 完了 | `m4-user` |
| [M5 最小のLinux互換+initramfs](m5-linux.md) | 完了(互換テストは未) | `m5-linux` |
| [M6 QuickJS](m6-qjs.md) | 完了(実機確認は未) | `m6-qjs` |

## フェーズ2

| 項目 | 状態 | タグ |
| --- | --- | --- |
| 権限の入れ物(ADR-0005) | 完了 | — |
| 実機用イメージの終わり方 | 完了 | — |
| UART 入力、TTY、JS の REPL | 完了 | `p2-repl` |
| スケジューラ、fork、シェル、シバン | 完了 | `p2-shell` |
| BusyBox(ash とコマンド)、パイプ | 完了 | `p2-busybox` |
| システムコールの互換テスト(qemu-aarch64 と比べる) | 完了 | `p2-compat` |
| tmpfs、timerfd、epoll、サービスマネージャ | 完了 | `p2-services` |
| サービスマネージャを Rust で(依存クレートなし) | 完了 | `p2-rust-svcman` |
| MicroPython(unix ポート) | 完了 | `p2-micropython` |
| 動的リンク(ld-musl) | 完了 | `p2-dynamic` |
| CPython 3.15(QEMU virt のディスクから) | 完了 | `p2-cpython` |
| VFS の作り込み:rename、dirfd、シンボリックリンク、chmod/chown、時刻、ハードリンク | 完了 | `p2-vfs` |
| GPIO 割り込みの fd、リアルタイム優先度、ジッタ計測 | 完了 | `p2-realtime` |
| ブロック層(virtio-blk、MBR)、ext2 の読み書き、procfs | 完了 | `p2-ext2` |
| FAT32 の読み込み(`/boot`) | 完了 | `p2-fat32` |
| ルート(`/`)を ext2 に(QEMU virt。ディスクのない実機は initramfs) | 完了 | `p2-rootfs` |
