//! `cargo xtask <task>` の中身。
//!
//! 「何をどの順で実行するか」を [`plan`] が純粋な値([`Step`] の列)として返し、
//! 実際の実行は `main.rs` が受け持つ。こうしておくと計画の部分を単体テストできる。

use std::fmt;

/// カーネル側のクレートをビルドするターゲット。
pub const TARGET: &str = "aarch64-unknown-none";

/// `TARGET` 向けにビルド・検査するクレート。クレートが増えたらここに足す。
pub const TARGET_CRATES: &[&str] = &["hal", "kernel", "pl011", "arch-aarch64", "bsp-qemu-virt"];

/// ホストではビルドできない(`TARGET` 専用の)クレート。ホストのテストと clippy から外す。
pub const TARGET_ONLY_CRATES: &[&str] = &["arch-aarch64", "bsp-qemu-virt"];

pub const USAGE: &str = "usage: cargo xtask <build | run [--bsp <bsp>] | \
                         test [--bsp <bsp> | --snapshot] | ci>\n  bsp: qemu-virt";

/// xtask のサブコマンド。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Task {
    Build,
    Run(Bsp),
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
    /// QEMU で起動し、UART の出力を期待値(スナップショット)と比べる
    Snapshot,
}

/// ボード(BSP)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bsp {
    QemuVirt,
}

impl Bsp {
    pub const ALL: &[Bsp] = &[Bsp::QemuVirt];

    fn parse(name: &str) -> Result<Self, String> {
        Self::ALL
            .iter()
            .copied()
            .find(|bsp| bsp.name() == name)
            .ok_or_else(|| format!("unknown bsp `{name}`\n{USAGE}"))
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::QemuVirt => "qemu-virt",
        }
    }

    /// このボードのカーネルイメージを作るクレート。
    pub fn crate_name(self) -> &'static str {
        match self {
            Self::QemuVirt => "bsp-qemu-virt",
        }
    }

    /// ビルドしたカーネルの ELF(ワークスペースのルートからの相対パス)。
    pub fn kernel_elf(self) -> String {
        format!("target/{TARGET}/debug/{}", self.crate_name())
    }

    /// QEMU の引数(`-kernel` の手前まで)。
    fn qemu_args(self) -> &'static [&'static str] {
        match self {
            Self::QemuVirt => &[
                "-M",
                "virt",
                "-cpu",
                "cortex-a53",
                "-m",
                "128M",
                "-nographic",
                "-semihosting",
            ],
        }
    }

    /// `kernel` を起動する QEMU のコマンド。
    fn qemu(self, kernel: &str) -> Cmd {
        let mut args: Vec<&str> = self.qemu_args().to_vec();
        args.extend(["-kernel", kernel]);
        Cmd::new("qemu-system-aarch64", &args)
    }

    /// cargo の runner として使う QEMU のコマンド。
    ///
    /// cargo はこの後ろにテストの ELF のパスを付け足して実行するので、`-kernel` で終える。
    /// テストが終了しないままになったときのために `timeout` で包む。
    pub fn qemu_runner(self) -> String {
        format!(
            "timeout 60 qemu-system-aarch64 {} -kernel",
            self.qemu_args().join(" ")
        )
    }
}

/// コマンドライン引数(プログラム名を除く)をタスクに変換する。
pub fn parse(args: &[&str]) -> Result<Task, String> {
    match args {
        ["build"] => Ok(Task::Build),
        ["run"] => Ok(Task::Run(Bsp::QemuVirt)),
        ["run", "--bsp", bsp] => Ok(Task::Run(Bsp::parse(bsp)?)),
        ["test"] => Ok(Task::Test(TestKind::Host)),
        ["test", "--bsp", bsp] => Ok(Task::Test(TestKind::Qemu(Bsp::parse(bsp)?))),
        ["test", "--snapshot"] => Ok(Task::Test(TestKind::Snapshot)),
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
    fn new<S: AsRef<str>>(program: &str, args: &[S]) -> Self {
        Self {
            program: program.to_string(),
            args: args.iter().map(|a| a.as_ref().to_string()).collect(),
            env: Vec::new(),
        }
    }

    fn cargo<S: AsRef<str>>(args: &[S]) -> Self {
        Self::new("cargo", args)
    }

    fn env(mut self, key: &str, value: impl Into<String>) -> Self {
        self.env.push((key.to_string(), value.into()));
        self
    }

    /// `timeout <secs>` で包んだコマンドにする。
    fn with_timeout(self, secs: u32) -> Self {
        let mut args = vec![secs.to_string(), self.program];
        args.extend(self.args);
        Self {
            program: "timeout".to_string(),
            args,
            env: self.env,
        }
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

/// 計画の1手順。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    /// コマンドを実行し、失敗したら止める
    Run(Cmd),
    /// コマンドの標準出力を `expected`(ルートからの相対パス)の内容と比べる
    Snapshot { expected: String, cmd: Cmd },
}

impl fmt::Display for Step {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Run(cmd) => write!(f, "{cmd}"),
            Self::Snapshot { expected, cmd } => write!(f, "snapshot {expected} <= {cmd}"),
        }
    }
}

/// タスクを、順に実行する手順の列に変換する。
pub fn plan(task: Task) -> Vec<Step> {
    match task {
        Task::Build => target_builds(),
        Task::Run(bsp) => vec![
            Step::Run(kernel_build(bsp)),
            Step::Run(bsp.qemu(&bsp.kernel_elf())),
        ],
        Task::Test(TestKind::Host) => vec![Step::Run(host_tests())],
        Task::Test(TestKind::Qemu(bsp)) => vec![Step::Run(qemu_tests(bsp))],
        Task::Test(TestKind::Snapshot) => snapshot_tests(),
        Task::Ci => {
            let mut steps = vec![Step::Run(Cmd::cargo(&["fmt", "--all", "--check"]))];
            let mut host_clippy = vec!["clippy".to_string(), "--workspace".to_string()];
            host_clippy.extend(exclude_target_only());
            host_clippy.extend(
                ["--all-targets", "--all-features", "--", "-D", "warnings"].map(String::from),
            );
            steps.push(Step::Run(Cmd::cargo(&host_clippy)));
            steps.extend(TARGET_CRATES.iter().map(|krate| {
                Step::Run(Cmd::cargo(&[
                    "clippy", "-p", krate, "--target", TARGET, "--", "-D", "warnings",
                ]))
            }));
            steps.push(Step::Run(host_tests()));
            steps.extend(Bsp::ALL.iter().map(|&bsp| Step::Run(qemu_tests(bsp))));
            steps.extend(snapshot_tests());
            steps.extend(target_builds());
            steps
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

fn kernel_build(bsp: Bsp) -> Cmd {
    Cmd::cargo(&["build", "-p", bsp.crate_name(), "--target", TARGET])
}

fn snapshot_tests() -> Vec<Step> {
    Bsp::ALL
        .iter()
        .flat_map(|&bsp| {
            [
                Step::Run(kernel_build(bsp)),
                Step::Snapshot {
                    expected: format!("tests/snapshots/{}.txt", bsp.name()),
                    cmd: bsp.qemu(&bsp.kernel_elf()).with_timeout(30),
                },
            ]
        })
        .collect()
}

fn target_builds() -> Vec<Step> {
    TARGET_CRATES
        .iter()
        .map(|krate| Step::Run(Cmd::cargo(&["build", "-p", krate, "--target", TARGET])))
        .collect()
}

/// QEMU のシリアル出力を比較しやすい形にそろえる(`\r` を消し、末尾の空行を1つの改行にまとめる)。
pub fn normalize_output(raw: &str) -> String {
    let text = raw.replace('\r', "");
    let trimmed = text.trim_end_matches('\n');
    if trimmed.is_empty() {
        String::new()
    } else {
        format!("{trimmed}\n")
    }
}

/// 期待する出力と実際の出力を比べ、違えば最初に食い違った行を示す。
pub fn compare_snapshot(expected: &str, actual: &str) -> Result<(), String> {
    let expected = normalize_output(expected);
    let actual = normalize_output(actual);
    if expected == actual {
        return Ok(());
    }
    let mut exp_lines = expected.lines();
    let mut act_lines = actual.lines();
    let mut line = 1;
    loop {
        match (exp_lines.next(), act_lines.next()) {
            (Some(e), Some(a)) if e == a => line += 1,
            (e, a) => {
                return Err(format!(
                    "snapshot mismatch at line {line}\n  expected: {}\n  actual:   {}\n\
                     --- full output ---\n{actual}",
                    e.unwrap_or("<end of output>"),
                    a.unwrap_or("<end of output>"),
                ));
            }
        }
    }
}

#[cfg(test)]
mod tests;
