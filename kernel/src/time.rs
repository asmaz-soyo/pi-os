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
    /// 次のティックの時刻(カウンタの値)
    next_tick: AtomicU64,
    /// 頼まれている一番早い目覚ましの時刻(カウンタの値)。なければ u64::MAX
    alarm: AtomicU64,
}

impl Clock {
    pub const fn new() -> Self {
        Self {
            ticks: AtomicU64::new(0),
            interval: AtomicU64::new(0),
            hz: AtomicU64::new(0),
            next_tick: AtomicU64::new(0),
            alarm: AtomicU64::new(u64::MAX),
        }
    }

    /// 1 秒に `hz` 回のタイマ割り込みを始める。
    pub fn start(&self, timer: &mut impl Timer, hz: u64) {
        let interval = timer.frequency() / hz;
        self.interval.store(interval, Ordering::Relaxed);
        self.hz.store(hz, Ordering::Relaxed);
        self.next_tick
            .store(timer.now() + interval, Ordering::Relaxed);
        self.arm(timer);
    }

    /// タイマ割り込みのたびに呼ぶ。ティックの時刻なら数えて true を返す
    /// (目覚ましのための割り込みなら false)。そして次の割り込みを予約する。
    pub fn tick(&self, timer: &mut impl Timer) -> bool {
        let now = timer.now();
        let interval = self.interval.load(Ordering::Relaxed).max(1);
        let next = self.next_tick.load(Ordering::Relaxed);
        let is_tick = now >= next;
        if is_tick {
            self.ticks.fetch_add(1, Ordering::Relaxed);
            // 遅れていたら、今より後の最初のティックに合わせる
            let behind = (now - next) / interval;
            self.next_tick
                .store(next + (behind + 1) * interval, Ordering::Relaxed);
        }
        self.arm(timer);
        is_tick
    }

    /// 時刻 `ns`(起動からのナノ秒)に割り込みを起こしてもらう(ティックより早ければ)。
    /// 眠っているタスクを、次のティックを待たずに起こすため。
    pub fn request_wake_ns(&self, timer: &mut impl Timer, ns: u64) {
        let at = (ns as u128 * timer.frequency() as u128 / 1_000_000_000) as u64;
        self.alarm.fetch_min(at, Ordering::Relaxed);
        self.arm(timer);
    }

    /// 次のティックと目覚ましの早いほうで、タイマを予約する。
    fn arm(&self, timer: &mut impl Timer) {
        let now = timer.now();
        let mut alarm = self.alarm.load(Ordering::Relaxed);
        if alarm <= now {
            // もう過ぎた目覚まし(この割り込みがそれ)は消す
            self.alarm.store(u64::MAX, Ordering::Relaxed);
            alarm = u64::MAX;
        }
        let target = self.next_tick.load(Ordering::Relaxed).min(alarm);
        timer.set_timeout(target.saturating_sub(now).max(1));
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

/// 起動時の CLOCK_REALTIME(秒)。RTC がないので、ビルドした時刻を使う(NTP はフェーズ2以降)。
pub const BOOT_EPOCH: u64 = parse_u64(env!("PIOS_BUILD_EPOCH"));

const fn parse_u64(s: &str) -> u64 {
    let b = s.as_bytes();
    let mut v = 0;
    let mut i = 0;
    while i < b.len() {
        v = v * 10 + (b[i] - b'0') as u64;
        i += 1;
    }
    v
}

/// 目覚ましを頼む関数(BSP が登録する。`Clock::request_wake_ns` を本物のタイマで呼ぶ)
static ALARM: spin::Once<fn(u64)> = spin::Once::new();

pub fn set_alarm(f: fn(u64)) {
    ALARM.call_once(|| f);
}

/// 時刻 `ns`(起動からのナノ秒)にタイマ割り込みを起こしてもらう。登録がなければ何もしない
/// (次のティックで気づく)。
pub fn wake_at(ns: u64) {
    if let Some(f) = ALARM.get() {
        f(ns);
    }
}

/// 高い分解能のカウンタ(BSP が登録する)。読む関数と周波数。
static COUNTER: spin::Once<(fn() -> u64, u64)> = spin::Once::new();

/// カウンタを登録する。
pub fn set_counter(read: fn() -> u64, frequency: u64) {
    COUNTER.call_once(|| (read, frequency));
}

/// 起動してからの時間(ナノ秒)。カウンタがなければティックから求める。
pub fn now_ns() -> u64 {
    match COUNTER.get() {
        Some(&(read, freq)) if freq > 0 => {
            let t = read() as u128;
            (t * 1_000_000_000 / freq as u128) as u64
        }
        _ => {
            let hz = CLOCK.hz().max(1);
            CLOCK.ticks() * (1_000_000_000 / hz)
        }
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
        now: u64,
    }

    impl Timer for FakeTimer {
        fn frequency(&self) -> u64 {
            62_500_000
        }
        fn now(&self) -> u64 {
            self.now
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
        timer.now = 625_000;
        assert!(clock.tick(&mut timer));
        timer.now = 1_250_000;
        assert!(clock.tick(&mut timer));
        assert_eq!(clock.ticks(), 2);
        assert_eq!(timer.timeouts, [625_000, 625_000, 625_000]);
    }

    #[test]
    fn an_alarm_before_the_next_tick_fires_early() {
        let clock = Clock::new();
        let mut timer = FakeTimer::default();
        clock.start(&mut timer, 100);
        // 1ms 後(62_500 カウント)に起こしてほしい
        clock.request_wake_ns(&mut timer, 1_000_000);
        assert_eq!(timer.timeouts.last(), Some(&62_500));
        // その割り込みはティックではない。次はふつうのティックまで
        timer.now = 62_500;
        assert!(!clock.tick(&mut timer));
        assert_eq!(clock.ticks(), 0);
        assert_eq!(timer.timeouts.last(), Some(&562_500));
    }

    #[test]
    fn an_alarm_after_the_next_tick_waits_for_the_tick() {
        let clock = Clock::new();
        let mut timer = FakeTimer::default();
        clock.start(&mut timer, 100);
        clock.request_wake_ns(&mut timer, 50_000_000);
        assert_eq!(timer.timeouts.last(), Some(&625_000));
        // ティックが来たあとに、残りの時間で起こす
        timer.now = 625_000;
        assert!(clock.tick(&mut timer));
        assert_eq!(timer.timeouts.last(), Some(&625_000));
        timer.now = 3_125_000 - 100;
        clock.tick(&mut timer);
        assert_eq!(timer.timeouts.last(), Some(&100));
    }
}
