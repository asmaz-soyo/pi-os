//! テスト用のポート実装。ホストの `cargo test` でだけ使う。

use crate::console::Console;
use std::vec::Vec;

/// 書かれたバイトをすべて記録するだけの `Console`。
#[derive(Debug, Default)]
pub struct MockConsole {
    output: Vec<u8>,
}

impl MockConsole {
    pub fn new() -> Self {
        Self::default()
    }

    /// これまでに書かれたバイト列。
    pub fn output(&self) -> &[u8] {
        &self.output
    }
}

impl Console for MockConsole {
    fn write_bytes(&mut self, buf: &[u8]) {
        self.output.extend_from_slice(buf);
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
