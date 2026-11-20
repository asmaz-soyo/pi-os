//! initramfs(tar)を作る。
//!
//! 1. `programs/` のユーザープログラムを zig でビルドする(aarch64-linux-musl の静的リンク)→ `/bin/<名前>`
//! 2. `rootfs/` の下のファイルをそのまま入れる(`rootfs/etc/motd` → `/etc/motd`)
//!
//! できた tar は `src/lib.rs` が `include_bytes!` でカーネルに埋め込む。

use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let root = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    println!("cargo:rerun-if-changed=programs");
    println!("cargo:rerun-if-changed=rootfs");
    println!("cargo:rerun-if-env-changed=ZIG");

    let mut files: Vec<(String, Vec<u8>)> = Vec::new();
    for path in sorted(&root.join("programs")) {
        let name = path.file_stem().unwrap().to_str().unwrap().to_string();
        let bin = out.join(&name);
        if build_program(&path, &bin, &out) {
            files.push((format!("bin/{name}"), std::fs::read(&bin).unwrap()));
        }
    }
    collect(&root.join("rootfs"), "", &mut files);
    std::fs::write(out.join("initramfs.tar"), tar(&files)).unwrap();
}

fn sorted(dir: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .collect();
    v.sort();
    v
}

fn collect(dir: &Path, prefix: &str, files: &mut Vec<(String, Vec<u8>)>) {
    for path in sorted(dir) {
        let name = format!("{prefix}{}", path.file_name().unwrap().to_str().unwrap());
        if path.is_dir() {
            collect(&path, &format!("{name}/"), files);
        } else {
            files.push((name, std::fs::read(&path).unwrap()));
        }
    }
}

/// `src`(.S か .c)を `bin` にビルドする。それ以外の拡張子なら false。
fn build_program(src: &Path, bin: &Path, out: &Path) -> bool {
    let zig = std::env::var("ZIG").unwrap_or_else(|_| "zig".to_string());
    let mut cmd = Command::new(&zig);
    cmd.args([
        "cc",
        "-target",
        "aarch64-linux-musl",
        "-static",
        "-s",
        "-O2",
    ]);
    match src.extension().and_then(|e| e.to_str()) {
        // アセンブリだけのプログラム(libc を使わない)
        Some("S") => cmd.arg("-nostdlib"),
        Some("c") => &mut cmd,
        _ => return false,
    };
    cmd.arg("-o").arg(bin).arg(src);
    // zig のキャッシュはビルドディレクトリの中に置く
    cmd.env("ZIG_LOCAL_CACHE_DIR", out.join("zig-cache"));
    cmd.env("ZIG_GLOBAL_CACHE_DIR", out.join("zig-global-cache"));
    let status = cmd
        .status()
        .unwrap_or_else(|e| panic!("failed to run `{zig}` (is zig installed?): {e}"));
    assert!(status.success(), "building {} failed", src.display());
    true
}

/// ustar 形式の tar を作る(ディレクトリの項目は省く。カーネルはパスから推測する)。
fn tar(files: &[(String, Vec<u8>)]) -> Vec<u8> {
    let mut out = Vec::new();
    for (name, data) in files {
        assert!(name.len() < 100, "path too long for ustar: {name}");
        let mut h = [0u8; 512];
        h[..name.len()].copy_from_slice(name.as_bytes());
        let mode = if name.starts_with("bin/") {
            "0000755"
        } else {
            "0000644"
        };
        h[100..107].copy_from_slice(mode.as_bytes());
        h[108..115].copy_from_slice(b"0000000");
        h[116..123].copy_from_slice(b"0000000");
        h[124..135].copy_from_slice(format!("{:011o}", data.len()).as_bytes());
        h[136..147].copy_from_slice(b"00000000000");
        h[156] = b'0';
        h[257..263].copy_from_slice(b"ustar\0");
        h[263..265].copy_from_slice(b"00");
        h[148..156].copy_from_slice(b"        ");
        let sum: u32 = h.iter().map(|&b| b as u32).sum();
        h[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
        out.extend_from_slice(&h);
        out.extend_from_slice(data);
        out.resize(out.len().div_ceil(512) * 512, 0);
    }
    out.extend_from_slice(&[0; 1024]);
    out
}
