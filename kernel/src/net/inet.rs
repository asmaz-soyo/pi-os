//! AF_INET:IPv4 の TCP と UDP のソケット(中身は smoltcp)。今はループバック(127.0.0.0/8)だけ。
//!
//! Linux のソケットと smoltcp のソケットは、待ち受けの考え方が違う。
//! - Linux:listen した 1 本のソケットが、何本もの接続を受ける(accept のたびに新しい fd)
//! - smoltcp:listen したソケットは、接続が来るとそのままその接続になる
//!
//! そこで listen では backlog の数だけ smoltcp のソケットを待たせておき、accept でつながったものを
//! 1 本渡して、代わりの 1 本を補充する。
//!
//! smoltcp は `poll` を呼んだときだけ動く。今はシステムコールが待つあいだ、自分で `poll` を呼ぶ
//! (ループバックは、送った側か受ける側のどちらかが必ず待っているので、これで回る)。

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
use smoltcp::iface::SocketHandle;
use smoltcp::socket::{tcp, udp};
use smoltcp::wire::{IpAddress, IpCidr, IpEndpoint, IpListenEndpoint, Ipv4Address};

const AF_INET: u16 = super::AF_INET as u16;
/// 1 本のソケットの送り・受けのバッファの大きさ
const BUFFER: usize = 16 * 1024;
/// 待ち受けで待たせておくソケットの数の上限
const BACKLOG_MAX: usize = 8;
/// 自動で振るポートの範囲(Linux の既定の ip_local_port_range に近い)
const EPHEMERAL: core::ops::RangeInclusive<u16> = 49152..=65535;

/// ネットワーク全体(今はループバックだけ)
struct Net {
    lo: Stack,
    /// 使っているポート(TCP と UDP は別々)
    ports: BTreeSet<(Proto, u16)>,
    next_port: u16,
    /// 閉じた(close した)が、FIN のやりとりが残っているソケット
    closing: Vec<SocketHandle>,
}

impl Net {
    fn new() -> Self {
        let now = crate::time::now_ns();
        let lo = Stack::new(
            Box::new(super::loopback::Loopback::new()),
            &[IpCidr::new(IpAddress::v4(127, 0, 0, 1), 8)],
            now ^ 0x9e37_79b9_7f4a_7c15,
            now,
        );
        Self {
            lo,
            ports: BTreeSet::new(),
            next_port: *EPHEMERAL.start(),
            closing: Vec::new(),
        }
    }

    /// 届いたものを処理しきるまで poll し、閉じ終わったソケットを片付ける
    fn poll(&mut self) {
        let now = crate::time::now_ns();
        for _ in 0..32 {
            if !self.lo.poll(now) {
                break;
            }
        }
        let sockets = &mut self.lo.sockets;
        self.closing.retain(|&h| {
            let done = matches!(
                sockets.get::<tcp::Socket>(h).state(),
                tcp::State::Closed | tcp::State::TimeWait
            );
            if done {
                sockets.remove(h);
            }
            !done
        });
    }

    fn tcp(&mut self, h: SocketHandle) -> &mut tcp::Socket<'static> {
        self.lo.sockets.get_mut::<tcp::Socket>(h)
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
        // ループバックでは待つ理由がない:ACK をまとめず、小さいものもすぐ送る
        s.set_ack_delay(None);
        s.set_nagle_enabled(false);
        s
    }

    /// 待ち受けのソケットを 1 本足す
    fn add_listener(&mut self, local: IpEndpoint) -> Result<SocketHandle, Errno> {
        let mut s = self.new_tcp();
        let addr = (!local.addr.is_unspecified()).then_some(local.addr);
        s.listen(IpListenEndpoint {
            addr,
            port: local.port,
        })
        .map_err(|_| Errno::EINVAL)?;
        Ok(self.lo.sockets.add(s))
    }
}

/// ポートの持ち主のプロトコル
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Proto {
    Tcp,
    Udp,
}

static NET: SpinLock<Option<Net>> = SpinLock::new(None);

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

/// このマシンのアドレスか(今はループバックだけ)
fn is_local(addr: IpAddress) -> bool {
    let IpAddress::Ipv4(ip) = addr;
    ip.is_unspecified() || ip.octets()[0] == 127
}

enum State {
    /// 作っただけ(bind していれば local がある。ポートはこのソケットのもの)
    Idle { local: Option<IpEndpoint> },
    /// 待ち受け中。pending は待たせている smoltcp のソケット
    Listening {
        local: IpEndpoint,
        pending: Vec<SocketHandle>,
    },
    /// つながった。`owns_port`:ポートをこのソケットが持っているか(accept したものは待ち受け側のもの)
    Connected {
        handle: SocketHandle,
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
    fn connection(&self) -> Result<(SocketHandle, bool), Errno> {
        match &*self.state.lock() {
            State::Connected {
                handle, read_shut, ..
            } => Ok((*handle, *read_shut)),
            _ => Err(Errno::ENOTCONN),
        }
    }
}

/// 待たせているソケットのうち、つながったもの(の位置)
fn ready(net: &mut Net, pending: &[SocketHandle]) -> Option<usize> {
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
        if !is_local(ep.addr) {
            return Err(Errno::EADDRNOTAVAIL);
        }
        with(|net| {
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
            for _ in 0..n {
                pending.push(net.add_listener(local)?);
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
            match net.add_listener(*local) {
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
        if !is_local(remote.addr) || remote.addr.is_unspecified() {
            return Err(Errno::ENETUNREACH);
        }
        let (handle, port) = with(|net| {
            let port = match local {
                Some(l) => l.port,
                None => net.ephemeral(Proto::Tcp)?,
            };
            let s = net.new_tcp();
            let handle = net.lo.sockets.add(s);
            let cx = net.lo.iface.context();
            let r = net
                .lo
                .sockets
                .get_mut::<tcp::Socket>(handle)
                .connect(cx, remote, port);
            if r.is_err() {
                net.lo.sockets.remove(handle);
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
                    net.lo.sockets.remove(handle);
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
                    net.lo.sockets.remove(h);
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
    handle: Option<SocketHandle>,
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
    fn open(&mut self, net: &mut Net, mut ep: IpEndpoint) -> Result<SocketHandle, Errno> {
        if ep.port == 0 {
            ep.port = net.ephemeral(Proto::Udp)?;
        } else if !net.ports.insert((Proto::Udp, ep.port)) {
            return Err(Errno::EADDRINUSE);
        }
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
        let addr = (!ep.addr.is_unspecified()).then_some(ep.addr);
        s.bind(IpListenEndpoint {
            addr,
            port: ep.port,
        })
        .map_err(|_| Errno::EINVAL)?;
        let h = net.lo.sockets.add(s);
        self.handle = Some(h);
        self.local = Some(ep);
        Ok(h)
    }

    /// bind していなければ、自動で bind する
    fn ensure_open(&mut self, net: &mut Net) -> Result<SocketHandle, Errno> {
        match self.handle {
            Some(h) => Ok(h),
            None => self.open(net, unspecified()),
        }
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
        if state.handle.is_some() {
            return Err(Errno::EINVAL);
        }
        if !is_local(ep.addr) {
            return Err(Errno::EADDRNOTAVAIL);
        }
        with(|net| state.open(net, ep).map(|_| ()))
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
        if !is_local(remote.addr) {
            return Err(Errno::ENETUNREACH);
        }
        let mut state = self.state.lock();
        with(|net| state.ensure_open(net))?;
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
            if !is_local(dest.addr) || dest.addr.is_unspecified() {
                return Err(Errno::ENETUNREACH);
            }
            (with(|net| state.ensure_open(net))?, dest)
        };
        if buf.len() > BUFFER {
            return Err(Errno::EMSGSIZE);
        }
        wait(nonblock, |net| {
            let s = net.lo.sockets.get_mut::<udp::Socket>(h);
            match s.send_slice(buf, dest) {
                Ok(()) => Some(Ok(buf.len())),
                Err(udp::SendError::BufferFull) => None,
                Err(udp::SendError::Unaddressable) => Some(Err(Errno::ENETUNREACH)),
            }
        })
    }

    fn recv_from(&self, buf: &mut [u8], nonblock: bool) -> Result<(usize, Vec<u8>), Errno> {
        let h = {
            let mut state = self.state.lock();
            with(|net| state.ensure_open(net))?
        };
        wait(nonblock, |net| {
            let s = net.lo.sockets.get_mut::<udp::Socket>(h);
            let (data, meta) = s.recv().ok()?;
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
        let Some(h) = self.state.lock().handle else {
            return false;
        };
        with(|net| net.lo.sockets.get_mut::<udp::Socket>(h).can_recv())
    }

    fn as_socket(&self) -> Option<&dyn Socket> {
        Some(self)
    }
}

impl Drop for UdpSocket {
    fn drop(&mut self) {
        let state = core::mem::take(self.state.get_mut());
        let (Some(h), Some(local)) = (state.handle, state.local) else {
            return;
        };
        if let Some(net) = NET.lock().as_mut() {
            net.lo.sockets.remove(h);
            net.ports.remove(&(Proto::Udp, local.port));
        }
    }
}
