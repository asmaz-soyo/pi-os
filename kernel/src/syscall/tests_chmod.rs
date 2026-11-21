//! chmod / chown(fchmodat、fchmod、fchownat、fchown)と、作ったファイルの持ち主のテスト。tmpfs(/tmp)で。

use super::tests::{SCRATCH, call, err, get, open, process, put};
use super::*;
use crate::Errno;
use crate::process::Process;

const AT_FDCWD: u64 = -100i64 as u64;
const O_WRONLY: u64 = 0o1;
const O_CREAT: u64 = 0o100;
const NO_CHANGE: u64 = u32::MAX as u64; // chown の -1:変えない

fn cstr(p: &mut Process, s: &[u8]) -> u64 {
    put(p, SCRATCH + 0x3800, &[s, b"\0"].concat());
    SCRATCH + 0x3800
}

/// (mode の下 12 ビット, uid, gid)
fn owner_and_mode(p: &mut Process, path: &[u8]) -> (u32, u32, u32) {
    let at = cstr(p, path);
    assert_eq!(
        call(p, nr::NEWFSTATAT, [AT_FDCWD, at, SCRATCH + 0x400, 0, 0, 0]),
        0
    );
    let b = get(p, SCRATCH + 0x400, 128);
    let w = |i: usize| u32::from_le_bytes(b[i..i + 4].try_into().unwrap());
    (w(16) & 0o7777, w(24), w(28))
}

fn chmod(p: &mut Process, path: &[u8], mode: u64) -> u64 {
    let at = cstr(p, path);
    call(p, nr::FCHMODAT, [AT_FDCWD, at, mode, 0, 0, 0])
}

fn chown(p: &mut Process, path: &[u8], uid: u64, gid: u64) -> u64 {
    let at = cstr(p, path);
    call(p, nr::FCHOWNAT, [AT_FDCWD, at, uid, gid, 0, 0])
}

fn create(p: &mut Process, path: &[u8]) -> u64 {
    open(p, path, O_CREAT | O_WRONLY)
}

#[test]
fn chmod_changes_the_permission_bits() {
    let (mut p, _) = process();
    let fd = create(&mut p, b"/tmp/f");
    assert_eq!(chmod(&mut p, b"/tmp/f", 0o600), 0);
    assert_eq!(owner_and_mode(&mut p, b"/tmp/f").0, 0o600);
    // fd でも。setuid などの上のビットも持てる
    assert_eq!(call(&mut p, nr::FCHMOD, [fd, 0o4755, 0, 0, 0, 0]), 0);
    assert_eq!(owner_and_mode(&mut p, b"/tmp/f").0, 0o4755);
    assert_eq!(chmod(&mut p, b"/tmp/missing", 0o600), err(Errno::ENOENT));
    // 読み取り専用のファイルシステム
    assert_eq!(chmod(&mut p, b"/etc/motd", 0o600), err(Errno::EROFS));
}

#[test]
fn files_belong_to_whoever_created_them() {
    let (mut p, _) = process();
    p.cred = crate::cred::Credentials::user(1000, 100);
    create(&mut p, b"/tmp/mine");
    assert_eq!(owner_and_mode(&mut p, b"/tmp/mine").1, 1000);
    assert_eq!(owner_and_mode(&mut p, b"/tmp/mine").2, 100);
    let d = cstr(&mut p, b"/tmp/dir");
    assert_eq!(call(&mut p, nr::MKDIRAT, [AT_FDCWD, d, 0o755, 0, 0, 0]), 0);
    assert_eq!(owner_and_mode(&mut p, b"/tmp/dir").1, 1000);
    // 自分のファイルなら chmod できる
    assert_eq!(chmod(&mut p, b"/tmp/mine", 0o600), 0);
}

#[test]
fn only_the_owner_or_root_may_chmod_and_only_root_may_chown() {
    let (mut p, _) = process();
    create(&mut p, b"/tmp/roots");
    p.cred = crate::cred::Credentials::user(1000, 100);
    assert_eq!(chmod(&mut p, b"/tmp/roots", 0o777), err(Errno::EPERM));
    assert_eq!(chown(&mut p, b"/tmp/roots", 1000, 100), err(Errno::EPERM));
    p.cred = crate::cred::Credentials::root();
    assert_eq!(chown(&mut p, b"/tmp/roots", 1000, 100), 0);
    assert_eq!(owner_and_mode(&mut p, b"/tmp/roots").1, 1000);
    // -1 は変えない
    assert_eq!(chown(&mut p, b"/tmp/roots", NO_CHANGE, 7), 0);
    let (_, uid, gid) = owner_and_mode(&mut p, b"/tmp/roots");
    assert_eq!((uid, gid), (1000, 7));
    // 持ち主になったので、一般ユーザーでも chmod できる
    p.cred = crate::cred::Credentials::user(1000, 100);
    assert_eq!(chmod(&mut p, b"/tmp/roots", 0o600), 0);
    let fd = open(&mut p, b"/tmp/roots", O_WRONLY);
    assert_eq!(
        call(&mut p, nr::FCHOWN, [fd, 0, 0, 0, 0, 0]),
        err(Errno::EPERM)
    );
}
