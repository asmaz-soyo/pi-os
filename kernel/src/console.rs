//! カーネル全体で使う出力先と `print!` / `println!`。
//!
//! BSP が起動時に [`set_console`] で出力先(UART など)を登録する。
//! 複数のコアから同時に書いても文字が混ざらないよう、出力先はロック付きで持つ。

use core::fmt;
use hal::console::Console;

/// 複数のコアから共有できる出力先。`SpinLock<C>` に実装してある。
pub trait SharedConsole: Sync {
    fn write_bytes(&self, buf: &[u8]);

    /// 届いている入力を1バイト取り出す(待たない)
    fn read_byte(&self) -> Option<u8>;

    /// ロックを強制的に外す。
    ///
    /// # Safety
    /// パニックの報告のように、ロックを持っていた処理が二度と続きを実行しないときだけ使う。
    unsafe fn force_unlock(&self);
}

impl<C: Console + Send> SharedConsole for crate::sync::SpinLock<C> {
    fn write_bytes(&self, buf: &[u8]) {
        self.lock().write_bytes(buf);
    }

    fn read_byte(&self) -> Option<u8> {
        self.lock().read_byte()
    }

    unsafe fn force_unlock(&self) {
        if self.is_locked() {
            // SAFETY: 呼び出し元が保証する
            unsafe { crate::sync::SpinLock::force_unlock(self) };
        }
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

/// 登録された出力先に書き、そこから読む `Console`。登録前の出力は捨てられる。
pub struct Global;

impl Console for Global {
    fn write_bytes(&mut self, buf: &[u8]) {
        if let Some(console) = CONSOLE.get() {
            console.write_bytes(buf);
        }
    }

    fn read_byte(&mut self) -> Option<u8> {
        CONSOLE.get().and_then(|console| console.read_byte())
    }
}

/// 出力先のロックを強制的に外す(パニックの報告の直前に使う)。
///
/// # Safety
/// [`SharedConsole::force_unlock`] と同じ。
pub unsafe fn break_lock() {
    if let Some(console) = CONSOLE.get() {
        // SAFETY: 呼び出し元が保証する
        unsafe { console.force_unlock() };
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
