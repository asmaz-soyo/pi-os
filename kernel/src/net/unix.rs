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

use super::{ProtocolFamily, SOCK_STREAM, split_type};
use crate::Errno;
use crate::fs::pipe::{self, ReadEnd, WriteEnd};
use crate::fs::vfs::{File, FileKind, Stat};
use alloc::sync::Arc;

pub struct Unix;

impl ProtocolFamily for Unix {
    fn socketpair(&self, ty: u32, protocol: u32) -> Result<super::Pair, Errno> {
        let (kind, _) = split_type(ty)?;
        if protocol != 0 && protocol != super::AF_UNIX {
            return Err(Errno::EPROTONOSUPPORT);
        }
        if kind != SOCK_STREAM {
            return Err(Errno::ESOCKTNOSUPPORT);
        }
        let (a, b) = Stream::pair();
        Ok((Arc::new(a), Arc::new(b)))
    }
}

/// つながったストリームの片端
pub struct Stream {
    rx: ReadEnd,
    tx: WriteEnd,
}

impl Stream {
    /// つながった 1 組を作る
    pub fn pair() -> (Self, Self) {
        let (r1, w1) = pipe::ends();
        let (r2, w2) = pipe::ends();
        (Self { rx: r2, tx: w1 }, Self { rx: r1, tx: w2 })
    }
}

impl File for Stream {
    fn stat(&self) -> Stat {
        Stat {
            kind: FileKind::Socket,
            mode: 0o777,
            ino: 0x6_0000,
            ..Default::default()
        }
    }

    fn read(&self, offset: u64, buf: &mut [u8]) -> Result<usize, Errno> {
        self.rx.read(offset, buf)
    }

    fn write(&self, offset: u64, buf: &[u8]) -> Result<usize, Errno> {
        self.tx.write(offset, buf)
    }

    fn poll_readable(&self) -> bool {
        self.rx.poll_readable()
    }
}
