//! ハードリンク(linkat)と、stat のリンク数のテスト。tmpfs(/tmp)で。

use super::tests::{SCRATCH, call, err, get, open, process, put};
use super::*;
use crate::Errno;
use crate::process::Process;

const AT_FDCWD: u64 = -100i64 as u64;
const O_WRONLY: u64 = 0o1;
const O_CREAT: u64 = 0o100;

fn link(p: &mut Process, old: &[u8], new: &[u8]) -> u64 {
    let (a, b) = (SCRATCH + 0x3000, SCRATCH + 0x3400);
    put(p, a, &[old, b"\0"].concat());
    put(p, b, &[new, b"\0"].concat());
    call(p, nr::LINKAT, [AT_FDCWD, a, AT_FDCWD, b, 0, 0])
}

/// (inode 番号, リンク数)
fn ino_nlink(p: &mut Process, path: &[u8]) -> (u64, u32) {
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
    (
        u64::from_le_bytes(b[8..16].try_into().unwrap()),
        u32::from_le_bytes(b[20..24].try_into().unwrap()),
    )
}

fn write_file(p: &mut Process, path: &[u8], data: &[u8]) {
    let fd = open(p, path, O_CREAT | O_WRONLY);
    put(p, SCRATCH, data);
    call(p, nr::WRITE, [fd, SCRATCH, data.len() as u64, 0, 0, 0]);
    call(p, nr::CLOSE, [fd, 0, 0, 0, 0, 0]);
}

fn read_file(p: &mut Process, path: &[u8]) -> std::vec::Vec<u8> {
    let fd = open(p, path, 0);
    let n = call(p, nr::READ, [fd, SCRATCH + 0x100, 64, 0, 0, 0]);
    get(p, SCRATCH + 0x100, n as usize)
}

fn unlink(p: &mut Process, path: &[u8]) -> u64 {
    put(p, SCRATCH + 0x3800, &[path, b"\0"].concat());
    call(p, nr::UNLINKAT, [AT_FDCWD, SCRATCH + 0x3800, 0, 0, 0, 0])
}

#[test]
fn a_hard_link_is_another_name_for_the_same_file() {
    let (mut p, _) = process();
    write_file(&mut p, b"/tmp/a", b"shared");
    assert_eq!(ino_nlink(&mut p, b"/tmp/a").1, 1);
    assert_eq!(link(&mut p, b"/tmp/a", b"/tmp/b"), 0);
    let (ia, na) = ino_nlink(&mut p, b"/tmp/a");
    let (ib, nb) = ino_nlink(&mut p, b"/tmp/b");
    assert_eq!((ia, na), (ib, nb));
    assert_eq!(na, 2);
    // 片方から書くと、もう片方からも見える
    write_file(&mut p, b"/tmp/b", b"SHARED");
    assert_eq!(read_file(&mut p, b"/tmp/a"), b"SHARED");
    // 片方を消しても、もう片方は残る
    assert_eq!(unlink(&mut p, b"/tmp/a"), 0);
    assert_eq!(read_file(&mut p, b"/tmp/b"), b"SHARED");
    assert_eq!(ino_nlink(&mut p, b"/tmp/b").1, 1);
}

#[test]
fn link_errors() {
    let (mut p, _) = process();
    write_file(&mut p, b"/tmp/a", b"x");
    write_file(&mut p, b"/tmp/b", b"y");
    put(&mut p, SCRATCH + 0x3800, b"/tmp/d\0");
    call(
        &mut p,
        nr::MKDIRAT,
        [AT_FDCWD, SCRATCH + 0x3800, 0o755, 0, 0, 0],
    );
    assert_eq!(link(&mut p, b"/tmp/a", b"/tmp/b"), err(Errno::EEXIST));
    assert_eq!(link(&mut p, b"/tmp/nope", b"/tmp/c"), err(Errno::ENOENT));
    assert_eq!(link(&mut p, b"/tmp/d", b"/tmp/d2"), err(Errno::EPERM));
    assert_eq!(link(&mut p, b"/etc/motd", b"/tmp/motd"), err(Errno::EXDEV));
    assert_eq!(link(&mut p, b"/tmp/a", b"/tmp/nodir/c"), err(Errno::ENOENT));
}
