//! initramfs(tar)を作る。
//!
//! 1. `programs/` のユーザープログラムを zig でビルドする(aarch64-linux-musl の静的リンク)→ `/bin/<名前>`
//! 2. `third_party/quickjs/` から QuickJS の `qjs` をビルドする → `/bin/qjs`
//! 3. `rootfs/` の下のファイルをそのまま入れる(`rootfs/etc/motd` → `/etc/motd`)
//!
//! できた tar は `src/lib.rs` が `include_bytes!` でカーネルに埋め込む。

use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let root = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    println!("cargo:rerun-if-changed=programs");
    println!("cargo:rerun-if-changed=rootfs");
    println!("cargo:rerun-if-changed=third_party");
    println!("cargo:rerun-if-changed=rust");
    println!("cargo:rerun-if-env-changed=ZIG");

    let mut files: Vec<(String, Vec<u8>)> = Vec::new();
    // 動的リンカ(musl の libc.so)
    let ld = build_musl_ldso(&root.join("third_party/musl"), &out);
    files.push((
        "lib/ld-musl-aarch64.so.1".to_string(),
        std::fs::read(ld).unwrap(),
    ));
    for path in sorted(&root.join("programs")) {
        // foo.dyn.c は動的リンクの foo
        let name = path.file_stem().unwrap().to_str().unwrap();
        let name = name.strip_suffix(".dyn").unwrap_or(name).to_string();
        let bin = out.join(&name);
        if build_program(&path, &bin, &out) {
            files.push((format!("bin/{name}"), std::fs::read(&bin).unwrap()));
        }
    }
    let qjs = build_quickjs(&root.join("third_party/quickjs"), &out);
    files.push(("bin/qjs".to_string(), std::fs::read(qjs).unwrap()));
    // Rust のプログラム(user/rust/<名前>)
    let svcman = build_rust_program(&root, "svcman", &out);
    files.push(("bin/svcman".to_string(), std::fs::read(svcman).unwrap()));
    let micropython = build_micropython(&root.join("third_party/micropython"), &out);
    files.push((
        "bin/micropython".to_string(),
        std::fs::read(micropython).unwrap(),
    ));
    let busybox = build_busybox(&root.join("third_party/busybox"), &out);
    files.push(("bin/busybox".to_string(), std::fs::read(busybox).unwrap()));
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

fn zig(out: &Path) -> Command {
    let zig = std::env::var("ZIG").unwrap_or_else(|_| "zig".to_string());
    let mut cmd = Command::new(zig);
    // zig のキャッシュはビルドディレクトリの中に置く
    cmd.env("ZIG_LOCAL_CACHE_DIR", out.join("zig-cache"));
    cmd.env("ZIG_GLOBAL_CACHE_DIR", out.join("zig-global-cache"));
    cmd
}

fn run(mut cmd: Command, what: &str) {
    let status = cmd
        .status()
        .unwrap_or_else(|e| panic!("{what}: failed to start (is zig installed?): {e}"));
    assert!(status.success(), "{what} failed");
}

/// QuickJS の `qjs` をビルドする(Makefile の QJS_OBJS と同じ組み合わせ)。
fn build_quickjs(src: &Path, out: &Path) -> PathBuf {
    let version = std::fs::read_to_string(src.join("VERSION")).unwrap();
    let defines = [
        "-D_GNU_SOURCE".to_string(),
        format!("-DCONFIG_VERSION=\"{}\"", version.trim()),
    ];
    let flags = ["-O2", "-fwrapv", "-funsigned-char", "-w"];
    let lib = [
        "quickjs.c",
        "dtoa.c",
        "libregexp.c",
        "libunicode.c",
        "cutils.c",
        "quickjs-libc.c",
    ];

    // 1. ホスト向けの qjsc で repl.js をバイトコード(repl.c)にする
    let qjsc = out.join("qjsc-host");
    let mut cmd = zig(out);
    cmd.arg("cc")
        .args(flags)
        .args(&defines)
        .arg("-o")
        .arg(&qjsc);
    cmd.arg(src.join("qjsc.c"));
    cmd.args(lib.iter().map(|f| src.join(f)));
    cmd.arg("-lm");
    run(cmd, "building qjsc for the host");
    let repl = out.join("repl.c");
    let mut cmd = Command::new(&qjsc);
    cmd.current_dir(src)
        .args(["-s", "-c", "-o"])
        .arg(&repl)
        .args(["-m", "repl.js"]);
    run(cmd, "generating repl.c with qjsc");

    // 2. aarch64-linux-musl の静的リンクで qjs を作る
    let qjs = out.join("qjs");
    let mut cmd = zig(out);
    cmd.args(["cc", "-target", "aarch64-linux-musl", "-static", "-s"]);
    cmd.args(flags)
        .args(&defines)
        .arg("-I")
        .arg(src)
        .arg("-o")
        .arg(&qjs);
    cmd.arg(src.join("qjs.c")).arg(&repl);
    cmd.args(lib.iter().map(|f| src.join(f)));
    cmd.arg("-lm");
    run(cmd, "building qjs for aarch64-linux-musl");
    qjs
}

/// `rust/<name>` の Rust のプログラムを aarch64-unknown-linux-musl(静的リンク)でビルドする。
/// リンカは Rust に付いてくる rust-lld を使う(musl の C ツールチェーンは要らない)。
fn build_rust_program(root: &Path, name: &str, out: &Path) -> PathBuf {
    const TARGET: &str = "aarch64-unknown-linux-musl";
    let target_dir = out.join("rust");
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    let mut cmd = Command::new(cargo);
    cmd.current_dir(root.parent().unwrap())
        .args(["build", "--release", "-q", "-p", name, "--target", TARGET])
        .arg("--target-dir")
        .arg(&target_dir)
        .env("CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER", "rust-lld")
        // 外側のビルド(aarch64-unknown-none)の設定を持ち込まない
        .env_remove("CARGO_ENCODED_RUSTFLAGS")
        .env_remove("RUSTFLAGS")
        .env_remove("CARGO_BUILD_TARGET");
    let status = cmd
        .status()
        .unwrap_or_else(|e| panic!("building {name}: {e}"));
    assert!(
        status.success(),
        "building {name} for {TARGET} failed (rustup target add {TARGET})"
    );
    target_dir.join(TARGET).join("release").join(name)
}

/// musl の共有ライブラリ libc.so(動的リンカを兼ねる)をビルドする。
fn build_musl_ldso(dir: &Path, out: &Path) -> PathBuf {
    const VERSION: &str = "musl-1.2.6";
    let src = out.join(VERSION);
    let lib = src.join("lib/libc.so");
    if lib.exists() {
        return lib;
    }
    if !src.join("configure").exists() {
        let mut cmd = Command::new("tar");
        cmd.arg("-xzf")
            .arg(dir.join(format!("{VERSION}.tar.gz")))
            .arg("-C")
            .arg(out);
        run(cmd, "extracting musl");
    }
    let zig = std::env::var("ZIG").unwrap_or_else(|_| "zig".to_string());
    let mut cmd = Command::new("sh");
    cmd.current_dir(&src)
        .env("ZIG_LOCAL_CACHE_DIR", out.join("zig-cache"))
        .env("ZIG_GLOBAL_CACHE_DIR", out.join("zig-global-cache"))
        .arg("./configure")
        .args([
            "--target=aarch64-linux-musl",
            "--prefix=/",
            "--syslibdir=/lib",
        ])
        .arg(format!("CC={zig} cc -target aarch64-linux-musl"))
        .arg(format!("AR={zig} ar"))
        .arg(format!("RANLIB={zig} ranlib"))
        // デバッグ情報と(動的リンクに要らない)シンボル表を付けない
        .arg("CFLAGS=-g0")
        .arg("LDFLAGS=-s")
        .stdout(std::process::Stdio::null());
    run(cmd, "configuring musl");
    let jobs = std::env::var("NUM_JOBS").unwrap_or_else(|_| "4".to_string());
    let mut cmd = Command::new("make");
    cmd.current_dir(&src)
        .env_remove("DEBUG")
        .env("ZIG_LOCAL_CACHE_DIR", out.join("zig-cache"))
        .env("ZIG_GLOBAL_CACHE_DIR", out.join("zig-global-cache"))
        .arg(format!("-j{jobs}"))
        .arg("lib/libc.so")
        .stdout(std::process::Stdio::null());
    run(cmd, "building musl libc.so");
    lib
}

/// MicroPython の unix ポートをビルドする(ffi、btree、ssl、frozen モジュールはなし:サブモジュールが要るため)。
/// 1. ホストの cc で mpy-cross(.py をバイトコードにする道具)を作る
/// 2. zig cc(`zcc.sh` / `zcpp.sh`)で aarch64-linux-musl の静的リンクの `micropython` を作る
fn build_micropython(dir: &Path, out: &Path) -> PathBuf {
    const VERSION: &str = "micropython-1.29.0";
    let src = out.join(VERSION);
    let bin = src.join("ports/unix/build-standard/micropython");
    if bin.exists() {
        return bin;
    }
    if !src.join("py").exists() {
        let mut cmd = Command::new("tar");
        cmd.arg("-xzf")
            .arg(dir.join(format!("{VERSION}.tar.gz")))
            .arg("-C")
            .arg(out);
        run(cmd, "extracting MicroPython");
    }
    let jobs = std::env::var("NUM_JOBS").unwrap_or_else(|_| "4".to_string());
    // cargo がビルドスクリプトに渡す DEBUG=true を、MicroPython の Makefile は
    // 「デバッグビルド(-O0 -g)」と読むので外す
    let mut cmd = Command::new("make");
    cmd.current_dir(src.join("mpy-cross"))
        .env_remove("DEBUG")
        .arg(format!("-j{jobs}"))
        .stdout(std::process::Stdio::null());
    run(cmd, "building mpy-cross");
    let mut cmd = Command::new("make");
    cmd.current_dir(src.join("ports/unix"))
        .env_remove("DEBUG")
        .env("ZIG_LOCAL_CACHE_DIR", out.join("zig-cache"))
        .env("ZIG_GLOBAL_CACHE_DIR", out.join("zig-global-cache"))
        .arg(format!("-j{jobs}"))
        .arg("VARIANT=standard")
        .arg(format!("CC={}", dir.join("zcc.sh").display()))
        .arg(format!("LD={}", dir.join("zcc.sh").display()))
        .arg(format!("CPP={}", dir.join("zcpp.sh").display()))
        .args(["STRIP=true", "SIZE=true", "FROZEN_MANIFEST="])
        .args(["MICROPY_PY_FFI=0", "MICROPY_PY_BTREE=0", "MICROPY_PY_SSL=0"])
        .arg("MICROPY_STANDALONE=0")
        // zig のリンカは -Map を知らない
        .arg("LDFLAGS_ARCH=-Wl,--gc-sections")
        .arg("LDFLAGS_EXTRA=-static -s")
        .stdout(std::process::Stdio::null());
    run(cmd, "building MicroPython");
    bin
}

/// BusyBox を、tarball を展開して `pios.config`(機能を絞った設定)でビルドする。
fn build_busybox(dir: &Path, out: &Path) -> PathBuf {
    const VERSION: &str = "busybox-1.38.0";
    let src = out.join(VERSION);
    let bin = src.join("busybox_unstripped");
    if !src.join("Makefile").exists() {
        let mut cmd = Command::new("tar");
        cmd.arg("-xjf")
            .arg(dir.join(format!("{VERSION}.tar.bz2")))
            .arg("-C")
            .arg(out);
        run(cmd, "extracting BusyBox");
    }
    let config = std::fs::read(dir.join("pios.config")).unwrap();
    if std::fs::read(src.join(".config")).ok().as_deref() != Some(&config[..]) {
        std::fs::write(src.join(".config"), &config).unwrap();
        let _ = std::fs::remove_file(&bin);
    }
    if !bin.exists() {
        let zig = std::env::var("ZIG").unwrap_or_else(|_| "zig".to_string());
        let jobs = std::env::var("NUM_JOBS").unwrap_or_else(|_| "4".to_string());
        let mut cmd = Command::new("make");
        cmd.current_dir(&src)
            .env("ZIG_LOCAL_CACHE_DIR", out.join("zig-cache"))
            .env("ZIG_GLOBAL_CACHE_DIR", out.join("zig-global-cache"))
            .arg(format!("-j{jobs}"))
            .arg(format!("CC={zig} cc -target aarch64-linux-musl -s"))
            .arg("HOSTCC=cc")
            .arg(format!("AR={zig} ar"))
            // strip は CC の -s で済ませる(make の strip の段は busybox_unstripped を作った後)
            .arg("STRIP=true")
            .arg("busybox_unstripped")
            .stdout(std::process::Stdio::null());
        run(cmd, "building BusyBox");
    }
    bin
}

/// `src`(.S か .c)を `bin` にビルドする。それ以外の拡張子なら false。
fn build_program(src: &Path, bin: &Path, out: &Path) -> bool {
    let mut cmd = zig(out);
    cmd.args(["cc", "-target", "aarch64-linux-musl", "-s", "-O2"]);
    if src.to_str().is_some_and(|p| p.ends_with(".dyn.c")) {
        // 動的リンク:実行時に /lib/ld-musl-aarch64.so.1 が libc を結びつける
        cmd.arg("-dynamic");
    } else {
        cmd.arg("-static");
    }
    match src.extension().and_then(|e| e.to_str()) {
        // アセンブリだけのプログラム(libc を使わない)
        Some("S") => cmd.arg("-nostdlib"),
        Some("c") => &mut cmd,
        _ => return false,
    };
    cmd.arg("-o").arg(bin).arg(src);
    run(cmd, &format!("building {}", src.display()));
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
