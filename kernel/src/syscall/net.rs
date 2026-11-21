//! ソケットのシステムコール。実際の処理は `crate::net` のプロトコルファミリーに任せる。

use crate::Errno;
use crate::fs::vfs::{O_RDWR, OpenFile};
use crate::net::{self, SOCK_CLOEXEC, SOCK_NONBLOCK};
use crate::process::Process;
use alloc::sync::Arc;
use hal::addr::VirtAddr;

/// 作ったソケットを fd 表に入れる
fn install(p: &mut Process, file: Arc<dyn crate::fs::vfs::File>, ty: u32) -> Result<usize, Errno> {
    let mut flags = O_RDWR;
    if ty & SOCK_NONBLOCK != 0 {
        flags |= SOCK_NONBLOCK; // O_NONBLOCK と同じ値
    }
    let fd = p.fds.insert(OpenFile::new(file, flags))?;
    p.fds.set_cloexec(fd as u64, ty & SOCK_CLOEXEC != 0)?;
    Ok(fd)
}

/// socketpair(domain, type, protocol, sv):つながった 2 つのソケットを作り、fd を `int sv[2]` に書く
pub fn socketpair(
    p: &mut Process,
    domain: u64,
    ty: u64,
    protocol: u64,
    sv: u64,
) -> Result<u64, Errno> {
    let (ty, protocol) = (ty as u32, protocol as u32);
    let (a, b) = net::family(domain as u32)?.socketpair(ty, protocol)?;
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
