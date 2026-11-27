//! ソケットのシステムコール。実際の処理は `crate::net` のプロトコルファミリーに任せる。
//!
//! ここでやるのは、ユーザーのメモリとのやりとり(アドレスの読み書き)、fd 表の出し入れ、
//! それと AF_UNIX のためにパスをファイルシステムで作ったり探したりする口([`Namespace`])。

use crate::Errno;
use crate::cred::Access;
use crate::fs::vfs::{self, File, FileKind, O_RDWR, OpenFile};
use crate::net::{self, Namespace, SOCK_CLOEXEC, SOCK_NONBLOCK, Shutdown, Socket};
use crate::process::Process;
use alloc::sync::Arc;
use alloc::vec::Vec;
use hal::addr::VirtAddr;

/// O_NONBLOCK(SOCK_NONBLOCK と同じ値)
const O_NONBLOCK: u32 = 0o4000;
/// 受け付けるアドレスの長さの上限(struct sockaddr_storage)
const ADDR_MAX: u64 = 128;

/// 作ったソケットを fd 表に入れる。`flags` は SOCK_NONBLOCK と SOCK_CLOEXEC
fn install(p: &mut Process, file: Arc<dyn File>, flags: u32) -> Result<usize, Errno> {
    let fd = p
        .fds
        .insert(OpenFile::new(file, O_RDWR | (flags & SOCK_NONBLOCK)))?;
    p.fds.set_cloexec(fd as u64, flags & SOCK_CLOEXEC != 0)?;
    Ok(fd)
}

/// fd のソケット。ソケットでなければ ENOTSOCK
fn socket_of(p: &Process, fd: u64) -> Result<(Arc<OpenFile>, bool), Errno> {
    let f = p.fds.get(fd)?;
    if f.file.as_socket().is_none() {
        return Err(Errno::ENOTSOCK);
    }
    let nonblock = f.flags & O_NONBLOCK != 0;
    Ok((f, nonblock))
}

fn sock(f: &OpenFile) -> &dyn Socket {
    f.file.as_socket().expect("socket_of で確かめた")
}

/// ユーザーのアドレス(`struct sockaddr *`, `socklen_t`)を読む
fn read_addr(p: &mut Process, addr: u64, len: u64) -> Result<Vec<u8>, Errno> {
    if len > ADDR_MAX {
        return Err(Errno::EINVAL);
    }
    let mut buf = alloc::vec![0u8; len as usize];
    p.mem.copy_from_user(VirtAddr(addr), &mut buf)?;
    Ok(buf)
}

/// アドレスをユーザーに書く。`*len` に入る分だけ書き、`*len` には本当の長さを入れる(Linux と同じ)
fn write_addr(p: &mut Process, addr: u64, len: u64, bytes: &[u8]) -> Result<(), Errno> {
    if addr == 0 {
        return Ok(());
    }
    let mut raw = [0u8; 4];
    p.mem.copy_from_user(VirtAddr(len), &mut raw)?;
    let room = u32::from_le_bytes(raw) as i32;
    if room < 0 {
        return Err(Errno::EINVAL);
    }
    let n = bytes.len().min(room as usize);
    p.mem.copy_to_user(VirtAddr(addr), &bytes[..n])?;
    p.mem
        .copy_to_user(VirtAddr(len), &(bytes.len() as u32).to_le_bytes())
}

/// AF_UNIX のパスを、このプロセスから見たファイルシステムで作ったり探したりする
struct ProcessNamespace<'a>(&'a mut Process);

impl Namespace for ProcessNamespace<'_> {
    fn create_socket(&mut self, path: &str) -> Result<(u64, u64), Errno> {
        let p = &mut *self.0;
        let path = vfs::normalize(&p.cwd, path);
        let dir = p.vfs.lookup(&vfs::normalize(&path, ".."))?;
        p.cred.check(&dir.stat(), Access::WRITE | Access::EXEC)?;
        p.vfs
            .mknod(&path, FileKind::Socket, 0o777 & !p.umask)
            .map_err(|e| {
                if e == Errno::EEXIST {
                    Errno::EADDRINUSE
                } else {
                    e
                }
            })?;
        let file = p.vfs.lookup_nofollow(&path)?;
        super::fs::own_new(p, &*file, &path)?;
        Ok((p.vfs.device_of(&path, false)?, file.stat().ino))
    }

    fn find_socket(&mut self, path: &str) -> Result<(u64, u64), Errno> {
        let p = &mut *self.0;
        let path = vfs::normalize(&p.cwd, path);
        let st = p.vfs.lookup(&path)?.stat();
        if st.kind != FileKind::Socket {
            return Err(Errno::ECONNREFUSED);
        }
        // つなぐには、ソケットのファイルに書き込みの権限が要る
        p.cred.check(&st, Access::WRITE)?;
        Ok((p.vfs.device_of(&path, true)?, st.ino))
    }

    fn exists(&mut self, path: &str) -> bool {
        let path = vfs::normalize(&self.0.cwd, path);
        self.0.vfs.lookup_nofollow(&path).is_ok()
    }
}

/// socket(domain, type, protocol)
pub fn socket(p: &mut Process, domain: u64, ty: u64, protocol: u64) -> Result<u64, Errno> {
    let ty = ty as u32;
    let file = net::family(domain as u32)?.socket(ty, protocol as u32)?;
    Ok(install(p, file, ty)? as u64)
}

/// socketpair(domain, type, protocol, sv):つながった 2 つのソケットを作り、fd を `int sv[2]` に書く
pub fn socketpair(
    p: &mut Process,
    domain: u64,
    ty: u64,
    protocol: u64,
    sv: u64,
) -> Result<u64, Errno> {
    let ty = ty as u32;
    let (a, b) = net::family(domain as u32)?.socketpair(ty, protocol as u32)?;
    let fa = install(p, a, ty)?;
    let fb = match install(p, b, ty) {
        Ok(fd) => fd,
        Err(e) => {
            let _ = p.fds.close(fa as u64);
            return Err(e);
        }
    };
    let mut buf = [0u8; 8];
    buf[..4].copy_from_slice(&(fa as i32).to_le_bytes());
    buf[4..].copy_from_slice(&(fb as i32).to_le_bytes());
    if let Err(e) = p.mem.copy_to_user(VirtAddr(sv), &buf) {
        let _ = p.fds.close(fa as u64);
        let _ = p.fds.close(fb as u64);
        return Err(e);
    }
    Ok(0)
}

/// bind(fd, addr, addrlen)
pub fn bind(p: &mut Process, fd: u64, addr: u64, len: u64) -> Result<u64, Errno> {
    let (f, _) = socket_of(p, fd)?;
    let raw = read_addr(p, addr, len)?;
    sock(&f).bind(&mut ProcessNamespace(p), &raw)?;
    Ok(0)
}

/// listen(fd, backlog)
pub fn listen(p: &mut Process, fd: u64, backlog: u64) -> Result<u64, Errno> {
    let (f, _) = socket_of(p, fd)?;
    // backlog は int:負なら 0 として扱う(Linux と同じ)
    sock(&f).listen((backlog as i32).max(0) as usize)?;
    Ok(0)
}

/// accept4(fd, addr, addrlen, flags)。accept は flags = 0
pub fn accept4(p: &mut Process, fd: u64, addr: u64, len: u64, flags: u64) -> Result<u64, Errno> {
    let flags = flags as u32;
    if flags & !(SOCK_NONBLOCK | SOCK_CLOEXEC) != 0 {
        return Err(Errno::EINVAL);
    }
    let (f, nonblock) = socket_of(p, fd)?;
    let (file, peer) = sock(&f).accept(nonblock)?;
    let new = install(p, file, flags)?;
    if let Err(e) = write_addr(p, addr, len, &peer) {
        let _ = p.fds.close(new as u64);
        return Err(e);
    }
    Ok(new as u64)
}

/// connect(fd, addr, addrlen)
pub fn connect(p: &mut Process, fd: u64, addr: u64, len: u64) -> Result<u64, Errno> {
    let (f, nonblock) = socket_of(p, fd)?;
    let raw = read_addr(p, addr, len)?;
    sock(&f).connect(&mut ProcessNamespace(p), &raw, nonblock)?;
    Ok(0)
}

/// getsockname(fd, addr, addrlen)
pub fn getsockname(p: &mut Process, fd: u64, addr: u64, len: u64) -> Result<u64, Errno> {
    let (f, _) = socket_of(p, fd)?;
    let bytes = sock(&f).local_addr();
    write_addr(p, addr, len, &bytes)?;
    Ok(0)
}

/// getpeername(fd, addr, addrlen)
pub fn getpeername(p: &mut Process, fd: u64, addr: u64, len: u64) -> Result<u64, Errno> {
    let (f, _) = socket_of(p, fd)?;
    let bytes = sock(&f).peer_addr()?;
    write_addr(p, addr, len, &bytes)?;
    Ok(0)
}

/// shutdown(fd, how)
pub fn shutdown(p: &mut Process, fd: u64, how: u64) -> Result<u64, Errno> {
    let (f, _) = socket_of(p, fd)?;
    sock(&f).shutdown(Shutdown::from_raw(how)?)?;
    Ok(0)
}

/// 送り受けのフラグ:このときだけ待たない
const MSG_DONTWAIT: u64 = 0x40;
/// 1 回の sendto / recvfrom で扱う最大の大きさ(カーネルの中に一度に置く量)
const IO_MAX: u64 = 1 << 20;

/// sendto(fd, buf, len, flags, addr, addrlen)。addr が 0 なら宛先なし(write と同じ)
pub fn sendto(
    p: &mut Process,
    fd: u64,
    buf: u64,
    len: u64,
    flags: u64,
    addr: u64,
    alen: u64,
) -> Result<u64, Errno> {
    let (f, nonblock) = socket_of(p, fd)?;
    let mut data = alloc::vec![0u8; len.min(IO_MAX) as usize];
    p.mem.copy_from_user(VirtAddr(buf), &mut data)?;
    let to = if addr != 0 {
        Some(read_addr(p, addr, alen)?)
    } else {
        None
    };
    let n = sock(&f).send_to(&data, to.as_deref(), nonblock || flags & MSG_DONTWAIT != 0)?;
    Ok(n as u64)
}

/// recvfrom(fd, buf, len, flags, addr, addrlen)。addr が 0 なら送り元は返さない
pub fn recvfrom(
    p: &mut Process,
    fd: u64,
    buf: u64,
    len: u64,
    flags: u64,
    addr: u64,
    alen: u64,
) -> Result<u64, Errno> {
    let (f, nonblock) = socket_of(p, fd)?;
    let mut data = alloc::vec![0u8; len.min(IO_MAX) as usize];
    let (n, from) = sock(&f).recv_from(&mut data, nonblock || flags & MSG_DONTWAIT != 0)?;
    p.mem.copy_to_user(VirtAddr(buf), &data[..n])?;
    write_addr(p, addr, alen, &from)?;
    Ok(n as u64)
}

/// ユーザーの struct msghdr(aarch64 では 56 バイト)のうち、使うところ
struct MsgHdr {
    name: u64,
    namelen: u32,
    iov: u64,
    iovlen: u64,
    controllen: u64,
}

/// msg_namelen は 8 バイト目、msg_controllen は 40 バイト目、msg_flags は 48 バイト目
const MSG_NAMELEN: u64 = 8;
const MSG_CONTROLLEN: u64 = 40;
const MSG_FLAGS: u64 = 48;

fn read_msghdr(p: &mut Process, msg: u64) -> Result<MsgHdr, Errno> {
    let mut raw = [0u8; 56];
    p.mem.copy_from_user(VirtAddr(msg), &mut raw)?;
    let w = |i: usize| u64::from_le_bytes(raw[i..i + 8].try_into().unwrap());
    Ok(MsgHdr {
        name: w(0),
        namelen: u32::from_le_bytes(raw[8..12].try_into().unwrap()),
        iov: w(16),
        iovlen: w(24),
        controllen: w(40),
    })
}

/// sendmsg(fd, msg, flags):iovec のバッファをつないで1回の send_to にする。
/// 補助データ(SCM_RIGHTS など)はまだ扱わない
pub fn sendmsg(p: &mut Process, fd: u64, msg: u64, flags: u64) -> Result<u64, Errno> {
    let (f, nonblock) = socket_of(p, fd)?;
    let m = read_msghdr(p, msg)?;
    if m.controllen != 0 {
        return Err(Errno::EOPNOTSUPP);
    }
    let mut data = Vec::new();
    for (base, len) in super::fs::iovecs(p, m.iov, m.iovlen)? {
        let len = len.min(IO_MAX - data.len() as u64) as usize;
        let start = data.len();
        data.resize(start + len, 0);
        p.mem.copy_from_user(VirtAddr(base), &mut data[start..])?;
    }
    let to = if m.name != 0 {
        Some(read_addr(p, m.name, m.namelen as u64)?)
    } else {
        None
    };
    let n = sock(&f).send_to(&data, to.as_deref(), nonblock || flags & MSG_DONTWAIT != 0)?;
    Ok(n as u64)
}

/// recvmsg(fd, msg, flags):1回の recv_from で受け取り、iovec のバッファに順に分けて書く
pub fn recvmsg(p: &mut Process, fd: u64, msg: u64, flags: u64) -> Result<u64, Errno> {
    let (f, nonblock) = socket_of(p, fd)?;
    let m = read_msghdr(p, msg)?;
    let iovs = super::fs::iovecs(p, m.iov, m.iovlen)?;
    let room = iovs.iter().map(|(_, len)| *len).sum::<u64>().min(IO_MAX);
    let mut data = alloc::vec![0u8; room as usize];
    let (n, from) = sock(&f).recv_from(&mut data, nonblock || flags & MSG_DONTWAIT != 0)?;
    let mut done = 0;
    for (base, len) in iovs {
        let k = (len as usize).min(n - done);
        p.mem.copy_to_user(VirtAddr(base), &data[done..done + k])?;
        done += k;
    }
    write_addr(p, m.name, msg + MSG_NAMELEN, &from)?;
    // 補助データは返さない。フラグもなし
    p.mem
        .copy_to_user(VirtAddr(msg + MSG_CONTROLLEN), &0u64.to_le_bytes())?;
    p.mem
        .copy_to_user(VirtAddr(msg + MSG_FLAGS), &0u32.to_le_bytes())?;
    Ok(n as u64)
}

/// setsockopt(fd, level, name, value, len)。値は int のものだけ
pub fn setsockopt(
    p: &mut Process,
    fd: u64,
    level: u64,
    name: u64,
    value: u64,
    len: u64,
) -> Result<u64, Errno> {
    let (f, _) = socket_of(p, fd)?;
    if len < 4 {
        return Err(Errno::EINVAL);
    }
    let mut raw = [0u8; 4];
    p.mem.copy_from_user(VirtAddr(value), &mut raw)?;
    sock(&f)
        .options()
        .set(level as u32, name as u32, i32::from_le_bytes(raw))?;
    Ok(0)
}

/// getsockopt(fd, level, name, value, lenp)
pub fn getsockopt(
    p: &mut Process,
    fd: u64,
    level: u64,
    name: u64,
    value: u64,
    lenp: u64,
) -> Result<u64, Errno> {
    let (f, _) = socket_of(p, fd)?;
    let mut raw = [0u8; 4];
    p.mem.copy_from_user(VirtAddr(lenp), &mut raw)?;
    if (u32::from_le_bytes(raw) as i32) < 4 {
        return Err(Errno::EINVAL);
    }
    let s = sock(&f);
    let v = s.options().get(s, level as u32, name as u32)?;
    p.mem.copy_to_user(VirtAddr(value), &v.to_le_bytes())?;
    p.mem.copy_to_user(VirtAddr(lenp), &4u32.to_le_bytes())?;
    Ok(0)
}
