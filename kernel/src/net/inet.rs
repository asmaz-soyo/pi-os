//! AF_INET:IPv4 の TCP と UDP のソケット(中身は smoltcp)。
//!
//! インターフェースは何本でも持てる。0 番はループバック(127.0.0.0/8)、そのあとに
//! `attach` で足したもの(eth0 など)。smoltcp のソケットはインターフェースごとの SocketSet に入るので、
//! ソケットは「どのインターフェースの、どのハンドルか」([`H`])を覚える。
//! どのインターフェースを使うかは宛先で決める([`Net::route`])。0.0.0.0 での待ち受けは、
//! すべてのインターフェースで待つ。
//!
//! Linux のソケットと smoltcp のソケットは、待ち受けの考え方が違う。
//! - Linux:listen した 1 本のソケットが、何本もの接続を受ける(accept のたびに新しい fd)
//! - smoltcp:listen したソケットは、接続が来るとそのままその接続になる
//!
//! そこで listen では backlog の数だけ smoltcp のソケットを待たせておき、accept でつながったものを
//! 1 本渡して、代わりの 1 本を補充する。
//!
//! smoltcp は `poll` を呼んだときだけ動く。システムコールが待つあいだは自分で `poll` を呼び、
//! だれも待っていないときに届くもののために、カーネルのスレッドが定期的に [`poll_all`] を呼ぶ。

use super::stack::Stack;
use super::{
    Namespace, Options, ProtocolFamily, SOCK_DGRAM, SOCK_STREAM, Shutdown, Socket, split_type,
};
use crate::Errno;
use crate::fs::vfs::{File, FileKind, Stat};
use crate::sync::SpinLock;
use alloc::boxed::Box;
use alloc::collections::BTreeSet;
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use hal::net::NetDev;
use smoltcp::iface::SocketHandle;
use smoltcp::socket::{tcp, udp};
use smoltcp::wire::{IpAddress, IpEndpoint, IpListenEndpoint};
/// アドレスの型(インターフェースの設定と問い合わせで使う。smoltcp のものをそのまま見せる)
pub use smoltcp::wire::{IpCidr, Ipv4Address};

const AF_INET: u16 = super::AF_INET as u16;
/// 1 本のソケットの送り・受けのバッファの大きさ
const BUFFER: usize = 16 * 1024;
/// 待ち受けで待たせておくソケットの数の上限
const BACKLOG_MAX: usize = 8;
/// 自動で振るポートの範囲(Linux の既定の ip_local_port_range に近い)
const EPHEMERAL: core::ops::RangeInclusive<u16> = 49152..=65535;

/// smoltcp のソケットの居場所:(インターフェースの番号, その中のハンドル)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct H {
    iface: usize,
    handle: SocketHandle,
}

/// ループバックのインターフェースの番号
const LO: usize = 0;

/// ネットワーク全体
struct Net {
    /// 0 番がループバック
    stacks: Vec<Stack>,
    /// 使っているポート(TCP と UDP は別々。インターフェースをまたいで 1 つ)
    ports: BTreeSet<(Proto, u16)>,
    next_port: u16,
    /// 閉じた(close した)が、FIN のやりとりが残っているソケット
    closing: Vec<H>,
}

fn seed() -> u64 {
    crate::time::now_ns() ^ 0x9e37_79b9_7f4a_7c15
}

fn is_loopback(addr: IpAddress) -> bool {
    let IpAddress::Ipv4(ip) = addr;
    ip.octets()[0] == 127
}

impl Net {
    fn new() -> Self {
        let lo = Stack::new(
            Box::new(super::loopback::Loopback::new()),
            &[IpCidr::new(IpAddress::v4(127, 0, 0, 1), 8)],
            seed(),
            crate::time::now_ns(),
        );
        Self {
            stacks: vec![lo],
            ports: BTreeSet::new(),
            next_port: *EPHEMERAL.start(),
            closing: Vec::new(),
        }
    }

    /// 届いたものを処理しきるまで poll し、閉じ終わったソケットを片付ける
    fn poll(&mut self) {
        for _ in 0..32 {
            if !self.poll_once() {
                break;
            }
        }
    }

    /// すべてのインターフェースを 1 回ずつ poll し、閉じ終わったソケットを片付ける。何か変わったら true
    fn poll_once(&mut self) -> bool {
        let now = crate::time::now_ns();
        let mut changed = false;
        for stack in &mut self.stacks {
            changed |= stack.poll(now);
        }
        let stacks = &mut self.stacks;
        self.closing.retain(|&h| {
            let sockets = &mut stacks[h.iface].sockets;
            let done = matches!(
                sockets.get::<tcp::Socket>(h.handle).state(),
                tcp::State::Closed | tcp::State::TimeWait
            );
            if done {
                sockets.remove(h.handle);
            }
            !done
        });
        changed
    }

    fn tcp(&mut self, h: H) -> &mut tcp::Socket<'static> {
        self.stacks[h.iface]
            .sockets
            .get_mut::<tcp::Socket>(h.handle)
    }

    fn udp(&mut self, h: H) -> &mut udp::Socket<'static> {
        self.stacks[h.iface]
            .sockets
            .get_mut::<udp::Socket>(h.handle)
    }

    fn add<S: smoltcp::socket::AnySocket<'static>>(&mut self, iface: usize, s: S) -> H {
        H {
            iface,
            handle: self.stacks[iface].sockets.add(s),
        }
    }

    fn remove(&mut self, h: H) {
        self.stacks[h.iface].sockets.remove(h.handle);
    }

    /// 宛先 `dst` に送るインターフェース。127/8 はループバック、それ以外は同じネットワークにいる
    /// インターフェース、なければデフォルトルートのあるインターフェース
    fn route(&self, dst: IpAddress) -> Result<usize, Errno> {
        if dst.is_unspecified() {
            return Err(Errno::ENETUNREACH);
        }
        if is_loopback(dst) {
            return Ok(LO);
        }
        let others = || self.stacks.iter().enumerate().skip(1);
        if let Some((i, _)) =
            others().find(|(_, s)| s.iface.ip_addrs().iter().any(|c| c.contains_addr(&dst)))
        {
            return Ok(i);
        }
        others()
            .find(|(_, s)| s.has_default_route())
            .map(|(i, _)| i)
            .ok_or(Errno::ENETUNREACH)
    }

    /// bind できるアドレスなら、それを持つインターフェース(0.0.0.0 なら None = すべて)
    fn owner(&self, addr: IpAddress) -> Result<Option<usize>, Errno> {
        if addr.is_unspecified() {
            return Ok(None);
        }
        if is_loopback(addr) {
            return Ok(Some(LO));
        }
        self.stacks
            .iter()
            .position(|s| s.iface.has_ip_addr(addr))
            .map(Some)
            .ok_or(Errno::EADDRNOTAVAIL)
    }

    /// `addr` で待つインターフェースの一覧
    fn ifaces_for(&self, addr: IpAddress) -> Result<Vec<usize>, Errno> {
        Ok(match self.owner(addr)? {
            Some(i) => vec![i],
            None => (0..self.stacks.len()).collect(),
        })
    }

    /// 空いているポートを 1 つ振る
    fn ephemeral(&mut self, proto: Proto) -> Result<u16, Errno> {
        for _ in EPHEMERAL {
            let port = self.next_port;
            self.next_port = if port == *EPHEMERAL.end() {
                *EPHEMERAL.start()
            } else {
                port + 1
            };
            if self.ports.insert((proto, port)) {
                return Ok(port);
            }
        }
        Err(Errno::EADDRINUSE)
    }

    fn new_tcp(&mut self) -> tcp::Socket<'static> {
        let mut s = tcp::Socket::new(
            tcp::SocketBuffer::new(vec![0; BUFFER]),
            tcp::SocketBuffer::new(vec![0; BUFFER]),
        );
        // ACK をまとめず、小さいものもすぐ送る(対話的な使い方で待たせない)
        s.set_ack_delay(None);
        s.set_nagle_enabled(false);
        s
    }

    /// インターフェース `iface` に、待ち受けのソケットを 1 本足す
    fn add_listener(&mut self, iface: usize, local: IpEndpoint) -> Result<H, Errno> {
        let mut s = self.new_tcp();
        let addr = (!local.addr.is_unspecified()).then_some(local.addr);
        s.listen(IpListenEndpoint {
            addr,
            port: local.port,
        })
        .map_err(|_| Errno::EINVAL)?;
        Ok(self.add(iface, s))
    }
}

/// ポートの持ち主のプロトコル
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Proto {
    Tcp,
    Udp,
}

static NET: SpinLock<Option<Net>> = SpinLock::new(None);

/// インターフェースを足す。`addr` があればそのアドレスを、`gateway` があればデフォルトルートを設定する。
/// `addr` が None なら、アドレスなどは DHCP でもらう。返すのはインターフェースの番号
pub fn attach(dev: Box<dyn NetDev>, addr: Option<IpCidr>, gateway: Option<Ipv4Address>) -> usize {
    let mut guard = NET.lock();
    let net = guard.get_or_insert_with(Net::new);
    let addrs: Vec<IpCidr> = addr.into_iter().collect();
    let mut stack = Stack::new(dev, &addrs, seed(), crate::time::now_ns());
    if let Some(gw) = gateway {
        let _ = stack.iface.routes_mut().add_default_ipv4_route(gw);
    }
    if addr.is_none() {
        stack.start_dhcp();
    }
    net.stacks.push(stack);
    net.stacks.len() - 1
}

/// すべてのインターフェースを 1 巡だけ poll する(届いたパケットを処理し、再送などをする)。
/// カーネルのスレッドが定期的に呼ぶ。返すのは、次に poll が要るまでの時間(ナノ秒、`max_ns` まで。
/// まだやることがあれば 0)。長く続けて走らないように、1 巡で止める
pub fn poll_all(max_ns: u64) -> u64 {
    let mut guard = NET.lock();
    let Some(net) = guard.as_mut() else {
        return max_ns;
    };
    if net.poll_once() {
        return 0;
    }
    let now = crate::time::now_ns();
    net.stacks
        .iter_mut()
        .filter_map(|s| s.poll_delay(now))
        .fold(max_ns, u64::min)
}

/// インターフェース `iface` の DNS サーバー(DHCP で教えてもらったもの)
pub fn dns_of(iface: usize) -> Vec<Ipv4Address> {
    NET.lock()
        .as_ref()
        .and_then(|n| n.stacks.get(iface))
        .map(|s| s.dns.clone())
        .unwrap_or_default()
}

/// すべてのインターフェースの DNS サーバー(重なりは1つにする)
pub fn dns_servers() -> Vec<Ipv4Address> {
    let mut all: Vec<Ipv4Address> = Vec::new();
    if let Some(n) = NET.lock().as_ref() {
        for s in n.stacks.iter().flat_map(|s| s.dns.iter()) {
            if !all.contains(s) {
                all.push(*s);
            }
        }
    }
    all
}

/// インターフェース `iface` の IPv4 アドレス
pub fn ipv4_of(iface: usize) -> Option<Ipv4Address> {
    NET.lock()
        .as_ref()
        .and_then(|n| n.stacks.get(iface))
        .and_then(|s| s.iface.ipv4_addr())
}

/// ネットワークを poll してから `f` を呼ぶ(呼んだあとも poll して、書いたものを送り出す)
fn with<R>(f: impl FnOnce(&mut Net) -> R) -> R {
    let mut guard = NET.lock();
    let net = guard.get_or_insert_with(Net::new);
    net.poll();
    let r = f(net);
    net.poll();
    r
}

/// `f` が答え(Some)を返すまで、poll しながら待つ
fn wait<R>(
    nonblock: bool,
    mut f: impl FnMut(&mut Net) -> Option<Result<R, Errno>>,
) -> Result<R, Errno> {
    loop {
        if let Some(r) = with(&mut f) {
            return r;
        }
        if nonblock {
            return Err(Errno::EAGAIN);
        }
        crate::sched::relax();
    }
}

pub struct Inet;

impl ProtocolFamily for Inet {
    fn socket(&self, ty: u32, protocol: u32) -> Result<Arc<dyn File>, Errno> {
        const IPPROTO_TCP: u32 = 6;
        const IPPROTO_UDP: u32 = 17;
        let (kind, _) = split_type(ty)?;
        match (kind, protocol) {
            (SOCK_STREAM, 0 | IPPROTO_TCP) => Ok(Arc::new(InetSocket {
                state: SpinLock::new(State::Idle { local: None }),
                options: Options::default(),
            })),
            (SOCK_DGRAM, 0 | IPPROTO_UDP) => Ok(Arc::new(UdpSocket {
                state: SpinLock::new(UdpState::default()),
                options: Options::default(),
            })),
            (SOCK_STREAM | SOCK_DGRAM, _) => Err(Errno::EPROTONOSUPPORT),
            _ => Err(Errno::ESOCKTNOSUPPORT),
        }
    }
}

/// sockaddr_in を読む
fn parse(raw: &[u8]) -> Result<IpEndpoint, Errno> {
    if raw.len() < 16 {
        return Err(Errno::EINVAL);
    }
    if u16::from_le_bytes([raw[0], raw[1]]) != AF_INET {
        return Err(Errno::EAFNOSUPPORT);
    }
    let port = u16::from_be_bytes([raw[2], raw[3]]);
    let ip = Ipv4Address::new(raw[4], raw[5], raw[6], raw[7]);
    Ok(IpEndpoint::new(IpAddress::Ipv4(ip), port))
}

/// sockaddr_in にする(16 バイト)
fn to_bytes(ep: IpEndpoint) -> Vec<u8> {
    let mut b = vec![0u8; 16];
    b[..2].copy_from_slice(&AF_INET.to_le_bytes());
    b[2..4].copy_from_slice(&ep.port.to_be_bytes());
    let IpAddress::Ipv4(ip) = ep.addr;
    b[4..8].copy_from_slice(&ip.octets());
    b
}

fn unspecified() -> IpEndpoint {
    IpEndpoint::new(IpAddress::Ipv4(Ipv4Address::UNSPECIFIED), 0)
}

enum State {
    /// 作っただけ(bind していれば local がある。ポートはこのソケットのもの)
    Idle { local: Option<IpEndpoint> },
    /// 待ち受け中。pending は待たせている smoltcp のソケット
    Listening { local: IpEndpoint, pending: Vec<H> },
    /// つながった。`owns_port`:ポートをこのソケットが持っているか(accept したものは待ち受け側のもの)
    Connected {
        handle: H,
        local: IpEndpoint,
        peer: IpEndpoint,
        owns_port: bool,
        read_shut: bool,
    },
}

pub struct InetSocket {
    state: SpinLock<State>,
    options: Options,
}

impl InetSocket {
    /// つながったソケットの (handle, 読む側を閉じたか)
    fn connection(&self) -> Result<(H, bool), Errno> {
        match &*self.state.lock() {
            State::Connected {
                handle, read_shut, ..
            } => Ok((*handle, *read_shut)),
            _ => Err(Errno::ENOTCONN),
        }
    }
}

/// 待たせているソケットのうち、つながったもの(の位置)
fn ready(net: &mut Net, pending: &[H]) -> Option<usize> {
    pending.iter().position(|&h| {
        !matches!(
            net.tcp(h).state(),
            tcp::State::Listen | tcp::State::SynReceived | tcp::State::Closed
        )
    })
}

impl Socket for InetSocket {
    fn sock_type(&self) -> u32 {
        SOCK_STREAM
    }

    fn options(&self) -> &Options {
        &self.options
    }

    fn bind(&self, _ns: &mut dyn Namespace, raw: &[u8]) -> Result<(), Errno> {
        let mut ep = parse(raw)?;
        let mut state = self.state.lock();
        if !matches!(*state, State::Idle { local: None }) {
            return Err(Errno::EINVAL);
        }
        with(|net| {
            net.owner(ep.addr)?;
            if ep.port == 0 {
                ep.port = net.ephemeral(Proto::Tcp)?;
            } else if !net.ports.insert((Proto::Tcp, ep.port)) {
                return Err(Errno::EADDRINUSE);
            }
            Ok(())
        })?;
        *state = State::Idle { local: Some(ep) };
        Ok(())
    }

    fn listen(&self, backlog: usize) -> Result<(), Errno> {
        let mut state = self.state.lock();
        let local = match &*state {
            State::Idle { local } => *local,
            State::Listening { .. } => return Ok(()),
            State::Connected { .. } => return Err(Errno::EINVAL),
        };
        let n = backlog.clamp(1, BACKLOG_MAX);
        let (local, pending) = with(|net| {
            // bind していなければ、ポートを自動で振る(Linux と同じ)
            let local = match local {
                Some(l) => l,
                None => IpEndpoint::new(unspecified().addr, net.ephemeral(Proto::Tcp)?),
            };
            let mut pending = Vec::new();
            for iface in net.ifaces_for(local.addr)? {
                for _ in 0..n {
                    pending.push(net.add_listener(iface, local)?);
                }
            }
            Ok::<_, Errno>((local, pending))
        })?;
        *state = State::Listening { local, pending };
        Ok(())
    }

    fn accept(&self, nonblock: bool) -> Result<(Arc<dyn File>, Vec<u8>), Errno> {
        if !matches!(*self.state.lock(), State::Listening { .. }) {
            return Err(Errno::EINVAL);
        }
        wait(nonblock, |net| {
            let mut state = self.state.lock();
            let State::Listening { local, pending } = &mut *state else {
                return Some(Err(Errno::EINVAL));
            };
            let i = ready(net, pending)?;
            // つながったものを渡し、代わりを補充する
            let handle = pending.remove(i);
            match net.add_listener(handle.iface, *local) {
                Ok(h) => pending.push(h),
                Err(e) => return Some(Err(e)),
            }
            let s = net.tcp(handle);
            let me = s.local_endpoint().unwrap_or(*local);
            let peer = s.remote_endpoint().unwrap_or(unspecified());
            let file: Arc<dyn File> = Arc::new(InetSocket {
                state: SpinLock::new(State::Connected {
                    handle,
                    local: me,
                    peer,
                    owns_port: false,
                    read_shut: false,
                }),
                options: Options::default(),
            });
            Some(Ok((file, to_bytes(peer))))
        })
    }

    fn connect(&self, _ns: &mut dyn Namespace, raw: &[u8], _nonblock: bool) -> Result<(), Errno> {
        let remote = parse(raw)?;
        let local = match &*self.state.lock() {
            State::Idle { local } => *local,
            State::Listening { .. } => return Err(Errno::EINVAL),
            State::Connected { .. } => return Err(Errno::EISCONN),
        };
        let (handle, port) = with(|net| {
            let iface = net.route(remote.addr)?;
            let port = match local {
                Some(l) => l.port,
                None => net.ephemeral(Proto::Tcp)?,
            };
            let s = net.new_tcp();
            let handle = net.add(iface, s);
            let stack = &mut net.stacks[iface];
            let cx = stack.iface.context();
            let r = stack
                .sockets
                .get_mut::<tcp::Socket>(handle.handle)
                .connect(cx, remote, port);
            if r.is_err() {
                net.remove(handle);
                if local.is_none() {
                    net.ports.remove(&(Proto::Tcp, port));
                }
                return Err(Errno::EINVAL);
            }
            Ok((handle, port))
        })?;
        // 3 ウェイ・ハンドシェイクを待つ
        let result = wait(false, |net| match net.tcp(handle).state() {
            tcp::State::SynSent | tcp::State::SynReceived => None,
            tcp::State::Closed => Some(Err(Errno::ECONNREFUSED)),
            _ => Some(Ok(net.tcp(handle).local_endpoint())),
        });
        match result {
            Ok(me) => {
                let me = me.unwrap_or(IpEndpoint::new(remote.addr, port));
                *self.state.lock() = State::Connected {
                    handle,
                    local: me,
                    peer: remote,
                    owns_port: true,
                    read_shut: false,
                };
                Ok(())
            }
            Err(e) => {
                with(|net| {
                    net.remove(handle);
                    if local.is_none() {
                        net.ports.remove(&(Proto::Tcp, port));
                    }
                });
                // bind していたなら、そのポートはまだこのソケットのもの
                Err(e)
            }
        }
    }

    fn local_addr(&self) -> Vec<u8> {
        to_bytes(match &*self.state.lock() {
            State::Idle { local } => local.unwrap_or(unspecified()),
            State::Listening { local, .. } => *local,
            State::Connected { local, .. } => *local,
        })
    }

    fn peer_addr(&self) -> Result<Vec<u8>, Errno> {
        match &*self.state.lock() {
            State::Connected { peer, .. } => Ok(to_bytes(*peer)),
            _ => Err(Errno::ENOTCONN),
        }
    }

    fn shutdown(&self, how: Shutdown) -> Result<(), Errno> {
        let mut state = self.state.lock();
        let State::Connected {
            handle, read_shut, ..
        } = &mut *state
        else {
            return Err(Errno::ENOTCONN);
        };
        if how != Shutdown::Write {
            *read_shut = true;
        }
        if how != Shutdown::Read {
            let h = *handle;
            with(|net| net.tcp(h).close()); // FIN を送る
        }
        Ok(())
    }
}

impl File for InetSocket {
    fn stat(&self) -> Stat {
        Stat {
            kind: FileKind::Socket,
            mode: 0o777,
            ino: 0x6_1000,
            ..Default::default()
        }
    }

    fn read(&self, _offset: u64, buf: &mut [u8]) -> Result<usize, Errno> {
        let (h, read_shut) = self.connection()?;
        if read_shut || buf.is_empty() {
            return Ok(0);
        }
        wait(false, |net| {
            let s = net.tcp(h);
            if s.can_recv() {
                return Some(s.recv_slice(buf).map_err(|_| Errno::ECONNRESET));
            }
            // 相手が FIN を送った(か、接続が終わった):終わり
            if !s.may_recv() {
                return Some(Ok(0));
            }
            None
        })
    }

    fn write(&self, _offset: u64, buf: &[u8]) -> Result<usize, Errno> {
        // つながっていない TCP に書くのは EPIPE(Linux と同じ。AF_UNIX は ENOTCONN)
        let (h, _) = self.connection().map_err(|_| Errno::EPIPE)?;
        let mut done = 0;
        wait(false, |net| {
            let s = net.tcp(h);
            if !s.may_send() {
                // 自分が shutdown した、相手が切った
                return Some(if done > 0 {
                    Ok(done)
                } else {
                    Err(Errno::EPIPE)
                });
            }
            if s.can_send() {
                done += s.send_slice(&buf[done..]).unwrap_or(0);
            }
            (done == buf.len()).then_some(Ok(done))
        })
    }

    fn poll_readable(&self) -> bool {
        match &*self.state.lock() {
            State::Connected {
                handle, read_shut, ..
            } => {
                let h = *handle;
                *read_shut
                    || with(|net| {
                        let s = net.tcp(h);
                        s.can_recv() || !s.may_recv()
                    })
            }
            State::Listening { pending, .. } => with(|net| ready(net, pending).is_some()),
            State::Idle { .. } => false,
        }
    }

    fn as_socket(&self) -> Option<&dyn Socket> {
        Some(self)
    }
}

impl Drop for InetSocket {
    fn drop(&mut self) {
        let state = core::mem::replace(self.state.get_mut(), State::Idle { local: None });
        let mut guard = NET.lock();
        let Some(net) = guard.as_mut() else {
            return;
        };
        match state {
            State::Idle { local } => {
                if let Some(l) = local {
                    net.ports.remove(&(Proto::Tcp, l.port));
                }
            }
            State::Listening { local, pending } => {
                for h in pending {
                    net.remove(h);
                }
                net.ports.remove(&(Proto::Tcp, local.port));
            }
            State::Connected {
                handle,
                local,
                owns_port,
                ..
            } => {
                // FIN を送り、やりとりが終わるまで poll で面倒を見る
                net.tcp(handle).close();
                net.closing.push(handle);
                if owns_port {
                    net.ports.remove(&(Proto::Tcp, local.port));
                }
            }
        }
        net.poll();
    }
}

/// UDP のソケットの状態。smoltcp のソケットは、bind したとき(か、最初に送ったとき)に作る
#[derive(Default)]
struct UdpState {
    /// smoltcp のソケット(0.0.0.0 で bind したら、インターフェースごとに 1 つずつ)
    handles: Vec<H>,
    local: Option<IpEndpoint>,
    /// connect した相手(write の宛先)
    peer: Option<IpEndpoint>,
}

pub struct UdpSocket {
    state: SpinLock<UdpState>,
    options: Options,
}

/// 受け取りで覚えておけるデータグラムの数と、中身の合計
const UDP_PACKETS: usize = 32;

impl UdpState {
    /// smoltcp のソケットを作って bind する。`ep` のポートが 0 なら自動で振る
    fn open(&mut self, net: &mut Net, mut ep: IpEndpoint) -> Result<(), Errno> {
        let ifaces = net.ifaces_for(ep.addr)?;
        if ep.port == 0 {
            ep.port = net.ephemeral(Proto::Udp)?;
        } else if !net.ports.insert((Proto::Udp, ep.port)) {
            return Err(Errno::EADDRINUSE);
        }
        let addr = (!ep.addr.is_unspecified()).then_some(ep.addr);
        for iface in ifaces {
            let mut s = udp::Socket::new(
                udp::PacketBuffer::new(
                    vec![udp::PacketMetadata::EMPTY; UDP_PACKETS],
                    vec![0; BUFFER],
                ),
                udp::PacketBuffer::new(
                    vec![udp::PacketMetadata::EMPTY; UDP_PACKETS],
                    vec![0; BUFFER],
                ),
            );
            s.bind(IpListenEndpoint {
                addr,
                port: ep.port,
            })
            .map_err(|_| Errno::EINVAL)?;
            let h = net.add(iface, s);
            self.handles.push(h);
        }
        self.local = Some(ep);
        Ok(())
    }

    /// bind していなければ、自動で bind する
    fn ensure_open(&mut self, net: &mut Net) -> Result<(), Errno> {
        if self.handles.is_empty() {
            self.open(net, unspecified())?;
        }
        Ok(())
    }

    /// `dest` に送るときに使うソケット
    fn handle_for(&self, net: &Net, dest: IpAddress) -> Result<H, Errno> {
        let iface = net.route(dest)?;
        self.handles
            .iter()
            .find(|h| h.iface == iface)
            .copied()
            .ok_or(Errno::ENETUNREACH)
    }
}

impl Socket for UdpSocket {
    fn sock_type(&self) -> u32 {
        SOCK_DGRAM
    }

    fn options(&self) -> &Options {
        &self.options
    }

    fn bind(&self, _ns: &mut dyn Namespace, raw: &[u8]) -> Result<(), Errno> {
        let ep = parse(raw)?;
        let mut state = self.state.lock();
        if !state.handles.is_empty() {
            return Err(Errno::EINVAL);
        }
        with(|net| state.open(net, ep))
    }

    fn listen(&self, _backlog: usize) -> Result<(), Errno> {
        Err(Errno::EOPNOTSUPP)
    }

    fn accept(&self, _nonblock: bool) -> Result<(Arc<dyn File>, Vec<u8>), Errno> {
        Err(Errno::EOPNOTSUPP)
    }

    /// UDP の connect は、宛先を覚えるだけ(パケットは送らない)
    fn connect(&self, _ns: &mut dyn Namespace, raw: &[u8], _nonblock: bool) -> Result<(), Errno> {
        let remote = parse(raw)?;
        let mut state = self.state.lock();
        with(|net| {
            net.route(remote.addr)?;
            state.ensure_open(net)
        })?;
        state.peer = Some(remote);
        Ok(())
    }

    fn local_addr(&self) -> Vec<u8> {
        to_bytes(self.state.lock().local.unwrap_or(unspecified()))
    }

    fn peer_addr(&self) -> Result<Vec<u8>, Errno> {
        self.state.lock().peer.map(to_bytes).ok_or(Errno::ENOTCONN)
    }

    fn shutdown(&self, _how: Shutdown) -> Result<(), Errno> {
        if self.state.lock().peer.is_none() {
            return Err(Errno::ENOTCONN);
        }
        Ok(())
    }

    fn send_to(&self, buf: &[u8], to: Option<&[u8]>, nonblock: bool) -> Result<usize, Errno> {
        let (h, dest) = {
            let mut state = self.state.lock();
            let dest = match to {
                Some(raw) => parse(raw)?,
                None => state.peer.ok_or(Errno::EDESTADDRREQ)?,
            };
            let h = with(|net| {
                state.ensure_open(net)?;
                state.handle_for(net, dest.addr)
            })?;
            (h, dest)
        };
        if buf.len() > BUFFER {
            return Err(Errno::EMSGSIZE);
        }
        wait(nonblock, |net| {
            let s = net.udp(h);
            match s.send_slice(buf, dest) {
                Ok(()) => Some(Ok(buf.len())),
                Err(udp::SendError::BufferFull) => None,
                Err(udp::SendError::Unaddressable) => Some(Err(Errno::ENETUNREACH)),
            }
        })
    }

    fn recv_from(&self, buf: &mut [u8], nonblock: bool) -> Result<(usize, Vec<u8>), Errno> {
        let handles = {
            let mut state = self.state.lock();
            with(|net| state.ensure_open(net))?;
            state.handles.clone()
        };
        wait(nonblock, |net| {
            let h = *handles.iter().find(|&&h| net.udp(h).can_recv())?;
            let (data, meta) = net.udp(h).recv().ok()?;
            // バッファより長いデータグラムは切る(残りは捨てる。Linux と同じ)
            let n = data.len().min(buf.len());
            buf[..n].copy_from_slice(&data[..n]);
            Some(Ok((n, to_bytes(meta.endpoint))))
        })
    }
}

impl File for UdpSocket {
    fn stat(&self) -> Stat {
        Stat {
            kind: FileKind::Socket,
            mode: 0o777,
            ino: 0x6_2000,
            ..Default::default()
        }
    }

    fn read(&self, _offset: u64, buf: &mut [u8]) -> Result<usize, Errno> {
        Ok(self.recv_from(buf, false)?.0)
    }

    fn write(&self, _offset: u64, buf: &[u8]) -> Result<usize, Errno> {
        self.send_to(buf, None, false)
    }

    fn poll_readable(&self) -> bool {
        let handles = self.state.lock().handles.clone();
        !handles.is_empty() && with(|net| handles.iter().any(|&h| net.udp(h).can_recv()))
    }

    fn as_socket(&self) -> Option<&dyn Socket> {
        Some(self)
    }
}

impl Drop for UdpSocket {
    fn drop(&mut self) {
        let state = core::mem::take(self.state.get_mut());
        let Some(local) = state.local else {
            return;
        };
        if let Some(net) = NET.lock().as_mut() {
            for h in state.handles {
                net.remove(h);
            }
            net.ports.remove(&(Proto::Udp, local.port));
        }
    }
}
