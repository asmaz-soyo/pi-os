//! `cargo xtask <task>` の中身。
//!
//! 「どのコマンドをどの順で実行するか」を [`plan`] が純粋な値として返し、
//! 実際の実行は `main.rs` が受け持つ。こうしておくと計画の部分を単体テストできる。

use std::fmt;

/// カーネル側のクレートをビルドするターゲット。
pub const TARGET: &str = "aarch64-unknown-none";

/// `TARGET` 向けにビルド・検査するクレート。クレートが増えたらここに足す。
pub const TARGET_CRATES: &[&str] = &["hal"];

pub const USAGE: &str = "usage: cargo xtask <build|run|test|ci>";

/// xtask のサブコマンド。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Task {
    Build,
    Run,
    Test,
    Ci,
}

/// コマンドライン引数(プログラム名を除く)をタスクに変換する。
pub fn parse(args: &[&str]) -> Result<Task, String> {
    match args {
        ["build"] => Ok(Task::Build),
        ["run"] => Ok(Task::Run),
        ["test"] => Ok(Task::Test),
        ["ci"] => Ok(Task::Ci),
        _ => Err(USAGE.to_string()),
    }
}

/// 実行する1つのコマンド。`Display` で `cargo test --workspace` のような形になる。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cmd {
    pub program: String,
    pub args: Vec<String>,
}

impl Cmd {
    fn cargo(args: &[&str]) -> Self {
        Self {
            program: "cargo".to_string(),
            args: args.iter().map(|a| a.to_string()).collect(),
        }
    }
}

impl fmt::Display for Cmd {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.program)?;
        for arg in &self.args {
            write!(f, " {arg}")?;
        }
        Ok(())
    }
}

/// タスクを、順に実行するコマンドの列に変換する。
pub fn plan(task: Task) -> Result<Vec<Cmd>, String> {
    match task {
        Task::Build => Ok(target_builds()),
        Task::Run => Err("run: no kernel to boot yet (added in M1)".to_string()),
        Task::Test => Ok(vec![host_tests()]),
        Task::Ci => {
            let mut cmds = vec![
                Cmd::cargo(&["fmt", "--all", "--check"]),
                Cmd::cargo(&[
                    "clippy",
                    "--workspace",
                    "--all-targets",
                    "--all-features",
                    "--",
                    "-D",
                    "warnings",
                ]),
            ];
            cmds.extend(TARGET_CRATES.iter().map(|krate| {
                Cmd::cargo(&[
                    "clippy", "-p", krate, "--target", TARGET, "--", "-D", "warnings",
                ])
            }));
            cmds.push(host_tests());
            cmds.extend(target_builds());
            Ok(cmds)
        }
    }
}

fn host_tests() -> Cmd {
    Cmd::cargo(&["test", "--workspace"])
}

fn target_builds() -> Vec<Cmd> {
    TARGET_CRATES
        .iter()
        .map(|krate| Cmd::cargo(&["build", "-p", krate, "--target", TARGET]))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn displays(plan: &[Cmd]) -> Vec<String> {
        plan.iter().map(Cmd::to_string).collect()
    }

    #[test]
    fn parses_each_subcommand() {
        assert_eq!(parse(&["build"]), Ok(Task::Build));
        assert_eq!(parse(&["run"]), Ok(Task::Run));
        assert_eq!(parse(&["test"]), Ok(Task::Test));
        assert_eq!(parse(&["ci"]), Ok(Task::Ci));
    }

    #[test]
    fn rejects_missing_subcommand() {
        assert!(parse(&[]).is_err());
    }

    #[test]
    fn rejects_unknown_subcommand() {
        assert!(parse(&["deploy"]).is_err());
    }

    #[test]
    fn rejects_extra_arguments() {
        assert!(parse(&["ci", "--now"]).is_err());
    }

    #[test]
    fn build_plan_builds_target_crates_for_aarch64() {
        assert_eq!(
            displays(&plan(Task::Build).unwrap()),
            ["cargo build -p hal --target aarch64-unknown-none"]
        );
    }

    #[test]
    fn test_plan_runs_host_tests() {
        assert_eq!(
            displays(&plan(Task::Test).unwrap()),
            ["cargo test --workspace"]
        );
    }

    #[test]
    fn run_is_not_available_until_kernel_exists() {
        assert!(plan(Task::Run).is_err());
    }

    #[test]
    fn ci_plan_runs_fmt_clippy_test_and_target_build_in_order() {
        assert_eq!(
            displays(&plan(Task::Ci).unwrap()),
            [
                "cargo fmt --all --check",
                "cargo clippy --workspace --all-targets --all-features -- -D warnings",
                "cargo clippy -p hal --target aarch64-unknown-none -- -D warnings",
                "cargo test --workspace",
                "cargo build -p hal --target aarch64-unknown-none",
            ]
        );
    }
}
