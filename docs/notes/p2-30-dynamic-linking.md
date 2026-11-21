# フェーズ2-30 動的リンク

## 何をしたか

- musl 1.2.6 の公式 tarball(`user/third_party/musl/`)から、共有ライブラリ `libc.so` を zig cc でビルドし、`/lib/ld-musl-aarch64.so.1` に置いた(musl では libc.so が動的リンカも兼ねる)
- ELF の読み込み(`kernel::exec`):
  - `PT_INTERP`(「このプログラムはこのインタプリタで動かせ」)を読む:`exec::interpreter(image)`
  - 位置独立(ET_DYN)のものは、決めた場所にずらして置く(動的リンカは `INTERP_BASE` = 0x3f00_0000_0000、ET_DYN のプログラムは `PIE_BASE`)
  - `load_with_interp`:本体と動的リンカの両方を読み込み、動的リンカの入口から始める。補助ベクタに `AT_BASE`(動的リンカの場所)を足す。`AT_PHDR` と `AT_ENTRY` は本体のもの
  - 動的リンクなのにインタプリタがなければ `ENOEXEC`
- `execve` と `spawn_program` は、PT_INTERP のパスを VFS で探して渡す
- `user/programs/*.dyn.c` は `-dynamic` でビルドする。`/bin/dynhello` が QEMU の結合テストで動く

```
Hello from a dynamically linked program (interpreter at 0x3f0000000000)
```

## 中で何が起きているか

```
execve("/bin/dynhello")
  → カーネル:ELF を読む → PT_INTERP = "/lib/ld-musl-aarch64.so.1"
  → 本体のセグメントを 0x100_0000 に、動的リンカを 0x3f00_0000_0000 に置く
  → 補助ベクタ:AT_BASE = 0x3f00_0000_0000、AT_PHDR = 本体のプログラムヘッダ、AT_ENTRY = 本体の入口
  → 動的リンカの入口からユーザー空間へ
動的リンカ(ld-musl):
  → 自分自身の再配置(AT_BASE を足す)
  → 本体の PT_DYNAMIC を見る → NEEDED は libc.so(=自分)なので、ほかに読むものはない
  → 本体の printf などの参照を、自分の中の関数に結びつける
  → AT_ENTRY(本体の _start)に飛ぶ
```

## まだのこと

- libc 以外の共有ライブラリ(`dlopen`、`mmap` でファイルをそのまま置く)は試していない
- musl の署名(`.asc`)の検証
