//! ロック。マルチコアを前提に、スピンロックを使う。
//!
//! ロックを持っているあいだは、カーネルのプリエンプション(割り込みでの切り替え)をしない。
//! 持ったまま切り替わると、次のタスクが同じロックを待って回り続け、持ち主に順番が回らない。
//! 持っているロックの数を `preempt_count` で数える(Linux の preempt_count と同じ考え)。

use core::ops::{Deref, DerefMut};

/// 今持っているロックの数。今は 1 つのコアだけが動くので、カーネル全体で 1 つ
/// (コアを増やすときは、コアごとに持つ)
#[cfg(not(test))]
static PREEMPT: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);

#[cfg(test)]
std::thread_local! {
    // ホストのテストは並んで走るので、スレッドごとに数える
    static PREEMPT: core::cell::Cell<usize> = const { core::cell::Cell::new(0) };
}

#[cfg(not(test))]
fn preempt_add(delta: isize) {
    use core::sync::atomic::Ordering;
    if delta > 0 {
        PREEMPT.fetch_add(1, Ordering::SeqCst);
    } else {
        PREEMPT.fetch_sub(1, Ordering::SeqCst);
    }
}

#[cfg(test)]
fn preempt_add(delta: isize) {
    PREEMPT.with(|c| c.set((c.get() as isize + delta) as usize));
}

/// 今持っているロックの数(0 なら、割り込みで切り替えてよい)
pub fn preempt_count() -> usize {
    #[cfg(not(test))]
    return PREEMPT.load(core::sync::atomic::Ordering::SeqCst);
    #[cfg(test)]
    return PREEMPT.with(|c| c.get());
}

/// スピンロック。割り込みハンドラからも使うものは、持つ側で割り込みを止めること
pub struct SpinLock<T: ?Sized> {
    inner: spin::Mutex<T>,
}

/// ロックを持っている印。離すと(drop)ロックが外れ、`preempt_count` が 1 減る
pub struct SpinLockGuard<'a, T: ?Sized> {
    guard: core::mem::ManuallyDrop<spin::MutexGuard<'a, T>>,
}

impl<T> SpinLock<T> {
    pub const fn new(value: T) -> Self {
        Self {
            inner: spin::Mutex::new(value),
        }
    }
}

impl<T: ?Sized> SpinLock<T> {
    pub fn lock(&self) -> SpinLockGuard<'_, T> {
        // 先に数を増やす(待っているあいだに切り替わって、持ち主を待たせないように)
        preempt_add(1);
        SpinLockGuard {
            guard: core::mem::ManuallyDrop::new(self.inner.lock()),
        }
    }

    pub fn try_lock(&self) -> Option<SpinLockGuard<'_, T>> {
        preempt_add(1);
        match self.inner.try_lock() {
            Some(g) => Some(SpinLockGuard {
                guard: core::mem::ManuallyDrop::new(g),
            }),
            None => {
                preempt_add(-1);
                None
            }
        }
    }

    /// `&mut` で持っていれば、だれも持っていないので、ロックせずに中身を触れる
    pub fn get_mut(&mut self) -> &mut T {
        self.inner.get_mut()
    }

    pub fn is_locked(&self) -> bool {
        self.inner.is_locked()
    }

    /// 持ち主に関係なくロックを外す(パニックの表示で、固まらないようにするため)
    ///
    /// # Safety
    /// 持ち主はもう続きを実行しないこと
    pub unsafe fn force_unlock(&self) {
        // SAFETY: 呼ぶ側が保証する
        unsafe { self.inner.force_unlock() }
    }
}

impl<T: Default> Default for SpinLock<T> {
    fn default() -> Self {
        Self::new(T::default())
    }
}

impl<T: ?Sized> Deref for SpinLockGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.guard
    }
}

impl<T: ?Sized> DerefMut for SpinLockGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        &mut self.guard
    }
}

impl<T: ?Sized> Drop for SpinLockGuard<'_, T> {
    fn drop(&mut self) {
        // SAFETY: ここで一度だけ離す
        unsafe { core::mem::ManuallyDrop::drop(&mut self.guard) };
        preempt_add(-1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn holding_a_lock_disables_preemption_until_it_is_released() {
        // テストは並んで走るので、自分のスレッドでの増減だけを見る
        let lock = SpinLock::new(5);
        let before = preempt_count();
        {
            let mut a = lock.lock();
            *a += 1;
            assert_eq!(preempt_count(), before + 1);
            let other = SpinLock::new(0);
            let _b = other.lock();
            assert_eq!(preempt_count(), before + 2);
        }
        assert_eq!(preempt_count(), before);
        assert_eq!(*lock.lock(), 6);
        assert!(lock.try_lock().is_some());
        assert_eq!(preempt_count(), before);
    }
}
