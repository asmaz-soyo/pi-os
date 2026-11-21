//! 名前付きパイプ(mknodat の S_IFIFO)と fchdir のテスト。tmpfs(/tmp)で。

use super::tests::{SCRATCH, call, err, get, process, put};
use super::*;
use crate::Errno;
use crate::process::Process;

const AT_FDCWD: u64 = -100i64 as u64;
const O_RDONLY: u64 = 0;
const O_WRONLY: u64 = 0o1;
const O_NONBLOCK: u64 = 0o4000;
const O_DIRECTORY: u64 = 0o40000;
const S_IFIFO: u64 = 0o010000;
const S_IFCHR: u64 = 0o020000;

fn cstr(p: &mut Process, s: &[u8]) -> u64 {
    put(p, SCRATCH + 0x3800, &[s, b"\0"].concat());
    SCRATCH + 0x3800
}

fn mknod(p: &mut Process, path: &[u8], mode: u64) -> u64 {
    let at = cstr(p, path);
    call(p, nr::MKNODAT, [AT_FDCWD, at, mode, 0, 0, 0])
}

fn open(p: &mut Process, path: &[u8], flags: u64) -> u64 {
    let at = cstr(p, path);
    call(p, nr::OPENAT, [AT_FDCWD, at, flags, 0, 0, 0])
}

#[test]
fn a_fifo_connects_whoever_opens_it() {
    let (mut p, _) = process();
    assert_eq!(mknod(&mut p, b"/tmp/q", S_IFIFO | 0o644), 0);
    let at = cstr(&mut p, b"/tmp/q");
    assert_eq!(
        call(
            &mut p,
            nr::NEWFSTATAT,
            [AT_FDCWD, at, SCRATCH + 0x400, 0, 0, 0]
        ),
        0
    );
    let mode = u32::from_le_bytes(get(&mut p, SCRATCH + 0x410, 4).try_into().unwrap());
    assert_eq!(mode as u64 & 0o170000, S_IFIFO);
    // 読む側を先に(O_NONBLOCK なら書く側を待たない)、次に書く側
    let r = open(&mut p, b"/tmp/q", O_RDONLY | O_NONBLOCK);
    assert!((r as i64) >= 0);
    let w = open(&mut p, b"/tmp/q", O_WRONLY);
    assert!((w as i64) >= 0);
    put(&mut p, SCRATCH, b"via fifo");
    assert_eq!(call(&mut p, nr::WRITE, [w, SCRATCH, 8, 0, 0, 0]), 8);
    assert_eq!(call(&mut p, nr::READ, [r, SCRATCH + 0x100, 64, 0, 0, 0]), 8);
    assert_eq!(get(&mut p, SCRATCH + 0x100, 8), b"via fifo");
    // 書く側がみんな閉じたら、読むと終わり(0)
    call(&mut p, nr::CLOSE, [w, 0, 0, 0, 0, 0]);
    assert_eq!(call(&mut p, nr::READ, [r, SCRATCH + 0x100, 64, 0, 0, 0]), 0);
}

#[test]
fn writing_to_a_fifo_nobody_reads_is_enxio_with_o_nonblock() {
    let (mut p, _) = process();
    mknod(&mut p, b"/tmp/q", S_IFIFO | 0o644);
    assert_eq!(
        open(&mut p, b"/tmp/q", O_WRONLY | O_NONBLOCK),
        err(Errno::ENXIO)
    );
}

#[test]
fn mknod_errors() {
    let (mut p, _) = process();
    mknod(&mut p, b"/tmp/q", S_IFIFO | 0o644);
    assert_eq!(
        mknod(&mut p, b"/tmp/q", S_IFIFO | 0o644),
        err(Errno::EEXIST)
    );
    // デバイスファイルはまだ作れない
    assert_eq!(
        mknod(&mut p, b"/tmp/dev", S_IFCHR | 0o644),
        err(Errno::EPERM)
    );
    assert_eq!(mknod(&mut p, b"/etc/q", S_IFIFO | 0o644), err(Errno::EROFS));
}

#[test]
fn fchdir_moves_to_an_open_directory() {
    let (mut p, _) = process();
    let d = open(&mut p, b"/etc", O_DIRECTORY);
    assert_eq!(call(&mut p, nr::FCHDIR, [d, 0, 0, 0, 0, 0]), 0);
    assert_eq!(p.cwd, "/etc");
    let f = open(&mut p, b"/etc/motd", O_RDONLY);
    assert_eq!(
        call(&mut p, nr::FCHDIR, [f, 0, 0, 0, 0, 0]),
        err(Errno::ENOTDIR)
    );
    assert_eq!(
        call(&mut p, nr::FCHDIR, [99, 0, 0, 0, 0, 0]),
        err(Errno::EBADF)
    );
}
