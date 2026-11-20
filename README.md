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
```

フェーズ1のゴール(自作 OS 上で `qjs hello.js` が「Hello from JS」を出す)は、QEMU で達成した。

## 必要なもの

- Ubuntu、`qemu-system-arm`(`qemu-system-aarch64`)、`gdb-multiarch`
- rustup(`rust-toolchain.toml` の nightly とターゲットが自動で入る)
- zig 0.17(ユーザープログラムと QuickJS を aarch64-linux-musl の静的リンクでビルドする)

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
```

実機用のイメージは `cargo xtask build` で `target/hw/kernel8.img` にできる(セミホスティングを外したビルド。終わると「system halted」と出して止まる)。
