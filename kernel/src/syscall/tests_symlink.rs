//! シンボリックリンク(symlinkat、readlinkat、lstat、O_NOFOLLOW、パスの途中のリンク)のテスト。tmpfs(/tmp)で。

use super::tests::{SCRATCH, call, err, get, open, process, put};
use super::*;
use crate::Errno;
use crate::process::Process;

const AT_FDCWD: u64 = -100i64 as u64;
const O_WRONLY: u64 = 0o1;
const O_CREAT: u64 = 0o100;
/// AArch64 の値(x86 の 0o400000 とは違う)
const O_NOFOLLOW: u64 = 0o100000;
const AT_SYMLINK_NOFOLLOW: u64 = 0x100;
const S_IFMT: u32 = 0o170000;
const S_IFLNK: u32 = 0o120000;
const S_IFREG: u32 = 0o100000;

fn cstr(p: &mut Process, at: u64, s: &[u8]) -> u64 {
    put(p, at, &[s, b"\0"].concat());
    at
}

fn symlink(p: &mut Process, target: &[u8], link: &[u8]) -> u64 {
    let t = cstr(p, SCRATCH + 0x3000, target);
    let l = cstr(p, SCRATCH + 0x3400, link);
    call(p, nr::SYMLINKAT, [t, AT_FDCWD, l, 0, 0, 0])
}

fn readlink(p: &mut Process, link: &[u8], len: u64) -> Result<std::vec::Vec<u8>, u64> {
    let l = cstr(p, SCRATCH + 0x3400, link);
    let n = call(p, nr::READLINKAT, [AT_FDCWD, l, SCRATCH + 0x200, len, 0, 0]);
    if (n as i64) < 0 {
        return Err(n);
    }
    Ok(get(p, SCRATCH + 0x200, n as usize))
}

fn read_file(p: &mut Process, path: &[u8]) -> std::vec::Vec<u8> {
    let fd = open(p, path, 0);
    let n = call(p, nr::READ, [fd, SCRATCH + 0x100, 64, 0, 0, 0]);
    get(p, SCRATCH + 0x100, n as usize)
}

fn open_flags(p: &mut Process, path: &[u8], flags: u64) -> u64 {
    let at = cstr(p, SCRATCH + 0x3800, path);
    call(p, nr::OPENAT, [AT_FDCWD, at, flags, 0o644, 0, 0])
}

/// stat(follow = true)か lstat(false)の st_mode と st_size
fn stat(p: &mut Process, path: &[u8], follow: bool) -> Result<(u32, u64), u64> {
    let at = cstr(p, SCRATCH + 0x3800, path);
    let flags = if follow { 0 } else { AT_SYMLINK_NOFOLLOW };
    let r = call(
        p,
        nr::NEWFSTATAT,
        [AT_FDCWD, at, SCRATCH + 0x400, flags, 0, 0],
    );
    if r != 0 {
        return Err(r);
    }
    let b = get(p, SCRATCH + 0x400, 128);
    let mode = u32::from_le_bytes(b[16..20].try_into().unwrap());
    let size = u64::from_le_bytes(b[48..56].try_into().unwrap());
    Ok((mode, size))
}

fn write_file(p: &mut Process, path: &[u8], data: &[u8]) {
    let fd = open(p, path, O_CREAT | O_WRONLY);
    put(p, SCRATCH, data);
    call(p, nr::WRITE, [fd, SCRATCH, data.len() as u64, 0, 0, 0]);
    call(p, nr::CLOSE, [fd, 0, 0, 0, 0, 0]);
}

#[test]
fn a_symlink_stores_its_target_and_readlink_returns_it() {
    let (mut p, _) = process();
    assert_eq!(symlink(&mut p, b"some/where", b"/tmp/l"), 0);
    assert_eq!(readlink(&mut p, b"/tmp/l", 64), Ok(b"some/where".to_vec()));
    // バッファが小さければ切り詰める(NUL は付けない)
    assert_eq!(readlink(&mut p, b"/tmp/l", 4), Ok(b"some".to_vec()));
    assert_eq!(symlink(&mut p, b"x", b"/tmp/l"), err(Errno::EEXIST));
    // リンクでないものは EINVAL
    assert_eq!(readlink(&mut p, b"/etc/motd", 64), Err(err(Errno::EINVAL)));
}

#[test]
fn opening_a_symlink_opens_its_target() {
    let (mut p, _) = process();
    write_file(&mut p, b"/tmp/f", b"target data");
    // 相対のリンクは、リンクのあるディレクトリから
    symlink(&mut p, b"f", b"/tmp/rel");
    assert_eq!(read_file(&mut p, b"/tmp/rel"), b"target data");
    // 絶対のリンクは、マウントをまたいでもよい
    symlink(&mut p, b"/etc/motd", b"/tmp/abs");
    assert_eq!(read_file(&mut p, b"/tmp/abs"), b"Welcome to Pi-OS!\n");
    // リンクのリンク
    symlink(&mut p, b"rel", b"/tmp/rel2");
    assert_eq!(read_file(&mut p, b"/tmp/rel2"), b"target data");
}

#[test]
fn symlinks_in_the_middle_of_a_path_are_followed() {
    let (mut p, _) = process();
    symlink(&mut p, b"/etc", b"/tmp/etc");
    assert_eq!(read_file(&mut p, b"/tmp/etc/motd"), b"Welcome to Pi-OS!\n");
    // ".." はリンクをたどった後のディレクトリから(/tmp/etc/.. は /)
    assert_eq!(
        read_file(&mut p, b"/tmp/etc/../etc/motd"),
        b"Welcome to Pi-OS!\n"
    );
}

#[test]
fn lstat_sees_the_link_and_stat_sees_the_target() {
    let (mut p, _) = process();
    write_file(&mut p, b"/tmp/f", b"12345");
    symlink(&mut p, b"f", b"/tmp/l");
    let (mode, size) = stat(&mut p, b"/tmp/l", false).unwrap();
    assert_eq!(mode & S_IFMT, S_IFLNK);
    assert_eq!(size, 1, "a link's size is the length of its target");
    let (mode, size) = stat(&mut p, b"/tmp/l", true).unwrap();
    assert_eq!(mode & S_IFMT, S_IFREG);
    assert_eq!(size, 5);
}

#[test]
fn dangling_links_and_loops() {
    let (mut p, _) = process();
    symlink(&mut p, b"/tmp/nowhere", b"/tmp/dangling");
    assert_eq!(open_flags(&mut p, b"/tmp/dangling", 0), err(Errno::ENOENT));
    assert!(stat(&mut p, b"/tmp/dangling", false).is_ok());
    symlink(&mut p, b"b", b"/tmp/a");
    symlink(&mut p, b"a", b"/tmp/b");
    assert_eq!(open_flags(&mut p, b"/tmp/a", 0), err(Errno::ELOOP));
    assert_eq!(stat(&mut p, b"/tmp/a/x", true), Err(err(Errno::ELOOP)));
}

#[test]
fn o_nofollow_refuses_a_final_symlink() {
    let (mut p, _) = process();
    write_file(&mut p, b"/tmp/f", b"x");
    symlink(&mut p, b"f", b"/tmp/l");
    assert_eq!(open_flags(&mut p, b"/tmp/l", O_NOFOLLOW), err(Errno::ELOOP));
    assert!((open_flags(&mut p, b"/tmp/f", O_NOFOLLOW) as i64) >= 0);
}

#[test]
fn unlink_and_rename_act_on_the_link_itself() {
    let (mut p, _) = process();
    write_file(&mut p, b"/tmp/f", b"keep me");
    symlink(&mut p, b"f", b"/tmp/l");
    let (a, b) = (
        cstr(&mut p, SCRATCH + 0x3000, b"/tmp/l"),
        cstr(&mut p, SCRATCH + 0x3400, b"/tmp/m"),
    );
    assert_eq!(
        call(&mut p, nr::RENAMEAT, [AT_FDCWD, a, AT_FDCWD, b, 0, 0]),
        0
    );
    assert_eq!(readlink(&mut p, b"/tmp/m", 64), Ok(b"f".to_vec()));
    let m = cstr(&mut p, SCRATCH + 0x3400, b"/tmp/m");
    assert_eq!(call(&mut p, nr::UNLINKAT, [AT_FDCWD, m, 0, 0, 0, 0]), 0);
    assert_eq!(read_file(&mut p, b"/tmp/f"), b"keep me");
}

#[test]
fn creating_through_a_symlink_creates_the_target() {
    // O_CREAT でリンクを開くと、指す先にファイルができる
    let (mut p, _) = process();
    symlink(&mut p, b"made", b"/tmp/l");
    let fd = open_flags(&mut p, b"/tmp/l", O_CREAT | O_WRONLY);
    assert!((fd as i64) >= 0);
    assert!(p.vfs.lookup("/tmp/made").is_ok());
}
