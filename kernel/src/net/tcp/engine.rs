//! 自作 TCP の接続の表:届いたセグメントを接続に振り分け、送るものを集める。
//!
//! - 接続は (ローカル, リモート) の組で探す。なければ、そのポートで待ち受けているものに渡す
//!   (SYN なら子の接続を作り、accept されるまで待ち行列に置く)
//! - どこにも当てはまらないものには RST を返す(RFC 9293 の 3.10.7.1)
//! - IP のことは知らない。受け取るのは (送り元, 宛先, TCP のバイト列)、返すのも同じ形
//! - close された接続は、FIN のやりとり(と TIME-WAIT)が終わったら片付ける

use super::conn::{Conn, Endpoint, State};
use super::segment::{ACK, RST, SYN, Segment};
use alloc::collections::{BTreeMap, VecDeque};
use alloc::vec::Vec;
use core::net::Ipv4Addr;

/// 接続や待ち受けの番号
pub type Id = u32;

/// 送るもの:(送り元, 宛先, TCP のバイト列)
pub type Packet = (Ipv4Addr, Ipv4Addr, Vec<u8>);

struct Entry {
    conn: Conn,
    /// accept される前の、待ち受けの子
    parent: Option<Id>,
    /// ソケットが閉じられた(終わったら片付ける)
    released: bool,
}

struct Listener {
    local: Endpoint,
    backlog: usize,
    /// SYN を受けた子(accept を待っている)
    queue: VecDeque<Id>,
}

pub struct Engine {
    conns: BTreeMap<Id, Entry>,
    listeners: BTreeMap<Id, Listener>,
    next: Id,
    secret: u64,
    cap: usize,
    /// 接続に属さない、送るもの(RST)
    outbox: Vec<Packet>,
}

/// 64 ビットをよく混ぜる(splitmix64 の最後の段)
fn mix(mut x: u64) -> u64 {
    x = (x ^ (x >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    x ^ (x >> 31)
}

impl Engine {
    /// `secret` は最初のシーケンス番号を作る鍵(RFC 6528)。`cap` は 1 本の接続のバッファの大きさ
    pub fn new(secret: u64, cap: usize) -> Engine {
        Engine {
            conns: BTreeMap::new(),
            listeners: BTreeMap::new(),
            next: 1,
            secret,
            cap,
            outbox: Vec::new(),
        }
    }

    fn id(&mut self) -> Id {
        let id = self.next;
        self.next += 1;
        id
    }

    /// 最初のシーケンス番号(RFC 6528):4 マイクロ秒ごとに 1 進む時計 + 4 つ組と鍵のハッシュ。
    /// 時計の分で、同じ組の前の接続の番号と重ならない。ハッシュの分で、外から番号を当てられない
    fn iss(&self, local: Endpoint, remote: Endpoint, now: u64) -> u32 {
        let tuple = (u32::from(local.addr) as u64) << 32 | u32::from(remote.addr) as u64;
        let ports = (local.port as u64) << 16 | remote.port as u64;
        let f = mix(mix(tuple ^ self.secret) ^ ports);
        ((now / 4000) as u32).wrapping_add(f as u32)
    }

    /// `local` で待ち受ける(アドレスが 0.0.0.0 なら、どのアドレスに来たものも受ける)
    pub fn listen(&mut self, local: Endpoint, backlog: usize) -> Id {
        let id = self.id();
        let queue = VecDeque::new();
        self.listeners.insert(
            id,
            Listener {
                local,
                backlog,
                queue,
            },
        );
        id
    }

    /// つながって待っている接続を 1 本取り出す
    pub fn accept(&mut self, listener: Id) -> Option<Id> {
        let l = self.listeners.get_mut(&listener)?;
        let conns = &self.conns;
        let i = l.queue.iter().position(|id| {
            conns
                .get(id)
                .is_some_and(|e| !matches!(e.conn.state, State::SynReceived | State::Closed))
        })?;
        let id = l.queue.remove(i)?;
        self.conns.get_mut(&id)?.parent = None;
        Some(id)
    }

    /// accept できる接続があるか
    pub fn ready(&self, listener: Id) -> bool {
        self.listeners.get(&listener).is_some_and(|l| {
            l.queue.iter().any(|id| {
                self.conns
                    .get(id)
                    .is_some_and(|e| !matches!(e.conn.state, State::SynReceived | State::Closed))
            })
        })
    }

    /// 待ち受けをやめる。accept されていない接続は RST で切る
    pub fn unlisten(&mut self, listener: Id) {
        if let Some(l) = self.listeners.remove(&listener) {
            for id in l.queue {
                if let Some(e) = self.conns.get_mut(&id) {
                    e.conn.abort();
                    e.released = true;
                }
            }
        }
    }

    /// 接続を始める(SYN は次の `output` で出る)
    pub fn connect(&mut self, local: Endpoint, remote: Endpoint, now: u64) -> Id {
        let iss = self.iss(local, remote, now);
        let id = self.id();
        let conn = Conn::connect(local, remote, iss, self.cap);
        self.conns.insert(
            id,
            Entry {
                conn,
                parent: None,
                released: false,
            },
        );
        id
    }

    pub fn conn(&mut self, id: Id) -> Option<&mut Conn> {
        self.conns.get_mut(&id).map(|e| &mut e.conn)
    }

    /// ソケットが閉じられた:FIN を送り、やりとりが終わったら片付ける
    pub fn release(&mut self, id: Id) {
        if let Some(e) = self.conns.get_mut(&id) {
            e.conn.close();
            e.released = true;
        }
    }

    /// 接続と待ち受けが使っているポートか
    pub fn owns_port(&self, port: u16) -> bool {
        self.listeners.values().any(|l| l.local.port == port)
            || self.conns.values().any(|e| e.conn.local.port == port)
    }

    /// 届いた TCP のセグメントを処理する。`rst`:どこにも当てはまらなければ RST を返すか
    pub fn input(&mut self, src: Ipv4Addr, dst: Ipv4Addr, bytes: &[u8], now: u64, rst: bool) {
        let Ok(seg) = Segment::parse(src, dst, bytes) else {
            return; // チェックサムが合わない:黙って捨てる
        };
        let local = Endpoint {
            addr: dst,
            port: seg.dst_port,
        };
        let remote = Endpoint {
            addr: src,
            port: seg.src_port,
        };
        // 1. この組の接続
        if let Some(e) = self.conns.values_mut().find(|e| {
            e.conn.local == local && e.conn.remote == remote && e.conn.state != State::Closed
        }) {
            e.conn.on_segment(&seg, now);
            return;
        }
        // 2. 待ち受けへの SYN:子の接続を作る
        if seg.flags & (SYN | ACK | RST) == SYN {
            let found = self.listeners.iter().find(|(_, l)| {
                l.local.port == local.port && (l.local.addr.is_unspecified() || l.local.addr == dst)
            });
            if let Some((&lid, l)) = found {
                if l.queue.len() >= l.backlog {
                    return; // いっぱい:捨てる(相手は SYN を再送する)
                }
                let iss = self.iss(local, remote, now);
                let id = self.id();
                let conn = Conn::accept(local, remote, &seg, iss, self.cap);
                self.conns.insert(
                    id,
                    Entry {
                        conn,
                        parent: Some(lid),
                        released: false,
                    },
                );
                self.listeners.get_mut(&lid).unwrap().queue.push_back(id);
                return;
            }
        }
        // 3. どこにも当てはまらない:RST(RST には返さない)
        if seg.has(RST) || !rst {
            return;
        }
        let reply = if seg.has(ACK) {
            // 相手が「ここまで受け取った」と言う番号で RST(相手の窓に入る)
            Segment {
                seq: seg.ack,
                ack: 0,
                flags: RST,
                ..reset_to(&seg)
            }
        } else {
            Segment {
                seq: 0,
                ack: seg.seq.wrapping_add(seg.seq_len()),
                flags: RST | ACK,
                ..reset_to(&seg)
            }
        };
        self.outbox.push((dst, src, reply.emit(dst, src)));
    }

    /// 送るものを全部集める(タイマーの時間切れも、ここで処理する)
    pub fn output(&mut self, now: u64) -> Vec<Packet> {
        let mut out = core::mem::take(&mut self.outbox);
        for e in self.conns.values_mut() {
            for bytes in e.conn.dispatch(now) {
                out.push((e.conn.local.addr, e.conn.remote.addr, bytes));
            }
        }
        // 終わった接続を片付ける:閉じられたもの、accept される前に切れたもの
        let dead: Vec<Id> = self
            .conns
            .iter()
            .filter(|(_, e)| e.conn.state == State::Closed && (e.released || e.parent.is_some()))
            .map(|(&id, _)| id)
            .collect();
        for id in dead {
            if let Some(e) = self.conns.remove(&id)
                && let Some(l) = e.parent.and_then(|p| self.listeners.get_mut(&p))
            {
                l.queue.retain(|&x| x != id);
            }
        }
        out
    }

    /// 次に `output` を呼んでほしい時刻
    pub fn poll_at(&self) -> Option<u64> {
        self.conns.values().filter_map(|e| e.conn.poll_at()).min()
    }

    /// 生きている接続の数(片付いたかどうかを見るため)
    pub fn conn_count(&self) -> usize {
        self.conns.len()
    }
}

/// `seg` への答えの RST の、ポートを入れ替えた形
fn reset_to<'a>(seg: &Segment) -> Segment<'a> {
    Segment {
        src_port: seg.dst_port,
        dst_port: seg.src_port,
        seq: 0,
        ack: 0,
        flags: RST,
        window: 0,
        mss: None,
        payload: &[],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Errno;
    use crate::net::tcp::conn::TIME_WAIT;

    const A: Ipv4Addr = Ipv4Addr::new(10, 0, 0, 1);
    const B: Ipv4Addr = Ipv4Addr::new(10, 0, 0, 2);
    const ANY: Ipv4Addr = Ipv4Addr::UNSPECIFIED;

    fn ep(addr: Ipv4Addr, port: u16) -> Endpoint {
        Endpoint { addr, port }
    }

    /// 2 台のあいだで、送るものがなくなるまで渡し合う。線の上を通ったものを返す
    fn run(a: &mut Engine, b: &mut Engine, now: u64) -> Vec<Packet> {
        let mut wire = Vec::new();
        loop {
            let mut moved = false;
            for p in a.output(now) {
                b.input(p.0, p.1, &p.2, now, true);
                wire.push(p);
                moved = true;
            }
            for p in b.output(now) {
                a.input(p.0, p.1, &p.2, now, true);
                wire.push(p);
                moved = true;
            }
            if !moved {
                return wire;
            }
        }
    }

    fn flags(p: &Packet) -> u8 {
        Segment::parse(p.0, p.1, &p.2).unwrap().flags
    }

    #[test]
    fn a_listener_accepts_connections_and_data_flows() {
        let mut client = Engine::new(1, 4096);
        let mut server = Engine::new(2, 4096);
        let l = server.listen(ep(ANY, 80), 4);
        let c = client.connect(ep(A, 40000), ep(B, 80), 0);
        assert_eq!(server.accept(l), None);
        run(&mut client, &mut server, 0);
        assert!(server.ready(l));
        let s = server.accept(l).expect("accepted");
        assert!(!server.ready(l));
        assert_eq!(server.conn(s).unwrap().state, State::Established);
        assert_eq!(
            server.conn(s).unwrap().local,
            ep(B, 80),
            "0.0.0.0 の待ち受けでも、届いた宛先が自分"
        );
        assert_eq!(server.conn(s).unwrap().remote, ep(A, 40000));
        client.conn(c).unwrap().send(b"hi");
        run(&mut client, &mut server, 0);
        let mut buf = [0; 8];
        assert_eq!(server.conn(s).unwrap().recv(&mut buf), 2);
        assert_eq!(server.accept(l), None);
    }

    #[test]
    fn two_connections_to_one_port_are_kept_apart() {
        let mut client = Engine::new(1, 4096);
        let mut server = Engine::new(2, 4096);
        let l = server.listen(ep(B, 80), 4);
        let c1 = client.connect(ep(A, 40001), ep(B, 80), 0);
        let c2 = client.connect(ep(A, 40002), ep(B, 80), 0);
        run(&mut client, &mut server, 0);
        let s1 = server.accept(l).unwrap();
        let s2 = server.accept(l).unwrap();
        client.conn(c1).unwrap().send(b"one");
        client.conn(c2).unwrap().send(b"two");
        run(&mut client, &mut server, 0);
        let mut buf = [0; 8];
        for (s, want) in [(s1, b"one"), (s2, b"two")] {
            let n = server.conn(s).unwrap().recv(&mut buf);
            assert_eq!(&buf[..n], want);
        }
    }

    #[test]
    fn a_closed_port_answers_with_rst() {
        let mut client = Engine::new(1, 4096);
        let mut server = Engine::new(2, 4096);
        let c = client.connect(ep(A, 40000), ep(B, 81), 0);
        let wire = run(&mut client, &mut server, 0);
        // SYN に RST+ACK(番号は SYN の次)
        assert_eq!(flags(&wire[1]), RST | ACK);
        let rst = Segment::parse(wire[1].0, wire[1].1, &wire[1].2).unwrap();
        let syn = Segment::parse(wire[0].0, wire[0].1, &wire[0].2).unwrap();
        assert_eq!(rst.ack, syn.seq.wrapping_add(1));
        assert_eq!(client.conn(c).unwrap().error(), Some(Errno::ECONNREFUSED));
        // RST には RST を返さない(返し合いにならないように)。rst = false なら黙る
        assert_eq!(wire.len(), 2);
        let mut quiet = Engine::new(3, 4096);
        quiet.input(A, B, &wire[0].2, 0, false);
        assert!(quiet.output(0).is_empty());
    }

    #[test]
    fn a_segment_with_ack_for_no_connection_gets_rst_with_that_number() {
        let mut server = Engine::new(2, 4096);
        server.listen(ep(B, 80), 4);
        let stray = Segment {
            src_port: 40000,
            dst_port: 80,
            seq: 77,
            ack: 12345,
            flags: ACK,
            window: 100,
            mss: None,
            payload: b"late",
        };
        server.input(A, B, &stray.emit(A, B), 0, true);
        let out = server.output(0);
        let rst = Segment::parse(out[0].0, out[0].1, &out[0].2).unwrap();
        assert_eq!((rst.flags, rst.seq), (RST, 12345));
        assert_eq!((out[0].0, out[0].1), (B, A));
    }

    #[test]
    fn the_backlog_limits_waiting_connections() {
        let mut client = Engine::new(1, 4096);
        let mut server = Engine::new(2, 4096);
        let l = server.listen(ep(B, 80), 1);
        client.connect(ep(A, 40001), ep(B, 80), 0);
        client.connect(ep(A, 40002), ep(B, 80), 0);
        run(&mut client, &mut server, 0);
        // 2 本目の SYN は黙って捨てる(相手は再送する。Linux と同じ)
        assert!(server.accept(l).is_some());
        assert_eq!(server.accept(l), None);
    }

    #[test]
    fn initial_sequence_numbers_differ_per_connection() {
        let mut e = Engine::new(9, 4096);
        e.connect(ep(A, 40001), ep(B, 80), 0);
        e.connect(ep(A, 40002), ep(B, 80), 0);
        let out = e.output(0);
        let s1 = Segment::parse(out[0].0, out[0].1, &out[0].2).unwrap().seq;
        let s2 = Segment::parse(out[1].0, out[1].1, &out[1].2).unwrap().seq;
        assert_ne!(s1, s2);
        // 鍵が違えば、同じ組でも違う(外から当てられないように)
        let mut other = Engine::new(10, 4096);
        other.connect(ep(A, 40001), ep(B, 80), 0);
        let o = other.output(0);
        assert_ne!(Segment::parse(o[0].0, o[0].1, &o[0].2).unwrap().seq, s1);
        assert_eq!(flags(&out[0]), SYN);
    }

    #[test]
    fn released_connections_are_cleaned_up_after_close_and_time_wait() {
        let mut client = Engine::new(1, 4096);
        let mut server = Engine::new(2, 4096);
        let l = server.listen(ep(B, 80), 4);
        let c = client.connect(ep(A, 40000), ep(B, 80), 0);
        run(&mut client, &mut server, 0);
        let s = server.accept(l).unwrap();
        assert!(client.owns_port(40000) && server.owns_port(80));
        client.release(c); // 先に閉じた側は TIME-WAIT に残る
        run(&mut client, &mut server, 0);
        server.release(s);
        run(&mut client, &mut server, 0);
        assert_eq!(server.conn_count(), 0, "LAST-ACK のあと CLOSED で片付く");
        assert_eq!(client.conn_count(), 1);
        assert_eq!(client.poll_at(), Some(TIME_WAIT));
        client.output(TIME_WAIT);
        assert_eq!(client.conn_count(), 0);
        assert!(!client.owns_port(40000));
        assert!(server.owns_port(80), "待ち受けは残っている");
        server.unlisten(l);
        assert!(!server.owns_port(80));
    }

    #[test]
    fn unlisten_resets_connections_that_were_not_accepted() {
        let mut client = Engine::new(1, 4096);
        let mut server = Engine::new(2, 4096);
        let l = server.listen(ep(B, 80), 4);
        let c = client.connect(ep(A, 40000), ep(B, 80), 0);
        run(&mut client, &mut server, 0);
        server.unlisten(l);
        run(&mut client, &mut server, 0);
        assert_eq!(client.conn(c).unwrap().error(), Some(Errno::ECONNRESET));
        assert_eq!(server.conn_count(), 0);
    }
}
