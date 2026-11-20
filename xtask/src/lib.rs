//! `cargo xtask <task>` の中身。
//!
//! 「どのコマンドをどの順で実行するか」を [`plan`] が純粋な値として返し、
//! 実際の実行は `main.rs` が受け持つ。こうしておくと計画の部分を単体テストできる。

use std::fmt;

/// カーネル側のクレートをビルドするターゲット。
pub const TARGET: &str = "aarch64-unknown-none";

/// `TARGET` 向けにビルド・検査するクレート。クレートが増えたらここに足す。
pub const TARGET_CRATES: &[&str] = &["hal", "arch-aarch64", "bsp-qemu-virt"];

/// ホストではビルドできない(`TARGET` 専用の)クレート。ホストのテストと clippy から外す。
pub const TARGET_ONLY_CRATES: &[&str] = &["arch-aarch64", "bsp-qemu-virt"];

pub const USAGE: &str = "usage: cargo xtask <build|run|test [--bsp qemu-virt]|ci>";

/// xtask のサブコマンド。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Task {
    Build,
    Run,
    Test(TestKind),
    Ci,
}

/// どの層のテストを実行するか。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TestKind {
    /// ホストの単体テスト(`cargo test`)
    Host,
    /// QEMU 上でカーネル内のテストを動かす結合テスト
    Qemu(Bsp),
}

/// ボード(BSP)。raspi3 は M1 で足す。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bsp {
    QemuVirt,
}

impl Bsp {
    fn parse(name: &str) -> Result<Self, String> {
        match name {
            "qemu-virt" => Ok(Self::QemuVirt),
            other => Err(format!("unknown bsp `{other}`\n{USAGE}")),
        }
    }

    /// このボードのカーネルを持つクレート。
    pub fn crate_name(self) -> &'static str {
        match self {
            Self::QemuVirt => "bsp-qemu-virt",
        }
    }

    /// cargo の runner として使う QEMU のコマンド。
    ///
    /// cargo はこの後ろにテストの ELF のパスを付け足して実行するので、`-kernel` で終える。
    /// テストが終了しないままになったときのために `timeout` で包む。
    pub fn qemu_runner(self) -> String {
        match self {
            Self::QemuVirt => "timeout 60 qemu-system-aarch64 -M virt -cpu cortex-a53 -m 128M \
                               -nographic -semihosting -kernel"
                .to_string(),
        }
    }
}

/// コマンドライン引数(プログラム名を除く)をタスクに変換する。
pub fn parse(args: &[&str]) -> Result<Task, String> {
    match args {
        ["build"] => Ok(Task::Build),
        ["run"] => Ok(Task::Run),
        ["test"] => Ok(Task::Test(TestKind::Host)),
        ["test", "--bsp", bsp] => Ok(Task::Test(TestKind::Qemu(Bsp::parse(bsp)?))),
        ["ci"] => Ok(Task::Ci),
        _ => Err(USAGE.to_string()),
    }
}

/// 実行する1つのコマンド。`Display` で `cargo test --workspace` のような形になる。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cmd {
    pub program: String,
    pub args: Vec<String>,
    /// 追加で設定する環境変数
    pub env: Vec<(String, String)>,
}

impl Cmd {
    fn cargo<S: AsRef<str>>(args: &[S]) -> Self {
        Self {
            program: "cargo".to_string(),
            args: args.iter().map(|a| a.as_ref().to_string()).collect(),
            env: Vec::new(),
        }
    }

    fn env(mut self, key: &str, value: impl Into<String>) -> Self {
        self.env.push((key.to_string(), value.into()));
        self
    }
}

impl fmt::Display for Cmd {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (key, value) in &self.env {
            if value.contains(' ') {
                write!(f, "{key}='{value}' ")?;
            } else {
                write!(f, "{key}={value} ")?;
            }
        }
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
        Task::Test(TestKind::Host) => Ok(vec![host_tests()]),
        Task::Test(TestKind::Qemu(bsp)) => Ok(vec![qemu_tests(bsp)]),
        Task::Ci => {
            let mut cmds = vec![Cmd::cargo(&["fmt", "--all", "--check"])];
            let mut host_clippy = vec!["clippy".to_string(), "--workspace".to_string()];
            host_clippy.extend(exclude_target_only());
            host_clippy.extend(
                ["--all-targets", "--all-features", "--", "-D", "warnings"].map(String::from),
            );
            cmds.push(Cmd::cargo(&host_clippy));
            cmds.extend(TARGET_CRATES.iter().map(|krate| {
                Cmd::cargo(&[
                    "clippy", "-p", krate, "--target", TARGET, "--", "-D", "warnings",
                ])
            }));
            cmds.push(host_tests());
            cmds.push(qemu_tests(Bsp::QemuVirt));
            cmds.extend(target_builds());
            Ok(cmds)
        }
    }
}

fn exclude_target_only() -> Vec<String> {
    TARGET_ONLY_CRATES
        .iter()
        .flat_map(|krate| ["--exclude".to_string(), krate.to_string()])
        .collect()
}

fn host_tests() -> Cmd {
    let mut args = vec!["test".to_string(), "--workspace".to_string()];
    args.extend(exclude_target_only());
    Cmd::cargo(&args)
}

fn qemu_tests(bsp: Bsp) -> Cmd {
    Cmd::cargo(&["test", "-p", bsp.crate_name(), "--target", TARGET]).env(
        "CARGO_TARGET_AARCH64_UNKNOWN_NONE_RUNNER",
        bsp.qemu_runner(),
    )
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

    const HOST_TEST: &str = "cargo test --workspace --exclude arch-aarch64 --exclude bsp-qemu-virt";
    const QEMU_VIRT_TEST: &str = "CARGO_TARGET_AARCH64_UNKNOWN_NONE_RUNNER='timeout 60 \
        qemu-system-aarch64 -M virt -cpu cortex-a53 -m 128M -nographic -semihosting -kernel' \
        cargo test -p bsp-qemu-virt --target aarch64-unknown-none";

    #[test]
    fn parses_each_subcommand() {
        assert_eq!(parse(&["build"]), Ok(Task::Build));
        assert_eq!(parse(&["run"]), Ok(Task::Run));
        assert_eq!(parse(&["test"]), Ok(Task::Test(TestKind::Host)));
        assert_eq!(parse(&["ci"]), Ok(Task::Ci));
    }

    #[test]
    fn parses_qemu_test_with_bsp() {
        assert_eq!(
            parse(&["test", "--bsp", "qemu-virt"]),
            Ok(Task::Test(TestKind::Qemu(Bsp::QemuVirt)))
        );
    }

    #[test]
    fn rejects_unknown_bsp() {
        assert!(parse(&["test", "--bsp", "pc"]).is_err());
        assert!(parse(&["test", "--bsp"]).is_err());
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
            [
                "cargo build -p hal --target aarch64-unknown-none",
                "cargo build -p arch-aarch64 --target aarch64-unknown-none",
                "cargo build -p bsp-qemu-virt --target aarch64-unknown-none",
            ]
        );
    }

    #[test]
    fn host_test_plan_skips_target_only_crates() {
        assert_eq!(
            displays(&plan(Task::Test(TestKind::Host)).unwrap()),
            [HOST_TEST]
        );
    }

    #[test]
    fn qemu_test_plan_uses_qemu_as_cargo_runner() {
        assert_eq!(
            displays(&plan(Task::Test(TestKind::Qemu(Bsp::QemuVirt))).unwrap()),
            [QEMU_VIRT_TEST]
        );
    }

    #[test]
    fn qemu_runner_ends_with_kernel_flag_so_cargo_appends_the_elf() {
        assert!(Bsp::QemuVirt.qemu_runner().ends_with(" -kernel"));
    }

    #[test]
    fn run_is_not_available_until_kernel_exists() {
        assert!(plan(Task::Run).is_err());
    }

    #[test]
    fn ci_plan_runs_checks_then_host_and_qemu_tests_then_builds() {
        assert_eq!(
            displays(&plan(Task::Ci).unwrap()),
            [
                "cargo fmt --all --check",
                "cargo clippy --workspace --exclude arch-aarch64 --exclude bsp-qemu-virt \
                 --all-targets --all-features -- -D warnings",
                "cargo clippy -p hal --target aarch64-unknown-none -- -D warnings",
                "cargo clippy -p arch-aarch64 --target aarch64-unknown-none -- -D warnings",
                "cargo clippy -p bsp-qemu-virt --target aarch64-unknown-none -- -D warnings",
                HOST_TEST,
                QEMU_VIRT_TEST,
                "cargo build -p hal --target aarch64-unknown-none",
                "cargo build -p arch-aarch64 --target aarch64-unknown-none",
                "cargo build -p bsp-qemu-virt --target aarch64-unknown-none",
            ]
        );
    }
}
