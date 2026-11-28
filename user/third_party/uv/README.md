# uv

- `uv-aarch64-unknown-linux-musl.tar.gz`:https://github.com/astral-sh/uv/releases/tag/0.13.10 の公式リリース
  (2026-11-24 公開)。静的リンクの `uv` と `uvx` が入っている。ライセンスは MIT か Apache-2.0
- `.sha256` は同じリリースに付いているもの。取ってきたときに `sha256sum -c` で確かめた
- 計画書の項目7の「OS 上の uv」で使う。Pi-OS のディスク(QEMU virt の ext2)の `/usr/bin` に置く予定。
  uv は tokio のスレッド、futex、シグナル、TLS(rustls)の上の HTTPS を使う(p2-81〜p2-88 で用意した)
