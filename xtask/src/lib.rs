//! `cargo xtask <task>` の中身。
//!
//! 「何をどの順で実行するか」を [`plan`] が純粋な値([`Step`] の列)として返し、
//! 実際の実行は `main.rs` が受け持つ。こうしておくと計画の部分を単体テストできる。

use std::fmt;

/// カーネル側のクレートをビルドするターゲット。
pub const TARGET: &str = "aarch64-unknown-none";

/// `TARGET` 向けにビルド・検査するクレート。クレートが増えたらここに足す。
pub const TARGET_CRATES: &[&str] = &[
    "hal",
    "kernel",
    "pl011",
    "arch-aarch64",
    "userland",
    "boot",
    "ktest",
    "bsp-qemu-virt",
    "bsp-raspi3",
];

/// ホストではビルドできない(`TARGET` 専用の)クレート。ホストのテストと clippy から外す。
pub const TARGET_ONLY_CRATES: &[&str] =
    &["userland", "boot", "ktest", "bsp-qemu-virt", "bsp-raspi3"];

pub const USAGE: &str = "usage: cargo xtask <build | run [--bsp <bsp> | --trace syscall] | \
                         test [--bsp <bsp> | --snapshot] | docs [--check] | ci>\n  \
                         bsp: qemu-virt, raspi3";

/// xtask のサブコマンド。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Task {
    Build,
    /// QEMU で起動する。`trace` ならシステムコールを表示する
    Run {
        bsp: Bsp,
        trace: bool,
    },
    Test(TestKind),
    /// 自動生成のドキュメントを作る。`check` なら作らずに差分がないことだけ確かめる
    Docs {
        check: bool,
    },
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
    /// QEMU `-M virt`(開発用)
    QemuVirt,
    /// Raspberry Pi 3B(QEMU `-M raspi3b` と実機で共通)
    Raspi3,
}

impl Bsp {
    pub const ALL: &[Bsp] = &[Bsp::QemuVirt, Bsp::Raspi3];

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
            Self::Raspi3 => "raspi3",
        }
    }

    /// このボードのカーネルイメージを作るクレート。
    pub fn crate_name(self) -> &'static str {
        match self {
            Self::QemuVirt => "bsp-qemu-virt",
            Self::Raspi3 => "bsp-raspi3",
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
                // 既定のネットワークカードは使わない(ROM ファイルがない環境でも起動できるように)
                "-nic",
                "none",
                "-nographic",
                "-semihosting",
            ],
            // raspi3b は RAM 1GB 固定。-nographic で UART0(PL011)が標準入出力につながる
            Self::Raspi3 => &["-M", "raspi3b", "-nographic", "-semihosting"],
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
        ["run"] => Ok(Task::Run {
            bsp: Bsp::QemuVirt,
            trace: false,
        }),
        ["run", "--bsp", bsp] => Ok(Task::Run {
            bsp: Bsp::parse(bsp)?,
            trace: false,
        }),
        ["run", "--trace", "syscall"] => Ok(Task::Run {
            bsp: Bsp::QemuVirt,
            trace: true,
        }),
        ["docs"] => Ok(Task::Docs { check: false }),
        ["docs", "--check"] => Ok(Task::Docs { check: true }),
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
    /// 自動生成のドキュメント `path` を作る(`check` なら差分がないことだけ確かめる)
    GenerateDocs { path: String, check: bool },
    /// コマンドの標準出力を `expected`(ルートからの相対パス)の内容と比べ、
    /// 終了コードが `exit_code` であることを確かめる
    Snapshot {
        expected: String,
        exit_code: i32,
        cmd: Cmd,
    },
}

impl fmt::Display for Step {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Run(cmd) => write!(f, "{cmd}"),
            Self::GenerateDocs { path, check } => {
                write!(f, "generate {path}{}", if *check { " (check)" } else { "" })
            }
            Self::Snapshot {
                expected,
                exit_code,
                cmd,
            } => write!(f, "snapshot {expected} (exit {exit_code}) <= {cmd}"),
        }
    }
}

/// タスクを、順に実行する手順の列に変換する。
pub fn plan(task: Task) -> Vec<Step> {
    match task {
        Task::Build => target_builds(),
        Task::Run { bsp, trace } => {
            let mut build = kernel_build(bsp);
            if trace {
                build
                    .args
                    .extend(["--features".into(), "trace-syscalls".into()]);
            }
            vec![Step::Run(build), Step::Run(bsp.qemu(&bsp.kernel_elf()))]
        }
        Task::Docs { check } => vec![docs_step(check)],
        Task::Test(TestKind::Host) => vec![Step::Run(host_tests())],
        Task::Test(TestKind::Qemu(bsp)) => vec![Step::Run(qemu_tests(bsp))],
        Task::Test(TestKind::Snapshot) => snapshot_tests(),
        Task::Ci => {
            let mut steps = vec![
                docs_step(true),
                Step::Run(Cmd::cargo(&["fmt", "--all", "--check"])),
            ];
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

/// 自動生成のシステムコール表の置き場所
pub const SYSCALL_DOC: &str = "docs/generated/syscalls.md";

fn docs_step(check: bool) -> Step {
    Step::GenerateDocs {
        path: SYSCALL_DOC.to_string(),
        check,
    }
}

/// カーネルのシステムコール表から作る Markdown。
pub fn syscall_table_markdown() -> String {
    let mut md = String::from(
        "<!-- cargo xtask docs で生成。手で編集しない -->\n\
         # 実装済みのシステムコール\n\n\
         番号は Linux AArch64 と同じ。表の元は `kernel/src/syscall/mod.rs` の `TABLE`。\n\n\
         | 番号 | 名前 | 引数の数 |\n| --- | --- | --- |\n",
    );
    for s in kernel::syscall::TABLE {
        md.push_str(&format!("| {} | `{}` | {} |\n", s.nr, s.name, s.args));
    }
    md.push_str(&format!("\n合計 {} 個\n", kernel::syscall::TABLE.len()));
    md
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

/// 起動後にわざと異常を起こす feature(`<名前>-demo`)。報告の形をスナップショットで固定する。
pub const DEMOS: &[&str] = &["panic", "fault"];

/// 各 BSP で「通常の起動」と、各デモ(パニック、未処理の例外)の UART 出力を確かめる。
fn snapshot_tests() -> Vec<Step> {
    Bsp::ALL
        .iter()
        .flat_map(|&bsp| {
            let snapshot = |suffix: &str, exit_code| Step::Snapshot {
                expected: format!("tests/snapshots/{}{suffix}.txt", bsp.name()),
                exit_code,
                cmd: bsp.qemu(&bsp.kernel_elf()).with_timeout(30),
            };
            let mut steps = vec![Step::Run(kernel_build(bsp)), snapshot("", 0)];
            for demo in DEMOS {
                let mut build = kernel_build(bsp);
                build
                    .args
                    .extend(["--features".into(), format!("{demo}-demo")]);
                steps.push(Step::Run(build));
                steps.push(snapshot(&format!("-{demo}"), 1));
            }
            steps
        })
        .chain(hardware_image())
        .chain([Step::Snapshot {
            // 実機用のイメージを、実機と同じくセミホスティングなしで起動する。
            // 終われずに止まるので、timeout の終了コード 124 が正しい結果
            expected: "tests/snapshots/raspi3-hw.txt".to_string(),
            exit_code: 124,
            cmd: Cmd::new(
                "qemu-system-aarch64",
                &["-M", "raspi3b", "-nographic", "-kernel", KERNEL8_IMG],
            )
            .with_timeout(10),
        }])
        .collect()
}

fn target_builds() -> Vec<Step> {
    let mut steps: Vec<Step> = TARGET_CRATES
        .iter()
        .map(|krate| Step::Run(Cmd::cargo(&["build", "-p", krate, "--target", TARGET])))
        .collect();
    steps.extend(hardware_image());
    steps
}

/// 実機用の kernel8.img の置き場所
pub const KERNEL8_IMG: &str = "target/hw/kernel8.img";

/// 実機の Pi 3B 用のイメージを作る。
///
/// 実機にはセミホスティングがないので、`semihosting` feature を外し、
/// QEMU 用のビルドと混ざらないよう別の target ディレクトリでビルドする。
/// ファームウェアは ELF を読めないので、生のバイナリ(kernel8.img)にする。
fn hardware_image() -> Vec<Step> {
    let build = Cmd::cargo(&[
        "build",
        "-p",
        Bsp::Raspi3.crate_name(),
        "--target",
        TARGET,
        "--no-default-features",
        "--target-dir",
        "target/hw",
    ]);
    let elf = format!("target/hw/{TARGET}/debug/{}", Bsp::Raspi3.crate_name());
    let objcopy = Cmd::new("llvm-objcopy", &["-O", "binary", &elf, KERNEL8_IMG]);
    vec![Step::Run(build), Step::Run(objcopy)]
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

/// 期待する出力(パターン)と実際の出力を比べる。
///
/// パターンでは次が使える(アドレスのようにビルドごとに変わる値のため):
/// - 行の中の `{..}`:任意の文字列
/// - `...` だけの行:任意の行数(0 行も含む)
pub fn compare_snapshot(expected: &str, actual: &str) -> Result<(), String> {
    let expected = normalize_output(expected);
    let actual = normalize_output(actual);
    let patterns: Vec<&str> = expected.lines().collect();
    let lines: Vec<&str> = actual.lines().collect();
    if lines_match(&patterns, &lines) {
        return Ok(());
    }
    // 最初に食い違った行を探す(`...` より前の部分で)
    let line = patterns
        .iter()
        .take_while(|&&p| p != "...")
        .zip(lines.iter().map(Some).chain(std::iter::repeat(None)))
        .position(|(p, l)| !l.is_some_and(|l| line_matches(p, l)))
        .unwrap_or(patterns.iter().take_while(|&&p| p != "...").count());
    let hint = if patterns.contains(&"...") && patterns.get(line) == Some(&"...") {
        " (somewhere after a `...` line; compare the full output)"
    } else {
        ""
    };
    Err(format!(
        "snapshot mismatch at line {}{hint}\n  expected: {}\n  actual:   {}\n--- full output ---\n{actual}",
        line + 1,
        patterns.get(line).unwrap_or(&"<end of pattern>"),
        lines.get(line).unwrap_or(&"<end of output>"),
    ))
}

fn lines_match(patterns: &[&str], lines: &[&str]) -> bool {
    match patterns.split_first() {
        None => lines.is_empty(),
        Some((&"...", rest)) => (0..=lines.len()).any(|skip| lines_match(rest, &lines[skip..])),
        Some((pattern, rest)) => {
            !lines.is_empty() && line_matches(pattern, lines[0]) && lines_match(rest, &lines[1..])
        }
    }
}

fn line_matches(pattern: &str, line: &str) -> bool {
    let parts: Vec<&str> = pattern.split("{..}").collect();
    let (first, last) = (parts[0], parts[parts.len() - 1]);
    if parts.len() == 1 {
        return pattern == line;
    }
    let Some(mut rest) = line.strip_prefix(first) else {
        return false;
    };
    for middle in &parts[1..parts.len() - 1] {
        match rest.find(middle) {
            Some(i) => rest = &rest[i + middle.len()..],
            None => return false,
        }
    }
    rest.ends_with(last)
}

#[cfg(test)]
mod tests;
