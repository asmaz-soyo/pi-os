//! セミホスティング:`hlt #0xf000` 命令でデバッガや QEMU に処理を頼む仕組み。
//!
//! QEMU を `-semihosting` 付きで起動したときだけ使える。実機では使えないので、
//! テストとデバッグの専用。UART ドライバがなくても文字を出せて、QEMU を終了コード付きで止められる。

use core::arch::asm;
use hal::console::Console;

const SYS_WRITEC: u64 = 0x03;
const SYS_EXIT: u64 = 0x18;
/// `SYS_EXIT` の理由:アプリケーションが終了した
const ADP_STOPPED_APPLICATION_EXIT: u64 = 0x2_0026;

/// セミホスティングの呼び出し。`op` が操作の番号、`param` が引数ブロックのアドレス。
///
/// # Safety
/// `param` は `op` が要求する形のメモリを指していること。
unsafe fn call(op: u64, param: u64) -> u64 {
    let ret: u64;
    // SAFETY: 呼び出し元が param の正しさを保証する。hlt #0xf000 は QEMU が横取りする。
    unsafe {
        asm!("hlt #0xf000", inout("x0") op => ret, in("x1") param, options(nostack));
    }
    ret
}

/// QEMU を終了させる。`code` が QEMU プロセスの終了コードになる。
pub fn exit(code: u32) -> ! {
    // AArch64 では、終了理由とコードの2語を並べたブロックのアドレスを渡す
    let block: [u64; 2] = [ADP_STOPPED_APPLICATION_EXIT, code as u64];
    // SAFETY: block は SYS_EXIT が要求する2語のブロック
    unsafe {
        call(SYS_EXIT, block.as_ptr() as u64);
    }
    // セミホスティングが無効だと戻ってくるので、ここで止まる
    loop {
        // SAFETY: wfe は割り込みなどを待つだけの命令
        unsafe { asm!("wfe", options(nomem, nostack)) };
    }
}

/// セミホスティングでホストの標準出力に文字を出す `Console`。
pub struct SemihostingConsole;

impl Console for SemihostingConsole {
    fn write_bytes(&mut self, buf: &[u8]) {
        for byte in buf {
            // SAFETY: SYS_WRITEC は1バイトを指すアドレスを受け取る
            unsafe {
                call(SYS_WRITEC, byte as *const u8 as u64);
            }
        }
    }
}
