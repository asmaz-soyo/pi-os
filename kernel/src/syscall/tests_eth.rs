//! ループバックでないインターフェース(Ethernet)を通る TCP と UDP のテスト。
//!
//! 「線」の偽デバイス(2 つの端をつないだもの)の片方をカーネルに eth として付け(10.0.5.1/24)、
//! もう片方では別の smoltcp(10.0.5.2)をスレッドで動かして、TCP と UDP のエコーサーバーにする。

use super::tests::{SCRATCH, call, err, get, process, put};
use super::*;
use crate::Errno;
use crate::net::stack::Stack;
use crate::process::Process;
use hal::net::{LinkType, NetDev, NetError};
use smoltcp::socket::{tcp, udp};
use smoltcp::wire::{IpAddress, IpCidr, Ipv4Address};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex, Once};
use std::vec;
use std::vec::Vec;

const AF_INET: u64 = 2;
const SOCK_STREAM: u64 = 1;
const SOCK_DGRAM: u64 = 2;
const REMOTE: [u8; 4] = [10, 0, 5, 2];

type Queue = Arc<Mutex<VecDeque<Vec<u8>>>>;

/// 線の片方の端:`tx` に送り、`rx` から受ける
struct WireEnd {
    mac: [u8; 6],
    tx: Queue,
    rx: Queue,
}

impl NetDev for WireEnd {
    fn link_type(&self) -> LinkType {
        LinkType::Ethernet
    }
    fn mac(&self) -> [u8; 6] {
        self.mac
    }
    fn mtu(&self) -> usize {
        1500
    }
    fn send(&mut self, frame: &[u8]) -> Result<(), NetError> {
        self.tx.lock().unwrap().push_back(frame.to_vec());
        Ok(())
    }
    fn recv(&mut self, buf: &mut [u8]) -> Result<Option<usize>, NetError> {
        let Some(f) = self.rx.lock().unwrap().pop_front() else {
            return Ok(None);
        };
        buf[..f.len()].copy_from_slice(&f);
        Ok(Some(f.len()))
    }
}

/// 向こう側のホスト:TCP の 7 番と UDP の 7 番で、受けたものをそのまま返す
fn remote_host(end: WireEnd) {
    let start = std::time::Instant::now();
    let now = || start.elapsed().as_nanos() as u64;
    let mut stack = Stack::new(
        std::boxed::Box::new(end),
        &[IpCidr::new(IpAddress::v4(10, 0, 5, 2), 24)],
        7,
        now(),
    );
    let new_tcp = || {
        let mut s = tcp::Socket::new(
            tcp::SocketBuffer::new(vec![0; 4096]),
            tcp::SocketBuffer::new(vec![0; 4096]),
        );
        s.listen(7).unwrap();
        s
    };
    // 何本でも受けられるように、つながったら待ち受けを足す(下のループ)
    let mut ts = Vec::new();
    let mut u = udp::Socket::new(
        udp::PacketBuffer::new(vec![udp::PacketMetadata::EMPTY; 8], vec![0; 4096]),
        udp::PacketBuffer::new(vec![udp::PacketMetadata::EMPTY; 8], vec![0; 4096]),
    );
    u.bind(7).unwrap();
    let u = stack.sockets.add(u);
    loop {
        stack.poll(now());
        ts.retain(|&t| {
            let s = stack.sockets.get_mut::<tcp::Socket>(t);
            if s.can_recv() && s.can_send() {
                let mut buf = [0u8; 512];
                let n = s.recv_slice(&mut buf).unwrap();
                s.send_slice(&buf[..n]).unwrap();
            } else if s.state() == tcp::State::CloseWait {
                // 相手が閉じた:こちらも閉じる
                s.close();
            } else if matches!(s.state(), tcp::State::Closed | tcp::State::TimeWait) {
                // 終わった
                stack.sockets.remove(t);
                return false;
            }
            true
        });
        // 待ち受けは常に 4 本(つながってから足すまでの隙間に、次の SYN が来ても受けられるように)
        let listening = ts
            .iter()
            .filter(|&&t| stack.sockets.get::<tcp::Socket>(t).state() == tcp::State::Listen)
            .count();
        for _ in listening..4 {
            ts.push(stack.sockets.add(new_tcp()));
        }
        let s = stack.sockets.get_mut::<udp::Socket>(u);
        if let Ok((data, meta)) = s.recv() {
            let data = data.to_vec();
            s.send_slice(&data, meta.endpoint).unwrap();
        }
        std::thread::sleep(std::time::Duration::from_micros(200));
    }
}

/// 線を張り、片方をカーネルの eth に付け、もう片方を向こう側のホストにする(1 回だけ)
fn link_up() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let (ab, ba): (Queue, Queue) = Default::default();
        let mine = WireEnd {
            mac: [2, 0, 0, 0, 5, 1],
            tx: ab.clone(),
            rx: ba.clone(),
        };
        let theirs = WireEnd {
            mac: [2, 0, 0, 0, 5, 2],
            tx: ba,
            rx: ab,
        };
        crate::net::inet::attach(
            "eth-test",
            std::boxed::Box::new(mine),
            Some(IpCidr::new(IpAddress::v4(10, 0, 5, 1), 24)),
            None,
        );
        std::thread::spawn(move || remote_host(theirs));
    });
}

fn sin(ip: [u8; 4], port: u16) -> [u8; 16] {
    let mut b = [0u8; 16];
    b[..2].copy_from_slice(&2u16.to_le_bytes());
    b[2..4].copy_from_slice(&port.to_be_bytes());
    b[4..8].copy_from_slice(&ip);
    b
}

fn socket(p: &mut Process, ty: u64) -> u64 {
    socket_with(p, ty, 0)
}

/// `proto`:0 は smoltcp の TCP、253 は自作の TCP
fn socket_with(p: &mut Process, ty: u64, proto: u64) -> u64 {
    let fd = call(p, nr::SOCKET, [AF_INET, ty, proto, 0, 0, 0]);
    assert!((fd as i64) >= 0);
    fd
}

#[test]
fn tcp_reaches_another_host_over_ethernet() {
    tcp_echo_over_ethernet(0);
}

#[test]
fn own_tcp_reaches_another_host_over_ethernet() {
    // 自作の TCP が、ARP を通って別のホスト(smoltcp)とやりとりする
    tcp_echo_over_ethernet(253);
}

fn tcp_echo_over_ethernet(proto: u64) {
    link_up();
    let (mut p, _) = process();
    let c = socket_with(&mut p, SOCK_STREAM, proto);
    put(&mut p, SCRATCH + 0x3000, &sin(REMOTE, 7));
    assert_eq!(
        call(&mut p, nr::CONNECT, [c, SCRATCH + 0x3000, 16, 0, 0, 0]),
        0
    );
    // 自分のアドレスは eth のもの
    put(&mut p, SCRATCH + 0x600, &16u32.to_le_bytes());
    call(
        &mut p,
        nr::GETSOCKNAME,
        [c, SCRATCH + 0x500, SCRATCH + 0x600, 0, 0, 0],
    );
    assert_eq!(get(&mut p, SCRATCH + 0x504, 4), [10, 0, 5, 1]);
    put(&mut p, SCRATCH + 0x100, b"echo me");
    assert_eq!(call(&mut p, nr::WRITE, [c, SCRATCH + 0x100, 7, 0, 0, 0]), 7);
    let mut got = Vec::new();
    while got.len() < 7 {
        let n = call(&mut p, nr::READ, [c, SCRATCH + 0x200, 64, 0, 0, 0]);
        assert!((n as i64) > 0, "read: {}", n as i64);
        got.extend(get(&mut p, SCRATCH + 0x200, n as usize));
    }
    assert_eq!(got, b"echo me");
}

#[test]
fn udp_reaches_another_host_over_ethernet() {
    link_up();
    let (mut p, _) = process();
    let u = socket(&mut p, SOCK_DGRAM);
    put(&mut p, SCRATCH + 0x3000, &sin(REMOTE, 7));
    put(&mut p, SCRATCH + 0x100, b"datagram");
    assert_eq!(
        call(
            &mut p,
            nr::SENDTO,
            [u, SCRATCH + 0x100, 8, 0, SCRATCH + 0x3000, 16]
        ),
        8
    );
    put(&mut p, SCRATCH + 0x600, &16u32.to_le_bytes());
    let n = call(
        &mut p,
        nr::RECVFROM,
        [u, SCRATCH + 0x200, 64, 0, SCRATCH + 0x500, SCRATCH + 0x600],
    );
    assert_eq!(n, 8);
    assert_eq!(get(&mut p, SCRATCH + 0x200, 8), b"datagram");
    assert_eq!(get(&mut p, SCRATCH + 0x504, 4), REMOTE);
}

#[test]
fn addresses_without_a_route_are_unreachable() {
    link_up();
    let (mut p, _) = process();
    // 10.0.5.0/24 の外で、デフォルトルートもない
    let c = socket(&mut p, SOCK_STREAM);
    put(&mut p, SCRATCH + 0x3000, &sin([192, 0, 2, 1], 80));
    assert_eq!(
        call(&mut p, nr::CONNECT, [c, SCRATCH + 0x3000, 16, 0, 0, 0]),
        err(Errno::ENETUNREACH)
    );
    // eth のアドレスには bind できる
    let s = socket(&mut p, SOCK_STREAM);
    put(&mut p, SCRATCH + 0x3000, &sin([10, 0, 5, 1], 7301));
    assert_eq!(
        call(&mut p, nr::BIND, [s, SCRATCH + 0x3000, 16, 0, 0, 0]),
        0
    );
    let _ = Ipv4Address::UNSPECIFIED;
}
