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

// ---- 割り込みの状態 ----
//
// 割り込みのハンドラも取るロックを、割り込みを許したまま取ると、持っているあいだに来た
// 割り込みのハンドラが同じロックを待ち続ける(コアが 1 つなら、持ち主に二度と戻らない)。
// そういうロックは `lock_irqsave` で取る。BSP は CPU の割り込みの操作を `set_irq_ops` で登録し、
// 割り込みの入口は `irq_enter` / `irq_exit` で「割り込みの中」を知らせる。

/// CPU の割り込みの操作。BSP が [`set_irq_ops`] で登録する
#[derive(Clone, Copy)]
pub struct IrqOps {
    /// 割り込みを止め、止める前に許されていたかを返す
    pub save: fn() -> bool,
    /// `save` の前の状態に戻す
    pub restore: fn(bool),
    /// 今、割り込みが許されているか
    pub enabled: fn() -> bool,
}

#[cfg(not(test))]
static IRQ_OPS: spin::Once<IrqOps> = spin::Once::new();

/// 割り込みのハンドラを実行中の深さ(今は 1 つのコアだけが動くので、カーネル全体で 1 つ)
#[cfg(not(test))]
static IRQ_DEPTH: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);

#[cfg(not(test))]
pub fn set_irq_ops(ops: IrqOps) {
    IRQ_OPS.call_once(|| ops);
}

/// 割り込みを止め、止める前に許されていたかを返す(登録の前は何もしない)
#[cfg(not(test))]
pub fn irq_save() -> bool {
    IRQ_OPS.get().is_some_and(|o| (o.save)())
}

/// [`irq_save`] の前の状態に戻す
#[cfg(not(test))]
pub fn irq_restore(enabled: bool) {
    if let Some(o) = IRQ_OPS.get() {
        (o.restore)(enabled);
    }
}

/// 今、割り込みが許されているか(登録の前は、まだ許していないので false)
#[cfg(not(test))]
pub fn irqs_enabled() -> bool {
    IRQ_OPS.get().is_some_and(|o| (o.enabled)())
}

#[cfg(not(test))]
pub fn irq_enter() {
    IRQ_DEPTH.fetch_add(1, core::sync::atomic::Ordering::SeqCst);
}

#[cfg(not(test))]
pub fn irq_exit() {
    IRQ_DEPTH.fetch_sub(1, core::sync::atomic::Ordering::SeqCst);
}

/// 割り込みのハンドラを実行中か
#[cfg(not(test))]
pub fn in_irq() -> bool {
    IRQ_DEPTH.load(core::sync::atomic::Ordering::SeqCst) > 0
}

/// ホストのテストでの割り込みの状態。テストは並んで走るので、スレッドごとの作り物にする
#[cfg(test)]
pub(crate) mod test_irq {
    use core::cell::Cell;
    std::thread_local! {
        pub(super) static ENABLED: Cell<bool> = const { Cell::new(false) };
        pub(super) static DEPTH: Cell<usize> = const { Cell::new(0) };
    }
    pub fn set_enabled(on: bool) {
        ENABLED.with(|c| c.set(on));
    }
}

#[cfg(test)]
pub fn irq_save() -> bool {
    test_irq::ENABLED.with(|c| c.replace(false))
}

#[cfg(test)]
pub fn irq_restore(enabled: bool) {
    if enabled {
        test_irq::set_enabled(true);
    }
}

#[cfg(test)]
pub fn irqs_enabled() -> bool {
    test_irq::ENABLED.with(|c| c.get())
}

#[cfg(test)]
pub fn irq_enter() {
    test_irq::DEPTH.with(|c| c.set(c.get() + 1));
}

#[cfg(test)]
pub fn irq_exit() {
    test_irq::DEPTH.with(|c| c.set(c.get() - 1));
}

#[cfg(test)]
pub fn in_irq() -> bool {
    test_irq::DEPTH.with(|c| c.get() > 0)
}

// ---- ロックの使われ方の検査(デバッグのビルドだけ。Linux の lockdep の小さな版) ----

/// パニックの報告が始まったら、検査をやめる(報告は割り込みの中でもコンソールに書くので、
/// 検査がもう一度パニックして、最初の原因が隠れてしまう)
#[cfg(not(test))]
static LOCK_CHECKS_OFF: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

#[cfg(test)]
std::thread_local! {
    static LOCK_CHECKS_OFF: core::cell::Cell<bool> = const { core::cell::Cell::new(false) };
}

/// ロックの使われ方の検査をやめる(パニックの報告の最初に呼ぶ)
pub fn stop_lock_checks() {
    #[cfg(not(test))]
    LOCK_CHECKS_OFF.store(true, core::sync::atomic::Ordering::SeqCst);
    #[cfg(test)]
    LOCK_CHECKS_OFF.with(|c| c.set(true));
}

#[cfg(debug_assertions)]
fn lock_checks_off() -> bool {
    #[cfg(not(test))]
    return LOCK_CHECKS_OFF.load(core::sync::atomic::Ordering::Relaxed);
    #[cfg(test)]
    return LOCK_CHECKS_OFF.with(|c| c.get());
}

/// 割り込みのハンドラの中で取られたことがある
#[cfg(debug_assertions)]
const USED_IN_IRQ: u8 = 1;
/// 割り込みを許したまま取られたことがある
#[cfg(debug_assertions)]
const USED_WITH_IRQS_ON: u8 = 2;

/// スピンロック。割り込みハンドラからも使うものは、[`SpinLock::lock_irqsave`] で取ること。
/// デバッグのビルドでは、割り込みの中と、割り込みを許したままの両方で取られたらパニックする
pub struct SpinLock<T: ?Sized> {
    #[cfg(debug_assertions)]
    usage: core::sync::atomic::AtomicU8,
    inner: spin::Mutex<T>,
}

/// ロックを持っている印。離すと(drop)ロックが外れ、`preempt_count` が 1 減る
pub struct SpinLockGuard<'a, T: ?Sized> {
    guard: core::mem::ManuallyDrop<spin::MutexGuard<'a, T>>,
}

/// 割り込みを止めて持っている印。離すとロックが外れ、割り込みが元の状態に戻る
pub struct SpinLockIrqGuard<'a, T: ?Sized> {
    guard: core::mem::ManuallyDrop<SpinLockGuard<'a, T>>,
    irqs_were_enabled: bool,
}

impl<T> SpinLock<T> {
    pub const fn new(value: T) -> Self {
        Self {
            #[cfg(debug_assertions)]
            usage: core::sync::atomic::AtomicU8::new(0),
            inner: spin::Mutex::new(value),
        }
    }
}

impl<T: ?Sized> SpinLock<T> {
    #[track_caller]
    pub fn lock(&self) -> SpinLockGuard<'_, T> {
        self.check_usage();
        // 先に数を増やす(待っているあいだに切り替わって、持ち主を待たせないように)
        preempt_add(1);
        SpinLockGuard {
            guard: core::mem::ManuallyDrop::new(self.inner.lock()),
        }
    }

    /// 割り込みを止めてから取る。割り込みのハンドラも取るロックは、ハンドラの外ではこれで取る
    #[track_caller]
    pub fn lock_irqsave(&self) -> SpinLockIrqGuard<'_, T> {
        let irqs_were_enabled = irq_save();
        SpinLockIrqGuard {
            guard: core::mem::ManuallyDrop::new(self.lock()),
            irqs_were_enabled,
        }
    }

    /// 取るときの文脈を覚え、割り込みの中と、割り込みを許したままの両方を見たら知らせる。
    /// `#[track_caller]` なので、パニックの場所は今ロックを取ろうとした呼び出し元になる
    #[cfg(debug_assertions)]
    #[track_caller]
    fn check_usage(&self) {
        use core::sync::atomic::Ordering;
        let now = if in_irq() {
            USED_IN_IRQ
        } else if irqs_enabled() {
            USED_WITH_IRQS_ON
        } else {
            return;
        };
        if self.usage.load(Ordering::Relaxed) & now != 0 || lock_checks_off() {
            return;
        }
        let seen = self.usage.fetch_or(now, Ordering::Relaxed) | now;
        assert!(
            seen != USED_IN_IRQ | USED_WITH_IRQS_ON,
            "a SpinLock is taken both in an interrupt handler and with interrupts enabled \
             (use lock_irqsave outside the handler); this time {}",
            if now == USED_IN_IRQ {
                "in an interrupt handler"
            } else {
                "with interrupts enabled"
            }
        );
    }

    #[cfg(not(debug_assertions))]
    fn check_usage(&self) {}

    #[track_caller]
    pub fn try_lock(&self) -> Option<SpinLockGuard<'_, T>> {
        self.check_usage();
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

impl<T: ?Sized> Deref for SpinLockIrqGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.guard
    }
}

impl<T: ?Sized> DerefMut for SpinLockIrqGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        &mut self.guard
    }
}

impl<T: ?Sized> Drop for SpinLockIrqGuard<'_, T> {
    fn drop(&mut self) {
        // 先にロックを離してから、割り込みを戻す(逆だと、離す前に割り込みが来て待ち続ける)
        // SAFETY: ここで一度だけ離す
        unsafe { core::mem::ManuallyDrop::drop(&mut self.guard) };
        irq_restore(self.irqs_were_enabled);
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

    #[test]
    fn lock_irqsave_disables_interrupts_while_held_and_restores_them() {
        test_irq::set_enabled(true);
        let lock = SpinLock::new(1);
        {
            let mut g = lock.lock_irqsave();
            *g += 1;
            assert!(!irqs_enabled());
        }
        assert!(irqs_enabled());
        // もとから止まっていれば、止まったまま
        test_irq::set_enabled(false);
        drop(lock.lock_irqsave());
        assert!(!irqs_enabled());
        assert_eq!(*lock.lock(), 2);
    }

    #[test]
    fn in_irq_is_true_between_irq_enter_and_irq_exit() {
        assert!(!in_irq());
        irq_enter();
        assert!(in_irq());
        irq_exit();
        assert!(!in_irq());
    }

    #[test]
    #[should_panic(expected = "interrupt handler and with interrupts enabled")]
    fn a_lock_taken_in_an_interrupt_and_with_interrupts_enabled_panics() {
        // コアが 1 つなら、割り込みを許したまま持っているところへ割り込みが来て、
        // ハンドラが同じロックを待つと、二度と抜けない。両方の使い方を見たら知らせる
        let lock = SpinLock::new(0);
        irq_enter();
        drop(lock.lock());
        irq_exit();
        test_irq::set_enabled(true);
        drop(lock.lock());
    }

    #[test]
    fn lock_checks_stop_once_a_panic_is_being_reported() {
        // パニックの報告(割り込みの中でもコンソールに書く)で、もう一度パニックしないように
        let lock = SpinLock::new(0);
        stop_lock_checks();
        irq_enter();
        drop(lock.lock());
        irq_exit();
        test_irq::set_enabled(true);
        drop(lock.lock());
        test_irq::set_enabled(false);
    }

    #[test]
    fn a_lock_shared_with_an_interrupt_handler_is_fine_with_lock_irqsave() {
        let lock = SpinLock::new(0);
        irq_enter();
        drop(lock.lock());
        irq_exit();
        test_irq::set_enabled(true);
        drop(lock.lock_irqsave());
        // 割り込みと関係のないロックは、割り込みを許したまま取ってよい
        let other = SpinLock::new(0);
        drop(other.lock());
        drop(other.lock());
    }
}
