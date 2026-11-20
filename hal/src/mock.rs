//! テスト用のポート実装。ホストの `cargo test` でだけ使う。

use crate::console::Console;
use crate::mmio::Mmio;
use std::collections::{BTreeMap, VecDeque};
use std::vec::Vec;

/// 書かれたバイトをすべて記録するだけの `Console`。
#[derive(Debug, Default)]
pub struct MockConsole {
    output: Vec<u8>,
    input: VecDeque<u8>,
}

impl MockConsole {
    pub fn new() -> Self {
        Self::default()
    }

    /// これまでに書かれたバイト列。
    pub fn output(&self) -> &[u8] {
        &self.output
    }

    /// 入力として届いたことにするバイト列を足す。
    pub fn push_input(&mut self, bytes: &[u8]) {
        self.input.extend(bytes);
    }
}

impl Console for MockConsole {
    fn write_bytes(&mut self, buf: &[u8]) {
        self.output.extend_from_slice(buf);
    }

    fn read_byte(&mut self) -> Option<u8> {
        self.input.pop_front()
    }
}

/// 書き込みを記録し、読み出しには予約した値を返す偽の MMIO。
#[derive(Debug, Default)]
pub struct FakeMmio {
    writes: Vec<(usize, u32)>,
    queued: BTreeMap<usize, VecDeque<u32>>,
    read_counts: BTreeMap<usize, usize>,
}

impl FakeMmio {
    pub fn new() -> Self {
        Self::default()
    }

    /// `offset` を読んだときに返す値を順に予約する。使い切ったら 0 を返す。
    pub fn queue_reads(&mut self, offset: usize, values: &[u32]) {
        self.queued.entry(offset).or_default().extend(values);
    }

    /// これまでの書き込み(オフセット, 値)を順に。
    pub fn writes(&self) -> &[(usize, u32)] {
        &self.writes
    }

    /// `offset` を読んだ回数。
    pub fn reads_of(&self, offset: usize) -> usize {
        self.read_counts.get(&offset).copied().unwrap_or(0)
    }
}

impl Mmio for FakeMmio {
    fn read(&mut self, offset: usize) -> u32 {
        *self.read_counts.entry(offset).or_default() += 1;
        self.queued
            .get_mut(&offset)
            .and_then(VecDeque::pop_front)
            .unwrap_or(0)
    }

    fn write(&mut self, offset: usize, value: u32) {
        self.writes.push((offset, value));
    }
}

#[cfg(test)]
mod tests {
    use super::MockConsole;
    use crate::console::{Console, Writer};
    use core::fmt::Write;

    #[test]
    fn records_written_bytes() {
        let mut con = MockConsole::new();
        con.write_bytes(b"Hello");
        assert_eq!(con.output(), b"Hello");
    }

    #[test]
    fn queued_input_is_read_byte_by_byte() {
        let mut con = MockConsole::new();
        assert_eq!(con.read_byte(), None);
        con.push_input(b"hi");
        assert_eq!(con.read_byte(), Some(b'h'));
        assert_eq!(con.read_byte(), Some(b'i'));
        assert_eq!(con.read_byte(), None);
    }

    #[test]
    fn appends_successive_writes() {
        let mut con = MockConsole::new();
        con.write_bytes(b"Hel");
        con.write_bytes(b"lo");
        assert_eq!(con.output(), b"Hello");
    }

    #[test]
    fn supports_format_macros() {
        let mut con = MockConsole::new();
        write!(Writer(&mut con), "x = {}", 42).unwrap();
        assert_eq!(con.output(), b"x = 42");
    }
}

#[cfg(test)]
mod mmio_tests {
    use super::FakeMmio;
    use crate::mmio::Mmio;

    #[test]
    fn records_writes_in_order() {
        let mut m = FakeMmio::new();
        m.write(0x30, 1);
        m.write(0x00, 2);
        assert_eq!(m.writes(), [(0x30, 1), (0x00, 2)]);
    }

    #[test]
    fn reads_return_queued_values_then_zero() {
        let mut m = FakeMmio::new();
        m.queue_reads(0x18, &[5, 6]);
        assert_eq!([m.read(0x18), m.read(0x18), m.read(0x18)], [5, 6, 0]);
        assert_eq!(m.reads_of(0x18), 3);
        assert_eq!(m.reads_of(0x00), 0);
    }
}
