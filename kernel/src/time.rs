//! 時刻とタイマのティック。
//!
//! タイマ割り込みを一定間隔(`hz` 回/秒)で起こし、その回数(ティック)を数える。
//! スケジューラやスリープは、後でこのティックの上に作る。

use core::sync::atomic::{AtomicU64, Ordering};
use hal::timer::Timer;

pub struct Clock {
    ticks: AtomicU64,
    interval: AtomicU64,
    hz: AtomicU64,
}

impl Clock {
    pub const fn new() -> Self {
        Self {
            ticks: AtomicU64::new(0),
            interval: AtomicU64::new(0),
            hz: AtomicU64::new(0),
        }
    }

    /// 1 秒に `hz` 回のタイマ割り込みを始める。
    pub fn start(&self, timer: &mut impl Timer, hz: u64) {
        let interval = timer.frequency() / hz;
        self.interval.store(interval, Ordering::Relaxed);
        self.hz.store(hz, Ordering::Relaxed);
        timer.set_timeout(interval);
    }

    /// タイマ割り込みのたびに呼ぶ。ティックを数え、次の割り込みを予約する。
    pub fn tick(&self, timer: &mut impl Timer) {
        self.ticks.fetch_add(1, Ordering::Relaxed);
        timer.set_timeout(self.interval.load(Ordering::Relaxed));
    }

    pub fn ticks(&self) -> u64 {
        self.ticks.load(Ordering::Relaxed)
    }

    pub fn hz(&self) -> u64 {
        self.hz.load(Ordering::Relaxed)
    }
}

impl Default for Clock {
    fn default() -> Self {
        Self::new()
    }
}

/// カーネル全体の時計。
pub static CLOCK: Clock = Clock::new();

/// ティックが `n` 進むまで待つ(割り込みが有効になっていること)。
pub fn wait_ticks(n: u64) {
    let target = CLOCK.ticks() + n;
    while CLOCK.ticks() < target {
        core::hint::spin_loop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hal::timer::Timer;
    use std::vec::Vec;

    #[derive(Default)]
    struct FakeTimer {
        timeouts: Vec<u64>,
    }

    impl Timer for FakeTimer {
        fn frequency(&self) -> u64 {
            62_500_000
        }
        fn now(&self) -> u64 {
            0
        }
        fn set_timeout(&mut self, ticks: u64) {
            self.timeouts.push(ticks);
        }
    }

    #[test]
    fn start_arms_the_timer_for_one_tick_interval() {
        let clock = Clock::new();
        let mut timer = FakeTimer::default();
        clock.start(&mut timer, 100);
        assert_eq!(timer.timeouts, [625_000]);
        assert_eq!(clock.hz(), 100);
    }

    #[test]
    fn tick_counts_and_rearms() {
        let clock = Clock::new();
        let mut timer = FakeTimer::default();
        clock.start(&mut timer, 100);
        clock.tick(&mut timer);
        clock.tick(&mut timer);
        assert_eq!(clock.ticks(), 2);
        assert_eq!(timer.timeouts, [625_000, 625_000, 625_000]);
    }
}
