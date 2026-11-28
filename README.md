# Pi-OS

Raspberry Pi 3B 向けの、Rust 製の学習用カーネル。OS を作りながら OS の仕組みを学ぶためのプロジェクトで、作る過程(1ステップ = 1コミット)と中身(学習ノート)を後から追えるようにしている。

- 計画:[docs/plan.md](docs/plan.md)
- 構造:[docs/architecture.md](docs/architecture.md)
- 進み具合:[docs/milestones/](docs/milestones/README.md)
- 学習ノート:[docs/notes/](docs/notes/)
- 設計判断:[docs/adr/](docs/adr/)
- 実装済みのシステムコール:[docs/generated/syscalls.md](docs/generated/syscalls.md)

## 今できること(QEMU)

```
$ cargo xtask run
Pi-OS booting on qemu-virt
Hello from Pi-OS
timer: 10 ticks at 100 Hz
Hello from user space!
init: /bin/hello exited with status 0
Hello from musl! argc=1 argv[0]=/bin/hello_musl
Welcome to Pi-OS!
malloc ok
init: /bin/hello_musl exited with status 0
Hello from JS
init: /bin/qjs exited with status 0
pi-os:/$ hello
Hello from user space!
pi-os:/$ qjs
QuickJS - Type "\h" for help
qjs > 1+2
3
qjs > ^D
pi-os:/$ exit
init: /bin/sh exited with status 0
```

起動の最後に小さなシェル(`/bin/sh`)が動く。`/bin` のプログラムを名前で動かせる(`qjs` で JS の REPL)。組み込みコマンドは `cd`、`pwd`、`exit`。`exit` か `^D` でシェルを終えるとカーネルも終わる。QEMU 自体を止めるには `Ctrl-A` に続けて `x`。

フェーズ1のゴール(自作 OS 上で `qjs hello.js` が「Hello from JS」を出す)は、QEMU で達成した。

フェーズ2で、ほかにもこんなことができるようになった(QEMU virt。くわしくは `docs/notes/p2-*.md`):

| 見られるもの | 試し方 |
| --- | --- |
| BusyBox のシェルとコマンド、MicroPython、CPython | `busybox sh`、`micropython`、`python3` |
| ネットワーク(DHCP、DNS) | `resolve example.com`、`cat /etc/resolv.conf` |
| 2 つの TCP(smoltcp と自作)を比べる | `tcpbulk 7000 262144 0` と `tcpbulk 7000 262144 253`、`cat /proc/pios/tcp` |
| 割り込みの回数 | `cat /proc/interrupts` |
| 眠っているか(待ち行列) | `cat /proc/<番号>/status` の `State` |
| スレッド(pthread、Rust) | `threads`、`rthreads` |
| シグナル | `signals`、`sigwait`、`sleep 30 & kill $!`、`sleep 1 & wait $!` |

## 必要なもの

- Ubuntu、`qemu-system-arm`(`qemu-system-aarch64`)、`qemu-user`(互換テストの `qemu-aarch64`)、`gdb-multiarch`、`dosfstools`(FAT32 のイメージを作る `mkfs.fat`)
- rustup(`rust-toolchain.toml` の nightly とターゲットが自動で入る)
- zig 0.17(ユーザープログラムと QuickJS を aarch64-linux-musl の静的リンクでビルドする)

```bash
sudo apt-get install -y qemu-system-arm qemu-user gdb-multiarch dosfstools
```

QuickJS のソースは `user/third_party/quickjs/`(MIT ライセンス)にある。

## 使い方

```bash
cargo xtask run                    # QEMU virt で起動
cargo xtask run --bsp raspi3       # QEMU raspi3b で起動
cargo xtask run --trace syscall    # システムコールを表示しながら起動
cargo xtask test                   # ホストの単体テスト
cargo xtask test --bsp qemu-virt   # QEMU の中で動くカーネル内テスト
cargo xtask test --snapshot        # UART 出力のスナップショット
cargo xtask docs                   # 自動生成のドキュメントを更新
cargo xtask ci                     # 上のすべて(push の前に必ず通す)
cargo xtask disk-get /a target/a   # QEMU virt のディスクのルートからファイルを取り出す
```

TCP は smoltcp と自作(`kernel/src/net/tcp/`)の 2 つがあり、Pi-OS の中で `echo own > /proc/sys/net/pios/tcp` とすると、そのあとに作る TCP のソケットが自作の TCP を使う(`echo smoltcp > ...` で戻す。ADR-0008)。自作の TCP の接続の一覧は `cat /proc/pios/tcp`、速さは `tcpbulk 7000 262144 253`(253 = 自作、0 = 既定)で見られる。

パケットを見るには、Pi-OS の中で `cp /proc/pios/net/eth0.pcap /eth0.pcap` としてから終わり、`cargo xtask disk-get /eth0.pcap target/eth0.pcap` で取り出して Wireshark か `tcpdump -nr` で開く(`docs/notes/p2-68-pcap.md`)。

実機用のイメージは `cargo xtask build` で `target/hw/kernel8.img` にできる(セミホスティングを外したビルド。終わると「system halted」と出して止まる)。
