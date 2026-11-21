//! umask(作るときに消す権限のビット)のテスト。

use super::tests::{SCRATCH, call, get, process, put};
use super::*;
use crate::process::Process;

const AT_FDCWD: u64 = -100i64 as u64;
const O_WRONLY: u64 = 0o1;
const O_CREAT: u64 = 0o100;

fn mode_of(p: &mut Process, path: &[u8]) -> u32 {
    put(p, SCRATCH + 0x3800, &[path, b"\0"].concat());
    assert_eq!(
        call(
            p,
            nr::NEWFSTATAT,
            [AT_FDCWD, SCRATCH + 0x3800, SCRATCH + 0x400, 0, 0, 0]
        ),
        0
    );
    let b = get(p, SCRATCH + 0x400, 128);
    u32::from_le_bytes(b[16..20].try_into().unwrap()) & 0o7777
}

fn create(p: &mut Process, path: &[u8], mode: u64) {
    put(p, SCRATCH + 0x3800, &[path, b"\0"].concat());
    let fd = call(
        p,
        nr::OPENAT,
        [AT_FDCWD, SCRATCH + 0x3800, O_CREAT | O_WRONLY, mode, 0, 0],
    );
    assert!((fd as i64) >= 0);
}

fn mkdir(p: &mut Process, path: &[u8], mode: u64) {
    put(p, SCRATCH + 0x3800, &[path, b"\0"].concat());
    assert_eq!(
        call(p, nr::MKDIRAT, [AT_FDCWD, SCRATCH + 0x3800, mode, 0, 0, 0]),
        0
    );
}

#[test]
fn the_default_umask_removes_group_and_other_write() {
    let (mut p, _) = process();
    create(&mut p, b"/tmp/f", 0o666);
    assert_eq!(mode_of(&mut p, b"/tmp/f"), 0o644);
    mkdir(&mut p, b"/tmp/d", 0o777);
    assert_eq!(mode_of(&mut p, b"/tmp/d"), 0o755);
}

#[test]
fn umask_returns_the_old_mask_and_sets_a_new_one() {
    let (mut p, _) = process();
    assert_eq!(call(&mut p, nr::UMASK, [0o077, 0, 0, 0, 0, 0]), 0o022);
    assert_eq!(
        call(&mut p, nr::UMASK, [0o7027, 0, 0, 0, 0, 0]),
        0o077,
        "only 0o777 is kept"
    );
    assert_eq!(call(&mut p, nr::UMASK, [0o027, 0, 0, 0, 0, 0]), 0o027);
    create(&mut p, b"/tmp/f", 0o666);
    assert_eq!(mode_of(&mut p, b"/tmp/f"), 0o640);
}

#[test]
fn a_forked_child_inherits_the_umask() {
    let (mut p, _) = process();
    call(&mut p, nr::UMASK, [0o077, 0, 0, 0, 0, 0]);
    let child = p.fork().unwrap();
    assert_eq!(child.umask, 0o077);
}
