//! パニック時の情報出力(レジスタとスタックトレース)。
//!
//! 「どこで・なぜ」に加えて「どういう経路で呼ばれたか」を UART に出す。
//! アドレスから関数を知るには、ホストで次のようにする:
//! `llvm-objdump -d --start-address=<アドレス-4> --stop-address=<アドレス+4> <カーネルの ELF>`
//! (`llvm-objdump` は rustup の llvm-tools に入っている)

use core::fmt::{self, Write};
use hal::console::{Console, Writer};

/// たどるフレームの上限(壊れたスタックで無限にたどらないため)。
pub const MAX_FRAMES: usize = 32;

/// パニックした時点のレジスタ。値の取り方は CPU アーキテクチャに依存するので、BSP が埋める。
#[derive(Debug, Clone, Copy, Default)]
pub struct Registers {
    pub current_el: u8,
    pub sp: u64,
    pub fp: u64,
    pub lr: u64,
}

/// フレームポインタ `fp` から、呼び出し元の戻りアドレスを順に返す。
///
/// AArch64 の関数は(フレームポインタを有効にしてビルドすると)スタックに
/// 「フレームレコード」= `[fp] = 呼び出し元の fp`、`[fp + 8] = 戻りアドレス(lr)` を積む。
/// これを連結リストのようにたどる。`read` は 8 バイトを読む関数で、読めない番地なら `None`。
pub fn frames<R>(fp: usize, read: &R) -> impl Iterator<Item = u64> + '_
where
    R: Fn(usize) -> Option<u64>,
{
    let mut fp = fp;
    let mut depth = 0;
    core::iter::from_fn(move || {
        if fp == 0 || fp % 16 != 0 || depth >= MAX_FRAMES {
            return None;
        }
        let lr = read(fp + 8)?;
        let next = read(fp).unwrap_or(0) as usize;
        depth += 1;
        // スタックは下に伸びるので、呼び出し元のフレームは必ず高いアドレスにある
        fp = if next > fp { next } else { 0 };
        Some(lr)
    })
}

/// パニックの報告を書く。`what` には `PanicInfo`(「panicked at 場所:\nメッセージ」と表示される)を渡す。
pub fn write_report(
    out: &mut impl Console,
    what: impl fmt::Display,
    regs: &Registers,
    backtrace: impl Iterator<Item = u64>,
) {
    let mut w = Writer(out);
    let _ = writeln!(w, "\n!!! KERNEL PANIC !!!");
    let _ = writeln!(w, "{what}");
    let _ = writeln!(w, "registers:");
    let _ = writeln!(
        w,
        "  CurrentEL = {}  SP = {:#018x}  FP = {:#018x}  LR = {:#018x}",
        regs.current_el, regs.sp, regs.fp, regs.lr
    );
    let _ = writeln!(w, "backtrace:");
    for (i, addr) in backtrace.enumerate() {
        let _ = writeln!(w, "  #{i} {addr:#018x}");
    }
    let _ = writeln!(w, "end of backtrace");
}

/// パニックの報告を、登録された出力先に書く。`stack` はフレームレコードを読んでよい範囲。
pub fn report(info: &core::panic::PanicInfo, regs: &Registers, stack: core::ops::Range<usize>) {
    // パニックした処理が出力のロックを持ったままかもしれないので、外してから書く
    // SAFETY: パニックした処理はこの先実行されない
    unsafe { crate::console::break_lock() };
    let read = |addr: usize| {
        if stack.contains(&addr) && stack.contains(&(addr + 7)) {
            // SAFETY: スタックの範囲内で、8 バイト境界(frames が保証)
            Some(unsafe { core::ptr::read_volatile(addr as *const u64) })
        } else {
            None
        }
    };
    write_report(
        &mut crate::console::Global,
        info,
        regs,
        frames(regs.fp as usize, &read),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use hal::mock::MockConsole;
    use std::collections::BTreeMap;
    use std::vec::Vec;

    fn memory(words: &[(usize, u64)]) -> impl Fn(usize) -> Option<u64> {
        let map: BTreeMap<usize, u64> = words.iter().copied().collect();
        move |addr| map.get(&addr).copied()
    }

    fn walk(fp: usize, mem: &impl Fn(usize) -> Option<u64>) -> Vec<u64> {
        frames(fp, mem).collect()
    }

    #[test]
    fn follows_frame_records_until_null_fp() {
        let mem = memory(&[
            (0x1000, 0x1010),
            (0x1008, 0xaaa),
            (0x1010, 0),
            (0x1018, 0xbbb),
        ]);
        assert_eq!(walk(0x1000, &mem), [0xaaa, 0xbbb]);
    }

    #[test]
    fn stops_at_misaligned_fp() {
        let mem = memory(&[(0x1000, 0x1013), (0x1008, 0xaaa)]);
        assert_eq!(walk(0x1000, &mem), [0xaaa]);
    }

    #[test]
    fn stops_when_fp_does_not_move_up_the_stack() {
        // 前のフレームは必ず高いアドレスにある。そうでなければ壊れているので止める
        let mem = memory(&[(0x1000, 0x1000), (0x1008, 0xaaa)]);
        assert_eq!(walk(0x1000, &mem), [0xaaa]);
    }

    #[test]
    fn stops_when_memory_is_unreadable() {
        let mem = memory(&[(0x1000, 0x2000), (0x1008, 0xaaa)]);
        assert_eq!(walk(0x1000, &mem), [0xaaa]);
    }

    #[test]
    fn stops_after_max_depth() {
        let words: Vec<(usize, u64)> = (0..100)
            .flat_map(|i| {
                let fp = 0x1000 + i * 16;
                [(fp, (fp + 16) as u64), (fp + 8, i as u64)]
            })
            .collect();
        assert_eq!(walk(0x1000, &memory(&words)).len(), MAX_FRAMES);
    }

    #[test]
    fn report_shows_message_registers_and_backtrace() {
        let mut con = MockConsole::new();
        let regs = Registers {
            current_el: 1,
            sp: 0x4008_f000,
            fp: 0x4008_f010,
            lr: 0x4008_1234,
        };
        write_report(
            &mut con,
            format_args!("panicked at src/main.rs:3:5:\nboom"),
            &regs,
            [0x4008_1000u64, 0x4008_2000].into_iter(),
        );
        assert_eq!(
            std::str::from_utf8(con.output()).unwrap(),
            "\n!!! KERNEL PANIC !!!\n\
             panicked at src/main.rs:3:5:\nboom\n\
             registers:\n\
             \x20 CurrentEL = 1  SP = 0x000000004008f000  FP = 0x000000004008f010  LR = 0x0000000040081234\n\
             backtrace:\n\
             \x20 #0 0x0000000040081000\n\
             \x20 #1 0x0000000040082000\n\
             end of backtrace\n"
        );
    }
}
