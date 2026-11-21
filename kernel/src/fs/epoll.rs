//! epoll:登録した fd のうち、準備ができたものを教えるファイル。

use crate::fs::vfs::{File, FileKind, Stat};
use crate::sync::SpinLock;
use alloc::collections::BTreeMap;

pub const EPOLLIN: u32 = 0x1;
pub const EPOLLOUT: u32 = 0x4;

/// 関心のある fd → (待つ出来事, 返すときに添える値)
#[derive(Default)]
pub struct Epoll {
    pub interest: SpinLock<BTreeMap<u64, (u32, u64)>>,
}

impl Epoll {
    pub fn new() -> Self {
        Self::default()
    }
}

impl File for Epoll {
    fn stat(&self) -> Stat {
        Stat {
            kind: FileKind::CharDevice,
            mode: 0o600,
            size: 0,
            ino: 0x3_0000,
            uid: 0,
            gid: 0,
            ..Default::default()
        }
    }

    fn poll_readable(&self) -> bool {
        false // epoll の入れ子はまだ扱わない
    }

    fn as_any(&self) -> Option<&dyn core::any::Any> {
        Some(self)
    }
}
