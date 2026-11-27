//! AF_INET(TCP、ループバック 127.0.0.1)のテスト。
//!
//! ネットワークスタックはテストのあいだで共有なので、ポート番号はテストごとに変える。
//!
//! TCP のテストは、smoltcp の TCP(protocol 0)と自作の TCP(protocol 253)の両方で回す
//! (ADR-0006:同じテストを通す)。ポートは `base` からの差で書き、実装ごとに `base` を変える。

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

/// smoltcp の TCP と自作の TCP(socket の 3 つ目の引数)
const SMOLTCP: u64 = 0;
const OWN: u64 = 253;

fn socket(p: &mut Process, ty: u64, proto: u64) -> u64 {
    let fd = call(p, nr::SOCKET, [AF_INET, ty, proto, 0, 0, 0]);
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

fn server(p: &mut Process, port: u16, proto: u64) -> u64 {
    let s = socket(p, SOCK_STREAM, proto);
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

fn tcp_over_loopback(proto: u64, base: u16) {
    let (mut p, _) = process();
    let s = server(&mut p, base + 1, proto);
    let c = socket(&mut p, SOCK_STREAM, proto);
    assert_eq!(connect(&mut p, c, LO, base + 1), 0);
    let a = call(&mut p, nr::ACCEPT, [s, 0, 0, 0, 0, 0]);
    assert!((a as i64) >= 0, "accept: {}", a as i64);
    assert_eq!(send(&mut p, c, b"hello tcp"), 9);
    assert_eq!(recv(&mut p, a), b"hello tcp");
    assert_eq!(send(&mut p, a, b"back"), 4);
    assert_eq!(recv(&mut p, c), b"back");
    // 名前:相手のポートは自動で振られたもの(49152 以上)
    assert_eq!(name(&mut p, nr::GETPEERNAME, c), (LO, base + 1));
    let (ip, port) = name(&mut p, nr::GETSOCKNAME, c);
    assert_eq!(ip, LO);
    assert!(port >= 49152, "{port}");
    assert_eq!(name(&mut p, nr::GETPEERNAME, a), (LO, port));
    // 閉じると相手は終わり(0)を読む
    call(&mut p, nr::CLOSE, [c, 0, 0, 0, 0, 0]);
    assert_eq!(recv(&mut p, a), b"");
}

fn a_listener_accepts_several_connections(proto: u64, base: u16) {
    let (mut p, _) = process();
    let s = server(&mut p, base + 2, proto);
    let c1 = socket(&mut p, SOCK_STREAM, proto);
    let c2 = socket(&mut p, SOCK_STREAM, proto);
    assert_eq!(connect(&mut p, c1, LO, base + 2), 0);
    assert_eq!(connect(&mut p, c2, LO, base + 2), 0);
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
    let c3 = socket(&mut p, SOCK_STREAM, proto);
    assert_eq!(connect(&mut p, c3, LO, base + 2), 0);
    assert!((call(&mut p, nr::ACCEPT, [s, 0, 0, 0, 0, 0]) as i64) >= 0);
}

fn connecting_to_a_closed_port_is_refused(proto: u64, base: u16) {
    let (mut p, _) = process();
    let c = socket(&mut p, SOCK_STREAM, proto);
    assert_eq!(connect(&mut p, c, LO, base + 99), err(Errno::ECONNREFUSED));
}

fn bind_and_state_errors(proto: u64, base: u16) {
    let (mut p, _) = process();
    let s = server(&mut p, base + 3, proto);
    let t = socket(&mut p, SOCK_STREAM, proto);
    assert_eq!(bind(&mut p, t, LO, base + 3), err(Errno::EADDRINUSE));
    // このマシンのものでないアドレス
    assert_eq!(
        bind(&mut p, t, [10, 9, 8, 7], base + 4),
        err(Errno::EADDRNOTAVAIL)
    );
    // 短すぎるアドレス
    put(&mut p, SCRATCH + 0x3000, &sin(LO, base + 5));
    assert_eq!(
        call(&mut p, nr::BIND, [t, SCRATCH + 0x3000, 4, 0, 0, 0]),
        err(Errno::EINVAL)
    );
    // つながっていない(TCP は EPIPE。AF_UNIX の ENOTCONN と違う)
    assert_eq!(send(&mut p, t, b"x"), err(Errno::EPIPE));
    // 待ち受けのソケットに accept するものがない(待たない)
    let n = socket(&mut p, SOCK_STREAM | SOCK_NONBLOCK, proto);
    assert_eq!(bind(&mut p, n, LO, base + 6), 0);
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
    assert_eq!(bind(&mut p, t, LO, base + 3), 0);
}

fn shutdown_write_sends_end_of_file(proto: u64, base: u16) {
    let (mut p, _) = process();
    let s = server(&mut p, base + 7, proto);
    let c = socket(&mut p, SOCK_STREAM, proto);
    connect(&mut p, c, LO, base + 7);
    let a = call(&mut p, nr::ACCEPT, [s, 0, 0, 0, 0, 0]);
    send(&mut p, c, b"last");
    assert_eq!(call(&mut p, nr::SHUTDOWN, [c, SHUT_WR, 0, 0, 0, 0]), 0);
    assert_eq!(recv(&mut p, a), b"last");
    assert_eq!(recv(&mut p, a), b"");
    // 逆向きはまだ使える
    assert_eq!(send(&mut p, a, b"ok"), 2);
    assert_eq!(recv(&mut p, c), b"ok");
}

fn large_writes_arrive_whole(proto: u64, base: u16) {
    let (mut p, _) = process();
    let s = server(&mut p, base + 8, proto);
    let c = socket(&mut p, SOCK_STREAM, proto);
    connect(&mut p, c, LO, base + 8);
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

fn an_accepted_connection_outlives_its_listener(proto: u64, base: u16) {
    // BusyBox の nc -l と同じ:1 本 accept したら、待ち受けのソケットを閉じる
    let (mut p, _) = process();
    let s = server(&mut p, base + 9, proto);
    let c = socket(&mut p, SOCK_STREAM, proto);
    assert_eq!(connect(&mut p, c, LO, base + 9), 0);
    let a = call(&mut p, nr::ACCEPT, [s, 0, 0, 0, 0, 0]);
    call(&mut p, nr::CLOSE, [s, 0, 0, 0, 0, 0]);
    assert_eq!(send(&mut p, c, b"still here"), 10);
    assert_eq!(recv(&mut p, a), b"still here");
    assert_eq!(send(&mut p, a, b"yes"), 3);
    assert_eq!(recv(&mut p, c), b"yes");
}

/// 自作の TCP と smoltcp の TCP が、線の上で話せる(`client` から `server` につなぐ)
fn interop(client: u64, server_proto: u64, port: u16) {
    let (mut p, _) = process();
    let s = server(&mut p, port, server_proto);
    let c = socket(&mut p, SOCK_STREAM, client);
    assert_eq!(connect(&mut p, c, LO, port), 0);
    let a = call(&mut p, nr::ACCEPT, [s, 0, 0, 0, 0, 0]);
    assert!((a as i64) >= 0, "accept: {}", a as i64);
    assert_eq!(send(&mut p, c, b"from client"), 11);
    assert_eq!(recv(&mut p, a), b"from client");
    assert_eq!(send(&mut p, a, b"from server"), 11);
    assert_eq!(recv(&mut p, c), b"from server");
    call(&mut p, nr::CLOSE, [c, 0, 0, 0, 0, 0]);
    assert_eq!(recv(&mut p, a), b"");
}

/// 同じテストを、両方の実装で 1 つずつの #[test] にする
macro_rules! both {
    ($($name:ident),* $(,)?) => {
        mod smoltcp_tcp {
            $( #[test] fn $name() { super::$name(super::SMOLTCP, 7100) } )*
        }
        mod own_tcp {
            $( #[test] fn $name() { super::$name(super::OWN, 7300) } )*
        }
    };
}

both!(
    tcp_over_loopback,
    a_listener_accepts_several_connections,
    connecting_to_a_closed_port_is_refused,
    bind_and_state_errors,
    shutdown_write_sends_end_of_file,
    large_writes_arrive_whole,
    an_accepted_connection_outlives_its_listener,
);

#[test]
fn own_tcp_client_talks_to_a_smoltcp_server() {
    interop(OWN, SMOLTCP, 7501);
}

#[test]
fn smoltcp_client_talks_to_an_own_tcp_server() {
    interop(SMOLTCP, OWN, 7502);
}
