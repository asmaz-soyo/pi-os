//! tmpfs を使うファイルのシステムコール(作成、削除、ディレクトリの一覧)のテスト。

use super::tests::{SCRATCH, call, err, get, open, process, put};
use super::*;
use crate::Errno;

const O_WRONLY: u64 = 0o1;
const O_RDWR: u64 = 0o2;
const O_CREAT: u64 = 0o100;
const O_EXCL: u64 = 0o200;
const O_TRUNC: u64 = 0o1000;
const O_DIRECTORY: u64 = 0o200000;
const AT_FDCWD: u64 = -100i64 as u64;
const AT_REMOVEDIR: u64 = 0x200;

fn path(p: &mut crate::process::Process, s: &[u8]) -> u64 {
    put(p, SCRATCH + 0x3800, s);
    put(p, SCRATCH + 0x3800 + s.len() as u64, b"\0");
    SCRATCH + 0x3800
}

/// getdents64 の結果から名前を取り出す
fn names(p: &mut crate::process::Process, fd: u64) -> std::vec::Vec<std::string::String> {
    let mut out = std::vec::Vec::new();
    loop {
        let n = call(p, nr::GETDENTS64, [fd, SCRATCH, 1024, 0, 0, 0]);
        assert!((n as i64) >= 0, "getdents64 failed: {}", n as i64);
        if n == 0 {
            return out;
        }
        let buf = get(p, SCRATCH, n as usize);
        let mut at = 0;
        while at < buf.len() {
            let reclen = u16::from_le_bytes([buf[at + 16], buf[at + 17]]) as usize;
            let name = &buf[at + 19..at + reclen];
            let end = name.iter().position(|&b| b == 0).unwrap();
            out.push(std::string::String::from_utf8(name[..end].to_vec()).unwrap());
            at += reclen;
        }
    }
}

#[test]
fn created_files_keep_what_was_written() {
    let (mut p, _) = process();
    let fd = open(&mut p, b"/tmp/note", O_CREAT | O_WRONLY);
    put(&mut p, SCRATCH, b"abc");
    assert_eq!(call(&mut p, nr::WRITE, [fd, SCRATCH, 3, 0, 0, 0]), 3);
    call(&mut p, nr::CLOSE, [fd, 0, 0, 0, 0, 0]);
    let fd = open(&mut p, b"/tmp/note", 0);
    assert_eq!(
        call(&mut p, nr::READ, [fd, SCRATCH + 0x100, 10, 0, 0, 0]),
        3
    );
    assert_eq!(get(&mut p, SCRATCH + 0x100, 3), b"abc");
}

#[test]
fn o_trunc_empties_and_o_excl_refuses_existing_files() {
    let (mut p, _) = process();
    let fd = open(&mut p, b"/tmp/f", O_CREAT | O_RDWR);
    put(&mut p, SCRATCH, b"hello");
    call(&mut p, nr::WRITE, [fd, SCRATCH, 5, 0, 0, 0]);
    assert_eq!(
        open(&mut p, b"/tmp/f", O_CREAT | O_EXCL | O_WRONLY),
        err(Errno::EEXIST)
    );
    let fd2 = open(&mut p, b"/tmp/f", O_WRONLY | O_TRUNC);
    assert_eq!(call(&mut p, nr::FSTAT, [fd2, SCRATCH, 0, 0, 0, 0]), 0);
    assert_eq!(
        u64::from_le_bytes(get(&mut p, SCRATCH + 48, 8).try_into().unwrap()),
        0
    );
}

#[test]
fn ftruncate_changes_the_size() {
    let (mut p, _) = process();
    let fd = open(&mut p, b"/tmp/f", O_CREAT | O_RDWR);
    assert_eq!(call(&mut p, nr::FTRUNCATE, [fd, 100, 0, 0, 0, 0]), 0);
    call(&mut p, nr::FSTAT, [fd, SCRATCH, 0, 0, 0, 0]);
    assert_eq!(
        u64::from_le_bytes(get(&mut p, SCRATCH + 48, 8).try_into().unwrap()),
        100
    );
    let ro = open(&mut p, b"/etc/motd", 0);
    assert_eq!(
        call(&mut p, nr::FTRUNCATE, [ro, 0, 0, 0, 0, 0]),
        err(Errno::EINVAL)
    );
}

#[test]
fn mkdir_and_getdents_list_directory_contents() {
    let (mut p, _) = process();
    let dir = path(&mut p, b"/tmp/d");
    assert_eq!(
        call(&mut p, nr::MKDIRAT, [AT_FDCWD, dir, 0o755, 0, 0, 0]),
        0
    );
    assert_eq!(
        call(&mut p, nr::MKDIRAT, [AT_FDCWD, dir, 0o755, 0, 0, 0]),
        err(Errno::EEXIST)
    );
    open(&mut p, b"/tmp/d/x", O_CREAT | O_WRONLY);
    let fd = open(&mut p, b"/tmp/d", O_DIRECTORY);
    assert_eq!(names(&mut p, fd), [".", "..", "x"]);
}

#[test]
fn getdents_on_the_root_shows_mount_points() {
    let (mut p, _) = process();
    let fd = open(&mut p, b"/", O_DIRECTORY);
    let n = names(&mut p, fd);
    for want in ["bin", "dev", "etc", "tmp"] {
        assert!(n.iter().any(|x| x == want), "{want} missing from {n:?}");
    }
}

#[test]
fn unlinkat_removes_files_and_directories() {
    let (mut p, _) = process();
    open(&mut p, b"/tmp/f", O_CREAT | O_WRONLY);
    let f = path(&mut p, b"/tmp/f");
    assert_eq!(call(&mut p, nr::UNLINKAT, [AT_FDCWD, f, 0, 0, 0, 0]), 0);
    assert_eq!(open(&mut p, b"/tmp/f", 0), err(Errno::ENOENT));
    let d = path(&mut p, b"/tmp/d");
    call(&mut p, nr::MKDIRAT, [AT_FDCWD, d, 0o755, 0, 0, 0]);
    assert_eq!(
        call(&mut p, nr::UNLINKAT, [AT_FDCWD, d, 0, 0, 0, 0]),
        err(Errno::EISDIR)
    );
    assert_eq!(
        call(&mut p, nr::UNLINKAT, [AT_FDCWD, d, AT_REMOVEDIR, 0, 0, 0]),
        0
    );
}

#[test]
fn the_initramfs_stays_read_only() {
    let (mut p, _) = process();
    assert_eq!(
        open(&mut p, b"/etc/new", O_CREAT | O_WRONLY),
        err(Errno::EROFS)
    );
    let f = path(&mut p, b"/etc/motd");
    assert_eq!(
        call(&mut p, nr::UNLINKAT, [AT_FDCWD, f, 0, 0, 0, 0]),
        err(Errno::EROFS)
    );
}

#[test]
fn faccessat_checks_existence_and_permissions() {
    let (mut p, _) = process();
    let motd = path(&mut p, b"/etc/motd");
    assert_eq!(
        call(&mut p, nr::FACCESSAT, [AT_FDCWD, motd, 0, 0, 0, 0]),
        0,
        "F_OK"
    );
    assert_eq!(
        call(&mut p, nr::FACCESSAT, [AT_FDCWD, motd, 4, 0, 0, 0]),
        0,
        "R_OK"
    );
    assert_eq!(
        call(&mut p, nr::FACCESSAT, [AT_FDCWD, motd, 1, 0, 0, 0]),
        err(Errno::EACCES),
        "X_OK"
    );
    let nope = path(&mut p, b"/nope");
    assert_eq!(
        call(&mut p, nr::FACCESSAT, [AT_FDCWD, nope, 0, 0, 0, 0]),
        err(Errno::ENOENT)
    );
}

#[test]
fn fcntl_duplicates_descriptors_and_reports_flags() {
    const F_DUPFD: u64 = 0;
    const F_GETFD: u64 = 1;
    const F_SETFD: u64 = 2;
    const F_GETFL: u64 = 3;
    let (mut p, _) = process();
    let fd = open(&mut p, b"/etc/motd", 0);
    assert_eq!(
        call(&mut p, nr::FCNTL, [fd, F_DUPFD, 10, 0, 0, 0]),
        10,
        "lowest free >= 10"
    );
    assert_eq!(call(&mut p, nr::FCNTL, [fd, F_SETFD, 1, 0, 0, 0]), 0);
    assert_eq!(
        call(&mut p, nr::FCNTL, [fd, F_GETFD, 0, 0, 0, 0]),
        1,
        "FD_CLOEXEC"
    );
    assert_eq!(
        call(&mut p, nr::FCNTL, [fd, F_GETFL, 0, 0, 0, 0]),
        0,
        "O_RDONLY"
    );
    assert_eq!(
        call(&mut p, nr::FCNTL, [99, F_GETFD, 0, 0, 0, 0]),
        err(Errno::EBADF)
    );
}

#[test]
fn dup3_places_a_copy_at_the_given_number() {
    let (mut p, _) = process();
    let fd = open(&mut p, b"/etc/motd", 0);
    assert_eq!(call(&mut p, nr::DUP3, [fd, 7, 0, 0, 0, 0]), 7);
    assert_eq!(call(&mut p, nr::READ, [7, SCRATCH, 7, 0, 0, 0]), 7);
    assert_eq!(call(&mut p, nr::DUP, [fd, 0, 0, 0, 0, 0]), 4);
}
