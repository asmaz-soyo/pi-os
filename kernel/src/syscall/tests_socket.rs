//! ソケット(AF_UNIX)のテスト。

use super::tests::{SCRATCH, call, err, get, process, put};
use super::*;
use crate::Errno;

const AF_UNIX: u64 = 1;
const AF_INET: u64 = 2;
const SOCK_STREAM: u64 = 1;
const SOCK_CLOEXEC: u64 = 0o2000000;
const F_GETFD: u64 = 1;

/// socketpair を呼び、2 つの fd を返す
fn pair(p: &mut crate::process::Process, ty: u64) -> (u64, u64) {
    assert_eq!(call(p, nr::SOCKETPAIR, [AF_UNIX, ty, 0, SCRATCH, 0, 0]), 0);
    let b = get(p, SCRATCH, 8);
    let a = i32::from_le_bytes(b[..4].try_into().unwrap()) as u64;
    let c = i32::from_le_bytes(b[4..].try_into().unwrap()) as u64;
    (a, c)
}

#[test]
fn a_socketpair_carries_bytes_both_ways() {
    let (mut p, _) = process();
    let (a, b) = pair(&mut p, SOCK_STREAM);
    assert_eq!((a, b), (3, 4));
    put(&mut p, SCRATCH + 0x100, b"ping");
    assert_eq!(call(&mut p, nr::WRITE, [a, SCRATCH + 0x100, 4, 0, 0, 0]), 4);
    assert_eq!(call(&mut p, nr::READ, [b, SCRATCH + 0x200, 16, 0, 0, 0]), 4);
    assert_eq!(get(&mut p, SCRATCH + 0x200, 4), b"ping");
    // 逆向きも(パイプと違い、どちらの端も読み書きできる)
    put(&mut p, SCRATCH + 0x100, b"pong!");
    assert_eq!(call(&mut p, nr::WRITE, [b, SCRATCH + 0x100, 5, 0, 0, 0]), 5);
    assert_eq!(call(&mut p, nr::READ, [a, SCRATCH + 0x200, 16, 0, 0, 0]), 5);
    assert_eq!(get(&mut p, SCRATCH + 0x200, 5), b"pong!");
}

#[test]
fn closing_one_end_gives_eof_and_epipe_to_the_other() {
    let (mut p, _) = process();
    let (a, b) = pair(&mut p, SOCK_STREAM);
    put(&mut p, SCRATCH + 0x100, b"bye");
    call(&mut p, nr::WRITE, [a, SCRATCH + 0x100, 3, 0, 0, 0]);
    call(&mut p, nr::CLOSE, [a, 0, 0, 0, 0, 0]);
    // 残りを読み切ったあとは 0(相手が閉じた)
    assert_eq!(call(&mut p, nr::READ, [b, SCRATCH + 0x200, 16, 0, 0, 0]), 3);
    assert_eq!(call(&mut p, nr::READ, [b, SCRATCH + 0x200, 16, 0, 0, 0]), 0);
    assert_eq!(
        call(&mut p, nr::WRITE, [b, SCRATCH + 0x100, 3, 0, 0, 0]),
        err(Errno::EPIPE)
    );
}

#[test]
fn a_socket_looks_like_a_socket() {
    let (mut p, _) = process();
    let (a, _) = pair(&mut p, SOCK_STREAM);
    assert_eq!(call(&mut p, nr::FSTAT, [a, SCRATCH + 0x100, 0, 0, 0, 0]), 0);
    let st = get(&mut p, SCRATCH + 0x100, 128);
    let mode = u32::from_le_bytes(st[16..20].try_into().unwrap());
    assert_eq!(mode & 0o170000, 0o140000, "S_IFSOCK");
    assert_eq!(
        call(&mut p, nr::LSEEK, [a, 0, 0, 0, 0, 0]),
        err(Errno::ESPIPE)
    );
}

#[test]
fn sock_cloexec_marks_both_ends() {
    let (mut p, _) = process();
    let (a, b) = pair(&mut p, SOCK_STREAM | SOCK_CLOEXEC);
    assert_eq!(call(&mut p, nr::FCNTL, [a, F_GETFD, 0, 0, 0, 0]), 1);
    assert_eq!(call(&mut p, nr::FCNTL, [b, F_GETFD, 0, 0, 0, 0]), 1);
}

#[test]
fn unknown_families_and_types_are_rejected() {
    let (mut p, _) = process();
    // まだ登録していないファミリー
    assert_eq!(
        call(&mut p, nr::SOCKETPAIR, [99, SOCK_STREAM, 0, SCRATCH, 0, 0]),
        err(Errno::EAFNOSUPPORT)
    );
    // AF_INET に socketpair はない(Linux と同じ)
    assert_eq!(
        call(
            &mut p,
            nr::SOCKETPAIR,
            [AF_INET, SOCK_STREAM, 0, SCRATCH, 0, 0]
        ),
        err(Errno::EOPNOTSUPP)
    );
    assert_eq!(
        call(&mut p, nr::SOCKETPAIR, [AF_UNIX, 20, 0, SCRATCH, 0, 0]),
        err(Errno::EINVAL)
    );
    // 種類の番号としてはあるが、AF_UNIX にはないもの(SOCK_DCCP = 6)
    assert_eq!(
        call(&mut p, nr::SOCKETPAIR, [AF_UNIX, 6, 0, SCRATCH, 0, 0]),
        err(Errno::ESOCKTNOSUPPORT)
    );
    // AF_UNIX のプロトコルは 0(か AF_UNIX)だけ
    assert_eq!(
        call(
            &mut p,
            nr::SOCKETPAIR,
            [AF_UNIX, SOCK_STREAM, 7, SCRATCH, 0, 0]
        ),
        err(Errno::EPROTONOSUPPORT)
    );
}
