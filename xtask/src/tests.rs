use super::*;

fn displays(plan: &[Step]) -> Vec<String> {
    plan.iter().map(Step::to_string).collect()
}

const HOST_TEST: &str = "cargo test --workspace --exclude arch-aarch64 --exclude bsp-qemu-virt";
const QEMU_VIRT_TEST: &str = "CARGO_TARGET_AARCH64_UNKNOWN_NONE_RUNNER='timeout 60 \
    qemu-system-aarch64 -M virt -cpu cortex-a53 -m 128M -nographic -semihosting -kernel' \
    cargo test -p bsp-qemu-virt --target aarch64-unknown-none";
const QEMU_VIRT_BUILD: &str = "cargo build -p bsp-qemu-virt --target aarch64-unknown-none";
const QEMU_VIRT_SNAPSHOT: &str = "snapshot tests/snapshots/qemu-virt.txt <= timeout 30 \
    qemu-system-aarch64 -M virt -cpu cortex-a53 -m 128M -nographic -semihosting \
    -kernel target/aarch64-unknown-none/debug/bsp-qemu-virt";

// ---- parse ----

#[test]
fn parses_each_subcommand() {
    assert_eq!(parse(&["build"]), Ok(Task::Build));
    assert_eq!(parse(&["test"]), Ok(Task::Test(TestKind::Host)));
    assert_eq!(parse(&["ci"]), Ok(Task::Ci));
}

#[test]
fn run_defaults_to_qemu_virt() {
    assert_eq!(parse(&["run"]), Ok(Task::Run(Bsp::QemuVirt)));
    assert_eq!(
        parse(&["run", "--bsp", "qemu-virt"]),
        Ok(Task::Run(Bsp::QemuVirt))
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
fn build_plan_builds_target_crates_for_aarch64() {
    assert_eq!(
        displays(&plan(Task::Build)),
        [
            "cargo build -p hal --target aarch64-unknown-none",
            "cargo build -p arch-aarch64 --target aarch64-unknown-none",
            QEMU_VIRT_BUILD,
        ]
    );
}

#[test]
fn host_test_plan_skips_target_only_crates() {
    assert_eq!(displays(&plan(Task::Test(TestKind::Host))), [HOST_TEST]);
}

#[test]
fn qemu_test_plan_uses_qemu_as_cargo_runner() {
    assert_eq!(
        displays(&plan(Task::Test(TestKind::Qemu(Bsp::QemuVirt)))),
        [QEMU_VIRT_TEST]
    );
}

#[test]
fn qemu_runner_ends_with_kernel_flag_so_cargo_appends_the_elf() {
    assert!(Bsp::QemuVirt.qemu_runner().ends_with(" -kernel"));
}

#[test]
fn snapshot_plan_builds_then_compares_uart_output() {
    assert_eq!(
        displays(&plan(Task::Test(TestKind::Snapshot))),
        [QEMU_VIRT_BUILD, QEMU_VIRT_SNAPSHOT]
    );
}

#[test]
fn run_plan_builds_then_boots_interactively() {
    assert_eq!(
        displays(&plan(Task::Run(Bsp::QemuVirt))),
        [
            QEMU_VIRT_BUILD,
            "qemu-system-aarch64 -M virt -cpu cortex-a53 -m 128M -nographic -semihosting \
             -kernel target/aarch64-unknown-none/debug/bsp-qemu-virt",
        ]
    );
}

#[test]
fn ci_plan_runs_checks_then_all_test_layers_then_builds() {
    assert_eq!(
        displays(&plan(Task::Ci)),
        [
            "cargo fmt --all --check",
            "cargo clippy --workspace --exclude arch-aarch64 --exclude bsp-qemu-virt \
             --all-targets --all-features -- -D warnings",
            "cargo clippy -p hal --target aarch64-unknown-none -- -D warnings",
            "cargo clippy -p arch-aarch64 --target aarch64-unknown-none -- -D warnings",
            "cargo clippy -p bsp-qemu-virt --target aarch64-unknown-none -- -D warnings",
            HOST_TEST,
            QEMU_VIRT_TEST,
            QEMU_VIRT_BUILD,
            QEMU_VIRT_SNAPSHOT,
            "cargo build -p hal --target aarch64-unknown-none",
            "cargo build -p arch-aarch64 --target aarch64-unknown-none",
            QEMU_VIRT_BUILD,
        ]
    );
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
