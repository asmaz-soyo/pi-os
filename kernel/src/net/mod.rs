//! ネットワーク:ソケットと、プロトコルファミリーの登録表。
//!
//! `socket(AF_xxx, ...)` の最初の引数(アドレスファミリー)で、どのプロトコルの実装に渡すかを決める。
//! 実装は [`ProtocolFamily`] の形をしていて、作ったソケットは普通の `File` として fd 表に入る
//! (read / write / poll はファイルやパイプと同じ道を通る)。
//!
//! ```text
//! socket(AF_UNIX, ...) ─┐
//! socket(AF_INET, ...) ─┼─ family(af) → ProtocolFamily::socket → Arc<dyn File>
//! socketpair(...)      ─┘
//! ```

pub mod unix;

use crate::Errno;
use crate::fs::vfs::File;
use crate::sync::SpinLock;
use alloc::sync::Arc;
use alloc::vec::Vec;

/// アドレスファミリーの番号(Linux と同じ)
pub const AF_UNIX: u32 = 1;
pub const AF_INET: u32 = 2;

/// ソケットの種類(`type` の下位ビット)
pub const SOCK_STREAM: u32 = 1;
pub const SOCK_DGRAM: u32 = 2;
/// 種類の番号の上限(これ以上は EINVAL)
const SOCK_MAX: u32 = 11;
/// `type` に OR できるフラグ
pub const SOCK_NONBLOCK: u32 = 0o4000;
pub const SOCK_CLOEXEC: u32 = 0o2000000;

/// つながった 2 つのソケット(socketpair の結果)
pub type Pair = (Arc<dyn File>, Arc<dyn File>);

/// プロトコルファミリーの実装(AF_UNIX、AF_INET…)
pub trait ProtocolFamily: Send + Sync {
    /// つながった 2 つのソケットを作る(socketpair)。できないファミリーは EOPNOTSUPP
    fn socketpair(&self, _ty: u32, _protocol: u32) -> Result<Pair, Errno> {
        Err(Errno::EOPNOTSUPP)
    }
}

/// 最初から入っているファミリー
static BUILTIN: [(u32, &dyn ProtocolFamily); 1] = [(AF_UNIX, &unix::Unix)];

/// あとから登録したファミリー(ネットワークスタックが起動時に足す)。
/// 実装はずっと生きているので `'static` で持つ(動的に作るなら `Box::leak` する)
static FAMILIES: SpinLock<Vec<(u32, &'static dyn ProtocolFamily)>> = SpinLock::new(Vec::new());

/// ファミリーを登録する(同じ番号は後から登録したものが勝つ)
pub fn register(af: u32, family: &'static dyn ProtocolFamily) {
    FAMILIES.lock().insert(0, (af, family));
}

/// 番号からファミリーを引く。なければ EAFNOSUPPORT
pub fn family(af: u32) -> Result<&'static dyn ProtocolFamily, Errno> {
    let registered = FAMILIES
        .lock()
        .iter()
        .find(|(n, _)| *n == af)
        .map(|(_, f)| *f);
    registered
        .or_else(|| BUILTIN.iter().find(|(n, _)| *n == af).map(|(_, f)| *f))
        .ok_or(Errno::EAFNOSUPPORT)
}

/// `type` を (種類, フラグ) に分ける。種類が範囲外なら EINVAL
pub fn split_type(ty: u32) -> Result<(u32, u32), Errno> {
    let flags = ty & (SOCK_NONBLOCK | SOCK_CLOEXEC);
    let kind = ty & !flags;
    if kind == 0 || kind >= SOCK_MAX {
        return Err(Errno::EINVAL);
    }
    Ok((kind, flags))
}
