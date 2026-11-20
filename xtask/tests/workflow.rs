//! GitHub Actions のワークフローが「`cargo xtask ci` を呼ぶだけ」になっていることを確かめる。

use std::path::Path;

fn workflow() -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../.github/workflows/ci.yml");
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

#[test]
fn ci_workflow_runs_xtask_ci() {
    assert!(workflow().contains("run: cargo xtask ci"));
}

#[test]
fn ci_workflow_installs_qemu_for_integration_tests() {
    assert!(workflow().contains("qemu-system-arm"));
}

#[test]
fn ci_workflow_cancels_superseded_runs() {
    assert!(workflow().contains("cancel-in-progress: true"));
}

#[test]
fn ci_workflow_installs_zig_for_user_programs() {
    let wf = workflow();
    assert!(wf.contains("setup-zig"), "zig builds the user programs");
    assert!(
        wf.contains("version: 0.17.1"),
        "same zig version as the dev PC"
    );
}
