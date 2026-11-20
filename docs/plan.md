# 自作OS 計画書

Nov 20, 2026 · @まさ

## 概要とコンセプト

Pi-OSは、Raspberry Pi 3B向けのRust製学習用カーネル。最初のゴールは、自作OS上で `qjs hello.js` を実行してJSを動かすこと。

- **プロジェクトの軸**:OSを作りながら、OSの仕組みを学ぶ。作る過程と中身を、後から追えるようにする
- **自作ならではの要素**:カーネルの動き(システムコール、VFS、パケット、割り込みの遅延)をトレースし、観察できる
- **構造**:ポート&アダプター。ハードウェア依存部(BSP)は付け替え可能
- **アプリ**:Linux互換のシステムコールにより、muslで静的リンクしたRust、Python、JSのプログラムを無改造で動かす
- **アプリの起動方式**:スタートアップ、周期、割り込み駆動の3種類をサポートする
- **後回し**:Node.js本体と、OS上でのnpmの実行。後から足せる余地は残す

## ターゲットと開発環境

開発はUbuntu上のQEMUで進め、マイルストーンの区切りごとに実機のPi 3Bで確かめる。

| 項目 | 内容 |
| --- | --- |
| 実機 | Raspberry Pi 3B(所有済み、Cortex-A53・4コア・RAM 1GB) |
| BSP | `raspi3`(QEMU `-M raspi3b` と実機で共通)、`qemu-virt`(virtioが使える開発用) |
| 開発PC | Ubuntu(標準のQEMU 8.2でraspi3bは動作する) |
| カーネルの言語 | Rust(`no_std`、`aarch64-unknown-none`) |
| ユーザー空間のビルド | `zig cc -target aarch64-linux-musl`(QuickJSなど)、Rustは `aarch64-unknown-linux-musl` |
| デバッグ | QEMUの `-s -S` と `gdb-multiarch`、UART出力 |
| 実機用の道具 | USB-シリアル変換ケーブル(3.3V、GPIO14/15)、microSD |

実機の `config.txt` には `arm_64bit=1` と `dtoverlay=disable-bt` を書く(PL011をBluetoothから取り戻すため)。Pi 5はQEMUでエミュレートできないため対象外。

## アーキテクチャ

ポート&アダプターで構成し、コアは `hal` のトレイトだけに依存させる。機能の追加は「アダプターを足して登録する」だけで済む形にする。

&#91;embedded content: 層の構成 · ポート&アダプター\]

fs/ と net/ も、VFSやプロトコル登録表に差し込むアダプターとして同じ形で追加する。

```
pi-os/
├─ hal/          トレイト定義のみ(Console, Timer, IrqCtrl, Gpio, BlockDev, NetDev)
├─ arch/aarch64/ 例外、MMU、コンテキストスイッチ(半分コアの層)
├─ drivers/      pl011, bcm2837-irq, bcm2837-gpio, gic, sdhci, virtio-blk, virtio-net
├─ bsp/
│   ├─ raspi3/     どのドライバをどのアドレスで使うかの「配線」だけ
│   └─ qemu-virt/
├─ kernel/
│   ├─ mm/       メモリ管理
│   ├─ task/     プロセス、スレッド、スケジューラ
│   ├─ syscall/  Linux互換ABI(fs.rs, mm.rs, proc.rs, time.rs, net.rs …)
│   ├─ vfs/      File / Inode / FileSystem トレイト
│   └─ trace/    トレースポイント(自作ならではの部分)
├─ fs/           initramfs(tar), tmpfs, devfs, procfs, fat32, ext2
├─ net/          smoltcpのラッパー、AF_UNIX
├─ user/         init、サービスマネージャ、互換テスト用プログラム
├─ docs/
└─ xtask/        cargo xtask build / run / test / ci
```

**差し込み口**

- **システムコール表**:番号ごとにハンドラを登録。未実装は `ENOSYS` を返してログに残す
- **ABIもアダプター**:Linux互換層は駆動する側のアダプターの1つ。独自ABIやWASIを後から並べられる
- **VFSの `File` トレイト**:`read` / `write` / `ioctl` / `poll`。ファイル、パイプ、ソケット、timerfd、GPIOをすべて同じ形で扱う
- **デバイスツリー**:起動時に渡されるDTBを `fdt` クレートで読み、ドライバを探して登録する
- **Cargoのfeature**:`net`・`fat32`・`trace` などをビルド時に選ぶ
- **トレースポイント**:システムコールの出入り、コンテキストスイッチ、割り込み、VFS、リンク層にフックを置き、出力先(UART・リングバッファ・`/proc`)を差し替えられるようにする

**カーネルならではの注意点**:BSPはビルド時に1つに決まるので、ジェネリクスやfeatureで静的に解決する。実行時に決まるドライバや `File` は `dyn Trait` を使う。起動処理は初期化の順番(UART → ヒープ → …)を段階に分けて明示的に書く。

## サブシステム設計

どのサブシステムも、「共通の入口+登録制の中身」という同じ形で作る。

### システムコール(Linux互換)

- Linux AArch64のシステムコールを必要な分だけ実装し、muslで静的リンクしたバイナリを無改造で動かす
- 必要なシステムコールは、Ubuntuで `qemu-aarch64 -strace` を使って事前に調べる
- qjsに必要なのは、おそらく `read` / `write` / `writev` / `openat` / `close` / `fstat` / `lseek` / `brk` / `mmap` / `munmap` / `clock_gettime` / `exit_group` が中心。`ioctl` / `rt_sigaction` / `set_tid_address` などは成功を返すだけのスタブでよい
- ユーザーから渡されたポインタは、必ず `copy_from_user` / `copy_to_user` を通して検証する

### ファイルシステム

- 層構成:fdテーブル → OpenFile → VFS(パス解決、マウント表、dentryキャッシュ)→ FSアダプター → ブロック層(バッファキャッシュ、MBR)→ ドライバ
- 実装順:initramfs(tar、読み取り専用)+devfs → tmpfs → FAT32の読み込み(`/boot`)→ ext2の読み書き(`/`)→ procfs
- SDカードは「p1:FAT32(`/boot`)、p2:ext2(`/`)」。ルートをext2にするのは、uvやnpmが使うシンボリックリンクとパーミッションがFAT32にないため
- VFSとext2は学習のため自作。FAT32は既存クレート(`fatfs` など)をアダプターで包んでもよい

### ネットワーク

- 差し込み口は3つ:`ProtocolFamily` の登録表(AF\_UNIX、AF\_INET…)、EtherTypeの振り分け表、`NetDev` トレイト(`link_type` でEthernet以外にも対応)
- 実装順:AF\_UNIX+ループバック → virtio-net+smoltcp(qemu-virt)→ DHCP → 自作TCP(smoltcp版と差し替えて比較)→ IPv6 → Pi 3Bの実機LAN → Bluetoothなど
- Pi 3BのEthernetはUSB接続(LAN9514)なので、USBホストドライバ(DWC OTG)が必要。それまではネットワークをqemu-virtで進める
- DNSはカーネルに入れない(muslがUDPで問い合わせる)
- 送受信したパケットをpcapで保存し、UbuntuのWiresharkで確認できるようにする

### アプリの実行モデル

- 3種類のアプリを「起動のきっかけ(トリガー)」の違いとして統一する。カーネルはきっかけをfdとして提供し、管理はユーザー空間のサービスマネージャ(Rust製)が行う
- スタートアップ:initが定義ファイルを読んで起動する
- 周期:`timerfd` + `epoll`
- 割り込み駆動:`/dev/gpio/N` のようなfdを待つ。常駐型と、割り込みで起動するオンデマンド型の2方式
- シバン(`#!/bin/qjs`)を `execve` で解釈し、スクリプトを直接実行できるようにする
- パッケージは当面、Ubuntu側で用意する(npm+esbuildで1ファイルにまとめる、`uv pip install --target` でmusllinuxのwheelを入れる)。OS上でのuvは中盤以降、npmはNode.jsの後

```toml
[service.sensor]
exec = "/bin/python /apps/sensor.py"
trigger = { every = "5s" }

[service.doorbell]
exec = "/bin/qjs /apps/bell.js"
trigger = { gpio = 17, edge = "falling" }
restart = "on-failure"
```

## ロードマップとマイルストーン

フェーズ1のゴールは「`qjs hello.js` がUARTに「Hello from JS」を出すこと」。それまでは他の機能に手を出さない。

### フェーズ1:JSが動くまで

1. **M0 開発基盤**:ワークスペース、xtask、ホストとQEMUのテストの仕組み、CI、docsの雛形
2. **M1 起動とUART**:raspi3とqemu-virtの両BSPで「Hello」。パニック時にレジスタとスタックトレースを出す
3. **M2 例外と割り込み**:例外ベクタ、割り込みコントローラ、ARM Generic Timer。実機向けにUART経由のブートローダも作る
4. **M3 メモリ管理**:MMU、物理ページ割り当て、カーネルヒープ(`alloc`)、higher half配置
5. **M4 ユーザープロセス**:EL0、SVCの入口、静的ELFローダ、ユーザー空間への戻り口を1か所にまとめる
6. **M5 最小のLinux互換+initramfs**:tarを埋め込み、muslの `hello.c` を動かす。未実装システムコールのログとトレースもここで
7. **M6 QuickJS**:`qjs hello.js` が動けばフェーズ1完了。実機のPi 3Bでも確認する

マルチタスクはフェーズ1では不要(qjsは1プロセスで動く)。

### フェーズ2以降(順は目安)

1. UART入力、TTY、JSのREPL
2. スケジューラ、`fork`(最初はvfork相当)、シェル(BusyBox)、シバン対応
3. tmpfs、`timerfd`、`epoll`、initとサービスマネージャ
4. GPIO割り込みのfd、リアルタイム優先度、ジッタ計測
5. ブロック層、FAT32、ext2、procfs
6. MicroPython → CPython、動的リンク
7. ネットワーク(qemu-virtで先行)、OS上でのuv
8. スレッド、futex、txiki.js(QuickJS+libuv)
9. Node.js、OS上でのnpm(最終目標)

## 設計上の決定事項と後回し項目

後からの変更が大変な4項目だけをM1〜M4で固め、残りは「入れ物だけ今」か「完全に後で」にする。

| 区分 | 項目 | 今やること |
| --- | --- | --- |
| 最初から作る | パニック時の情報出力 | レジスタとスタックトレースをUARTに出す(M1) |
| 最初から作る | ロックの方針 | マルチコア前提。`spin::Mutex` とCPUごとのデータ |
| 最初から作る | ユーザー空間とのやり取り | `copy_from_user` / `copy_to_user` を必ず通す |
| 最初から作る | アドレス空間とエラー型 | higher half配置、`Result<T, Errno>` で統一 |
| 入れ物だけ今 | シグナル | タスクに保留中シグナルとハンドラ表のフィールド |
| 入れ物だけ今 | スレッドとfutex | アドレス空間とfdテーブルを `Arc` で共有可能に。futexは物理アドレスで引く |
| 入れ物だけ今 | `fork` とコピーオンライト | PTEに権限ビット、物理ページに参照カウント |
| 入れ物だけ今 | 権限 | タスクにuid/gid、VFSに「常に許可」を返すチェック関数 |
| 入れ物だけ今 | JIT向けのメモリ権限 | `mprotect` で読み・書き・実行を後から変えられる構造 |
| 後で足す | TTY、RTCとNTP、動的リンク、メモリ最適化 | なし(アダプターやユーザー空間で追加できる) |

ユーザー空間に戻る直前の処理は1か所にまとめる。シグナル配送、プリエンプション、トレースはすべてここに後から差し込む。Pi 3BはRTCがないため、NTPまではビルド時刻を初期値にする。

**未決事項**

- [x] OS名とリポジトリ名 → Pi-OS(pi-os)に決定
- [x] 公開リポジトリにするか(CIの無料枠に影響)→ 公開に決定
- [x] 権限モデルを単一ユーザーにするか → 複数ユーザー(ユーザーを作成できる)に決定(ADR-0005)

## 作業の進め方

各ステップは「テストが通る・リファクタリング済み・ドキュメント更新済み」の3つがそろって完了とする。

### TDD(4層のテスト)

| 層 | 対象 | 実行方法 |
| --- | --- | --- |
| ホストの単体テスト | コア(VFS、スケジューラ、メモリ管理)をモックのポートで検証。TDDの主戦場 | Ubuntu上の `cargo test` |
| QEMUの結合テスト | カーネル内で動くテスト。QEMUの終了コードで成否を返す | `cargo xtask test --bsp qemu-virt` |
| システムコールの互換テスト | muslの小さなテストプログラムをLinux(`qemu-aarch64`)と自作OSの両方で動かし、結果を比べる | `cargo xtask test --compat` |
| UART出力のスナップショット | 起動処理など単体テストが難しい部分。期待するシリアル出力と比較 | `cargo xtask test --snapshot` |

実機での確認はマイルストーンの区切りごとに手動で行う。ただし、実機なしで(QEMU で)できることを先に終えてから、実機に進む(2026-11-21 に決定)。

### ドキュメント

```
docs/
├─ concept.md        コンセプトと目標
├─ architecture.md   層の構成、ポート一覧(Mermaid)
├─ adr/              設計判断の記録
├─ milestones/       マイルストーンごとの目的、完了条件、学んだこと
├─ notes/            ステップごとの学習ノート
└─ generated/        システムコール対応表、BSP対応表、トレースポイント一覧
```

- 対応表など事実の一覧はコードから自動生成し、CIで差分がないことを確かめる。手書きは「なぜそうしたか」に絞る
- 設計判断はADRに残す(例:ルートはext2、Node.jsは後回し、Linux互換ABI)
- mdBookでまとめ、「自作OSの教科書」として読めるようにする

### 実装の過程と中身を追えるようにする

- **1ステップ=1コミット**:テスト、実装、ドキュメントを1コミットに収める。マイルストーンごとにgitタグ(`m1-uart` など)を打つ
- **学習ノート**:「何をしたか」「なぜ必要か」「カーネル内で何が起きているか」を短く書く
- **観察環境**:GDBスクリプト(起動処理を1命令ずつ追うなど)と `cargo xtask run --trace syscall` を初期から用意する

### 1ステップの流れ(Claude Codeと進める場合)

1. 次のステップの目的と背景を説明する
2. テストを先に書く(ユーザーが書いてもよい)
3. テストが失敗することを確認する(Red)
4. 最小限の実装で通し、何をしたか解説する(Green)
5. 整理する(Refactor)
6. ドキュメントと学習ノートを更新し、コミットする

コードを一度に大量に書かず、ユーザーが各ステップを理解してから次に進む。

## CI

CIは「ローカルでも同じコマンドが動く」ことを前提にし、GitHub Actionsはそれを呼ぶだけにする。公開リポジトリにすれば、制限の問題はほぼ解消する。

### Actionsの制限(2026年11月時点)

- 公開リポジトリでは、標準のGitHubホストランナーは無料かつ無制限([GitHub Docs](https://docs.github.com/en/billing/managing-billing-for-github-actions/about-billing-for-github-actions))
- 非公開リポジトリはプランごとの無料枠まで。超えた分はLinux標準ランナーで1分$0.006
- セルフホストランナーへの1分$0.002の課金は2026年3月開始予定として発表されたが、延期されている([Socket](https://socket.dev/blog/github-actions-pricing-whiplash))

### 構成

1. **`cargo xtask ci` が正本**:fmt、clippy、ホストのテスト、QEMUのテスト、生成ドキュメントの差分チェックをまとめて実行。push前にgitフックで自動実行する
2. **Actionsは軽く**:PRとmainへのpushだけで走らせる。キャッシュ(Cargo、ツールチェーン)、パスフィルタ(docsだけの変更はテストを省く)、`concurrency` で古い実行を取り消す
3. **重いテストは夜間に**:互換テストの全件実行やraspi3とqemu-virtの両BSPマトリクスは、定期実行に回す
4. **非公開にする場合**:Ubuntuの開発PCをセルフホストランナーにする。公開リポジトリでは、フォークからのPRが自分のPCで任意のコードを動かせてしまうため使わない
5. **将来:実機テスト**:Pi 3Bをシリアルでつないだランナーにし、UARTブートローダでカーネルを送って出力を検証する(ハードウェア・イン・ザ・ループ)

## Claude Codeへの引き継ぎ

最初のタスクはM0(開発基盤)。カーネルのコードより先に、テストとドキュメントの仕組みを作る。

- [x] リポジトリを作成し、この計画書を `docs/plan.md` として入れる
- [x] `CLAUDE.md` に作業ルールを書く(TDDの手順、1ステップ=1コミット、完了の定義、各ステップで解説してから進むこと)
- [x] Cargoワークスペースと `rust-toolchain.toml`(nightly、`aarch64-unknown-none`)
- [x] `hal` クレートに `Console` トレイトと、テスト用のモックを作る(最初のTDD)
- [x] `xtask` に `build` / `run` / `test` / `ci` を用意する(`run` の中身は M1)
- [x] QEMUの結合テストを動かす仕組み(終了コードで成否を返す)
- [x] GitHub Actionsのワークフロー(`cargo xtask ci` を呼ぶだけ)
- [x] `docs/` の雛形、ADRのテンプレート、最初のADR(「Linux互換ABIを採用」「ポート&アダプター構造」)
- [x] M0完了後、M1(起動とUART)の最初のテストとして「UART出力のスナップショットで「Hello」を期待する」を書く

**参考資料**:RustでのAArch64ベアメタルは rust-raspberrypi-OS-tutorials、Pi 3Bのペリフェラルは「BCM2837 ARM Peripherals」、QEMUのボード仕様は [QEMUのRaspberry Piボードの説明](https://qemu.readthedocs.io/en/master/system/arm/raspi.html)。

## Sources

- [Raspberry Pi boards — QEMU documentation](https://qemu.readthedocs.io/en/master/system/arm/raspi.html)
- [About billing for GitHub Actions — GitHub Docs](https://docs.github.com/en/billing/managing-billing-for-github-actions/about-billing-for-github-actions)
- [GitHub Actions Pricing Whiplash — Socket](https://socket.dev/blog/github-actions-pricing-whiplash)
