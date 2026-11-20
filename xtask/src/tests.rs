use super::*;

fn displays(plan: &[Step]) -> Vec<String> {
    plan.iter().map(Step::to_string).collect()
}

fn build(krate: &str) -> String {
    format!("cargo build -p {krate} --target aarch64-unknown-none")
}

fn clippy(krate: &str) -> String {
    format!("cargo clippy -p {krate} --target aarch64-unknown-none -- -D warnings")
}

const ALL_TARGET: [&str; 9] = [
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
const EXCLUDES: &str = "--exclude userland --exclude boot --exclude ktest \
                        --exclude bsp-qemu-virt --exclude bsp-raspi3";
const VIRT_ARGS: &str = "-M virt -cpu cortex-a53 -m 128M -nic none -nographic -semihosting";
const RASPI3_ARGS: &str = "-M raspi3b -nographic -semihosting";
const KERNEL8_IMG: &str = "llvm-objcopy -O binary target/aarch64-unknown-none/debug/bsp-raspi3 \
    target/aarch64-unknown-none/debug/kernel8.img";

fn host_test() -> String {
    format!("cargo test --workspace {EXCLUDES}")
}

fn qemu_test(krate: &str, args: &str) -> String {
    format!(
        "CARGO_TARGET_AARCH64_UNKNOWN_NONE_RUNNER='timeout 60 qemu-system-aarch64 {args} -kernel' \
         cargo test -p {krate} --target aarch64-unknown-none"
    )
}

fn snapshot(name: &str, exit: i32, krate: &str, args: &str) -> String {
    format!(
        "snapshot tests/snapshots/{name}.txt (exit {exit}) <= timeout 30 qemu-system-aarch64 \
         {args} -kernel target/aarch64-unknown-none/debug/{krate}"
    )
}

fn build_demo(krate: &str, demo: &str) -> String {
    format!("cargo build -p {krate} --target aarch64-unknown-none --features {demo}-demo")
}

fn all_builds() -> Vec<String> {
    let mut v: Vec<String> = ALL_TARGET.iter().map(|k| build(k)).collect();
    v.push(KERNEL8_IMG.to_string());
    v
}

// ---- parse ----

#[test]
fn parses_each_subcommand() {
    assert_eq!(parse(&["build"]), Ok(Task::Build));
    assert_eq!(parse(&["test"]), Ok(Task::Test(TestKind::Host)));
    assert_eq!(parse(&["ci"]), Ok(Task::Ci));
}

#[test]
fn run_defaults_to_qemu_virt() {
    assert_eq!(
        parse(&["run"]),
        Ok(Task::Run {
            bsp: Bsp::QemuVirt,
            trace: false
        })
    );
    assert_eq!(
        parse(&["run", "--bsp", "qemu-virt"]),
        Ok(Task::Run {
            bsp: Bsp::QemuVirt,
            trace: false
        })
    );
}

#[test]
fn parses_qemu_test_with_bsp() {
    assert_eq!(
        parse(&["test", "--bsp", "qemu-virt"]),
        Ok(Task::Test(TestKind::Qemu(Bsp::QemuVirt)))
    );
}

#[test]
fn parses_snapshot_test() {
    assert_eq!(
        parse(&["test", "--snapshot"]),
        Ok(Task::Test(TestKind::Snapshot))
    );
}

#[test]
fn parses_raspi3_bsp() {
    assert_eq!(
        parse(&["run", "--bsp", "raspi3"]),
        Ok(Task::Run {
            bsp: Bsp::Raspi3,
            trace: false
        })
    );
    assert_eq!(
        parse(&["test", "--bsp", "raspi3"]),
        Ok(Task::Test(TestKind::Qemu(Bsp::Raspi3)))
    );
}

#[test]
fn rejects_unknown_bsp() {
    assert!(parse(&["test", "--bsp", "pc"]).is_err());
    assert!(parse(&["test", "--bsp"]).is_err());
}

#[test]
fn rejects_missing_unknown_or_extra_arguments() {
    assert!(parse(&[]).is_err());
    assert!(parse(&["deploy"]).is_err());
    assert!(parse(&["ci", "--now"]).is_err());
}

// ---- plan ----

#[test]
fn build_plan_builds_target_crates_and_raspi3_image() {
    assert_eq!(displays(&plan(Task::Build)), all_builds());
}

#[test]
fn host_test_plan_skips_target_only_crates() {
    assert_eq!(displays(&plan(Task::Test(TestKind::Host))), [host_test()]);
}

#[test]
fn qemu_test_plan_uses_qemu_as_cargo_runner() {
    assert_eq!(
        displays(&plan(Task::Test(TestKind::Qemu(Bsp::QemuVirt)))),
        [qemu_test("bsp-qemu-virt", VIRT_ARGS)]
    );
    assert_eq!(
        displays(&plan(Task::Test(TestKind::Qemu(Bsp::Raspi3)))),
        [qemu_test("bsp-raspi3", RASPI3_ARGS)]
    );
}

#[test]
fn qemu_runner_ends_with_kernel_flag_so_cargo_appends_the_elf() {
    for bsp in Bsp::ALL {
        assert!(bsp.qemu_runner().ends_with(" -kernel"));
    }
}

#[test]
fn snapshot_plan_checks_boot_panic_and_fault_output_for_each_bsp() {
    assert_eq!(
        displays(&plan(Task::Test(TestKind::Snapshot))),
        [
            build("bsp-qemu-virt"),
            snapshot("qemu-virt", 0, "bsp-qemu-virt", VIRT_ARGS),
            build_demo("bsp-qemu-virt", "panic"),
            snapshot("qemu-virt-panic", 1, "bsp-qemu-virt", VIRT_ARGS),
            build_demo("bsp-qemu-virt", "fault"),
            snapshot("qemu-virt-fault", 1, "bsp-qemu-virt", VIRT_ARGS),
            build("bsp-raspi3"),
            snapshot("raspi3", 0, "bsp-raspi3", RASPI3_ARGS),
            build_demo("bsp-raspi3", "panic"),
            snapshot("raspi3-panic", 1, "bsp-raspi3", RASPI3_ARGS),
            build_demo("bsp-raspi3", "fault"),
            snapshot("raspi3-fault", 1, "bsp-raspi3", RASPI3_ARGS),
        ]
    );
}

#[test]
fn run_plan_builds_then_boots_interactively() {
    assert_eq!(
        displays(&plan(Task::Run {
            bsp: Bsp::Raspi3,
            trace: false
        })),
        [
            build("bsp-raspi3"),
            format!(
                "qemu-system-aarch64 {RASPI3_ARGS} -kernel target/aarch64-unknown-none/debug/bsp-raspi3"
            ),
        ]
    );
}

#[test]
fn ci_plan_runs_checks_then_all_test_layers_then_builds() {
    let mut want = vec![
        "generate docs/generated/syscalls.md (check)".to_string(),
        "cargo fmt --all --check".to_string(),
        format!("cargo clippy --workspace {EXCLUDES} --all-targets --all-features -- -D warnings"),
    ];
    want.extend(ALL_TARGET.iter().map(|k| clippy(k)));
    want.push(host_test());
    want.push(qemu_test("bsp-qemu-virt", VIRT_ARGS));
    want.push(qemu_test("bsp-raspi3", RASPI3_ARGS));
    want.extend(displays(&plan(Task::Test(TestKind::Snapshot))));
    want.extend(all_builds());
    assert_eq!(displays(&plan(Task::Ci)), want);
}

// ---- snapshot comparison ----

#[test]
fn normalize_removes_carriage_returns_and_trailing_blank_lines() {
    assert_eq!(normalize_output("Hello\r\nworld\r\n\r\n"), "Hello\nworld\n");
}

#[test]
fn compare_accepts_identical_output() {
    assert!(compare_snapshot("Hello\n", "Hello\r\n").is_ok());
}

#[test]
fn compare_reports_first_differing_line() {
    let err = compare_snapshot("Hello\nworld\n", "Hello\nWORLD\n").unwrap_err();
    assert!(err.contains("line 2"), "{err}");
    assert!(err.contains("world") && err.contains("WORLD"), "{err}");
}

#[test]
fn compare_reports_missing_output() {
    assert!(compare_snapshot("Hello\n", "").is_err());
}

#[test]
fn wildcard_matches_any_text_within_a_line() {
    assert!(compare_snapshot("SP = {..}  FP = {..}\n", "SP = 0x10  FP = 0x20\n").is_ok());
    assert!(compare_snapshot("SP = {..}\n", "FP = 0x10\n").is_err());
}

#[test]
fn ellipsis_line_matches_any_number_of_lines() {
    let expected = "backtrace:\n...\nend\n";
    assert!(compare_snapshot(expected, "backtrace:\nend\n").is_ok());
    assert!(compare_snapshot(expected, "backtrace:\n#0\n#1\nend\n").is_ok());
    assert!(compare_snapshot(expected, "backtrace:\n#0\n").is_err());
}

#[test]
fn literal_braces_without_dots_must_match_exactly() {
    assert!(compare_snapshot("a{b}\n", "a{b}\n").is_ok());
    assert!(compare_snapshot("a{b}\n", "axb\n").is_err());
}

#[test]
fn run_with_syscall_trace_builds_the_trace_feature() {
    assert_eq!(
        parse(&["run", "--trace", "syscall"]),
        Ok(Task::Run {
            bsp: Bsp::QemuVirt,
            trace: true
        })
    );
    assert!(parse(&["run", "--trace", "everything"]).is_err());
    let steps = displays(&plan(Task::Run {
        bsp: Bsp::QemuVirt,
        trace: true,
    }));
    assert_eq!(
        steps[0],
        "cargo build -p bsp-qemu-virt --target aarch64-unknown-none --features trace-syscalls"
    );
}

#[test]
fn docs_task_generates_and_checks_the_syscall_table() {
    assert_eq!(parse(&["docs"]), Ok(Task::Docs { check: false }));
    assert_eq!(parse(&["docs", "--check"]), Ok(Task::Docs { check: true }));
    assert_eq!(
        displays(&plan(Task::Docs { check: true })),
        ["generate docs/generated/syscalls.md (check)"]
    );
}

#[test]
fn generated_syscall_table_lists_every_syscall() {
    let md = syscall_table_markdown();
    assert!(md.starts_with("<!-- cargo xtask docs で生成。手で編集しない -->"));
    assert!(md.contains("| 56 | `openat` | 4 |"));
    assert!(md.contains("| 222 | `mmap` | 6 |"));
}

#[test]
fn ci_checks_generated_docs_first() {
    assert_eq!(
        displays(&plan(Task::Ci))[0],
        "generate docs/generated/syscalls.md (check)"
    );
}
