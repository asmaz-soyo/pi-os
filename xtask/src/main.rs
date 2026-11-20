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
        Step::MakeDisk { path } => make_disk(path),
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
fn make_disk(path: &str) -> Result<(), String> {
    const PART_START: usize = 2048;
    const PART_SECTORS: usize = 6144;
    let mut disk = vec![0u8; (PART_START + PART_SECTORS) * 512];
    let entry = &mut disk[446..462];
    entry[4] = 0x83;
    entry[8..12].copy_from_slice(&(PART_START as u32).to_le_bytes());
    entry[12..16].copy_from_slice(&(PART_SECTORS as u32).to_le_bytes());
    disk[510] = 0x55;
    disk[511] = 0xaa;
    let mark = b"Pi-OS test disk\n";
    disk[512..512 + mark.len()].copy_from_slice(mark);

    let full = root().join(path);
    std::fs::create_dir_all(full.parent().unwrap()).map_err(|e| e.to_string())?;
    let ext2 = full.with_extension("ext2");
    // 先に空のファイルを作っておくと、mke2fs が「Creating regular file」と言わない
    std::fs::write(&ext2, b"").map_err(|e| e.to_string())?;
    let status = Command::new("mke2fs")
        .args(["-q", "-F", "-t", "ext2", "-b", "1024", "-I", "128", "-d"])
        .arg(root().join("user/diskroot"))
        .arg(&ext2)
        .arg((PART_SECTORS / 2).to_string())
        .status()
        .map_err(|e| format!("mke2fs (e2fsprogs) is needed: {e}"))?;
    if !status.success() {
        return Err("mke2fs failed".to_string());
    }
    let fs = std::fs::read(&ext2).map_err(|e| e.to_string())?;
    disk[PART_START * 512..PART_START * 512 + fs.len()].copy_from_slice(&fs);
    std::fs::write(&full, disk).map_err(|e| e.to_string())
}
