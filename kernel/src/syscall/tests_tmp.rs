//! tmpfs を使うファイルのシステムコール(作成、削除、ディレクトリの一覧)のテスト。

use super::tests::{SCRATCH, call, err, get, open, process, put};
use super::*;
use crate::Errno;

const O_WRONLY: u64 = 0o1;
const O_RDWR: u64 = 0o2;
const O_CREAT: u64 = 0o100;
const O_EXCL: u64 = 0o200;
const O_TRUNC: u64 = 0o1000;
// AArch64 の値(x86 の 0o200000 とは違う)
const O_DIRECTORY: u64 = 0o40000;
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

#[test]
fn pread_and_pwrite_use_their_own_offset() {
    let (mut p, _) = process();
    let fd = open(&mut p, b"/tmp/f", O_CREAT | O_RDWR);
    put(&mut p, SCRATCH, b"0123456789");
    assert_eq!(call(&mut p, nr::WRITE, [fd, SCRATCH, 10, 0, 0, 0]), 10);
    put(&mut p, SCRATCH, b"AB");
    assert_eq!(call(&mut p, nr::PWRITE64, [fd, SCRATCH, 2, 3, 0, 0]), 2);
    assert_eq!(
        call(&mut p, nr::PREAD64, [fd, SCRATCH + 0x100, 4, 2, 0, 0]),
        4
    );
    assert_eq!(get(&mut p, SCRATCH + 0x100, 4), b"2AB5");
    // ファイルの位置は動かない(書いたあとの 10 のまま)
    assert_eq!(call(&mut p, nr::LSEEK, [fd, 0, 1, 0, 0, 0]), 10);
}

#[test]
fn dirfd_is_read_as_a_32_bit_int() {
    // int の AT_FDCWD(-100)は、上位 32 ビットが 0 のまま届くことがある(musl の syscall() 経由)
    let (mut p, _) = process();
    let at_fdcwd_zero_extended = 0xffff_ff9c_u64;
    p.cwd = alloc::string::String::from("/tmp");
    put(&mut p, SCRATCH, b"sub\0");
    assert_eq!(
        call(
            &mut p,
            nr::MKDIRAT,
            [at_fdcwd_zero_extended, SCRATCH, 0o755, 0, 0, 0]
        ),
        0
    );
    const AT_REMOVEDIR: u64 = 0x200;
    assert_eq!(
        call(
            &mut p,
            nr::UNLINKAT,
            [at_fdcwd_zero_extended, SCRATCH, AT_REMOVEDIR, 0, 0, 0]
        ),
        0
    );
}

#[test]
fn paths_can_be_relative_to_a_directory_fd() {
    let (mut p, _) = process();
    let dir = open(&mut p, b"/tmp", O_DIRECTORY);
    // mkdirat(dirfd, "sub")、openat(dirfd, "sub/x")
    let sub = path(&mut p, b"sub");
    assert_eq!(call(&mut p, nr::MKDIRAT, [dir, sub, 0o755, 0, 0, 0]), 0);
    let x = path(&mut p, b"sub/x");
    let fd = call(
        &mut p,
        nr::OPENAT,
        [dir, x, O_CREAT | O_WRONLY, 0o644, 0, 0],
    );
    assert!((fd as i64) >= 0);
    assert!(p.vfs.lookup("/tmp/sub/x").is_ok());
    // fstatat(dirfd, "sub/x")
    assert_eq!(call(&mut p, nr::NEWFSTATAT, [dir, x, SCRATCH, 0, 0, 0]), 0);
    // unlinkat(dirfd, "sub/x")
    assert_eq!(call(&mut p, nr::UNLINKAT, [dir, x, 0, 0, 0, 0]), 0);
    assert!(p.vfs.lookup("/tmp/sub/x").is_err());
    // 絶対パスなら dirfd は見ない
    let abs = path(&mut p, b"/etc/motd");
    assert!((call(&mut p, nr::OPENAT, [999, abs, 0, 0, 0, 0]) as i64) >= 0);
}

#[test]
fn a_bad_directory_fd_is_ebadf_or_enotdir() {
    let (mut p, _) = process();
    let file = open(&mut p, b"/etc/motd", 0);
    let rel = path(&mut p, b"x");
    assert_eq!(
        call(&mut p, nr::OPENAT, [file, rel, 0, 0, 0, 0]),
        err(Errno::ENOTDIR)
    );
    assert_eq!(
        call(&mut p, nr::OPENAT, [77, rel, 0, 0, 0, 0]),
        err(Errno::EBADF)
    );
}

#[test]
fn sync_and_fsync_succeed_on_open_files() {
    let (mut p, _) = process();
    let fd = open(&mut p, b"/tmp/s", O_CREAT | O_WRONLY);
    assert_eq!(call(&mut p, nr::SYNC, [0; 6]), 0);
    assert_eq!(call(&mut p, nr::FSYNC, [fd, 0, 0, 0, 0, 0]), 0);
    assert_eq!(call(&mut p, nr::FDATASYNC, [fd, 0, 0, 0, 0, 0]), 0);
    assert_eq!(
        call(&mut p, nr::FSYNC, [99, 0, 0, 0, 0, 0]),
        err(Errno::EBADF)
    );
}

const O_APPEND: u64 = 0o2000;
const F_GETFL: u64 = 3;
const F_SETFL: u64 = 4;

fn write_str(p: &mut crate::process::Process, fd: u64, s: &[u8]) {
    put(p, SCRATCH, s);
    assert_eq!(
        call(p, nr::WRITE, [fd, SCRATCH, s.len() as u64, 0, 0, 0]),
        s.len() as u64
    );
}

fn contents(p: &mut crate::process::Process, path_: &[u8]) -> std::vec::Vec<u8> {
    let fd = open(p, path_, 0);
    let n = call(p, nr::READ, [fd, SCRATCH + 0x100, 256, 0, 0, 0]);
    get(p, SCRATCH + 0x100, n as usize)
}

#[test]
fn o_append_writes_always_go_to_the_end() {
    let (mut p, _) = process();
    let fd = open(&mut p, b"/tmp/log", O_CREAT | O_WRONLY);
    write_str(&mut p, fd, b"first\n");
    let fd = open(&mut p, b"/tmp/log", O_WRONLY | O_APPEND);
    write_str(&mut p, fd, b"second\n");
    // 先頭に戻しても、書くときは末尾
    call(&mut p, nr::LSEEK, [fd, 0, 0, 0, 0, 0]);
    write_str(&mut p, fd, b"third\n");
    assert_eq!(contents(&mut p, b"/tmp/log"), b"first\nsecond\nthird\n");
    // 別の fd が途中で伸ばしても、その後ろに書く
    let other = open(&mut p, b"/tmp/log", O_WRONLY | O_APPEND);
    write_str(&mut p, other, b"4\n");
    write_str(&mut p, fd, b"5\n");
    assert_eq!(
        contents(&mut p, b"/tmp/log"),
        b"first\nsecond\nthird\n4\n5\n"
    );
}

#[test]
fn fcntl_reports_and_changes_o_append() {
    let (mut p, _) = process();
    let fd = open(&mut p, b"/tmp/f", O_CREAT | O_WRONLY | O_APPEND);
    assert_eq!(
        call(&mut p, nr::FCNTL, [fd, F_GETFL, 0, 0, 0, 0]) & O_APPEND,
        O_APPEND
    );
    assert_eq!(call(&mut p, nr::FCNTL, [fd, F_SETFL, 0, 0, 0, 0]), 0);
    assert_eq!(
        call(&mut p, nr::FCNTL, [fd, F_GETFL, 0, 0, 0, 0]) & O_APPEND,
        0
    );
    write_str(&mut p, fd, b"abc");
    call(&mut p, nr::LSEEK, [fd, 0, 0, 0, 0, 0]);
    write_str(&mut p, fd, b"X");
    assert_eq!(
        contents(&mut p, b"/tmp/f"),
        b"Xbc",
        "without O_APPEND, writes go to the offset"
    );
}

/// statfs の結果:(種類, ブロックの大きさ, ファイル名の最大長)
fn statfs_of(p: &mut crate::process::Process, path_: &[u8]) -> Result<(u64, u64, u64), u64> {
    let at = path(p, path_);
    let r = call(p, nr::STATFS, [at, SCRATCH + 0x800, 0, 0, 0, 0]);
    if r != 0 {
        return Err(r);
    }
    let b = get(p, SCRATCH + 0x800, 120);
    let w = |i: usize| u64::from_le_bytes(b[i..i + 8].try_into().unwrap());
    Ok((w(0), w(8), w(64)))
}

#[test]
fn statfs_names_the_filesystem_type() {
    const TMPFS_MAGIC: u64 = 0x0102_1994;
    const RAMFS_MAGIC: u64 = 0x8584_58f6;
    let (mut p, _) = process();
    assert_eq!(statfs_of(&mut p, b"/tmp").unwrap().0, TMPFS_MAGIC);
    let (kind, bsize, namelen) = statfs_of(&mut p, b"/etc/motd").unwrap();
    assert_eq!(kind, RAMFS_MAGIC);
    assert!(bsize > 0);
    assert_eq!(namelen, 255);
    let fd = open(&mut p, b"/tmp", O_DIRECTORY);
    assert_eq!(
        call(&mut p, nr::FSTATFS, [fd, SCRATCH + 0x800, 0, 0, 0, 0]),
        0
    );
    assert_eq!(statfs_of(&mut p, b"/nope"), Err(err(Errno::ENOENT)));
}

fn st_dev(p: &mut crate::process::Process, path_: &[u8]) -> u64 {
    let at = path(p, path_);
    assert_eq!(
        call(p, nr::NEWFSTATAT, [AT_FDCWD, at, SCRATCH + 0x400, 0, 0, 0]),
        0
    );
    u64::from_le_bytes(get(p, SCRATCH + 0x400, 8).try_into().unwrap())
}

#[test]
fn each_mounted_filesystem_has_its_own_st_dev() {
    // df や find -xdev は、st_dev でどのファイルシステムかを見分ける
    let (mut p, _) = process();
    open(&mut p, b"/tmp/x", O_CREAT | O_WRONLY);
    assert_eq!(st_dev(&mut p, b"/tmp"), st_dev(&mut p, b"/tmp/x"));
    assert_ne!(st_dev(&mut p, b"/tmp"), st_dev(&mut p, b"/etc"));
    assert_ne!(st_dev(&mut p, b"/dev"), st_dev(&mut p, b"/"));
    assert_eq!(st_dev(&mut p, b"/etc/motd"), st_dev(&mut p, b"/"));
    // fstat も同じ
    let fd = open(&mut p, b"/tmp/x", 0);
    assert_eq!(
        call(&mut p, nr::FSTAT, [fd, SCRATCH + 0x400, 0, 0, 0, 0]),
        0
    );
    let dev = u64::from_le_bytes(get(&mut p, SCRATCH + 0x400, 8).try_into().unwrap());
    assert_eq!(dev, st_dev(&mut p, b"/tmp"));
}
