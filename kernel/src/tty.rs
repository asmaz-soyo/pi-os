//! 端末(TTY)の行規律。
//!
//! シリアル端末から届いたバイトを、プログラムが読む形に整える。
//! - canonical モード(既定):1 行ずつまとめる。Enter で読めるようになる。Backspace で消し、打った文字をエコーする
//! - raw モード(ICANON を外す):届いたバイトをそのまま渡す。REPL のように自分で行を編集するプログラムが使う
//!
//! 設定(termios)は Linux と同じ値と並びで、`ioctl(TCGETS / TCSETS)` で読み書きする。

use alloc::collections::VecDeque;
use alloc::vec::Vec;

// c_iflag
pub const ICRNL: u32 = 0o400;
// c_oflag
pub const OPOST: u32 = 0o1;
pub const ONLCR: u32 = 0o4;
// c_cflag
pub const CS8: u32 = 0o60;
pub const CREAD: u32 = 0o200;
// c_lflag
pub const ISIG: u32 = 0o1;
pub const ICANON: u32 = 0o2;
pub const ECHO: u32 = 0o10;
pub const ECHOE: u32 = 0o20;
pub const ECHOK: u32 = 0o40;
// c_cc の添字
pub const VINTR: usize = 0;
pub const VQUIT: usize = 1;
pub const VERASE: usize = 2;
pub const VKILL: usize = 3;
pub const VEOF: usize = 4;
pub const VTIME: usize = 5;
pub const VMIN: usize = 6;

pub const NCCS: usize = 19;
/// カーネルの `struct termios`(ioctl の TCGETS / TCSETS が使う並び)の大きさ
pub const TERMIOS_SIZE: usize = 4 * 4 + 1 + NCCS;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Termios {
    pub iflag: u32,
    pub oflag: u32,
    pub cflag: u32,
    pub lflag: u32,
    pub line: u8,
    pub cc: [u8; NCCS],
}

impl Default for Termios {
    fn default() -> Self {
        let mut cc = [0u8; NCCS];
        cc[VINTR] = 0x03; // ^C
        cc[VQUIT] = 0x1c; // ^\
        cc[VERASE] = 0x7f; // Backspace(DEL)
        cc[VKILL] = 0x15; // ^U
        cc[VEOF] = 0x04; // ^D
        cc[VTIME] = 0;
        cc[VMIN] = 1;
        Self {
            iflag: ICRNL,
            oflag: OPOST | ONLCR,
            cflag: CS8 | CREAD,
            lflag: ISIG | ICANON | ECHO | ECHOE | ECHOK,
            line: 0,
            cc,
        }
    }
}

impl Termios {
    pub fn to_bytes(&self) -> [u8; TERMIOS_SIZE] {
        let mut b = [0u8; TERMIOS_SIZE];
        for (i, v) in [self.iflag, self.oflag, self.cflag, self.lflag]
            .iter()
            .enumerate()
        {
            b[i * 4..i * 4 + 4].copy_from_slice(&v.to_le_bytes());
        }
        b[16] = self.line;
        b[17..].copy_from_slice(&self.cc);
        b
    }

    pub fn from_bytes(b: &[u8; TERMIOS_SIZE]) -> Self {
        let word = |i: usize| u32::from_le_bytes(b[i * 4..i * 4 + 4].try_into().unwrap());
        let mut cc = [0u8; NCCS];
        cc.copy_from_slice(&b[17..]);
        Self {
            iflag: word(0),
            oflag: word(1),
            cflag: word(2),
            lflag: word(3),
            line: b[16],
            cc,
        }
    }
}

pub struct Tty {
    termios: Termios,
    /// 編集中の行(canonical モード)
    line: Vec<u8>,
    /// 読めるようになったバイト
    ready: VecDeque<u8>,
    /// 空の行で ^D が押された(次の read で 0 を返す)
    eof: bool,
}

impl Tty {
    pub fn new() -> Self {
        Self {
            termios: Termios::default(),
            line: Vec::new(),
            ready: VecDeque::new(),
            eof: false,
        }
    }

    pub fn termios(&self) -> Termios {
        self.termios
    }

    pub fn set_termios(&mut self, t: Termios) {
        // canonical から raw に変わったら、編集中の行はそのまま読めるようにする
        if t.lflag & ICANON == 0 {
            self.ready.extend(self.line.drain(..));
        }
        self.termios = t;
    }

    /// 端末から1バイト届いた。エコーする内容は `echo` に渡す。
    pub fn input(&mut self, byte: u8, echo: &mut dyn FnMut(&[u8])) {
        let t = self.termios;
        let byte = if byte == b'\r' && t.iflag & ICRNL != 0 {
            b'\n'
        } else {
            byte
        };
        let echoing = t.lflag & ECHO != 0;
        if t.lflag & ICANON == 0 {
            self.ready.push_back(byte);
            if echoing {
                echo(&[byte]);
            }
            return;
        }
        if byte == t.cc[VERASE] || byte == 0x08 {
            if self.line.pop().is_some() && echoing {
                echo(b"\x08 \x08");
            }
        } else if byte == t.cc[VEOF] {
            if self.line.is_empty() {
                self.eof = true;
            }
            self.ready.extend(self.line.drain(..));
        } else if byte == b'\n' {
            self.line.push(b'\n');
            self.ready.extend(self.line.drain(..));
            if echoing {
                echo(b"\n");
            }
        } else {
            self.line.push(byte);
            if echoing {
                echo(&[byte]);
            }
        }
    }

    /// 読めるものがあるか(select / poll 用)。
    pub fn readable(&self) -> bool {
        !self.ready.is_empty() || self.eof
    }

    /// 読む。何もなければ `None`(読み手は待つ)、EOF なら `Some(0)`。
    /// canonical モードでは、1 回の read で 1 行(改行まで)だけを返す。
    pub fn read(&mut self, buf: &mut [u8]) -> Option<usize> {
        if self.ready.is_empty() {
            return if core::mem::take(&mut self.eof) {
                Some(0)
            } else {
                None
            };
        }
        let canonical = self.termios.lflag & ICANON != 0;
        let mut n = 0;
        while n < buf.len() {
            let Some(b) = self.ready.pop_front() else {
                break;
            };
            buf[n] = b;
            n += 1;
            if canonical && b == b'\n' {
                break;
            }
        }
        Some(n)
    }
}

impl Default for Tty {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::vec::Vec;

    /// 入力を1バイトずつ渡し、エコーされた内容を返す
    fn type_in(tty: &mut Tty, bytes: &[u8]) -> Vec<u8> {
        let mut echo = Vec::new();
        for &b in bytes {
            tty.input(b, &mut |out| echo.extend_from_slice(out));
        }
        echo
    }

    fn read(tty: &mut Tty) -> Option<Vec<u8>> {
        let mut buf = [0u8; 64];
        tty.read(&mut buf).map(|n| buf[..n].to_vec())
    }

    #[test]
    fn canonical_mode_delivers_whole_lines_and_echoes() {
        let mut tty = Tty::new();
        let echo = type_in(&mut tty, b"ab");
        assert_eq!(read(&mut tty), None, "nothing until Enter");
        assert!(!tty.readable());
        let echo2 = type_in(&mut tty, b"\r");
        assert_eq!([echo, echo2].concat(), b"ab\n");
        assert!(tty.readable());
        assert_eq!(read(&mut tty).unwrap(), b"ab\n", "CR becomes NL");
    }

    #[test]
    fn backspace_erases_the_last_character() {
        let mut tty = Tty::new();
        let echo = type_in(&mut tty, b"abc\x7f\n");
        assert_eq!(read(&mut tty).unwrap(), b"ab\n");
        assert_eq!(echo, b"abc\x08 \x08\n");
        // 空の行では何も消さない
        let echo = type_in(&mut tty, b"\x7f\n");
        assert_eq!(echo, b"\n");
    }

    #[test]
    fn reads_stop_at_each_line_end() {
        let mut tty = Tty::new();
        type_in(&mut tty, b"one\ntwo\n");
        assert_eq!(read(&mut tty).unwrap(), b"one\n");
        assert_eq!(read(&mut tty).unwrap(), b"two\n");
    }

    #[test]
    fn ctrl_d_ends_input_on_an_empty_line_and_flushes_otherwise() {
        let mut tty = Tty::new();
        type_in(&mut tty, b"\x04");
        assert_eq!(read(&mut tty).unwrap(), b"", "EOF");
        assert_eq!(read(&mut tty), None, "EOF is reported once");
        type_in(&mut tty, b"xy\x04");
        assert_eq!(read(&mut tty).unwrap(), b"xy");
    }

    #[test]
    fn raw_mode_passes_bytes_through_without_echo() {
        let mut tty = Tty::new();
        let mut t = tty.termios();
        t.lflag &= !(ICANON | ECHO);
        t.iflag &= !ICRNL;
        tty.set_termios(t);
        let echo = type_in(&mut tty, b"a\r");
        assert!(echo.is_empty());
        assert_eq!(read(&mut tty).unwrap(), b"a\r");
    }

    #[test]
    fn termios_round_trips_through_the_linux_layout() {
        let t = Termios::default();
        let bytes = t.to_bytes();
        assert_eq!(bytes.len(), TERMIOS_SIZE);
        assert_eq!(Termios::from_bytes(&bytes), t);
        assert_ne!(t.lflag & ICANON, 0);
        assert_eq!(t.cc[VMIN], 1);
        assert_eq!(t.cc[VERASE], 0x7f);
    }
}
