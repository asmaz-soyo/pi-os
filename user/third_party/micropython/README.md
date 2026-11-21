# MicroPython

- `micropython-1.29.0.tar.gz`:GitHub のタグ v1.29.0 のソース(サブモジュールなし)。
  `https://github.com/micropython/micropython/archive/refs/tags/v1.29.0.tar.gz`
- `.sha256`:取ってきたときの値(GitHub はこの形のチェックサムを出していないので、自分で記録したもの)
- `zcc.sh` / `zcpp.sh`:Makefile の `CC` / `CPP` に渡す zig cc のラッパー
- `user/build.rs` が unix ポートを `VARIANT=standard` でビルドし、`/bin/micropython` に置く
  (ffi、btree、ssl、frozen モジュールはなし。どれもサブモジュールが要るため)
