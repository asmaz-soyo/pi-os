//! rename(renameat、renameat2)のテスト。tmpfs(/tmp)で、名前の変更と移動、置き換え、間違いを確かめる。

use super::tests::{SCRATCH, call, err, get, open, process, put};
use super::*;
use crate::Errno;
use crate::process::Process;

const AT_FDCWD: u64 = -100i64 as u64;
const O_WRONLY: u64 = 0o1;
const O_CREAT: u64 = 0o100;
const RENAME_NOREPLACE: u64 = 1;

/// 2 つのパスを置き、(old, new) のアドレスを返す
fn paths(p: &mut Process, old: &[u8], new: &[u8]) -> (u64, u64) {
    let (a, b) = (SCRATCH + 0x3000, SCRATCH + 0x3400);
    put(p, a, &[old, b"\0"].concat());
    put(p, b, &[new, b"\0"].concat());
    (a, b)
}

fn rename(p: &mut Process, old: &[u8], new: &[u8]) -> u64 {
    let (a, b) = paths(p, old, new);
    call(p, nr::RENAMEAT, [AT_FDCWD, a, AT_FDCWD, b, 0, 0])
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
    call(p, nr::CLOSE, [fd, 0, 0, 0, 0, 0]);
    get(p, SCRATCH + 0x100, n as usize)
}

fn exists(p: &mut Process, path: &[u8]) -> bool {
    p.vfs.lookup(core::str::from_utf8(path).unwrap()).is_ok()
}

fn mkdir(p: &mut Process, path: &[u8]) {
    put(p, SCRATCH + 0x3800, &[path, b"\0"].concat());
    assert_eq!(
        call(p, nr::MKDIRAT, [AT_FDCWD, SCRATCH + 0x3800, 0o755, 0, 0, 0]),
        0
    );
}

#[test]
fn a_file_is_renamed_and_moved() {
    let (mut p, _) = process();
    write_file(&mut p, b"/tmp/a", b"hello");
    mkdir(&mut p, b"/tmp/d");
    assert_eq!(rename(&mut p, b"/tmp/a", b"/tmp/b"), 0);
    assert!(!exists(&mut p, b"/tmp/a"));
    assert_eq!(read_file(&mut p, b"/tmp/b"), b"hello");
    // 別のディレクトリへ移す。相対パスは cwd から
    p.cwd = alloc::string::String::from("/tmp");
    assert_eq!(rename(&mut p, b"b", b"d/c"), 0);
    assert_eq!(read_file(&mut p, b"/tmp/d/c"), b"hello");
    // 同じ名前への rename は何もしない
    assert_eq!(rename(&mut p, b"/tmp/d/c", b"/tmp/d/c"), 0);
    assert_eq!(read_file(&mut p, b"/tmp/d/c"), b"hello");
}

#[test]
fn an_existing_file_is_replaced() {
    // .pyc を書くときの形:一時ファイルに書いてから、本来の名前に rename する
    let (mut p, _) = process();
    write_file(&mut p, b"/tmp/new.tmp", b"new");
    write_file(&mut p, b"/tmp/target", b"old contents");
    assert_eq!(rename(&mut p, b"/tmp/new.tmp", b"/tmp/target"), 0);
    assert_eq!(read_file(&mut p, b"/tmp/target"), b"new");
    assert!(!exists(&mut p, b"/tmp/new.tmp"));
}

#[test]
fn a_directory_moves_with_its_contents() {
    let (mut p, _) = process();
    mkdir(&mut p, b"/tmp/src");
    mkdir(&mut p, b"/tmp/src/sub");
    write_file(&mut p, b"/tmp/src/sub/f", b"deep");
    mkdir(&mut p, b"/tmp/empty");
    // 空のディレクトリは置き換えられる
    assert_eq!(rename(&mut p, b"/tmp/src", b"/tmp/empty"), 0);
    assert_eq!(read_file(&mut p, b"/tmp/empty/sub/f"), b"deep");
    assert!(!exists(&mut p, b"/tmp/src"));
    assert!(!exists(&mut p, b"/tmp/src/sub/f"));
}

#[test]
fn rename_errors() {
    let (mut p, _) = process();
    write_file(&mut p, b"/tmp/f", b"x");
    write_file(&mut p, b"/tmp/g", b"y");
    mkdir(&mut p, b"/tmp/d");
    mkdir(&mut p, b"/tmp/full");
    write_file(&mut p, b"/tmp/full/x", b"z");
    assert_eq!(rename(&mut p, b"/tmp/nope", b"/tmp/x"), err(Errno::ENOENT));
    assert_eq!(
        rename(&mut p, b"/tmp/f", b"/tmp/nodir/x"),
        err(Errno::ENOENT)
    );
    assert_eq!(rename(&mut p, b"/tmp/f", b"/tmp/d"), err(Errno::EISDIR));
    assert_eq!(rename(&mut p, b"/tmp/d", b"/tmp/f"), err(Errno::ENOTDIR));
    assert_eq!(
        rename(&mut p, b"/tmp/d", b"/tmp/full"),
        err(Errno::ENOTEMPTY)
    );
    // 自分の下には移せない
    assert_eq!(
        rename(&mut p, b"/tmp/d", b"/tmp/d/inside"),
        err(Errno::EINVAL)
    );
    // ファイルシステムをまたぐ(tmpfs → initramfs)
    assert_eq!(rename(&mut p, b"/tmp/f", b"/etc/f"), err(Errno::EXDEV));
    // 読み取り専用のファイルシステムの中
    assert_eq!(rename(&mut p, b"/etc/motd", b"/etc/m"), err(Errno::EROFS));
    // renameat2 の RENAME_NOREPLACE:あれば EEXIST
    let (a, b) = paths(&mut p, b"/tmp/f", b"/tmp/g");
    assert_eq!(
        call(
            &mut p,
            nr::RENAMEAT2,
            [AT_FDCWD, a, AT_FDCWD, b, RENAME_NOREPLACE, 0]
        ),
        err(Errno::EEXIST)
    );
    assert_eq!(read_file(&mut p, b"/tmp/g"), b"y");
    // それ以外の印(RENAME_EXCHANGE など)はまだない
    assert_eq!(
        call(&mut p, nr::RENAMEAT2, [AT_FDCWD, a, AT_FDCWD, b, 2, 0]),
        err(Errno::EINVAL)
    );
}

#[test]
fn renaming_needs_write_permission_on_both_directories() {
    let (mut p, _) = process();
    mkdir(&mut p, b"/tmp/locked");
    write_file(&mut p, b"/tmp/locked/x", b"x");
    // locked は root の 0755。一般ユーザーは、そこから名前を消せない
    p.cred = crate::cred::Credentials::user(1000, 100);
    assert_eq!(
        rename(&mut p, b"/tmp/locked/x", b"/tmp/y"),
        err(Errno::EACCES)
    );
}
