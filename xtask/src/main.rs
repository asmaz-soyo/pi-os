use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};

use xtask::{Cmd, Step, compare_snapshot, normalize_output, parse, plan, syscall_table_markdown};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();

    let result = parse(&args).and_then(|task| plan(task).iter().try_for_each(execute));
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(msg) => {
            eprintln!("xtask: {msg}");
            ExitCode::FAILURE
        }
    }
}

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask lives directly under the workspace root")
        .to_path_buf()
}

fn execute(step: &Step) -> Result<(), String> {
    eprintln!("==> {step}");
    match step {
        Step::Run(cmd) => {
            let status = command(cmd)
                .status()
                .map_err(|e| format!("failed to start `{cmd}`: {e}"))?;
            if status.success() {
                Ok(())
            } else {
                Err(format!("`{cmd}` failed ({status})"))
            }
        }
        Step::GenerateDocs { path, check } => generate_docs(path, *check),
        Step::MakeDisk { path } => make_disk(path, &[]),
        Step::DiskGet { disk, from, to } => disk_get(disk, from, to),
        Step::Compat { cases, cmd } => compat(cases, cmd),
        Step::Snapshot {
            expected,
            exit_code,
            script,
            cmd,
        } => snapshot(expected, *exit_code, script, cmd),
    }
}

/// `cmd` の標準出力を `expected` と比べる。`UPDATE_SNAPSHOTS=1` なら期待値を書き換える
/// (ワイルドカードは消えるので、書き換えたあとに手で戻すこと)。
fn snapshot(
    expected: &str,
    exit_code: i32,
    script: &[(String, String)],
    cmd: &Cmd,
) -> Result<(), String> {
    use std::io::{Read, Write};
    use std::sync::{Arc, Mutex};

    let mut child = command(cmd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|e| format!("failed to start `{cmd}`: {e}"))?;
    // 出力を別のスレッドで読み続け、input_after が現れたら入力を送る
    let collected = Arc::new(Mutex::new(Vec::new()));
    let reader = {
        let collected = collected.clone();
        let mut stdout = child.stdout.take().expect("piped stdout");
        std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            while let Ok(n) = stdout.read(&mut buf) {
                if n == 0 {
                    break;
                }
                collected.lock().unwrap().extend_from_slice(&buf[..n]);
            }
        })
    };
    // 入力を送ったあとも標準入力は閉じない(閉じると QEMU が残りの入力を受け取らないことがある)
    let mut stdin = child.stdin.take();
    let mut step = 0;
    // 前の入力を送ったときまでに読んだ出力の長さ。次の合図はそれより後ろから探す
    let mut seen_upto = 0;
    loop {
        if child.try_wait().map_err(|e| e.to_string())?.is_some() {
            break;
        }
        if let Some((trigger, text)) = script.get(step) {
            let out = collected.lock().unwrap().clone();
            let rest = String::from_utf8_lossy(&out[seen_upto.min(out.len())..]).into_owned();
            if let Some(i) = rest.find(trigger.as_str()) {
                seen_upto += rest[..i + trigger.len()].len();
                if let Some(s) = stdin.as_mut() {
                    let _ = s.write_all(text.as_bytes());
                    let _ = s.flush();
                }
                step += 1;
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    drop(stdin);
    reader.join().expect("reader thread");
    let status = child.wait().map_err(|e| e.to_string())?;
    let output = std::process::Output {
        status,
        stdout: collected.lock().unwrap().clone(),
        stderr: Vec::new(),
    };
    let actual = String::from_utf8_lossy(&output.stdout);
    if output.status.code() != Some(exit_code) {
        return Err(format!(
            "`{cmd}` exited with {} (expected exit {exit_code})\n--- output ---\n{actual}",
            output.status
        ));
    }
    let path = root().join(expected);
    if std::env::var_os("UPDATE_SNAPSHOTS").is_some() {
        std::fs::create_dir_all(path.parent().unwrap()).map_err(|e| e.to_string())?;
        std::fs::write(&path, normalize_output(&actual)).map_err(|e| e.to_string())?;
        eprintln!("updated {expected}");
        return Ok(());
    }
    let want = std::fs::read_to_string(&path)
        .map_err(|e| format!("{expected}: {e} (UPDATE_SNAPSHOTS=1 to create it)"))?;
    compare_snapshot(&want, &actual).map_err(|e| format!("{expected}: {e}"))
}

/// ワークスペースのルートで動く `Command` を作る。
fn command(cmd: &Cmd) -> Command {
    // `cargo xtask` から呼ばれたときと同じ cargo を使う(CARGO は cargo が設定する)
    let program = match cmd.program.as_str() {
        "cargo" => std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string()),
        // rustup の llvm-tools コンポーネントに入っているものを使う
        "llvm-objcopy" => llvm_tool("llvm-objcopy"),
        other => other.to_string(),
    };
    let mut command = Command::new(program);
    command
        .args(&cmd.args)
        .envs(cmd.env.iter().map(|(k, v)| (k, v)))
        .current_dir(root());
    command
}

/// rustup の `llvm-tools` コンポーネントの道具のパス(`<sysroot>/lib/rustlib/<host>/bin/`)。
fn llvm_tool(name: &str) -> String {
    let rustc = |arg: &str| {
        Command::new("rustc")
            .arg(arg)
            .current_dir(root())
            .output()
            .ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
    };
    let sysroot = rustc("--print=sysroot").unwrap_or_default();
    let host = rustc("-vV")
        .and_then(|v| {
            v.lines()
                .find_map(|l| l.strip_prefix("host: ").map(str::to_string))
        })
        .unwrap_or_default();
    format!("{sysroot}/lib/rustlib/{host}/bin/{name}")
}

/// 自動生成のドキュメントを書く。`check` なら、今のファイルと違えば失敗する。
fn generate_docs(path: &str, check: bool) -> Result<(), String> {
    let full = root().join(path);
    let want = syscall_table_markdown();
    if check {
        let have = std::fs::read_to_string(&full).unwrap_or_default();
        if have != want {
            return Err(format!("{path} is out of date (run `cargo xtask docs`)"));
        }
        return Ok(());
    }
    std::fs::create_dir_all(full.parent().unwrap()).map_err(|e| e.to_string())?;
    std::fs::write(&full, want).map_err(|e| e.to_string())
}

/// QEMU virt に渡すテスト用のディスクイメージ(4MiB)を作る。
///
/// セクタ 0 は MBR、セクタ 1 は目印の文字列、LBA 2048 から ext2 のパーティション
/// (`user/diskroot/` の中身を `mke2fs -d` で入れる)。
/// 互換テストの最後に Pi-OS で動かす:ext2 のルートにファイルを書く
const PERSIST_SCRIPT: &str = "seq 1 500 > /compat-persist.txt\n";

/// Pi-OS が終わったあとのディスクの p2(ext2)を取り出し、e2fsck できれいで、
/// `PERSIST_SCRIPT` が書いたファイルがあることを確かめる(write-back のキャッシュを、終わるときに書いたか)
fn check_persisted() -> Result<(), String> {
    let part = root_partition(xtask::VIRT_DISK)?;
    let fsck = Command::new("e2fsck")
        .arg("-fn")
        .arg(&part)
        .output()
        .map_err(|e| e.to_string())?;
    if !fsck.status.success() {
        return Err(format!(
            "e2fsck:\n{}",
            String::from_utf8_lossy(&fsck.stdout)
        ));
    }
    let cat = Command::new("debugfs")
        .args(["-R", "cat /compat-persist.txt"])
        .arg(&part)
        .output()
        .map_err(|e| e.to_string())?;
    let want: String = (1..=500).map(|i| format!("{i}\n")).collect();
    if String::from_utf8_lossy(&cat.stdout) != want {
        return Err("/compat-persist.txt is missing or wrong".to_string());
    }
    Ok(())
}

/// ディスクイメージ `disk` の 2 つ目のパーティション(ルートの ext2)を、別のファイルに取り出す
fn root_partition(disk: &str) -> Result<std::path::PathBuf, String> {
    let disk = std::fs::read(root().join(disk)).map_err(|e| format!("{disk}: {e}"))?;
    // MBR の 2 つ目のパーティション(開始位置と大きさ)
    let entry = &disk[446 + 16..462 + 16];
    let start = u32::from_le_bytes(entry[8..12].try_into().unwrap()) as usize * 512;
    let len = u32::from_le_bytes(entry[12..16].try_into().unwrap()) as usize * 512;
    let part = root().join("target/virt-p2.img");
    std::fs::write(&part, &disk[start..start + len]).map_err(|e| e.to_string())?;
    Ok(part)
}

/// ディスクのルートの ext2 から `from` を取り出して、ホストの `to` に書く(debugfs の dump)
fn disk_get(disk: &str, from: &str, to: &str) -> Result<(), String> {
    let part = root_partition(disk)?;
    let out = Command::new("debugfs")
        .args(["-R", &format!("dump {from} {to}")])
        .arg(&part)
        .output()
        .map_err(|e| format!("debugfs (e2fsprogs) is needed: {e}"))?;
    // debugfs は見つからなくても 0 で終わるので、エラーの文字列で見分ける
    let err = String::from_utf8_lossy(&out.stderr);
    if !out.status.success() || err.contains("File not found") {
        return Err(format!("{from}: {}", err.trim()));
    }
    let size = std::fs::metadata(to).map_err(|e| e.to_string())?.len();
    println!("{to}: {size} bytes");
    Ok(())
}

/// 互換テスト:それぞれのケースを Linux(qemu-aarch64)で動かし、次に Pi-OS で
/// (init を互換テスト用のスクリプトに差し替えたディスクで)まとめて動かして、出力と終了コードを比べる
fn compat(cases: &[(String, Vec<String>)], cmd: &Cmd) -> Result<(), String> {
    let bin = initramfs_tar()?.parent().unwrap().to_path_buf();
    let mut linux = Vec::new();
    for (prog, args) in cases {
        let path = match prog.as_str() {
            "busybox" => bin.join("busybox-1.38.0/busybox_unstripped"),
            "micropython" => bin.join("micropython-1.29.0/ports/unix/build-standard/micropython"),
            // Rust のプログラム(user/build.rs の build_rust_program の出力先)
            "svcman" | "rthreads" => bin
                .join("rust/aarch64-unknown-linux-musl/release")
                .join(prog),
            _ => bin.join(prog),
        };
        let out = Command::new("qemu-aarch64")
            .arg(&path)
            .args(args)
            .stdin(std::process::Stdio::null())
            .output()
            .map_err(|e| format!("qemu-aarch64 (qemu-user) is needed: {e}"))?;
        linux.push((
            String::from_utf8_lossy(&out.stdout).into_owned(),
            out.status.code(),
        ));
    }

    let refs: Vec<(&str, Vec<&str>)> = cases
        .iter()
        .map(|(p, a)| (p.as_str(), a.iter().map(String::as_str).collect()))
        .collect();
    let refs: Vec<(&str, &[&str])> = refs.iter().map(|(p, a)| (*p, a.as_slice())).collect();
    let script = xtask::compat_script(&refs);
    make_disk(
        xtask::VIRT_DISK,
        &[
            (
                "etc/init.rc",
                "/bin/busybox sh /etc/compat.sh\n".to_string(),
            ),
            // 最後に ext2(ルート)に書く:終わるときにキャッシュがディスクに書かれたかを、後で確かめる
            ("etc/compat.sh", script + PERSIST_SCRIPT),
        ],
    )?;
    let out = Command::new(&cmd.program)
        .args(&cmd.args)
        .current_dir(root())
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| e.to_string())?;
    let pios = xtask::split_compat_output(&String::from_utf8_lossy(&out.stdout));
    let persisted = check_persisted();

    let mut failed = 0;
    for (i, (prog, args)) in cases.iter().enumerate() {
        let name = format!("{prog} {}", args.join(" "));
        let Some((got, code)) = pios.get(i) else {
            println!("FAIL {name}: did not run on Pi-OS");
            failed += 1;
            continue;
        };
        let (want, want_code) = &linux[i];
        if got == want && code == want_code {
            println!("ok   {name}");
            continue;
        }
        failed += 1;
        println!("FAIL {name}");
        if code != want_code {
            println!("  exit: Linux {want_code:?}, Pi-OS {code:?}");
        }
        let (w, g): (Vec<&str>, Vec<&str>) = (want.lines().collect(), got.lines().collect());
        for j in 0..w.len().max(g.len()) {
            let (a, b) = (w.get(j), g.get(j));
            if a != b {
                println!("  line {}: Linux {:?}", j + 1, a.unwrap_or(&"(none)"));
                println!("  line {}: Pi-OS {:?}", j + 1, b.unwrap_or(&"(none)"));
            }
        }
    }
    match &persisted {
        Ok(()) => println!("ok   writes reached the disk (e2fsck clean)"),
        Err(e) => {
            println!("FAIL writes did not reach the disk: {e}");
            failed += 1;
        }
    }
    if failed > 0 {
        Err(format!("compat: {failed} check(s) failed"))
    } else {
        println!("compat: all {} case(s) match Linux", cases.len());
        Ok(())
    }
}

/// `overlay` の (パス, 中身) はルートのファイルシステム(p2)の上に重ねて書く(init の差し替えなど)
fn make_disk(path: &str, overlay: &[(&str, String)]) -> Result<(), String> {
    // SD カードと同じ形:p1 = FAT32(/boot)、p2 = ext2
    const BOOT_START: usize = 2048;
    const BOOT_MIB: usize = 34; // FAT32 にはクラスタが 65525 個以上いる(1 クラスタ 512 バイトで約 33MiB)
    const BOOT_SECTORS: usize = BOOT_MIB * 2048;
    const EXT2_START: usize = BOOT_START + BOOT_SECTORS;
    const EXT2_SECTORS: usize = 64 * 2048; // 64MiB(ユーザーランドと CPython が入る大きさ)
    let mut disk = vec![0u8; (EXT2_START + EXT2_SECTORS) * 512];
    let parts = [
        (0x0c, BOOT_START, BOOT_SECTORS),
        (0x83, EXT2_START, EXT2_SECTORS),
    ];
    for (i, (kind, start, count)) in parts.iter().enumerate() {
        let entry = &mut disk[446 + i * 16..462 + i * 16];
        entry[4] = *kind;
        entry[8..12].copy_from_slice(&(*start as u32).to_le_bytes());
        entry[12..16].copy_from_slice(&(*count as u32).to_le_bytes());
    }
    disk[510] = 0x55;
    disk[511] = 0xaa;
    let mark = b"Pi-OS test disk\n";
    disk[512..512 + mark.len()].copy_from_slice(mark);

    // p1:user/bootroot の中身を FAT32 に
    let mut files = Vec::new();
    collect_files(&root().join("user/bootroot"), "", &mut files)?;
    let refs: Vec<(&str, &[u8])> = files
        .iter()
        .map(|(p, d)| (p.as_str(), d.as_slice()))
        .collect();
    let fat = fatimg::build(BOOT_MIB as u32, "PIOSBOOT", &refs);
    disk[BOOT_START * 512..BOOT_START * 512 + fat.len()].copy_from_slice(&fat);

    // p2:ルートのファイルシステム(ext2)。initramfs と同じ中身に user/diskroot を足す
    let full = root().join(path);
    std::fs::create_dir_all(full.parent().unwrap()).map_err(|e| e.to_string())?;
    let stage = full.with_extension("root");
    let _ = std::fs::remove_dir_all(&stage);
    let tar = initramfs_tar()?;
    unpack_tar(&std::fs::read(&tar).map_err(|e| e.to_string())?, &stage)?;
    // CPython(大きいので initramfs には入れず、ディスクにだけ入れる)
    let cpython = tar.parent().unwrap().join("cpython-root");
    if cpython.exists() {
        copy_tree(&cpython, &stage)?;
    }
    copy_tree(&root().join("user/diskroot"), &stage)?;
    for (rel, content) in overlay {
        std::fs::write(stage.join(rel), content).map_err(|e| e.to_string())?;
    }
    let ext2 = full.with_extension("ext2");
    // 先に空のファイルを作っておくと、mke2fs が「Creating regular file」と言わない
    std::fs::write(&ext2, b"").map_err(|e| e.to_string())?;
    // fakeroot の中で持ち主を root にしてから作る(ディスクのファイルは root の持ち物にする)
    let script = format!(
        "chown -R 0:0 '{}' && mke2fs -q -F -t ext3 -O ^dir_index -J size=4 -b 1024 -I 128 -d '{}' '{}' {}",
        stage.display(),
        stage.display(),
        ext2.display(),
        EXT2_SECTORS / 2
    );
    let status = Command::new("fakeroot")
        .args(["sh", "-c", &script])
        .status()
        .map_err(|e| format!("fakeroot and mke2fs (e2fsprogs) are needed: {e}"))?;
    if !status.success() {
        return Err("mke2fs failed".to_string());
    }
    let fs = std::fs::read(&ext2).map_err(|e| e.to_string())?;
    disk[EXT2_START * 512..EXT2_START * 512 + fs.len()].copy_from_slice(&fs);
    std::fs::write(&full, disk).map_err(|e| e.to_string())
}

/// ユーザーランドをビルドし、build.rs が作った initramfs.tar の場所を cargo に聞く
fn initramfs_tar() -> Result<std::path::PathBuf, String> {
    let out = Command::new("cargo")
        .args([
            "build",
            "-q",
            "-p",
            "userland",
            "--target",
            "aarch64-unknown-none",
        ])
        .arg("--message-format=json")
        .current_dir(root())
        .output()
        .map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(format!(
            "building userland failed:\n{}",
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let line = text
        .lines()
        .find(|l| l.contains("\"build-script-executed\"") && l.contains("userland"))
        .ok_or("cargo did not report the userland build script")?;
    let key = "\"out_dir\":\"";
    let start = line.find(key).ok_or("no out_dir")? + key.len();
    let end = start + line[start..].find('"').ok_or("no out_dir")?;
    Ok(std::path::PathBuf::from(&line[start..end]).join("initramfs.tar"))
}

/// ustar の tar を `dest` に展開する(ファイルのモードも写す)
fn unpack_tar(tar: &[u8], dest: &std::path::Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    let octal = |b: &[u8]| {
        let s = String::from_utf8_lossy(b);
        u64::from_str_radix(s.trim_matches(|c: char| c == '\0' || c == ' '), 8).unwrap_or(0)
    };
    let mut off = 0;
    while off + 512 <= tar.len() && tar[off] != 0 {
        let h = &tar[off..off + 512];
        let cstr =
            |b: &[u8]| String::from_utf8_lossy(b.split(|&c| c == 0).next().unwrap()).into_owned();
        let (prefix, name) = (cstr(&h[345..500]), cstr(&h[..100]));
        let name = if prefix.is_empty() {
            name
        } else {
            format!("{prefix}/{name}")
        };
        let size = octal(&h[124..136]) as usize;
        let mode = octal(&h[100..108]) as u32;
        let data = &tar[off + 512..off + 512 + size];
        let path = dest.join(name.trim_start_matches("./"));
        if h[156] == b'5' {
            std::fs::create_dir_all(&path).map_err(|e| e.to_string())?;
        } else if h[156] == b'2' {
            // シンボリックリンク(mke2fs -d がそのまま ext2 のリンクにする)
            std::fs::create_dir_all(path.parent().unwrap()).map_err(|e| e.to_string())?;
            let _ = std::fs::remove_file(&path);
            std::os::unix::fs::symlink(cstr(&h[157..257]), &path).map_err(|e| e.to_string())?;
        } else {
            std::fs::create_dir_all(path.parent().unwrap()).map_err(|e| e.to_string())?;
            std::fs::write(&path, data).map_err(|e| e.to_string())?;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode & 0o7777))
                .map_err(|e| e.to_string())?;
        }
        off += 512 + size.div_ceil(512) * 512;
    }
    Ok(())
}

/// `src` の中身を `dest` に写す(ファイルのモード、つまり実行ビットも写す)
fn copy_tree(src: &std::path::Path, dest: &std::path::Path) -> Result<(), String> {
    std::fs::create_dir_all(dest).map_err(|e| e.to_string())?;
    for e in std::fs::read_dir(src).map_err(|e| format!("{}: {e}", src.display()))? {
        let e = e.map_err(|e| e.to_string())?;
        let to = dest.join(e.file_name());
        if e.path().is_dir() {
            copy_tree(&e.path(), &to)?;
        } else {
            std::fs::copy(e.path(), &to).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

/// ディレクトリの中のファイルを (相対パス, 中身) で集める
fn collect_files(
    dir: &std::path::Path,
    prefix: &str,
    out: &mut Vec<(String, Vec<u8>)>,
) -> Result<(), String> {
    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .map_err(|e| format!("{}: {e}", dir.display()))?
        .filter_map(Result::ok)
        .collect();
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let name = format!("{prefix}{}", e.file_name().to_string_lossy());
        if e.path().is_dir() {
            collect_files(&e.path(), &format!("{name}/"), out)?;
        } else {
            out.push((name, std::fs::read(e.path()).map_err(|e| e.to_string())?));
        }
    }
    Ok(())
}
