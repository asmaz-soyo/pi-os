//! AF_INET(TCP、ループバック 127.0.0.1)のテスト。
//!
//! ネットワークスタックはテストのあいだで共有なので、ポート番号はテストごとに変える。

use super::tests::{SCRATCH, call, err, get, process, put};
use super::*;
use crate::Errno;
use crate::process::Process;

const AF_INET: u64 = 2;
const SOCK_STREAM: u64 = 1;
const SOCK_NONBLOCK: u64 = 0o4000;
const SHUT_WR: u64 = 1;

/// sockaddr_in(16 バイト:ファミリー、ポート(ビッグエンディアン)、アドレス、詰め物)
fn sin(ip: [u8; 4], port: u16) -> [u8; 16] {
    let mut b = [0u8; 16];
    b[..2].copy_from_slice(&2u16.to_le_bytes());
    b[2..4].copy_from_slice(&port.to_be_bytes());
    b[4..8].copy_from_slice(&ip);
    b
}

const LO: [u8; 4] = [127, 0, 0, 1];

fn socket(p: &mut Process, ty: u64) -> u64 {
    let fd = call(p, nr::SOCKET, [AF_INET, ty, 0, 0, 0, 0]);
    assert!((fd as i64) >= 0, "socket: {}", fd as i64);
    fd
}

fn bind(p: &mut Process, fd: u64, ip: [u8; 4], port: u16) -> u64 {
    put(p, SCRATCH + 0x3000, &sin(ip, port));
    call(p, nr::BIND, [fd, SCRATCH + 0x3000, 16, 0, 0, 0])
}

fn connect(p: &mut Process, fd: u64, ip: [u8; 4], port: u16) -> u64 {
    put(p, SCRATCH + 0x3100, &sin(ip, port));
    call(p, nr::CONNECT, [fd, SCRATCH + 0x3100, 16, 0, 0, 0])
}

fn server(p: &mut Process, port: u16) -> u64 {
    let s = socket(p, SOCK_STREAM);
    assert_eq!(bind(p, s, LO, port), 0);
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

/// getsockname / getpeername の結果(アドレス, ポート)
fn name(p: &mut Process, which: u64, fd: u64) -> ([u8; 4], u16) {
    put(p, SCRATCH + 0x600, &16u32.to_le_bytes());
    assert_eq!(
        call(p, which, [fd, SCRATCH + 0x500, SCRATCH + 0x600, 0, 0, 0]),
        0
    );
    let b = get(p, SCRATCH + 0x500, 16);
    assert_eq!(u16::from_le_bytes([b[0], b[1]]), 2);
    (
        b[4..8].try_into().unwrap(),
        u16::from_be_bytes([b[2], b[3]]),
    )
}

#[test]
fn tcp_over_loopback() {
    let (mut p, _) = process();
    let s = server(&mut p, 7101);
    let c = socket(&mut p, SOCK_STREAM);
    assert_eq!(connect(&mut p, c, LO, 7101), 0);
    let a = call(&mut p, nr::ACCEPT, [s, 0, 0, 0, 0, 0]);
    assert!((a as i64) >= 0, "accept: {}", a as i64);
    assert_eq!(send(&mut p, c, b"hello tcp"), 9);
    assert_eq!(recv(&mut p, a), b"hello tcp");
    assert_eq!(send(&mut p, a, b"back"), 4);
    assert_eq!(recv(&mut p, c), b"back");
    // 名前:相手のポートは自動で振られたもの(49152 以上)
    assert_eq!(name(&mut p, nr::GETPEERNAME, c), (LO, 7101));
    let (ip, port) = name(&mut p, nr::GETSOCKNAME, c);
    assert_eq!(ip, LO);
    assert!(port >= 49152, "{port}");
    assert_eq!(name(&mut p, nr::GETPEERNAME, a), (LO, port));
    // 閉じると相手は終わり(0)を読む
    call(&mut p, nr::CLOSE, [c, 0, 0, 0, 0, 0]);
    assert_eq!(recv(&mut p, a), b"");
}

#[test]
fn a_listener_accepts_several_connections() {
    let (mut p, _) = process();
    let s = server(&mut p, 7102);
    let c1 = socket(&mut p, SOCK_STREAM);
    let c2 = socket(&mut p, SOCK_STREAM);
    assert_eq!(connect(&mut p, c1, LO, 7102), 0);
    assert_eq!(connect(&mut p, c2, LO, 7102), 0);
    let a1 = call(&mut p, nr::ACCEPT, [s, 0, 0, 0, 0, 0]);
    let a2 = call(&mut p, nr::ACCEPT, [s, 0, 0, 0, 0, 0]);
    send(&mut p, c1, b"one");
    send(&mut p, c2, b"two");
    // どちらが先に accept されたかは決まっていないので、組で確かめる
    let (r1, r2) = (recv(&mut p, a1), recv(&mut p, a2));
    let mut got = [r1, r2];
    got.sort();
    assert_eq!(got, [b"one".to_vec(), b"two".to_vec()]);
    // 3 本目も受けられる(待ち受けのソケットが補充されている)
    let c3 = socket(&mut p, SOCK_STREAM);
    assert_eq!(connect(&mut p, c3, LO, 7102), 0);
    assert!((call(&mut p, nr::ACCEPT, [s, 0, 0, 0, 0, 0]) as i64) >= 0);
}

#[test]
fn connecting_to_a_closed_port_is_refused() {
    let (mut p, _) = process();
    let c = socket(&mut p, SOCK_STREAM);
    assert_eq!(connect(&mut p, c, LO, 7199), err(Errno::ECONNREFUSED));
}

#[test]
fn bind_and_state_errors() {
    let (mut p, _) = process();
    let s = server(&mut p, 7103);
    let t = socket(&mut p, SOCK_STREAM);
    assert_eq!(bind(&mut p, t, LO, 7103), err(Errno::EADDRINUSE));
    // このマシンのものでないアドレス
    assert_eq!(
        bind(&mut p, t, [10, 9, 8, 7], 7104),
        err(Errno::EADDRNOTAVAIL)
    );
    // 短すぎるアドレス
    put(&mut p, SCRATCH + 0x3000, &sin(LO, 7105));
    assert_eq!(
        call(&mut p, nr::BIND, [t, SCRATCH + 0x3000, 4, 0, 0, 0]),
        err(Errno::EINVAL)
    );
    // つながっていない(TCP は EPIPE。AF_UNIX の ENOTCONN と違う)
    assert_eq!(send(&mut p, t, b"x"), err(Errno::EPIPE));
    // 待ち受けのソケットに accept するものがない(待たない)
    let n = socket(&mut p, SOCK_STREAM | SOCK_NONBLOCK);
    assert_eq!(bind(&mut p, n, LO, 7106), 0);
    assert_eq!(call(&mut p, nr::LISTEN, [n, 1, 0, 0, 0, 0]), 0);
    assert_eq!(
        call(&mut p, nr::ACCEPT, [n, 0, 0, 0, 0, 0]),
        err(Errno::EAGAIN)
    );
    // listen していない
    assert_eq!(
        call(&mut p, nr::ACCEPT, [t, 0, 0, 0, 0, 0]),
        err(Errno::EINVAL)
    );
    // 閉じたらポートはまた使える
    call(&mut p, nr::CLOSE, [s, 0, 0, 0, 0, 0]);
    assert_eq!(bind(&mut p, t, LO, 7103), 0);
}

#[test]
fn shutdown_write_sends_end_of_file() {
    let (mut p, _) = process();
    let s = server(&mut p, 7107);
    let c = socket(&mut p, SOCK_STREAM);
    connect(&mut p, c, LO, 7107);
    let a = call(&mut p, nr::ACCEPT, [s, 0, 0, 0, 0, 0]);
    send(&mut p, c, b"last");
    assert_eq!(call(&mut p, nr::SHUTDOWN, [c, SHUT_WR, 0, 0, 0, 0]), 0);
    assert_eq!(recv(&mut p, a), b"last");
    assert_eq!(recv(&mut p, a), b"");
    // 逆向きはまだ使える
    assert_eq!(send(&mut p, a, b"ok"), 2);
    assert_eq!(recv(&mut p, c), b"ok");
}

#[test]
fn large_writes_arrive_whole() {
    let (mut p, _) = process();
    let s = server(&mut p, 7108);
    let c = socket(&mut p, SOCK_STREAM);
    connect(&mut p, c, LO, 7108);
    let a = call(&mut p, nr::ACCEPT, [s, 0, 0, 0, 0, 0]);
    // バッファより大きいものは、相手が読むまで書けない。ここでは一度に書ける量にする
    let data: std::vec::Vec<u8> = (0..3000u32).map(|i| i as u8).collect();
    put(&mut p, SCRATCH + 0x1000, &data);
    assert_eq!(
        call(&mut p, nr::WRITE, [c, SCRATCH + 0x1000, 3000, 0, 0, 0]),
        3000
    );
    let mut got = std::vec::Vec::new();
    while got.len() < 3000 {
        let n = call(&mut p, nr::READ, [a, SCRATCH + 0x2000, 1024, 0, 0, 0]);
        assert!(n > 0 && (n as i64) > 0);
        got.extend(get(&mut p, SCRATCH + 0x2000, n as usize));
    }
    assert_eq!(got, data);
}
