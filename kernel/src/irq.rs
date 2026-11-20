//! 割り込みの振り分け。割り込み番号ごとにハンドラを登録しておき、届いたら呼ぶ。

use crate::Errno;
use crate::sync::SpinLock;
use core::sync::atomic::{AtomicUsize, Ordering};
use hal::irq::IrqController;

/// 登録できる割り込み番号の上限。
pub const MAX_IRQS: usize = 128;

/// 割り込みハンドラ。割り込みが禁止された状態で呼ばれるので、短く済ませること。
pub type Handler = fn();

pub struct IrqTable {
    handlers: SpinLock<[Option<Handler>; MAX_IRQS]>,
    unhandled: AtomicUsize,
}

impl IrqTable {
    pub const fn new() -> Self {
        Self {
            handlers: SpinLock::new([None; MAX_IRQS]),
            unhandled: AtomicUsize::new(0),
        }
    }

    pub fn register(&self, irq: u32, handler: Handler) -> Result<(), Errno> {
        let mut handlers = self.handlers.lock();
        let slot = handlers.get_mut(irq as usize).ok_or(Errno::EINVAL)?;
        *slot = Some(handler);
        Ok(())
    }

    /// 保留中の割り込みがなくなるまで、ハンドラを呼んで終了を伝える。
    pub fn dispatch(&self, intc: &mut impl IrqController) {
        while let Some(irq) = intc.pending() {
            // ハンドラを呼ぶ前にロックを離す(ハンドラが登録を変えても固まらない)
            let handler = self.handlers.lock().get(irq as usize).copied().flatten();
            match handler {
                Some(handler) => handler(),
                None => {
                    self.unhandled.fetch_add(1, Ordering::Relaxed);
                }
            }
            intc.end_of_interrupt(irq);
        }
    }

    /// ハンドラが登録されていなかった割り込みの数。
    pub fn unhandled(&self) -> usize {
        self.unhandled.load(Ordering::Relaxed)
    }
}

impl Default for IrqTable {
    fn default() -> Self {
        Self::new()
    }
}

/// カーネル全体の割り込み表。
pub static IRQS: IrqTable = IrqTable::new();

#[cfg(test)]
mod tests {
    use super::*;
    use core::sync::atomic::{AtomicUsize, Ordering};
    use hal::irq::IrqController;
    use std::vec::Vec;

    #[derive(Default)]
    struct FakeIntc {
        queue: Vec<u32>,
        eoi: Vec<u32>,
    }

    impl IrqController for FakeIntc {
        fn enable(&mut self, _irq: u32) {}
        fn pending(&mut self) -> Option<u32> {
            if self.queue.is_empty() {
                None
            } else {
                Some(self.queue.remove(0))
            }
        }
        fn end_of_interrupt(&mut self, irq: u32) {
            self.eoi.push(irq);
        }
    }

    static A: AtomicUsize = AtomicUsize::new(0);
    static B: AtomicUsize = AtomicUsize::new(0);
    fn handler_a() {
        A.fetch_add(1, Ordering::SeqCst);
    }
    fn handler_b() {
        B.fetch_add(1, Ordering::SeqCst);
    }

    #[test]
    fn dispatch_calls_handlers_for_all_pending_irqs_and_acknowledges_them() {
        let table = IrqTable::new();
        table.register(30, handler_a).unwrap();
        table.register(31, handler_b).unwrap();
        let mut intc = FakeIntc {
            queue: std::vec![30, 31, 30],
            ..Default::default()
        };
        let (a0, b0) = (A.load(Ordering::SeqCst), B.load(Ordering::SeqCst));
        table.dispatch(&mut intc);
        assert_eq!(A.load(Ordering::SeqCst) - a0, 2);
        assert_eq!(B.load(Ordering::SeqCst) - b0, 1);
        assert_eq!(intc.eoi, [30, 31, 30]);
    }

    #[test]
    fn unregistered_irq_is_counted_and_acknowledged() {
        let table = IrqTable::new();
        let mut intc = FakeIntc {
            queue: std::vec![7],
            ..Default::default()
        };
        table.dispatch(&mut intc);
        assert_eq!(table.unhandled(), 1);
        assert_eq!(intc.eoi, [7]);
    }

    #[test]
    fn register_rejects_out_of_range_irq() {
        let table = IrqTable::new();
        assert_eq!(
            table.register(MAX_IRQS as u32, handler_a),
            Err(crate::Errno::EINVAL)
        );
    }
}
