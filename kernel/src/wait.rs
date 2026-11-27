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

use crate::sched::{self, Tid};
use crate::sync::SpinLock;
use alloc::vec::Vec;

pub struct WaitQueue {
    waiters: SpinLock<Vec<Tid>>,
}

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

/// `cond` が Some を返すまで、`queues` のどれかで起こされるのを眠って待つ。
/// `deadline`(ナノ秒の時刻)を過ぎたら None
pub fn wait_event<R>(
    queues: &[&WaitQueue],
    deadline: Option<u64>,
    mut cond: impl FnMut() -> Option<R>,
) -> Option<R> {
    if !sched::running() {
        // 眠れない:条件を見ながら回る
        loop {
            if let Some(r) = cond() {
                return Some(r);
            }
            if deadline.is_some_and(|d| crate::time::now_ns() >= d) {
                return None;
            }
            core::hint::spin_loop();
        }
    }
    let me = sched::current();
    loop {
        // 1. 先に登録する(条件を見たあと、眠る前に起こされても失わないように)
        for q in queues {
            q.add(me);
        }
        // 2. 条件を見る
        let r = cond();
        let expired = deadline.is_some_and(|d| crate::time::now_ns() >= d);
        if r.is_some() || expired {
            for q in queues {
                q.remove(me);
            }
            return r;
        }
        // 3. 眠る(起こされるか、期限まで)
        sched::sleep_until(deadline.unwrap_or(u64::MAX));
        // 4. 登録を外して、もう一度見る(起こされても、条件が満たされたとは限らない)
        for q in queues {
            q.remove(me);
        }
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
