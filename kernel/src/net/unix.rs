//! AF_UNIX:同じマシンの中のプロセスどうしをつなぐソケット。
//!
//! つながった 1 組のストリームは、向きの違うパイプ 2 本でできている。
//!
//! ```text
//!  端 A  tx ──(パイプ 1)──> rx  端 B
//!        rx <──(パイプ 2)── tx
//! ```
//!
//! 片方を閉じると、その端が持つパイプの読む側・書く側が閉じるので、相手は読むと 0
//! (終わり)、書くと EPIPE になる。パイプと同じ規則がそのまま使える。
//!
//! 名前でつなぐときは、bind した名前を登録表(`BOUND`)に入れる。名前は 2 種類:
//! - パス(`/tmp/srv`):ファイルシステムにソケットのファイルを作り、(st_dev, inode 番号) で見分ける
//! - 抽象名(先頭が `\0`):ファイルは作らず、名前のバイト列で見分ける(Linux 独自)
//!
//! connect は、相手の待ち行列につながった 1 組の片方を入れて、すぐ戻る(accept を待たない)。
//! accept は待ち行列から 1 つ取り出す。

use super::{Namespace, Pair, ProtocolFamily, SOCK_STREAM, Shutdown, Socket, split_type};
use crate::Errno;
use crate::fs::pipe::{self, ReadEnd, WriteEnd};
use crate::fs::vfs::{File, FileKind, Stat};
use crate::sync::SpinLock;
use alloc::collections::{BTreeMap, VecDeque};
use alloc::string::String;
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;

const AF_UNIX: u16 = super::AF_UNIX as u16;
/// sockaddr_un の sun_path の大きさ
const PATH_MAX: usize = 108;

pub struct Unix;

impl Unix {
    fn check(ty: u32, protocol: u32) -> Result<(), Errno> {
        let (kind, _) = split_type(ty)?;
        if protocol != 0 && protocol != super::AF_UNIX {
            return Err(Errno::EPROTONOSUPPORT);
        }
        if kind != SOCK_STREAM {
            return Err(Errno::ESOCKTNOSUPPORT);
        }
        Ok(())
    }
}

impl ProtocolFamily for Unix {
    fn socket(&self, ty: u32, protocol: u32) -> Result<Arc<dyn File>, Errno> {
        Self::check(ty, protocol)?;
        Ok(Arc::new(UnixSocket::new(State::Idle)))
    }

    fn socketpair(&self, ty: u32, protocol: u32) -> Result<Pair, Errno> {
        Self::check(ty, protocol)?;
        let (a, b) = Stream::pair();
        Ok((
            Arc::new(UnixSocket::connected(a, Addr::Unnamed, Addr::Unnamed)),
            Arc::new(UnixSocket::connected(b, Addr::Unnamed, Addr::Unnamed)),
        ))
    }
}

/// ソケットの名前
#[derive(Debug, Clone, PartialEq, Eq)]
enum Addr {
    /// 名前なし(bind していない、socketpair で作った)
    Unnamed,
    /// パス(ユーザーが渡したままの形で覚える。getsockname でそのまま返す)
    Path(String),
    /// 抽象名(先頭の `\0` を除いたバイト列)
    Abstract(Vec<u8>),
}

impl Addr {
    /// sockaddr_un のバイト列から読む
    fn parse(raw: &[u8]) -> Result<Self, Errno> {
        if raw.len() < 2 || u16::from_le_bytes([raw[0], raw[1]]) != AF_UNIX {
            return Err(Errno::EINVAL);
        }
        let name = &raw[2..raw.len().min(2 + PATH_MAX)];
        match name.first() {
            None => Err(Errno::EINVAL),
            Some(0) => Ok(Self::Abstract(name[1..].to_vec())),
            Some(_) => {
                // パスは最初の NUL まで
                let end = name.iter().position(|&b| b == 0).unwrap_or(name.len());
                let path = core::str::from_utf8(&name[..end]).map_err(|_| Errno::EINVAL)?;
                Ok(Self::Path(String::from(path)))
            }
        }
    }

    /// sockaddr_un のバイト列にする(Linux と同じ長さ:パスは終わりの NUL まで、抽象名は名前まで)
    fn to_bytes(&self) -> Vec<u8> {
        let mut b = Vec::from(AF_UNIX.to_le_bytes());
        match self {
            Self::Unnamed => {}
            Self::Path(p) => {
                b.extend_from_slice(p.as_bytes());
                b.push(0);
            }
            Self::Abstract(n) => {
                b.push(0);
                b.extend_from_slice(n);
            }
        }
        b
    }
}

/// 登録表のキー
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum Key {
    /// パスのソケット:(st_dev, inode 番号)
    Inode(u64, u64),
    Abstract(Vec<u8>),
}

/// 名前を付けたソケットの登録。待ち受けを始めると `listener` が入る
struct Binding {
    key: Key,
    addr: Addr,
    listener: SpinLock<Option<Arc<Listener>>>,
}

impl Drop for Binding {
    fn drop(&mut self) {
        let mut bound = BOUND.lock();
        // 同じキーで別のソケットが登録し直していたら、そちらは消さない
        if bound.get(&self.key).is_some_and(|w| w.strong_count() == 0) {
            bound.remove(&self.key);
        }
    }
}

/// bind した名前 → 登録
static BOUND: SpinLock<BTreeMap<Key, Weak<Binding>>> = SpinLock::new(BTreeMap::new());

/// 待ち受け中のソケットの、accept 待ちの行列
struct Listener {
    /// (サーバー側の端, 相手の名前)
    queue: SpinLock<VecDeque<(Stream, Addr)>>,
    backlog: usize,
}

/// つながったストリームの片端。shutdown で片方ずつ閉じられるように、両端を Option で持つ
pub struct Stream {
    rx: SpinLock<Option<Arc<ReadEnd>>>,
    tx: SpinLock<Option<Arc<WriteEnd>>>,
}

impl Stream {
    /// つながった 1 組を作る
    pub fn pair() -> (Self, Self) {
        let (r1, w1) = pipe::ends();
        let (r2, w2) = pipe::ends();
        (Self::new(r2, w1), Self::new(r1, w2))
    }

    fn new(rx: ReadEnd, tx: WriteEnd) -> Self {
        Self {
            rx: SpinLock::new(Some(Arc::new(rx))),
            tx: SpinLock::new(Some(Arc::new(tx))),
        }
    }

    fn read(&self, buf: &mut [u8]) -> Result<usize, Errno> {
        // 読み終わるまで待つことがあるので、ロックの外で読む
        let rx = self.rx.lock().clone();
        match rx {
            Some(rx) => rx.read(0, buf),
            None => Ok(0), // 読む側を閉じた
        }
    }

    fn write(&self, buf: &[u8]) -> Result<usize, Errno> {
        let tx = self.tx.lock().clone();
        match tx {
            Some(tx) => tx.write(0, buf),
            None => Err(Errno::EPIPE), // 書く側を閉じた
        }
    }

    fn poll_readable(&self) -> bool {
        self.rx.lock().as_ref().is_none_or(|rx| rx.poll_readable())
    }

    fn shutdown(&self, how: Shutdown) {
        if how != Shutdown::Write {
            self.rx.lock().take();
        }
        if how != Shutdown::Read {
            self.tx.lock().take();
        }
    }
}

enum State {
    /// 作っただけ
    Idle,
    /// 名前を付けた(待ち受けもしているなら、Binding の listener に入っている)
    Bound(Arc<Binding>),
    /// つながった
    Connected {
        stream: Arc<Stream>,
        local: Addr,
        peer: Addr,
    },
}

/// AF_UNIX のソケット
pub struct UnixSocket {
    state: SpinLock<State>,
    options: super::Options,
}

impl UnixSocket {
    fn new(state: State) -> Self {
        Self {
            state: SpinLock::new(state),
            options: super::Options::default(),
        }
    }

    fn connected(stream: Stream, local: Addr, peer: Addr) -> Self {
        Self::new(State::Connected {
            stream: Arc::new(stream),
            local,
            peer,
        })
    }

    fn stream(&self) -> Result<Arc<Stream>, Errno> {
        match &*self.state.lock() {
            State::Connected { stream, .. } => Ok(stream.clone()),
            _ => Err(Errno::ENOTCONN),
        }
    }

    fn listener(&self) -> Option<Arc<Listener>> {
        match &*self.state.lock() {
            State::Bound(b) => b.listener.lock().clone(),
            _ => None,
        }
    }
}

/// 名前から登録表のキーを作る。パスは VFS に作る(bind)か探す(connect)
fn key_of(ns: &mut dyn Namespace, addr: &Addr, create: bool) -> Result<Key, Errno> {
    match addr {
        Addr::Unnamed => Err(Errno::EINVAL),
        Addr::Abstract(n) => Ok(Key::Abstract(n.clone())),
        Addr::Path(p) => {
            let (dev, ino) = if create {
                ns.create_socket(p)?
            } else {
                ns.find_socket(p)?
            };
            Ok(Key::Inode(dev, ino))
        }
    }
}

impl Socket for UnixSocket {
    fn sock_type(&self) -> u32 {
        SOCK_STREAM
    }

    fn options(&self) -> &super::Options {
        &self.options
    }

    fn bind(&self, ns: &mut dyn Namespace, raw: &[u8]) -> Result<(), Errno> {
        let addr = Addr::parse(raw)?;
        let mut state = self.state.lock();
        if !matches!(*state, State::Idle) {
            // Linux はパスを先に作るので、パスがもうあれば EADDRINUSE が先に出る
            return Err(match &addr {
                Addr::Path(p) if ns.exists(p) => Errno::EADDRINUSE,
                _ => Errno::EINVAL,
            });
        }
        if let Addr::Abstract(n) = &addr
            && BOUND
                .lock()
                .get(&Key::Abstract(n.clone()))
                .is_some_and(|w| w.strong_count() > 0)
        {
            return Err(Errno::EADDRINUSE);
        }
        let key = key_of(ns, &addr, true)?;
        let binding = Arc::new(Binding {
            key: key.clone(),
            addr,
            listener: SpinLock::new(None),
        });
        BOUND.lock().insert(key, Arc::downgrade(&binding));
        *state = State::Bound(binding);
        Ok(())
    }

    fn listen(&self, backlog: usize) -> Result<(), Errno> {
        match &*self.state.lock() {
            State::Bound(b) => {
                let mut l = b.listener.lock();
                if l.is_none() {
                    *l = Some(Arc::new(Listener {
                        queue: SpinLock::new(VecDeque::new()),
                        backlog: backlog.clamp(1, 4096),
                    }));
                }
                Ok(())
            }
            // 名前のないソケットの listen(Linux は自動で抽象名を付ける)はまだ
            State::Idle => Err(Errno::EINVAL),
            State::Connected { .. } => Err(Errno::EINVAL),
        }
    }

    fn accept(&self, nonblock: bool) -> Result<(Arc<dyn File>, Vec<u8>), Errno> {
        let (listener, local) = match &*self.state.lock() {
            State::Bound(b) => (b.listener.lock().clone(), b.addr.clone()),
            _ => (None, Addr::Unnamed),
        };
        let listener = listener.ok_or(Errno::EINVAL)?;
        loop {
            if let Some((stream, peer)) = listener.queue.lock().pop_front() {
                let bytes = peer.to_bytes();
                return Ok((Arc::new(UnixSocket::connected(stream, local, peer)), bytes));
            }
            if nonblock {
                return Err(Errno::EAGAIN);
            }
            crate::sched::relax();
        }
    }

    fn connect(&self, ns: &mut dyn Namespace, raw: &[u8], nonblock: bool) -> Result<(), Errno> {
        let target = Addr::parse(raw)?;
        let local = match &*self.state.lock() {
            State::Connected { .. } => return Err(Errno::EISCONN),
            State::Bound(b) => b.addr.clone(),
            State::Idle => Addr::Unnamed,
        };
        let key = key_of(ns, &target, false)?;
        let binding = BOUND
            .lock()
            .get(&key)
            .and_then(|w| w.upgrade())
            .ok_or(Errno::ECONNREFUSED)?;
        let listener = binding.listener.lock().clone().ok_or(Errno::ECONNREFUSED)?;
        let (mine, theirs) = Stream::pair();
        let mut theirs = Some(theirs);
        loop {
            {
                let mut q = listener.queue.lock();
                if q.len() < listener.backlog {
                    q.push_back((theirs.take().unwrap(), local.clone()));
                    break;
                }
            }
            // 待ち行列がいっぱい:accept されるのを待つ
            if nonblock {
                return Err(Errno::EAGAIN);
            }
            // 待ち受け側が閉じたら、つながらない
            if Arc::strong_count(&listener) == 1 {
                return Err(Errno::ECONNREFUSED);
            }
            crate::sched::relax();
        }
        *self.state.lock() = State::Connected {
            stream: Arc::new(mine),
            local,
            peer: binding.addr.clone(),
        };
        Ok(())
    }

    fn local_addr(&self) -> Vec<u8> {
        match &*self.state.lock() {
            State::Idle => Addr::Unnamed.to_bytes(),
            State::Bound(b) => b.addr.to_bytes(),
            State::Connected { local, .. } => local.to_bytes(),
        }
    }

    fn peer_addr(&self) -> Result<Vec<u8>, Errno> {
        match &*self.state.lock() {
            State::Connected { peer, .. } => Ok(peer.to_bytes()),
            _ => Err(Errno::ENOTCONN),
        }
    }

    fn shutdown(&self, how: Shutdown) -> Result<(), Errno> {
        self.stream()?.shutdown(how);
        Ok(())
    }
}

impl File for UnixSocket {
    fn stat(&self) -> Stat {
        Stat {
            kind: FileKind::Socket,
            mode: 0o777,
            ino: 0x6_0000,
            ..Default::default()
        }
    }

    fn read(&self, _offset: u64, buf: &mut [u8]) -> Result<usize, Errno> {
        // つながっていないストリームを読むのは EINVAL(Linux と同じ)
        self.stream().map_err(|_| Errno::EINVAL)?.read(buf)
    }

    fn write(&self, _offset: u64, buf: &[u8]) -> Result<usize, Errno> {
        self.stream()?.write(buf)
    }

    fn poll_readable(&self) -> bool {
        match self.stream() {
            Ok(s) => s.poll_readable(),
            // 待ち受け中なら、accept できるものがあるか
            Err(_) => self.listener().is_some_and(|l| !l.queue.lock().is_empty()),
        }
    }

    fn as_socket(&self) -> Option<&dyn Socket> {
        Some(self)
    }
}
