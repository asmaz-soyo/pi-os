//! AF_INET の UDP(ループバック)と、sendto / recvfrom / setsockopt / getsockopt のテスト。
//!
//! ネットワークスタックはテストのあいだで共有なので、ポート番号はテストごとに変える。

use super::tests::{SCRATCH, call, err, get, process, put};
use super::*;
use crate::Errno;
use crate::process::Process;

const AF_INET: u64 = 2;
const SOCK_STREAM: u64 = 1;
const SOCK_DGRAM: u64 = 2;
const MSG_DONTWAIT: u64 = 0x40;
const SOL_SOCKET: u64 = 1;
const SO_REUSEADDR: u64 = 2;
const SO_TYPE: u64 = 3;
const SO_ERROR: u64 = 4;
const LO: [u8; 4] = [127, 0, 0, 1];

fn sin(ip: [u8; 4], port: u16) -> [u8; 16] {
    let mut b = [0u8; 16];
    b[..2].copy_from_slice(&2u16.to_le_bytes());
    b[2..4].copy_from_slice(&port.to_be_bytes());
    b[4..8].copy_from_slice(&ip);
    b
}

fn socket(p: &mut Process, ty: u64) -> u64 {
    let fd = call(p, nr::SOCKET, [AF_INET, ty, 0, 0, 0, 0]);
    assert!((fd as i64) >= 0, "socket: {}", fd as i64);
    fd
}

fn bind(p: &mut Process, fd: u64, port: u16) -> u64 {
    put(p, SCRATCH + 0x3000, &sin(LO, port));
    call(p, nr::BIND, [fd, SCRATCH + 0x3000, 16, 0, 0, 0])
}

fn sendto(p: &mut Process, fd: u64, data: &[u8], to: Option<([u8; 4], u16)>) -> u64 {
    put(p, SCRATCH + 0x100, data);
    let (addr, len) = match to {
        Some((ip, port)) => {
            put(p, SCRATCH + 0x3100, &sin(ip, port));
            (SCRATCH + 0x3100, 16)
        }
        None => (0, 0),
    };
    call(
        p,
        nr::SENDTO,
        [fd, SCRATCH + 0x100, data.len() as u64, 0, addr, len],
    )
}

/// recvfrom の結果:(データ, 送り元のアドレスとポート)
fn recvfrom(
    p: &mut Process,
    fd: u64,
    cap: u64,
    flags: u64,
) -> Result<(std::vec::Vec<u8>, [u8; 4], u16), u64> {
    put(p, SCRATCH + 0x600, &16u32.to_le_bytes());
    let n = call(
        p,
        nr::RECVFROM,
        [
            fd,
            SCRATCH + 0x200,
            cap,
            flags,
            SCRATCH + 0x500,
            SCRATCH + 0x600,
        ],
    );
    if (n as i64) < 0 {
        return Err(n);
    }
    let a = get(p, SCRATCH + 0x500, 16);
    Ok((
        get(p, SCRATCH + 0x200, n as usize),
        a[4..8].try_into().unwrap(),
        u16::from_be_bytes([a[2], a[3]]),
    ))
}

fn local_port(p: &mut Process, fd: u64) -> u16 {
    put(p, SCRATCH + 0x600, &16u32.to_le_bytes());
    assert_eq!(
        call(
            p,
            nr::GETSOCKNAME,
            [fd, SCRATCH + 0x500, SCRATCH + 0x600, 0, 0, 0]
        ),
        0
    );
    let a = get(p, SCRATCH + 0x500, 4);
    u16::from_be_bytes([a[2], a[3]])
}

#[test]
fn datagrams_go_both_ways_with_their_senders() {
    let (mut p, _) = process();
    let a = socket(&mut p, SOCK_DGRAM);
    assert_eq!(bind(&mut p, a, 7201), 0);
    // b は bind していない:最初に送るときにポートが振られる
    let b = socket(&mut p, SOCK_DGRAM);
    assert_eq!(sendto(&mut p, b, b"ping", Some((LO, 7201))), 4);
    let bport = local_port(&mut p, b);
    assert!(bport >= 49152, "{bport}");
    let (data, ip, port) = recvfrom(&mut p, a, 64, 0).unwrap();
    assert_eq!((data.as_slice(), ip, port), (&b"ping"[..], LO, bport));
    assert_eq!(sendto(&mut p, a, b"pong", Some((LO, bport))), 4);
    let (data, _, port) = recvfrom(&mut p, b, 64, 0).unwrap();
    assert_eq!((data.as_slice(), port), (&b"pong"[..], 7201));
}

#[test]
fn datagram_boundaries_are_kept_and_long_ones_are_cut() {
    let (mut p, _) = process();
    let a = socket(&mut p, SOCK_DGRAM);
    assert_eq!(bind(&mut p, a, 7202), 0);
    let b = socket(&mut p, SOCK_DGRAM);
    sendto(&mut p, b, b"one", Some((LO, 7202)));
    sendto(&mut p, b, b"second", Some((LO, 7202)));
    sendto(&mut p, b, b"three", Some((LO, 7202)));
    assert_eq!(recvfrom(&mut p, a, 64, 0).unwrap().0, b"one");
    // バッファより長いデータグラムは切られ、残りは捨てられる
    assert_eq!(recvfrom(&mut p, a, 3, 0).unwrap().0, b"sec");
    assert_eq!(recvfrom(&mut p, a, 64, 0).unwrap().0, b"three");
    // もう何もない(待たない)
    assert_eq!(
        recvfrom(&mut p, a, 64, MSG_DONTWAIT),
        Err(err(Errno::EAGAIN))
    );
}

#[test]
fn a_connected_udp_socket_uses_read_and_write() {
    let (mut p, _) = process();
    let a = socket(&mut p, SOCK_DGRAM);
    assert_eq!(bind(&mut p, a, 7203), 0);
    let b = socket(&mut p, SOCK_DGRAM);
    // 宛先がないと書けない
    put(&mut p, SCRATCH + 0x100, b"x");
    assert_eq!(
        call(&mut p, nr::WRITE, [b, SCRATCH + 0x100, 1, 0, 0, 0]),
        err(Errno::EDESTADDRREQ)
    );
    put(&mut p, SCRATCH + 0x3100, &sin(LO, 7203));
    assert_eq!(
        call(&mut p, nr::CONNECT, [b, SCRATCH + 0x3100, 16, 0, 0, 0]),
        0
    );
    put(&mut p, SCRATCH + 0x100, b"hi");
    assert_eq!(call(&mut p, nr::WRITE, [b, SCRATCH + 0x100, 2, 0, 0, 0]), 2);
    let (data, _, port) = recvfrom(&mut p, a, 64, 0).unwrap();
    assert_eq!(data, b"hi");
    sendto(&mut p, a, b"yo", Some((LO, port)));
    assert_eq!(call(&mut p, nr::READ, [b, SCRATCH + 0x200, 64, 0, 0, 0]), 2);
    assert_eq!(get(&mut p, SCRATCH + 0x200, 2), b"yo");
}

#[test]
fn udp_ports_are_separate_from_tcp_ports() {
    let (mut p, _) = process();
    let u = socket(&mut p, SOCK_DGRAM);
    assert_eq!(bind(&mut p, u, 7204), 0);
    let u2 = socket(&mut p, SOCK_DGRAM);
    assert_eq!(bind(&mut p, u2, 7204), err(Errno::EADDRINUSE));
    let t = socket(&mut p, SOCK_STREAM);
    assert_eq!(bind(&mut p, t, 7204), 0);
    // このマシンの外にはまだ届かない
    assert_eq!(
        sendto(&mut p, u2, b"x", Some(([10, 0, 0, 1], 53))),
        err(Errno::ENETUNREACH)
    );
}

#[test]
fn socket_options() {
    let (mut p, _) = process();
    let u = socket(&mut p, SOCK_DGRAM);
    let t = socket(&mut p, SOCK_STREAM);
    let getopt = |p: &mut Process, fd: u64, name: u64| {
        put(p, SCRATCH + 0x710, &4u32.to_le_bytes());
        let r = call(
            p,
            nr::GETSOCKOPT,
            [fd, SOL_SOCKET, name, SCRATCH + 0x700, SCRATCH + 0x710, 0],
        );
        assert_eq!(r, 0, "getsockopt {name}");
        assert_eq!(get(p, SCRATCH + 0x710, 4), 4u32.to_le_bytes());
        i32::from_le_bytes(get(p, SCRATCH + 0x700, 4).try_into().unwrap())
    };
    assert_eq!(getopt(&mut p, u, SO_TYPE), 2);
    assert_eq!(getopt(&mut p, t, SO_TYPE), 1);
    assert_eq!(getopt(&mut p, t, SO_ERROR), 0);
    assert_eq!(getopt(&mut p, t, SO_REUSEADDR), 0);
    put(&mut p, SCRATCH + 0x700, &1i32.to_le_bytes());
    assert_eq!(
        call(
            &mut p,
            nr::SETSOCKOPT,
            [t, SOL_SOCKET, SO_REUSEADDR, SCRATCH + 0x700, 4, 0]
        ),
        0
    );
    assert_eq!(getopt(&mut p, t, SO_REUSEADDR), 1);
    // 知らない名前
    assert_eq!(
        call(
            &mut p,
            nr::SETSOCKOPT,
            [t, SOL_SOCKET, 9999, SCRATCH + 0x700, 4, 0]
        ),
        err(Errno::ENOPROTOOPT)
    );
    // ソケットでない fd
    assert_eq!(
        call(
            &mut p,
            nr::SETSOCKOPT,
            [1, SOL_SOCKET, SO_REUSEADDR, SCRATCH + 0x700, 4, 0]
        ),
        err(Errno::ENOTSOCK)
    );
}

#[test]
fn sendto_and_recvfrom_work_on_tcp_too() {
    let (mut p, _) = process();
    let s = socket(&mut p, SOCK_STREAM);
    assert_eq!(bind(&mut p, s, 7205), 0);
    assert_eq!(call(&mut p, nr::LISTEN, [s, 2, 0, 0, 0, 0]), 0);
    let c = socket(&mut p, SOCK_STREAM);
    put(&mut p, SCRATCH + 0x3100, &sin(LO, 7205));
    assert_eq!(
        call(&mut p, nr::CONNECT, [c, SCRATCH + 0x3100, 16, 0, 0, 0]),
        0
    );
    let a = call(&mut p, nr::ACCEPT, [s, 0, 0, 0, 0, 0]);
    assert_eq!(sendto(&mut p, c, b"via sendto", None), 10);
    put(&mut p, SCRATCH + 0x600, &16u32.to_le_bytes());
    let n = call(&mut p, nr::RECVFROM, [a, SCRATCH + 0x200, 64, 0, 0, 0]);
    assert_eq!(n, 10);
    assert_eq!(get(&mut p, SCRATCH + 0x200, 10), b"via sendto");
}

/// struct msghdr(aarch64):name, namelen(+詰め物), iov, iovlen, control, controllen, flags
fn msghdr(name: u64, namelen: u32, iov: u64, iovlen: u64, controllen: u64) -> [u8; 56] {
    let mut m = [0u8; 56];
    m[0..8].copy_from_slice(&name.to_le_bytes());
    m[8..12].copy_from_slice(&namelen.to_le_bytes());
    m[16..24].copy_from_slice(&iov.to_le_bytes());
    m[24..32].copy_from_slice(&iovlen.to_le_bytes());
    m[40..48].copy_from_slice(&controllen.to_le_bytes());
    m
}

fn iov(p: &mut Process, at: u64, parts: &[(u64, u64)]) {
    let mut raw = std::vec::Vec::new();
    for (base, len) in parts {
        raw.extend_from_slice(&base.to_le_bytes());
        raw.extend_from_slice(&len.to_le_bytes());
    }
    put(p, at, &raw);
}

#[test]
fn sendmsg_gathers_and_recvmsg_scatters_with_the_sender() {
    let (mut p, _) = process();
    let a = socket(&mut p, SOCK_DGRAM);
    assert_eq!(bind(&mut p, a, 7231), 0);
    let b = socket(&mut p, SOCK_DGRAM);
    // 2つのバッファをつないで、1つのデータグラムとして送る
    put(&mut p, SCRATCH + 0x100, b"hello, ");
    put(&mut p, SCRATCH + 0x180, b"msg");
    iov(
        &mut p,
        SCRATCH + 0x2000,
        &[(SCRATCH + 0x100, 7), (SCRATCH + 0x180, 3)],
    );
    put(&mut p, SCRATCH + 0x3100, &sin(LO, 7231));
    put(
        &mut p,
        SCRATCH + 0x2100,
        &msghdr(SCRATCH + 0x3100, 16, SCRATCH + 0x2000, 2, 0),
    );
    assert_eq!(
        call(&mut p, nr::SENDMSG, [b, SCRATCH + 0x2100, 0, 0, 0, 0]),
        10
    );
    let bport = local_port(&mut p, b);
    // 4 バイトと 16 バイトのバッファに分けて受け取る
    iov(
        &mut p,
        SCRATCH + 0x2200,
        &[(SCRATCH + 0x200, 4), (SCRATCH + 0x280, 16)],
    );
    put(
        &mut p,
        SCRATCH + 0x2300,
        &msghdr(SCRATCH + 0x500, 16, SCRATCH + 0x2200, 2, 0),
    );
    assert_eq!(
        call(&mut p, nr::RECVMSG, [a, SCRATCH + 0x2300, 0, 0, 0, 0]),
        10
    );
    assert_eq!(get(&mut p, SCRATCH + 0x200, 4), b"hell");
    assert_eq!(get(&mut p, SCRATCH + 0x280, 6), b"o, msg");
    let m = get(&mut p, SCRATCH + 0x2300, 56);
    // 送り元が name に入り、namelen が本当の長さになる。補助データはない
    assert_eq!(u32::from_le_bytes(m[8..12].try_into().unwrap()), 16);
    assert_eq!(u64::from_le_bytes(m[40..48].try_into().unwrap()), 0);
    let from = get(&mut p, SCRATCH + 0x500, 16);
    assert_eq!(u16::from_be_bytes([from[2], from[3]]), bport);
}

#[test]
fn ancillary_data_is_not_supported_yet() {
    let (mut p, _) = process();
    let b = socket(&mut p, SOCK_DGRAM);
    put(&mut p, SCRATCH + 0x3100, &sin(LO, 7232));
    iov(&mut p, SCRATCH + 0x2000, &[(SCRATCH + 0x100, 1)]);
    put(
        &mut p,
        SCRATCH + 0x2100,
        &msghdr(SCRATCH + 0x3100, 16, SCRATCH + 0x2000, 1, 24),
    );
    assert_eq!(
        call(&mut p, nr::SENDMSG, [b, SCRATCH + 0x2100, 0, 0, 0, 0]),
        err(Errno::EOPNOTSUPP)
    );
}
