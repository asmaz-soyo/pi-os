//! GPIO の割り込みを、ファイル(`/dev/gpio/N`)として見せる。
//!
//! 割り込みハンドラが線ごとに出来事を数え、`read` は「前回から何回あったか」(8 バイト)を返す。
//! まだなければ待つ。epoll とも組み合わせられる(計画書の「割り込み駆動」トリガー)。

use crate::Errno;
use crate::fs::vfs::{File, FileKind, Stat};
use core::sync::atomic::{AtomicU64, Ordering};

pub const LINES: usize = 32;

/// 線ごとの、まだ読まれていない出来事の数
pub struct GpioEvents {
    pending: [AtomicU64; LINES],
}

impl GpioEvents {
    pub const fn new() -> Self {
        Self {
            pending: [const { AtomicU64::new(0) }; LINES],
        }
    }

    /// 割り込みハンドラから呼ぶ:`mask` のビットの線に出来事が 1 回あった
    pub fn record(&self, mask: u32) {
        for (line, count) in self.pending.iter().enumerate() {
            if mask & (1 << line) != 0 {
                count.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}

impl Default for GpioEvents {
    fn default() -> Self {
        Self::new()
    }
}

/// カーネル全体の GPIO の出来事
pub static GPIO: GpioEvents = GpioEvents::new();

/// `/dev/gpio/N`
pub struct GpioFile {
    events: &'static GpioEvents,
    line: usize,
}

impl GpioFile {
    pub fn new(events: &'static GpioEvents, line: usize) -> Self {
        Self { events, line }
    }
}

impl File for GpioFile {
    fn stat(&self) -> Stat {
        Stat {
            kind: FileKind::CharDevice,
            mode: 0o660,
            size: 0,
            ino: 0x6_0000 + self.line as u64,
            uid: 0,
            gid: 0,
        }
    }

    fn read(&self, _offset: u64, buf: &mut [u8]) -> Result<usize, Errno> {
        if buf.len() < 8 {
            return Err(Errno::EINVAL);
        }
        loop {
            let n = self.events.pending[self.line].swap(0, Ordering::Relaxed);
            if n > 0 {
                buf[..8].copy_from_slice(&n.to_le_bytes());
                return Ok(8);
            }
            crate::sched::relax();
        }
    }

    fn poll_readable(&self) -> bool {
        self.events.pending[self.line].load(Ordering::Relaxed) > 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fs::vfs::File;

    #[test]
    fn events_are_counted_per_line_and_read_resets_them() {
        let lines: &'static GpioEvents =
            std::boxed::Box::leak(std::boxed::Box::new(GpioEvents::new()));
        let f = GpioFile::new(lines, 5);
        assert!(!f.poll_readable());
        lines.record(1 << 5);
        lines.record(1 << 5 | 1 << 2);
        assert!(f.poll_readable());
        let mut buf = [0u8; 8];
        assert_eq!(f.read(0, &mut buf), Ok(8));
        assert_eq!(u64::from_le_bytes(buf), 2);
        assert!(!f.poll_readable(), "read resets the count");
        assert!(GpioFile::new(lines, 2).poll_readable());
    }

    #[test]
    fn reading_needs_an_8_byte_buffer() {
        let lines: &'static GpioEvents =
            std::boxed::Box::leak(std::boxed::Box::new(GpioEvents::new()));
        let f = GpioFile::new(lines, 0);
        let mut small = [0u8; 4];
        assert_eq!(f.read(0, &mut small), Err(crate::Errno::EINVAL));
    }
}
