//! timerfd:設定した時刻が来るたびに「満了回数」が増えるファイル。
//!
//! `read` すると満了回数(8 バイト)が返り、0 に戻る。まだ満了していなければ待つ。
//! epoll と組み合わせると「5 秒ごとに起きる」アプリが書ける(計画書の「周期」トリガー)。

use crate::Errno;
use crate::fs::vfs::{File, FileKind, Stat};
use crate::sync::SpinLock;

#[derive(Debug, Default, Clone, Copy)]
struct State {
    /// 次に満了する時刻(起動からのナノ秒)。None なら止まっている
    next: Option<u64>,
    /// 2 回目以降の間隔(0 なら 1 回だけ)
    interval: u64,
    /// まだ読まれていない満了回数
    pending: u64,
}

#[derive(Default)]
pub struct TimerFd {
    state: SpinLock<State>,
    /// 止まっているタイマーを読んで待つタスク(settime で起こす)
    wq: crate::wait::WaitQueue,
}

impl TimerFd {
    pub fn new() -> Self {
        Self::default()
    }

    /// 今の時刻までの満了を数える。
    fn update(state: &mut State, now: u64) {
        let Some(next) = state.next else { return };
        if now < next {
            return;
        }
        match (now - next).checked_div(state.interval) {
            // 間隔が 0 なら 1 回だけ
            None => {
                state.pending += 1;
                state.next = None;
            }
            Some(extra) => {
                let n = 1 + extra;
                state.pending += n;
                state.next = Some(next + n * state.interval);
            }
        }
    }

    /// 最初の満了を `first`(起動からのナノ秒、None なら止める)、以降の間隔を `interval` にする。
    pub fn set(&self, first: Option<u64>, interval: u64) {
        let mut s = self.state.lock();
        *s = State {
            next: first,
            interval,
            pending: 0,
        };
        self.wq.wake_all();
    }

    /// (間隔, 次の満了までの残り)
    pub fn get(&self) -> (u64, u64) {
        let mut s = self.state.lock();
        let now = crate::time::now_ns();
        Self::update(&mut s, now);
        (s.interval, s.next.map_or(0, |n| n.saturating_sub(now)))
    }
}

impl File for TimerFd {
    fn stat(&self) -> Stat {
        Stat {
            kind: FileKind::CharDevice,
            mode: 0o600,
            size: 0,
            ino: 0x2_0000,
            uid: 0,
            gid: 0,
            ..Default::default()
        }
    }

    fn read(&self, _offset: u64, buf: &mut [u8]) -> Result<usize, Errno> {
        if buf.len() < 8 {
            return Err(Errno::EINVAL);
        }
        loop {
            let next = {
                let mut s = self.state.lock();
                Self::update(&mut s, crate::time::now_ns());
                if s.pending > 0 {
                    buf[..8].copy_from_slice(&s.pending.to_le_bytes());
                    s.pending = 0;
                    return Ok(8);
                }
                s.next
            };
            // 次の満了まで眠る(止まっているタイマーなら、settime で動き出すのを待つ)
            match next {
                Some(t) => crate::sched::sleep_until(t),
                None => {
                    crate::wait::wait_event(&[&self.wq], None, || {
                        self.state.lock().next.map(|_| ())
                    });
                }
            }
        }
    }

    fn poll_readable(&self) -> bool {
        let mut s = self.state.lock();
        Self::update(&mut s, crate::time::now_ns());
        s.pending > 0
    }

    /// 次の満了の時刻(poll / epoll はそこで目を覚ます)
    fn next_event_at(&self) -> Option<u64> {
        self.state.lock().next
    }

    fn as_any(&self) -> Option<&dyn core::any::Any> {
        Some(self)
    }
}
