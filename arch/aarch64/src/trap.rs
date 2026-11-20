//! 例外の入口で保存するレジスタの並び(トラップフレーム)。

use crate::esr::Esr;
use core::fmt;

/// 例外が起きた瞬間の CPU の状態。`exception.rs` のアセンブリがスタックに積み、
/// Rust のハンドラに `&mut TrapFrame` として渡す。ハンドラが書き換えた値は、
/// 例外から戻るときに CPU へ書き戻される(システムコールの戻り値を x0 に入れる、など)。
#[repr(C)]
#[derive(Debug, Clone, Default)]
pub struct TrapFrame {
    /// 汎用レジスタ x0〜x30
    pub x: [u64; 31],
    /// EL0(ユーザー)のスタックポインタ
    pub sp_el0: u64,
    /// 例外から戻る先のアドレス
    pub elr: u64,
    /// 例外が起きる前の PSTATE(例外レベル、割り込みマスクなど)
    pub spsr: u64,
    /// 例外の原因
    pub esr: u64,
    /// アボートを起こしたアドレス
    pub far: u64,
}

impl TrapFrame {
    /// アセンブリが確保するバイト数。
    pub const SIZE: usize = 36 * 8;
}

impl fmt::Display for TrapFrame {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "exception: {}", Esr(self.esr))?;
        writeln!(
            f,
            "  ELR = {:#018x}  SPSR = {:#018x}  ESR = {:#018x}  FAR = {:#018x}",
            self.elr, self.spsr, self.esr, self.far
        )?;
        for (i, pair) in self.x.chunks(2).enumerate() {
            let n = i * 2;
            write!(f, "  x{n:<2} = {:#018x}", pair[0])?;
            match pair.get(1) {
                Some(v) => writeln!(f, "  x{:<2} = {v:#018x}", n + 1)?,
                None => writeln!(f, "  SP_EL0 = {:#018x}", self.sp_el0)?,
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::format;

    #[test]
    fn trap_frame_layout_matches_assembly() {
        // exception.rs のアセンブリはこの並びとサイズを前提に保存・復元する
        assert_eq!(core::mem::size_of::<TrapFrame>(), TrapFrame::SIZE);
        assert_eq!(TrapFrame::SIZE % 16, 0);
        assert_eq!(core::mem::offset_of!(TrapFrame, sp_el0), 31 * 8);
        assert_eq!(core::mem::offset_of!(TrapFrame, elr), 32 * 8);
        assert_eq!(core::mem::offset_of!(TrapFrame, spsr), 33 * 8);
        assert_eq!(core::mem::offset_of!(TrapFrame, esr), 34 * 8);
        assert_eq!(core::mem::offset_of!(TrapFrame, far), 35 * 8);
    }

    #[test]
    fn display_dumps_all_registers() {
        let mut frame = TrapFrame::default();
        for (i, x) in frame.x.iter_mut().enumerate() {
            *x = i as u64;
        }
        frame.elr = 0x4008_1000;
        frame.esr = (0x3c << 26) | (1 << 25) | 7;
        let text = format!("{frame}");
        assert!(text.starts_with("exception: brk #7\n"), "{text}");
        assert!(text.contains("ELR = 0x0000000040081000"), "{text}");
        assert!(text.contains("x0  = 0x0000000000000000"), "{text}");
        assert!(text.contains("x30 = 0x000000000000001e"), "{text}");
    }
}
