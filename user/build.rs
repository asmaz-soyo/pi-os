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
    // CPython は大きいので initramfs には入れない(ディスクのルートに xtask が入れる)
    build_cpython(&root.join("third_party/cpython"), &out);
    let busybox = build_busybox(&root.join("third_party/busybox"), &out);
    files.push(("bin/busybox".to_string(), std::fs::read(busybox).unwrap()));
    // アプレットは /bin/<名前> → busybox のシンボリックリンクで呼べるようにする
    // (自作のプログラムと同じ名前のもの:sh、ls、cat などは自作のほうを残す)
    let mut links: Vec<(String, String)> = Vec::new();
    for name in busybox_applets(&out) {
        let path = format!("bin/{name}");
        if !files.iter().any(|(p, _)| *p == path) && !links.iter().any(|(p, _)| *p == path) {
            links.push((path, "busybox".to_string()));
        }
    }
    collect(&root.join("rootfs"), "", &mut files, &mut links);
    std::fs::write(out.join("initramfs.tar"), tar(&files, &links)).unwrap();
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

fn collect(
    dir: &Path,
    prefix: &str,
    files: &mut Vec<(String, Vec<u8>)>,
    links: &mut Vec<(String, String)>,
) {
    for path in sorted(dir) {
        let name = format!("{prefix}{}", path.file_name().unwrap().to_str().unwrap());
        if path.is_symlink() {
            // リンクはたどらずにリンクのまま入れる(/etc/resolv.conf → /proc/pios/resolv.conf)
            let target = std::fs::read_link(&path).unwrap();
            links.push((name, target.to_str().unwrap().to_string()));
        } else if path.is_dir() {
            collect(&path, &format!("{name}/"), files, links);
        } else {
            files.push((name, std::fs::read(&path).unwrap()));
        }
    }
}

/// 使う zig。snap の zig は、入口(/snap/bin/zig)から呼ぶと snap-confine を通る。cargo(これも
/// snap)の下で CPython の configure から呼ぶと、そこで何も言わずに失敗したので、snap の中の実体を直接使う
fn zig_program() -> String {
    if let Ok(z) = std::env::var("ZIG") {
        return z;
    }
    let real = "/snap/zig/current/zig";
    if Path::new(real).exists() {
        real.to_string()
    } else {
        "zig".to_string()
    }
}

fn zig(out: &Path) -> Command {
    let zig = zig_program();
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

/// CPython をビルドし、ディスクに入れる木 `out/cpython-root/` を作る(README を参照)。
fn build_cpython(dir: &Path, out: &Path) -> PathBuf {
    const VERSION: &str = "Python-3.15.2";
    const LIB: &str = "python3.15";
    let root = out.join("cpython-root");
    if root.join("usr/bin/python3").exists() {
        return root;
    }
    let src = out.join(VERSION);
    if !src.join("configure").exists() {
        let mut cmd = Command::new("tar");
        cmd.arg("-xJf")
            .arg(dir.join(format!("{VERSION}.tar.xz")))
            .arg("-C")
            .arg(out);
        run(cmd, "extracting CPython");
    }
    let jobs = std::env::var("NUM_JOBS").unwrap_or_else(|_| "4".to_string());
    // configure と make は、cargo がビルドスクリプトに渡す環境(DEBUG=true、jobserver の
    // CARGO_MAKEFLAGS など)を持ち込まないように、空の環境に PATH と HOME だけ渡して動かす
    let clean = |cmd: &mut Command| {
        cmd.env_clear();
        for v in ["PATH", "HOME", "ZIG"] {
            if let Some(val) = std::env::var_os(v) {
                cmd.env(v, val);
            }
        }
    };

    // 1. ホスト用の Python(同じ版)
    let host = out.join("cpython-host");
    let host_python = host.join("install/bin/python3.15");
    if !host_python.exists() {
        std::fs::create_dir_all(&host).unwrap();
        let mut cmd = Command::new(src.join("configure"));
        clean(&mut cmd);
        cmd.current_dir(&host)
            .arg(format!("--prefix={}", host.join("install").display()))
            .stdout(std::process::Stdio::null());
        run(cmd, "configuring CPython for the host");
        for target in ["all", "install"] {
            let mut cmd = Command::new("make");
            clean(&mut cmd);
            cmd.current_dir(&host)
                .arg(format!("-j{jobs}"))
                .arg(target)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null());
            run(cmd, "building CPython for the host");
        }
    }

    // 2. aarch64-linux-musl の静的リンクの python
    let cross = out.join("cpython-cross");
    if !cross.join("python").exists() {
        std::fs::create_dir_all(&cross).unwrap();
        let zig = zig_program();
        let mut cmd = Command::new(src.join("configure"));
        clean(&mut cmd);
        cmd.current_dir(&cross)
            .env("ZIG_LOCAL_CACHE_DIR", out.join("zig-cache"))
            .env("ZIG_GLOBAL_CACHE_DIR", out.join("zig-global-cache"))
            .env("CONFIG_SITE", dir.join("config.site"))
            // ホストのライブラリ(zstd など)を見つけて混ぜないように
            .env("PKG_CONFIG", "false")
            // 拡張モジュールを全部本体に組み込む(静的リンクでは .so を読み込めない)
            .env("MODULE_BUILDTYPE", "static")
            .args([
                "--host=aarch64-linux-musl",
                "--build=x86_64-linux-gnu",
                "--prefix=/usr",
            ])
            .arg(format!("--with-build-python={}", host_python.display()))
            .args(["--disable-shared", "--disable-ipv6", "--without-ensurepip"])
            .arg("--disable-test-modules")
            .arg(format!("CC={zig} cc -target aarch64-linux-musl"))
            .arg(format!("CXX={zig} c++ -target aarch64-linux-musl"))
            .arg(format!("AR={zig} ar"))
            .arg(format!("RANLIB={zig} ranlib"))
            .arg("READELF=readelf")
            // zig は __DATE__ を使うとエラーにする(再現できるビルドのため)
            .arg("CFLAGS=-Wno-error=date-time -g0")
            .arg("LDFLAGS=-static -s")
            .stdout(std::process::Stdio::null());
        run(cmd, "configuring CPython for aarch64-linux-musl");
        let mut cmd = Command::new("make");
        clean(&mut cmd);
        cmd.current_dir(&cross)
            .env("ZIG_LOCAL_CACHE_DIR", out.join("zig-cache"))
            .env("ZIG_GLOBAL_CACHE_DIR", out.join("zig-global-cache"))
            .arg(format!("-j{jobs}"))
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        run(cmd, "building CPython for aarch64-linux-musl");
    }

    // 3. ディスクに入れる木
    let stage = out.join("cpython-root.tmp");
    let _ = std::fs::remove_dir_all(&stage);
    let lib = stage.join("usr/lib").join(LIB);
    std::fs::create_dir_all(stage.join("usr/bin")).unwrap();
    std::fs::copy(cross.join("python"), stage.join("usr/bin/python3")).unwrap();
    const SKIP: &[&str] = &[
        "test",
        "idlelib",
        "tkinter",
        "turtledemo",
        "ensurepip",
        "__pycache__",
    ];
    copy_dir(&src.join("Lib"), &lib, SKIP);
    // configure が作った sysconfig のデータ(build/lib.linux-aarch64-3.15/)
    let built = cross
        .join("build")
        .join(format!("lib.linux-aarch64-{}", &LIB[6..]));
    copy_dir(&built, &lib.join("lib-dynload"), &[]);
    let _ = std::fs::remove_dir_all(&root);
    std::fs::rename(&stage, &root).unwrap();
    root
}

/// ディレクトリを写す(`skip` の名前のディレクトリは除く)
fn copy_dir(from: &Path, to: &Path, skip: &[&str]) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap().flatten() {
        let name = e.file_name();
        let path = e.path();
        if path.is_dir() {
            if !skip.contains(&name.to_str().unwrap_or("")) {
                copy_dir(&path, &to.join(&name), skip);
            }
        } else {
            std::fs::copy(&path, to.join(&name)).unwrap();
        }
    }
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
    let zig = zig_program();
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
        .env("ZIG", zig_program())
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
        let zig = zig_program();
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
        // busybox.links:アプレットの一覧(/bin/ls、/usr/bin/seq、…)。上と同じ make に並べると、
        // -j で設定のヘッダ(autoconf.h)ができる前に作ろうとして失敗するので、後で別に作る
        let mut cmd = Command::new("make");
        cmd.current_dir(&src)
            .arg("busybox.links")
            .stdout(std::process::Stdio::null());
        run(cmd, "listing BusyBox applets");
    }
    bin
}

/// BusyBox のアプレットの名前(`busybox.links` から)
fn busybox_applets(out: &Path) -> Vec<String> {
    let list = std::fs::read_to_string(out.join("busybox-1.38.0/busybox.links")).unwrap();
    list.lines()
        .filter_map(|l| l.rsplit('/').next())
        .filter(|n| !n.is_empty())
        .map(String::from)
        .collect()
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
fn tar(files: &[(String, Vec<u8>)], links: &[(String, String)]) -> Vec<u8> {
    let mut out = Vec::new();
    for (name, target) in links {
        // シンボリックリンク:種類 '2'、指す先は 157 バイト目から
        assert!(name.len() < 100 && target.len() < 100);
        let mut h = [0u8; 512];
        h[..name.len()].copy_from_slice(name.as_bytes());
        h[100..107].copy_from_slice(b"0000777");
        h[108..115].copy_from_slice(b"0000000");
        h[116..123].copy_from_slice(b"0000000");
        h[124..135].copy_from_slice(b"00000000000");
        h[136..147].copy_from_slice(b"00000000000");
        h[156] = b'2';
        h[157..157 + target.len()].copy_from_slice(target.as_bytes());
        h[257..263].copy_from_slice(b"ustar\0");
        h[263..265].copy_from_slice(b"00");
        h[148..156].copy_from_slice(b"        ");
        let sum: u32 = h.iter().map(|&b| b as u32).sum();
        h[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
        out.extend_from_slice(&h);
    }
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
