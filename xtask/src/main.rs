use std::path::Path;
use std::process::{Command, ExitCode};

use xtask::{Cmd, parse, plan};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();

    let result = parse(&args)
        .and_then(plan)
        .and_then(|cmds| cmds.iter().try_for_each(execute));
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(msg) => {
            eprintln!("xtask: {msg}");
            ExitCode::FAILURE
        }
    }
}

/// コマンドをワークスペースのルートで実行し、失敗したら止める。
fn execute(cmd: &Cmd) -> Result<(), String> {
    eprintln!("==> {cmd}");
    // `cargo xtask` から呼ばれたときと同じ cargo を使う(CARGO は cargo が設定する)
    let program = match cmd.program.as_str() {
        "cargo" => std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string()),
        other => other.to_string(),
    };
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask lives directly under the workspace root");
    let status = Command::new(&program)
        .args(&cmd.args)
        .current_dir(root)
        .status()
        .map_err(|e| format!("failed to start `{cmd}`: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("`{cmd}` failed ({status})"))
    }
}
