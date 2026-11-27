# フェーズ2-84 Rust の標準ライブラリのスレッド

## 何をしたか

OS 上の uv(Rust 製。main を大きなスタックのスレッドで動かし、tokio のスレッドを使う)の準備として、Rust の標準ライブラリのスレッドを一通り使うプログラム `rthreads`(`user/rust/rthreads`)を足した。

| 使うもの | 確かめること |
| --- | --- |
| `thread::spawn` と `Mutex` | 4 スレッド × 10000 回 → 40000 |
| `mpsc::channel` | 3 つの送り手から 300 個 → 合計 314850 |
| `thread::scope` | 借りたデータを 4 つに分けて書き換える |
| `thread::Builder` | 名前("worker")と 4MB のスタック |
| `available_parallelism` | CPU の数(sched_getaffinity)を聞ける |

Rust の std は、musl の pthread の上に作られている(スレッドのスタックのガードのページ、TLS、futex)。p2-83 で pthread が動いたので、そのまま動いた。足りないシステムコールもなかった(`/proc/pios/unimplemented` が空)。

## 確かめたこと

- QEMU(両方の BSP)と互換テスト:Linux(qemu-aarch64)と同じ出力
