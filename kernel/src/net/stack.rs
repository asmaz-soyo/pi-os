//! ネットワークスタック:`NetDev`(ポート)と smoltcp(TCP/IP の実装)をつなぐ。
//!
//! ```text
//! ソケット(TCP/UDP)── smoltcp::Interface ── DevAdapter ── NetDev(ループバック、virtio-net…)
//! ```
//!
//! smoltcp は自分では動かない。`poll` を呼ぶたびに、届いたフレームを読んで処理し、
//! 送るべきもの(データ、ACK、再送)をデバイスに渡す。

use alloc::boxed::Box;
use alloc::vec;
use alloc::vec::Vec;
use hal::net::{LinkType, NetDev};
use smoltcp::iface::{Config, Interface, SocketSet};
use smoltcp::phy::{self, Device, DeviceCapabilities, Medium};
use smoltcp::time::Instant;
use smoltcp::wire::{EthernetAddress, HardwareAddress, IpCidr};

/// smoltcp の時刻(マイクロ秒)にする
pub fn instant(now_ns: u64) -> Instant {
    Instant::from_micros((now_ns / 1000) as i64)
}

/// `NetDev` を smoltcp の `Device` に見せる
struct DevAdapter<'a>(&'a mut dyn NetDev);

/// 受け取ったフレーム(すでに読んである)
struct RxToken(Vec<u8>);

/// 送る:smoltcp がバッファに書いたものを、そのままデバイスに渡す
struct TxToken<'a>(&'a mut dyn NetDev);

impl phy::RxToken for RxToken {
    fn consume<R, F: FnOnce(&[u8]) -> R>(self, f: F) -> R {
        f(&self.0)
    }
}

impl phy::TxToken for TxToken<'_> {
    fn consume<R, F: FnOnce(&mut [u8]) -> R>(self, len: usize, f: F) -> R {
        let mut buf = vec![0u8; len];
        let r = f(&mut buf);
        // 送れなかったフレームは捨てる(TCP なら再送される)
        let _ = self.0.send(&buf);
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

    fn receive(&mut self, _t: Instant) -> Option<(Self::RxToken<'_>, Self::TxToken<'_>)> {
        let mut buf = vec![0u8; self.0.mtu() + header_len(self.0.link_type())];
        match self.0.recv(&mut buf) {
            Ok(Some(n)) => {
                buf.truncate(n);
                Some((RxToken(buf), TxToken(&mut *self.0)))
            }
            _ => None,
        }
    }

    fn transmit(&mut self, _t: Instant) -> Option<Self::TxToken<'_>> {
        Some(TxToken(&mut *self.0))
    }

    fn capabilities(&self) -> DeviceCapabilities {
        let mut caps = DeviceCapabilities::default();
        caps.medium = match self.0.link_type() {
            LinkType::Ethernet => Medium::Ethernet,
            LinkType::Ip => Medium::Ip,
        };
        caps.max_transmission_unit = self.0.mtu() + header_len(self.0.link_type());
        caps
    }
}

/// 1 つのデバイスと、その上の IP の設定とソケット
pub struct Stack {
    dev: Box<dyn NetDev>,
    pub iface: Interface,
    pub sockets: SocketSet<'static>,
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
        let mut iface = Interface::new(config, &mut DevAdapter(&mut *dev), instant(now_ns));
        iface.update_ip_addrs(|a| {
            for cidr in addrs {
                let _ = a.push(*cidr);
            }
        });
        Self {
            dev,
            iface,
            sockets: SocketSet::new(Vec::new()),
        }
    }

    /// 届いたものを処理し、送るものを送る。何か変わったら true
    pub fn poll(&mut self, now_ns: u64) -> bool {
        let changed = self.iface.poll(
            instant(now_ns),
            &mut DevAdapter(&mut *self.dev),
            &mut self.sockets,
        );
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
}
