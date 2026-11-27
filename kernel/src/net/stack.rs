//! ネットワークスタック:`NetDev`(ポート)と smoltcp(TCP/IP の実装)をつなぐ。
//!
//! ```text
//! ソケット(TCP/UDP)── smoltcp::Interface ── DevAdapter ── NetDev(ループバック、virtio-net…)
//! ```
//!
//! smoltcp は自分では動かない。`poll` を呼ぶたびに、届いたフレームを読んで処理し、
//! 送るべきもの(データ、ACK、再送)をデバイスに渡す。

use super::capture::Capture;
use alloc::boxed::Box;
use alloc::vec;
use alloc::vec::Vec;
use hal::net::{LinkType, NetDev};
use smoltcp::iface::{Config, Interface, SocketHandle, SocketSet};
use smoltcp::phy::{self, Device, DeviceCapabilities, Medium};
use smoltcp::socket::{dhcpv4, raw};
use smoltcp::time::Instant;
use smoltcp::wire::{
    EthernetAddress, HardwareAddress, IpCidr, IpProtocol, IpVersion, Ipv4Address, Ipv4Cidr,
};

/// インターフェースごとに取っておくパケットの記録の大きさ(中身の合計)
const CAPTURE_LIMIT: usize = 256 * 1024;

/// smoltcp の時刻(マイクロ秒)にする
pub fn instant(now_ns: u64) -> Instant {
    Instant::from_micros((now_ns / 1000) as i64)
}

/// `NetDev` を smoltcp の `Device` に見せる。行き来するフレームは `Capture` に記録する
struct DevAdapter<'a> {
    dev: &'a mut dyn NetDev,
    capture: &'a mut Capture,
}

/// 受け取ったフレーム(すでに読んで、記録してある)
struct RxToken(Vec<u8>);

/// 送る:smoltcp がバッファに書いたものを記録し、そのままデバイスに渡す
struct TxToken<'a> {
    dev: &'a mut dyn NetDev,
    capture: &'a mut Capture,
    at: Instant,
}

impl phy::RxToken for RxToken {
    fn consume<R, F: FnOnce(&[u8]) -> R>(self, f: F) -> R {
        f(&self.0)
    }
}

impl phy::TxToken for TxToken<'_> {
    fn consume<R, F: FnOnce(&mut [u8]) -> R>(self, len: usize, f: F) -> R {
        let mut buf = vec![0u8; len];
        let r = f(&mut buf);
        self.capture.record(self.at.total_micros() as u64, &buf);
        // 送れなかったフレームは捨てる(TCP なら再送される)
        let _ = self.dev.send(&buf);
        r
    }
}

/// リンク層のヘッダの大きさ
fn header_len(link: LinkType) -> usize {
    match link {
        LinkType::Ethernet => 14,
        LinkType::Ip => 0,
    }
}

impl Device for DevAdapter<'_> {
    type RxToken<'a>
        = RxToken
    where
        Self: 'a;
    type TxToken<'a>
        = TxToken<'a>
    where
        Self: 'a;

    fn receive(&mut self, t: Instant) -> Option<(Self::RxToken<'_>, Self::TxToken<'_>)> {
        let mut buf = vec![0u8; self.dev.mtu() + header_len(self.dev.link_type())];
        match self.dev.recv(&mut buf) {
            Ok(Some(n)) => {
                buf.truncate(n);
                self.capture.record(t.total_micros() as u64, &buf);
                let tx = TxToken {
                    dev: &mut *self.dev,
                    capture: &mut *self.capture,
                    at: t,
                };
                Some((RxToken(buf), tx))
            }
            _ => None,
        }
    }

    fn transmit(&mut self, t: Instant) -> Option<Self::TxToken<'_>> {
        Some(TxToken {
            dev: &mut *self.dev,
            capture: &mut *self.capture,
            at: t,
        })
    }

    fn capabilities(&self) -> DeviceCapabilities {
        let mut caps = DeviceCapabilities::default();
        caps.medium = match self.dev.link_type() {
            LinkType::Ethernet => Medium::Ethernet,
            LinkType::Ip => Medium::Ip,
        };
        caps.max_transmission_unit = self.dev.mtu() + header_len(self.dev.link_type());
        caps
    }
}

/// 1 つのデバイスと、その上の IP の設定とソケット
pub struct Stack {
    dev: Box<dyn NetDev>,
    pub iface: Interface,
    pub sockets: SocketSet<'static>,
    /// DHCP のクライアント(アドレスを DHCP でもらうときだけ)
    dhcp: Option<SocketHandle>,
    /// DNS サーバー(DHCP で教えてもらったもの)
    pub dns: Vec<Ipv4Address>,
    /// 行き来したフレームの記録(pcap)
    pub capture: Capture,
    /// TCP の raw ソケット(自作の TCP が使う。smoltcp はこれが受けた TCP には RST を返さない)
    raw: Option<SocketHandle>,
}

impl Stack {
    /// デバイスとアドレスから作る。`seed` は TCP の初期シーケンス番号などに使う乱数の種
    pub fn new(mut dev: Box<dyn NetDev>, addrs: &[IpCidr], seed: u64, now_ns: u64) -> Self {
        let hw = match dev.link_type() {
            LinkType::Ethernet => HardwareAddress::Ethernet(EthernetAddress(dev.mac())),
            LinkType::Ip => HardwareAddress::Ip,
        };
        let mut config = Config::new(hw);
        config.random_seed = seed;
        let mut capture = Capture::new(dev.link_type(), CAPTURE_LIMIT);
        let mut iface = Interface::new(
            config,
            &mut DevAdapter {
                dev: &mut *dev,
                capture: &mut capture,
            },
            instant(now_ns),
        );
        iface.update_ip_addrs(|a| {
            for cidr in addrs {
                let _ = a.push(*cidr);
            }
        });
        Self {
            dev,
            iface,
            sockets: SocketSet::new(Vec::new()),
            dhcp: None,
            dns: Vec::new(),
            capture,
            raw: None,
        }
    }

    /// 届いた TCP の IP パケットを、すべて raw ソケットでも受け取るようにする(自作の TCP のため)
    pub fn enable_raw_tcp(&mut self) {
        let buffer =
            || raw::PacketBuffer::new(vec![raw::PacketMetadata::EMPTY; 64], vec![0; 128 * 1024]);
        let s = raw::Socket::new(
            Some(IpVersion::Ipv4),
            Some(IpProtocol::Tcp),
            buffer(),
            buffer(),
        );
        self.raw = Some(self.sockets.add(s));
    }

    /// smoltcp の TCP のソケットが使っているポート(待ち受け、つながっているもの、閉じている途中のもの)
    pub fn tcp_ports(&self) -> impl Iterator<Item = u16> + '_ {
        self.sockets.iter().filter_map(|(_, s)| match s {
            smoltcp::socket::Socket::Tcp(t) if t.state() != smoltcp::socket::tcp::State::Closed => {
                Some(
                    t.local_endpoint()
                        .map_or(t.listen_endpoint().port, |e| e.port),
                )
            }
            _ => None,
        })
    }

    /// raw ソケットに届いた IP パケット(ヘッダから)を 1 つ取り出す
    pub fn recv_raw(&mut self) -> Option<Vec<u8>> {
        let s = self.sockets.get_mut::<raw::Socket>(self.raw?);
        s.recv().ok().map(|p| p.to_vec())
    }

    /// IP パケット(ヘッダから)を raw ソケットで送る(実際に出るのは次の poll)。いっぱいなら捨てる
    pub fn send_raw(&mut self, packet: &[u8]) {
        if let Some(h) = self.raw {
            let _ = self.sockets.get_mut::<raw::Socket>(h).send_slice(packet);
        }
    }

    /// アドレスを DHCP でもらう(DHCP のクライアントのソケットを足す。続きは `poll` の中で進む)
    pub fn start_dhcp(&mut self) {
        self.dhcp = Some(self.sockets.add(dhcpv4::Socket::new()));
    }

    /// アドレス、デフォルトルート、DNS を設定する(None ならアドレスとルートを外す)
    pub fn apply(
        &mut self,
        address: Option<Ipv4Cidr>,
        router: Option<Ipv4Address>,
        dns: &[Ipv4Address],
    ) {
        self.iface.update_ip_addrs(|addrs| {
            addrs.clear();
            if let Some(a) = address {
                let _ = addrs.push(IpCidr::Ipv4(a));
            }
        });
        let routes = self.iface.routes_mut();
        routes.remove_default_ipv4_route();
        if let Some(gw) = router {
            let _ = routes.add_default_ipv4_route(gw);
        }
        self.dns = dns.to_vec();
    }

    /// 次に poll が要るまでの時間(ナノ秒)。急ぎのものがなければ None
    pub fn poll_delay(&mut self, now_ns: u64) -> Option<u64> {
        self.iface
            .poll_delay(instant(now_ns), &self.sockets)
            .map(|d| d.total_micros() * 1000)
    }

    /// デフォルトルートがあるか
    pub fn has_default_route(&self) -> bool {
        self.iface.routes().get_default_ipv4_route().is_some()
    }

    /// 届いたものを処理し、送るものを送る。何か変わったら true
    pub fn poll(&mut self, now_ns: u64) -> bool {
        let changed = self.iface.poll(
            instant(now_ns),
            &mut DevAdapter {
                dev: &mut *self.dev,
                capture: &mut self.capture,
            },
            &mut self.sockets,
        );
        // DHCP:アドレスをもらえた(か、失った)ら、インターフェースに当てる
        if let Some(h) = self.dhcp {
            let event = match self.sockets.get_mut::<dhcpv4::Socket>(h).poll() {
                Some(dhcpv4::Event::Configured(c)) => {
                    Some((Some(c.address), c.router, c.dns_servers.to_vec()))
                }
                Some(dhcpv4::Event::Deconfigured) => Some((None, None, Vec::new())),
                None => None,
            };
            if let Some((address, router, dns)) = event {
                self.apply(address, router, &dns);
                return true;
            }
        }
        changed != smoltcp::iface::PollResult::None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::loopback::Loopback;
    use smoltcp::socket::tcp;
    use smoltcp::wire::{IpAddress, IpEndpoint};

    fn tcp_socket() -> tcp::Socket<'static> {
        tcp::Socket::new(
            tcp::SocketBuffer::new(vec![0; 4096]),
            tcp::SocketBuffer::new(vec![0; 4096]),
        )
    }

    #[test]
    fn a_dhcp_lease_sets_the_address_the_gateway_and_dns() {
        use smoltcp::wire::{Ipv4Address, Ipv4Cidr};
        let mut stack = Stack::new(Box::new(Loopback::new()), &[], 1, 0);
        assert_eq!(stack.iface.ipv4_addr(), None);
        let lease = Ipv4Cidr::new(Ipv4Address::new(10, 0, 2, 15), 24);
        let gw = Ipv4Address::new(10, 0, 2, 2);
        let dns = Ipv4Address::new(10, 0, 2, 3);
        stack.apply(Some(lease), Some(gw), &[dns]);
        assert_eq!(
            stack.iface.ipv4_addr(),
            Some(Ipv4Address::new(10, 0, 2, 15))
        );
        assert!(stack.has_default_route());
        assert_eq!(stack.dns, [dns]);
        // 借りたものを失った(期限切れなど)
        stack.apply(None, None, &[]);
        assert_eq!(stack.iface.ipv4_addr(), None);
        assert!(!stack.has_default_route());
        assert!(stack.dns.is_empty());
    }

    #[test]
    fn tcp_over_loopback_carries_bytes_both_ways() {
        let lo = Box::new(Loopback::new());
        let mut stack = Stack::new(lo, &[IpCidr::new(IpAddress::v4(127, 0, 0, 1), 8)], 42, 0);
        let mut server = tcp_socket();
        server.listen(7).unwrap();
        let server = stack.sockets.add(server);
        let client = stack.sockets.add(tcp_socket());
        let remote = IpEndpoint::new(IpAddress::v4(127, 0, 0, 1), 7);
        stack
            .sockets
            .get_mut::<tcp::Socket>(client)
            .connect(stack.iface.context(), remote, 49152)
            .unwrap();

        let mut now = 0u64;
        let mut step = |stack: &mut Stack| {
            now += 1_000_000;
            stack.poll(now);
        };
        // 3 ウェイ・ハンドシェイク
        for _ in 0..10 {
            step(&mut stack);
        }
        assert!(stack.sockets.get::<tcp::Socket>(client).may_send());
        assert!(stack.sockets.get::<tcp::Socket>(server).is_active());

        stack
            .sockets
            .get_mut::<tcp::Socket>(client)
            .send_slice(b"hello over tcp")
            .unwrap();
        for _ in 0..5 {
            step(&mut stack);
        }
        let mut buf = [0u8; 64];
        let n = stack
            .sockets
            .get_mut::<tcp::Socket>(server)
            .recv_slice(&mut buf)
            .unwrap();
        assert_eq!(&buf[..n], b"hello over tcp");

        stack
            .sockets
            .get_mut::<tcp::Socket>(server)
            .send_slice(b"reply")
            .unwrap();
        for _ in 0..5 {
            step(&mut stack);
        }
        let n = stack
            .sockets
            .get_mut::<tcp::Socket>(client)
            .recv_slice(&mut buf)
            .unwrap();
        assert_eq!(&buf[..n], b"reply");
    }

    #[test]
    fn sent_and_received_frames_are_recorded_as_pcap() {
        let lo = Box::new(Loopback::new());
        let mut stack = Stack::new(lo, &[IpCidr::new(IpAddress::v4(127, 0, 0, 1), 8)], 7, 0);
        let mut server = tcp_socket();
        server.listen(9).unwrap();
        stack.sockets.add(server);
        let client = stack.sockets.add(tcp_socket());
        let remote = IpEndpoint::new(IpAddress::v4(127, 0, 0, 1), 9);
        stack
            .sockets
            .get_mut::<tcp::Socket>(client)
            .connect(stack.iface.context(), remote, 49153)
            .unwrap();
        for i in 1..10 {
            stack.poll(i * 1_000_000);
        }
        // pcap の記録を頭から読む:(時刻のマイクロ秒, 中身)
        let p = stack.capture.pcap();
        let mut frames = Vec::new();
        let mut at = 24;
        while at < p.len() {
            let w = |i: usize| u32::from_le_bytes(p[at + i..at + i + 4].try_into().unwrap());
            let (us, len) = (w(0) as u64 * 1_000_000 + w(4) as u64, w(8) as usize);
            frames.push((us, p[at + 16..at + 16 + len].to_vec()));
            at += 16 + len;
        }
        // ループバックは送ったものがそのまま届くので、どれも 2 回ずつ記録される(送ったときと受け取ったとき)。
        // IPv4(0x45)の TCP で、最初は SYN(フラグは IP の 20 バイト + TCP の 13 バイト目)
        let flags: Vec<u8> = frames.iter().map(|(_, f)| f[33]).collect();
        assert!(frames.iter().all(|(_, f)| f[0] == 0x45 && f[9] == 6));
        assert_eq!(
            &flags[..6],
            &[0x02, 0x02, 0x12, 0x12, 0x10, 0x10],
            "{flags:x?}"
        );
        assert_eq!(frames[0].0, 1_000);
    }
}
