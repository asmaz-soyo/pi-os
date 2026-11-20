//! `programs/` のユーザープログラムを zig でビルドする(aarch64-linux-musl の静的リンク)。
//!
//! できたバイナリは `OUT_DIR` に置き、`src/lib.rs` が `include_bytes!` でカーネルに埋め込む。

use std::path::Path;
use std::process::Command;

fn main() {
    let out = std::env::var("OUT_DIR").unwrap();
    let dir = Path::new(&std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("programs");
    let zig = std::env::var("ZIG").unwrap_or_else(|_| "zig".to_string());
    println!("cargo:rerun-if-changed=programs");
    println!("cargo:rerun-if-env-changed=ZIG");

    let mut entries: Vec<_> = std::fs::read_dir(&dir).unwrap().flatten().collect();
    entries.sort_by_key(|e| e.path());
    for entry in entries {
        let path = entry.path();
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        let name = path.file_stem().unwrap().to_str().unwrap().to_string();
        let mut cmd = Command::new(&zig);
        cmd.args([
            "cc",
            "-target",
            "aarch64-linux-musl",
            "-static",
            "-s",
            "-O2",
        ]);
        match ext {
            // アセンブリだけのプログラム(libc を使わない)
            "S" => cmd.arg("-nostdlib"),
            "c" => &mut cmd,
            _ => continue,
        };
        cmd.arg("-o").arg(Path::new(&out).join(&name)).arg(&path);
        // zig のキャッシュはビルドディレクトリの中に置く
        cmd.env("ZIG_LOCAL_CACHE_DIR", Path::new(&out).join("zig-cache"));
        cmd.env(
            "ZIG_GLOBAL_CACHE_DIR",
            Path::new(&out).join("zig-global-cache"),
        );
        let status = cmd
            .status()
            .unwrap_or_else(|e| panic!("failed to run `{zig}` (is zig installed?): {e}"));
        assert!(status.success(), "building {} failed", path.display());
    }
}
