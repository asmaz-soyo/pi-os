# M0-03 QEMU の結合テストの仕組み

## 何をしたか

- `arch/aarch64`(`arch-aarch64`)クレートを作った
  - `boot.rs`:起動直後のアセンブリ `_start`
  - `semihosting.rs`:QEMU の終了(`exit`)と文字出力(`SemihostingConsole`)
- `bsp/qemu-virt`(`bsp-qemu-virt`)クレートを作った。QEMU `-M virt` で起動するカーネルイメージ
  - `link.ld`:メモリ配置。`build.rs` でリンカに渡す
  - `ktest.rs`:カーネル内テストの枠組み
- `cargo xtask test --bsp qemu-virt` を足し、`cargo xtask ci` にも組み込んだ

```bash
$ cargo xtask test --bsp qemu-virt
running 2 tests
test bsp_qemu_virt::tests::arithmetic_works_in_kernel ... ok
test bsp_qemu_virt::tests::semihosting_console_accepts_bytes_and_format ... ok
test result: ok
```

## なぜ必要か

- ホストの `cargo test` では、本物の CPU の状態(例外レベル、システムレジスタ、メモリ配置)に触れるコードを検証できない。それを QEMU の中で動かして確かめる層
- CI で自動判定するには、結果を「QEMU の終了コード」で返す必要がある。成功は 0、失敗は 1
- UART ドライバは M1 で作るので、それまでの出力と終了にはセミホスティングを使う

## 中で何が起きているか

### テストが QEMU で動くまで

```
cargo xtask test --bsp qemu-virt
  └─ cargo test -p bsp-qemu-virt --target aarch64-unknown-none
       │  環境変数 CARGO_TARGET_AARCH64_UNKNOWN_NONE_RUNNER = "timeout 180 qemu-system-aarch64 ... -kernel"
       ├─ #[test_case] を集めたテスト用の ELF をビルド(custom_test_frameworks)
       └─ runner の後ろに ELF のパスを付けて実行 → QEMU がカーネルとして起動
            _start → kernel_main → test_main → ktest::runner → semihosting::exit(0 or 1)
```

- cargo は `--target` 向けのテストバイナリを、ホストで直接実行できない。そこで「runner」という仕組みで、実行するコマンドを差し替える。runner はターゲットごとの設定なので、BSP ごとに QEMU の引数を変えられるよう、xtask が環境変数で渡している
- `custom_test_frameworks`(nightly 機能)で、`std` のテストハーネスの代わりに自前の `ktest::runner` を使う。`#[test_case]` を付けた関数がスライスで渡される
- `timeout 180` は、カーネルが固まって QEMU が終わらないときの保険(最初は 60 秒だったが、テストが増えて WSL2 の QEMU では足りなくなった。p2-90)

### `_start` がやること(`boot.rs`)

1. `mpidr_el1` でコア番号を見て、コア 0 以外は `wfe` で眠らせる
2. `sp` にスタックの先頭(リンカスクリプトの `__stack_top`)を入れる
3. `.bss` を 0 で埋める。Rust は初期値なしの `static` が 0 であることを前提にしている
4. `CPACR_EL1.FPEN` を立てて FP/SIMD 命令を使えるようにする。`aarch64-unknown-none` は SIMD レジスタを使うコードを生成することがあり、これを忘れると例外になる
5. `x0`(QEMU が渡す DTB のアドレス)をそのままにして `kernel_main` を呼ぶ

### セミホスティング

- `hlt #0xf000` 命令を実行すると、QEMU(`-semihosting` 付き)が横取りして、`x0` の番号の処理をホスト側で行う
- `SYS_WRITEC`(0x03):1文字をホストの標準出力へ出す
- `SYS_EXIT`(0x18):QEMU を終了する。AArch64 では「理由(`ADP_Stopped_ApplicationExit`)とコード」の2語のブロックを渡し、そのコードが QEMU の終了コードになる
- 実機にはこの仕組みがないので、テストとデバッグ専用

### メモリ配置(`link.ld`)

- `-M virt` の RAM は `0x4000_0000` から。カーネルは `0x4008_0000` に置く(Linux の慣習と同じ)
- `.text.boot`(`_start`)を先頭に置き、`.text`、`.rodata`、`.data`、`.bss`、64KiB のスタックの順に並べる
- MMU はまだ使っていないので、アドレスはすべて物理アドレス

## ワークスペースの扱い

- `arch-aarch64` と `bsp-qemu-virt` はホスト(x86_64)ではビルドできない。xtask のホスト向けコマンドでは `--exclude` で外し、ルートの `Cargo.toml` では `default-members` に入れていない
- テストの枠組み(`ktest`)は今は `bsp-qemu-virt` の中にある。raspi3 の BSP を作るときに共通のクレートへ切り出す

## 確認したこと

- 正常なとき:2件のテストが通り、終了コード 0
- 失敗するテストを一時的に足したとき:`FAILED` と `assert` の左右の値が出て、終了コード 1、`cargo xtask` も失敗で止まる
- テスト用でない通常ビルドも QEMU で起動し、終了コード 0 で止まる

## 確認方法

```bash
cargo xtask test --bsp qemu-virt
# 起動を1命令ずつ追う場合(別の端末で gdb-multiarch をつなぐ)
qemu-system-aarch64 -M virt -cpu cortex-a53 -m 128M -nic none -nographic -semihosting -s -S \
  -kernel target/aarch64-unknown-none/debug/bsp-qemu-virt
```
