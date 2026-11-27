//! 待ち行列(wait queue):出来事を待つタスクを眠らせ、出来事が起きたら起こす。
//!
//! ```text
//! 待つ側:  wait_event(&[&q], 期限, || 条件)       起こす側:  (状態を変える)
//!           1. q に自分を登録                                  q.wake_all()
//!           2. 条件を見る(満たされていれば終わり)
//!           3. 眠る(起こされるか、期限まで)
//!           4. 登録を外して 1 へ
//! ```
//!
//! 「登録してから条件を見る」順番が大事。条件を見た直後(眠る前)に起こされても、
//! `sched::wake` が「次の眠りをすぐ終わらせる」印を付けるので、起こしたことが失われない。
//! スケジューラが動いていないとき(ホストのテスト、起動の途中)は、条件を見ながら回って待つ。
//!
//! poll / select / epoll は、いくつものファイルを同時に待つ。Linux はファイルごとの待ち行列に
//! 登録するが、ここでは簡単に、どの待ち行列が起こされても [`POLL_WQ`] も起こす(関係のない
//! 目覚めは増えるが、それぞれが見直すので正しい)。時間で変わるファイル(timerfd など)は
//! `File::next_event_at` の時刻を期限に入れる。

use crate::sched::{self, Tid};
use crate::sync::SpinLock;
use alloc::vec::Vec;

pub struct WaitQueue {
    waiters: SpinLock<Vec<Tid>>,
}

/// poll / select / epoll が待つ列。ほかのどの待ち行列が起こされても、これも起こされる
pub static POLL_WQ: WaitQueue = WaitQueue::new();

impl Default for WaitQueue {
    fn default() -> Self {
        Self::new()
    }
}

impl WaitQueue {
    pub const fn new() -> Self {
        WaitQueue {
            waiters: SpinLock::new(Vec::new()),
        }
    }

    /// 待っているタスクを全部起こす(割り込みのハンドラから呼んでよい)
    pub fn wake_all(&self) {
        let waiters = core::mem::take(&mut *self.waiters.lock());
        for tid in waiters {
            sched::wake(tid);
        }
        if !core::ptr::eq(self, &POLL_WQ) {
            POLL_WQ.wake_all();
        }
    }

    fn add(&self, tid: Tid) {
        let mut w = self.waiters.lock();
        if !w.contains(&tid) {
            w.push(tid);
        }
    }

    fn remove(&self, tid: Tid) {
        self.waiters.lock().retain(|&t| t != tid);
    }

    /// 待っているタスクの数
    pub fn waiting(&self) -> usize {
        self.waiters.lock().len()
    }
}

/// 待ち行列への登録。作ると登録し、捨てると外す。登録してから条件を見て、満たされなければ
/// [`Registration::sleep`] で眠る(`wait_event` の中身。poll のように、条件を関数で渡しにくいときに使う)
pub struct Registration<'a> {
    queues: &'a [&'a WaitQueue],
    me: Option<Tid>,
}

/// poll / select / epoll の登録([`POLL_WQ`] だけ)
pub fn register_poll() -> Registration<'static> {
    static QUEUES: [&WaitQueue; 1] = [&POLL_WQ];
    register(&QUEUES)
}

pub fn register<'a>(queues: &'a [&'a WaitQueue]) -> Registration<'a> {
    let me = sched::running().then(sched::current);
    if let Some(tid) = me {
        for q in queues {
            q.add(tid);
        }
    }
    Registration { queues, me }
}

impl Registration<'_> {
    /// 起こされるか、期限まで眠る(スケジューラが動いていなければ、少し回るだけ)
    pub fn sleep(&self, deadline: Option<u64>) {
        if self.me.is_none() {
            core::hint::spin_loop();
            return;
        }
        sched::sleep_until(deadline.unwrap_or(u64::MAX));
    }
}

impl Drop for Registration<'_> {
    fn drop(&mut self) {
        if let Some(tid) = self.me {
            for q in self.queues {
                q.remove(tid);
            }
        }
    }
}

/// `cond` が Some を返すまで、`queues` のどれかで起こされるのを眠って待つ。
/// `deadline`(ナノ秒の時刻)を過ぎたら None
pub fn wait_event<R>(
    queues: &[&WaitQueue],
    deadline: Option<u64>,
    mut cond: impl FnMut() -> Option<R>,
) -> Option<R> {
    loop {
        // 1. 先に登録する(条件を見たあと、眠る前に起こされても失わないように)
        let reg = register(queues);
        // 2. 条件を見る
        if let Some(r) = cond() {
            return Some(r);
        }
        if deadline.is_some_and(|d| crate::time::now_ns() >= d) {
            return None;
        }
        // 3. 眠る(起こされるか、期限まで)。4. 登録を外して(reg を捨てて)もう一度見る
        reg.sleep(deadline);
    }
}

/// `cond` が Some を返すまで眠って待つ。マスクされていないシグナルが来たら EINTR
/// (ユーザーのプロセスの待ち。シグナルは、ユーザーに戻る直前に配られる)
pub fn wait_event_intr<R>(
    queues: &[&WaitQueue],
    mut cond: impl FnMut() -> Option<R>,
) -> Result<R, crate::Errno> {
    loop {
        let reg = register(queues);
        if let Some(r) = cond() {
            return Ok(r);
        }
        if interrupted() {
            return Err(crate::Errno::EINTR);
        }
        reg.sleep(None);
    }
}

/// 今のスレッドに、待ちを中断するシグナルが来ているか
pub fn interrupted() -> bool {
    sched::running() && crate::process::PROCESSES.interrupted(sched::current())
}

/// 2 つの期限の早いほう
pub fn earliest(a: Option<u64>, b: Option<u64>) -> Option<u64> {
    match (a, b) {
        (Some(x), Some(y)) => Some(x.min(y)),
        (x, y) => x.or(y),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_satisfied_condition_returns_at_once_and_leaves_no_waiter() {
        let q = WaitQueue::new();
        assert_eq!(wait_event(&[&q], None, || Some(7)), Some(7));
        assert_eq!(q.waiting(), 0);
    }

    #[test]
    fn a_passed_deadline_gives_up() {
        let q = WaitQueue::new();
        let mut looked = 0;
        let r: Option<()> = wait_event(&[&q], Some(0), || {
            looked += 1;
            None
        });
        assert_eq!(r, None);
        assert_eq!(looked, 1, "期限を過ぎていても、一度は条件を見る");
        assert_eq!(q.waiting(), 0);
    }

    #[test]
    fn without_a_scheduler_it_polls_until_the_condition_holds() {
        let q = WaitQueue::new();
        let mut n = 0;
        assert_eq!(
            wait_event(&[&q], None, || {
                n += 1;
                (n == 5).then_some(n)
            }),
            Some(5)
        );
        q.wake_all(); // だれもいなくてもよい
    }
}
