//! 文字の出力先(と入力元)を表すポート。
//!
//! コアは「バイト列を書ける何か」としてしか出力先を知らない。
//! 本物の UART(PL011 など)もテスト用のモックも、このトレイトを実装する。

use core::fmt;

/// バイト列を書き出せる出力先。
pub trait Console {
    /// `buf` をすべて書き出す。書き終わるまで戻らない。
    fn write_bytes(&mut self, buf: &[u8]);

    /// 届いている入力を1バイト取り出す。なければすぐに `None` を返す(待たない)。
    fn read_byte(&mut self) -> Option<u8> {
        None
    }
}

/// `Console` を `core::fmt::Write` として使うためのアダプター。
///
/// `write!(Writer(&mut con), "x = {}", 42)` のように書式付きで出力できる。
pub struct Writer<'a, C: Console + ?Sized>(pub &'a mut C);

impl<C: Console + ?Sized> fmt::Write for Writer<'_, C> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        self.0.write_bytes(s.as_bytes());
        Ok(())
    }
}
