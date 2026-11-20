//! カーネル全体で使う出力先と `print!` / `println!`。
//!
//! BSP が起動時に [`set_console`] で出力先(UART など)を登録する。
//! 複数のコアから同時に書いても文字が混ざらないよう、出力先はロック付きで持つ。

use core::fmt;
use hal::console::Console;

/// 複数のコアから共有できる出力先。`spin::Mutex<C>` に実装してある。
pub trait SharedConsole: Sync {
    fn write_bytes(&self, buf: &[u8]);
}

impl<C: Console + Send> SharedConsole for spin::Mutex<C> {
    fn write_bytes(&self, buf: &[u8]) {
        self.lock().write_bytes(buf);
    }
}

static CONSOLE: spin::Once<&'static dyn SharedConsole> = spin::Once::new();

/// 出力先を登録する。2回目以降の呼び出しは無視される。
pub fn set_console(console: &'static dyn SharedConsole) {
    CONSOLE.call_once(|| console);
}

struct Out(&'static dyn SharedConsole);

impl fmt::Write for Out {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        self.0.write_bytes(s.as_bytes());
        Ok(())
    }
}

/// 登録された出力先に書く `Console`。登録前の出力は捨てられる。
pub struct Global;

impl Console for Global {
    fn write_bytes(&mut self, buf: &[u8]) {
        if let Some(console) = CONSOLE.get() {
            console.write_bytes(buf);
        }
    }
}

#[doc(hidden)]
pub fn _print(args: fmt::Arguments) {
    if let Some(&console) = CONSOLE.get() {
        let _ = fmt::Write::write_fmt(&mut Out(console), args);
    }
}

#[macro_export]
macro_rules! print {
    ($($arg:tt)*) => ($crate::console::_print(format_args!($($arg)*)));
}

#[macro_export]
macro_rules! println {
    () => ($crate::print!("\n"));
    ($($arg:tt)*) => ($crate::print!("{}\n", format_args!($($arg)*)));
}
