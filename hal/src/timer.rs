//! タイマのポート。

/// 単調増加するカウンタと、一定時間後に割り込みを起こす機能を持つタイマ。
pub trait Timer {
    /// カウンタの周波数(Hz)。
    fn frequency(&self) -> u64;
    /// 現在のカウンタの値。
    fn now(&self) -> u64;
    /// 今から `ticks` カウント後に割り込みを起こす。
    fn set_timeout(&mut self, ticks: u64);
}
