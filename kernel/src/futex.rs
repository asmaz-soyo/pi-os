//! futex(fast userspace mutex)の待ちの表。
//!
//! ユーザー空間のミューテックスは、争いがなければカーネルに来ない(アトミックな命令だけで済む)。
//! 争いがあったときだけ「このアドレスの値がまだ `val` なら眠らせて」(FUTEX_WAIT)、
//! 「このアドレスで待っている人を n 人起こして」(FUTEX_WAKE)とカーネルに頼む。
//!
//! 鍵は (アドレス空間の目印, アドレス)。同じアドレスでも、別のプロセスなら別の futex。
//! 起こすときは列から取り除くので、起きた側は「列にもういない = 起こされた」と分かる
//! (余分な目覚めや期限切れと区別できる)。

use crate::sched::Tid;
use crate::sync::SpinLock;
use alloc::collections::{BTreeMap, VecDeque};
use alloc::vec::Vec;

/// (アドレス空間の目印, ユーザーのアドレス)
pub type Key = (usize, u64);

#[derive(Default)]
pub struct Futexes {
    queues: BTreeMap<Key, VecDeque<Tid>>,
}

impl Futexes {
    pub const fn new() -> Self {
        Self {
            queues: BTreeMap::new(),
        }
    }

    /// `key` の列の最後に `tid` を積む
    pub fn enqueue(&mut self, key: Key, tid: Tid) {
        self.queues.entry(key).or_default().push_back(tid);
    }

    /// `key` の列の先頭から最大 `n` 人を取り出す(起こすのは呼ぶ側)
    pub fn take(&mut self, key: Key, n: usize) -> Vec<Tid> {
        let Some(q) = self.queues.get_mut(&key) else {
            return Vec::new();
        };
        let taken: Vec<Tid> = q.drain(..n.min(q.len())).collect();
        if q.is_empty() {
            self.queues.remove(&key);
        }
        taken
    }

    /// `tid` がまだ `key` の列にいるか(いなければ、だれかに起こされた)
    pub fn is_queued(&self, key: Key, tid: Tid) -> bool {
        self.queues.get(&key).is_some_and(|q| q.contains(&tid))
    }

    /// 列から外す(期限切れ、強制終了のとき)
    pub fn remove(&mut self, key: Key, tid: Tid) {
        if let Some(q) = self.queues.get_mut(&key) {
            q.retain(|&t| t != tid);
            if q.is_empty() {
                self.queues.remove(&key);
            }
        }
    }

    /// `from` の先頭から `n` 人を取り出し(起こす)、続く最大 `m` 人を `to` の列へ移す(FUTEX_REQUEUE)
    pub fn requeue(&mut self, from: Key, n: usize, to: Key, m: usize) -> Vec<Tid> {
        let woken = self.take(from, n);
        let moved = self.take(from, m);
        for t in moved {
            self.enqueue(to, t);
        }
        woken
    }
}

/// カーネル全体の futex の表
pub static FUTEXES: SpinLock<Futexes> = SpinLock::new(Futexes::new());

/// `key` で待っているスレッドを最大 `n` 人起こす。起こした数を返す
pub fn wake(key: Key, n: usize) -> usize {
    let woken = FUTEXES.lock().take(key, n);
    for &t in &woken {
        crate::sched::wake(t);
    }
    woken.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: Key = (1, 0x1000);
    const B: Key = (1, 0x2000);

    #[test]
    fn wake_takes_waiters_in_order_and_at_most_n() {
        let mut f = Futexes::new();
        for t in [10, 11, 12] {
            f.enqueue(A, t);
        }
        assert_eq!(f.take(A, 2), [10, 11]);
        assert!(!f.is_queued(A, 10) && f.is_queued(A, 12));
        assert_eq!(f.take(A, 5), [12]);
        assert_eq!(f.take(A, 5), Vec::<Tid>::new(), "だれも待っていない");
    }

    #[test]
    fn the_same_address_in_another_address_space_is_another_futex() {
        let mut f = Futexes::new();
        f.enqueue((1, 0x1000), 10);
        f.enqueue((2, 0x1000), 20);
        assert_eq!(f.take((2, 0x1000), 9), [20]);
        assert!(f.is_queued((1, 0x1000), 10));
    }

    #[test]
    fn removed_waiters_are_not_woken() {
        let mut f = Futexes::new();
        f.enqueue(A, 10);
        f.enqueue(A, 11);
        f.remove(A, 10); // 期限が切れた
        assert_eq!(f.take(A, 9), [11]);
    }

    #[test]
    fn requeue_wakes_some_and_moves_the_rest() {
        // 条件変数の broadcast:1 人だけ起こし、残りはミューテックスの列へ(一斉に起こして争わせない)
        let mut f = Futexes::new();
        for t in [10, 11, 12, 13] {
            f.enqueue(A, t);
        }
        f.enqueue(B, 99);
        assert_eq!(f.requeue(A, 1, B, 2), [10]);
        assert_eq!(f.take(A, 9), [13], "移しきれなかった分は残る");
        assert_eq!(f.take(B, 9), [99, 11, 12]);
    }
}
