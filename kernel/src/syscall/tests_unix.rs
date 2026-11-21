//! パスでつなぐ AF_UNIX ソケット(socket / bind / listen / accept / connect)のテスト。
//!
//! ソケットの登録表はテストのあいだで共有なので、パスと抽象名はテストごとに変える。

use super::tests::{SCRATCH, call, err, get, process, put};
use super::*;
use crate::Errno;
use crate::process::Process;

const AT_FDCWD: u64 = -100i64 as u64;
const AF_UNIX: u64 = 1;
const SOCK_STREAM: u64 = 1;
const SOCK_NONBLOCK: u64 = 0o4000;
const SHUT_WR: u64 = 1;

/// sockaddr_un を作る(ファミリー 2 バイト + パス)。返り値は (アドレス, 長さ)
fn addr(p: &mut Process, slot: u64, path: &[u8]) -> (u64, u64) {
    let mut b = std::vec::Vec::from(1u16.to_le_bytes());
    b.extend_from_slice(path);
    let at = SCRATCH + 0x3000 + slot * 0x100;
    put(p, at, &b);
    (at, b.len() as u64)
}

fn socket(p: &mut Process, ty: u64) -> u64 {
    let fd = call(p, nr::SOCKET, [AF_UNIX, ty, 0, 0, 0, 0]);
    assert!((fd as i64) >= 0, "socket: {}", fd as i64);
    fd
}

fn bind(p: &mut Process, fd: u64, path: &[u8]) -> u64 {
    let (a, len) = addr(p, 0, path);
    call(p, nr::BIND, [fd, a, len, 0, 0, 0])
}

fn connect(p: &mut Process, fd: u64, path: &[u8]) -> u64 {
    let (a, len) = addr(p, 1, path);
    call(p, nr::CONNECT, [fd, a, len, 0, 0, 0])
}

/// 待ち受けるソケットを作る
fn server(p: &mut Process, path: &[u8]) -> u64 {
    let s = socket(p, SOCK_STREAM);
    assert_eq!(bind(p, s, path), 0);
    assert_eq!(call(p, nr::LISTEN, [s, 4, 0, 0, 0, 0]), 0);
    s
}

fn send(p: &mut Process, fd: u64, data: &[u8]) -> u64 {
    put(p, SCRATCH + 0x100, data);
    call(
        p,
        nr::WRITE,
        [fd, SCRATCH + 0x100, data.len() as u64, 0, 0, 0],
    )
}

fn recv(p: &mut Process, fd: u64) -> std::vec::Vec<u8> {
    let n = call(p, nr::READ, [fd, SCRATCH + 0x200, 64, 0, 0, 0]);
    assert!((n as i64) >= 0, "read: {}", n as i64);
    get(p, SCRATCH + 0x200, n as usize)
}

#[test]
fn a_client_connects_to_a_server_by_path() {
    let (mut p, _) = process();
    let s = server(&mut p, b"/tmp/srv\0");
    // bind はソケットのファイルを作る
    put(&mut p, SCRATCH + 0x800, b"/tmp/srv\0");
    assert_eq!(
        call(
            &mut p,
            nr::NEWFSTATAT,
            [AT_FDCWD, SCRATCH + 0x800, SCRATCH + 0x900, 0, 0, 0]
        ),
        0
    );
    let mode = u32::from_le_bytes(get(&mut p, SCRATCH + 0x910, 4).try_into().unwrap());
    assert_eq!(mode & 0o170000, 0o140000, "S_IFSOCK");
    // つなぐのは accept より前でよい(待ち行列に入る)
    let c = socket(&mut p, SOCK_STREAM);
    assert_eq!(connect(&mut p, c, b"/tmp/srv\0"), 0);
    let a = call(&mut p, nr::ACCEPT, [s, 0, 0, 0, 0, 0]);
    assert!((a as i64) >= 0);
    assert_eq!(send(&mut p, c, b"hello"), 5);
    assert_eq!(recv(&mut p, a), b"hello");
    assert_eq!(send(&mut p, a, b"world"), 5);
    assert_eq!(recv(&mut p, c), b"world");
    // 片方を閉じると、相手は終わり(0)を読む
    call(&mut p, nr::CLOSE, [c, 0, 0, 0, 0, 0]);
    assert_eq!(recv(&mut p, a), b"");
}

#[test]
fn connecting_to_nothing_is_refused() {
    let (mut p, _) = process();
    let c = socket(&mut p, SOCK_STREAM);
    assert_eq!(connect(&mut p, c, b"/tmp/nobody\0"), err(Errno::ENOENT));
    // ソケットでないファイル
    assert_eq!(connect(&mut p, c, b"/etc/motd\0"), err(Errno::ECONNREFUSED));
    // bind しただけで listen していない
    let s = socket(&mut p, SOCK_STREAM);
    assert_eq!(bind(&mut p, s, b"/tmp/idle\0"), 0);
    assert_eq!(connect(&mut p, c, b"/tmp/idle\0"), err(Errno::ECONNREFUSED));
    // 待ち受けていたソケットを閉じたあと
    let s2 = server(&mut p, b"/tmp/gone\0");
    call(&mut p, nr::CLOSE, [s2, 0, 0, 0, 0, 0]);
    assert_eq!(connect(&mut p, c, b"/tmp/gone\0"), err(Errno::ECONNREFUSED));
}

#[test]
fn bind_rules() {
    let (mut p, _) = process();
    let s = socket(&mut p, SOCK_STREAM);
    assert_eq!(bind(&mut p, s, b"/tmp/b1\0"), 0);
    // 同じソケットを 2 回は bind できない。ただしパスがもうあれば、そちらが先(EADDRINUSE)
    assert_eq!(bind(&mut p, s, b"/tmp/b2\0"), err(Errno::EINVAL));
    assert_eq!(bind(&mut p, s, b"/tmp/b1\0"), err(Errno::EADDRINUSE));
    // もうあるパス
    let t = socket(&mut p, SOCK_STREAM);
    assert_eq!(bind(&mut p, t, b"/tmp/b1\0"), err(Errno::EADDRINUSE));
    // ファミリーが違う
    put(&mut p, SCRATCH + 0x3000, &[2, 0, b'x', 0]);
    assert_eq!(
        call(&mut p, nr::BIND, [t, SCRATCH + 0x3000, 4, 0, 0, 0]),
        err(Errno::EINVAL)
    );
    // ソケットでない fd
    assert_eq!(bind(&mut p, 1, b"/tmp/b3\0"), err(Errno::ENOTSOCK));
    assert_eq!(
        call(&mut p, nr::LISTEN, [1, 4, 0, 0, 0, 0]),
        err(Errno::ENOTSOCK)
    );
}

#[test]
fn abstract_names_make_no_file() {
    let (mut p, _) = process();
    let s = server(&mut p, b"\0pios-test-abstract");
    let t = socket(&mut p, SOCK_STREAM);
    assert_eq!(
        bind(&mut p, t, b"\0pios-test-abstract"),
        err(Errno::EADDRINUSE)
    );
    let c = socket(&mut p, SOCK_STREAM);
    assert_eq!(connect(&mut p, c, b"\0pios-test-abstract"), 0);
    let a = call(&mut p, nr::ACCEPT, [s, 0, 0, 0, 0, 0]);
    assert_eq!(send(&mut p, a, b"hi"), 2);
    assert_eq!(recv(&mut p, c), b"hi");
}

#[test]
fn accept_and_connect_states() {
    let (mut p, _) = process();
    let s = socket(&mut p, SOCK_STREAM | SOCK_NONBLOCK);
    // listen していない
    assert_eq!(
        call(&mut p, nr::ACCEPT, [s, 0, 0, 0, 0, 0]),
        err(Errno::EINVAL)
    );
    assert_eq!(bind(&mut p, s, b"\0pios-test-states"), 0);
    assert_eq!(call(&mut p, nr::LISTEN, [s, 4, 0, 0, 0, 0]), 0);
    // まだ誰も来ていない(待たない)
    assert_eq!(
        call(&mut p, nr::ACCEPT, [s, 0, 0, 0, 0, 0]),
        err(Errno::EAGAIN)
    );
    // つながっていないソケットは読み書きできない
    let c = socket(&mut p, SOCK_STREAM);
    assert_eq!(send(&mut p, c, b"x"), err(Errno::ENOTCONN));
    assert_eq!(connect(&mut p, c, b"\0pios-test-states"), 0);
    assert_eq!(
        connect(&mut p, c, b"\0pios-test-states"),
        err(Errno::EISCONN)
    );
}

#[test]
fn names_of_both_ends() {
    let (mut p, _) = process();
    let s = server(&mut p, b"\0pios-test-names");
    let c = socket(&mut p, SOCK_STREAM);
    assert_eq!(connect(&mut p, c, b"\0pios-test-names"), 0);
    // accept は相手の名前を返す(名前のないソケットは、ファミリーだけの 2 バイト)
    put(&mut p, SCRATCH + 0x600, &64u32.to_le_bytes());
    let a = call(
        &mut p,
        nr::ACCEPT,
        [s, SCRATCH + 0x500, SCRATCH + 0x600, 0, 0, 0],
    );
    assert!((a as i64) >= 0);
    assert_eq!(get(&mut p, SCRATCH + 0x600, 4), 2u32.to_le_bytes());
    assert_eq!(get(&mut p, SCRATCH + 0x500, 2), 1u16.to_le_bytes());
    // 待ち受け側の名前
    put(&mut p, SCRATCH + 0x600, &64u32.to_le_bytes());
    assert_eq!(
        call(
            &mut p,
            nr::GETPEERNAME,
            [c, SCRATCH + 0x500, SCRATCH + 0x600, 0, 0, 0]
        ),
        0
    );
    let want = b"\x01\x00\x00pios-test-names";
    assert_eq!(
        get(&mut p, SCRATCH + 0x600, 4),
        (want.len() as u32).to_le_bytes()
    );
    assert_eq!(get(&mut p, SCRATCH + 0x500, want.len()), want);
    put(&mut p, SCRATCH + 0x600, &64u32.to_le_bytes());
    assert_eq!(
        call(
            &mut p,
            nr::GETSOCKNAME,
            [a, SCRATCH + 0x500, SCRATCH + 0x600, 0, 0, 0]
        ),
        0
    );
    assert_eq!(get(&mut p, SCRATCH + 0x500, want.len()), want);
}

#[test]
fn shutdown_write_ends_the_peers_reading() {
    let (mut p, _) = process();
    let s = server(&mut p, b"\0pios-test-shutdown");
    let c = socket(&mut p, SOCK_STREAM);
    connect(&mut p, c, b"\0pios-test-shutdown");
    let a = call(&mut p, nr::ACCEPT, [s, 0, 0, 0, 0, 0]);
    send(&mut p, c, b"last");
    assert_eq!(call(&mut p, nr::SHUTDOWN, [c, SHUT_WR, 0, 0, 0, 0]), 0);
    assert_eq!(recv(&mut p, a), b"last");
    assert_eq!(recv(&mut p, a), b"");
    // 書けなくなったのは c だけ:a からはまだ c に書ける
    assert_eq!(send(&mut p, c, b"x"), err(Errno::EPIPE));
    assert_eq!(send(&mut p, a, b"reply"), 5);
    assert_eq!(recv(&mut p, c), b"reply");
}

#[test]
fn a_socket_file_cannot_be_opened() {
    let (mut p, _) = process();
    let _s = server(&mut p, b"/tmp/noopen\0");
    put(&mut p, SCRATCH + 0x800, b"/tmp/noopen\0");
    assert_eq!(
        call(&mut p, nr::OPENAT, [AT_FDCWD, SCRATCH + 0x800, 0, 0, 0, 0]),
        err(Errno::ENXIO)
    );
}
