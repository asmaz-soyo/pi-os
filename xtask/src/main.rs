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
        Step::Snapshot {
            expected,
            exit_code,
            cmd,
        } => snapshot(expected, *exit_code, cmd),
    }
}

/// `cmd` の標準出力を `expected` と比べる。`UPDATE_SNAPSHOTS=1` なら期待値を書き換える
/// (ワイルドカードは消えるので、書き換えたあとに手で戻すこと)。
fn snapshot(expected: &str, exit_code: i32, cmd: &Cmd) -> Result<(), String> {
    let output = command(cmd)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("failed to start `{cmd}`: {e}"))?;
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
