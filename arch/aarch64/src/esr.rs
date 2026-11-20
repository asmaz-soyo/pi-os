//! ESR_ELx(Exception Syndrome Register)の解読。
//!
//! 同期例外が起きると、CPU は「なぜ起きたか」を ESR に書く。上位 6 ビットが
//! 例外の種類(EC: Exception Class)、下位 25 ビットが種類ごとの詳細(ISS)。

use core::fmt;

/// 例外の種類(EC)。よく使うものだけ名前を付ける。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExceptionClass {
    /// 理由不明(多くは未定義命令)
    Unknown,
    /// FP/SIMD 命令のトラップ
    FpAccess,
    /// AArch64 の `svc`(システムコール)
    Svc64,
    /// システムレジスタへのアクセスのトラップ
    SysReg,
    InstructionAbortLowerEl,
    InstructionAbortSameEl,
    PcAlignment,
    DataAbortLowerEl,
    DataAbortSameEl,
    SpAlignment,
    /// AArch64 の `brk`(ブレークポイント)
    Brk,
    Other(u8),
}

impl ExceptionClass {
    pub fn from_esr(esr: u64) -> Self {
        match ((esr >> 26) & 0x3f) as u8 {
            0x00 => Self::Unknown,
            0x07 => Self::FpAccess,
            0x15 => Self::Svc64,
            0x18 => Self::SysReg,
            0x20 => Self::InstructionAbortLowerEl,
            0x21 => Self::InstructionAbortSameEl,
            0x22 => Self::PcAlignment,
            0x24 => Self::DataAbortLowerEl,
            0x25 => Self::DataAbortSameEl,
            0x26 => Self::SpAlignment,
            0x3c => Self::Brk,
            other => Self::Other(other),
        }
    }
}

/// `svc #imm` / `brk #imm` の即値(ISS の下位 16 ビット)。
pub fn imm16(esr: u64) -> u16 {
    esr as u16
}

/// アボートの原因(DFSC / IFSC、ISS の下位 6 ビット)を文章にする。
fn fault_status(fsc: u64) -> (&'static str, Option<u64>) {
    let level = Some(fsc & 0b11);
    match fsc {
        0b00_0000..=0b00_0011 => ("address size fault", level),
        0b00_0100..=0b00_0111 => ("translation fault", level),
        0b00_1000..=0b00_1011 => ("access flag fault", level),
        0b00_1100..=0b00_1111 => ("permission fault", level),
        0b01_0000 => ("synchronous external abort", None),
        0b10_0001 => ("alignment fault", None),
        _ => ("other fault", None),
    }
}

/// ESR を人が読める形で表示するためのラッパー。
#[derive(Debug, Clone, Copy)]
pub struct Esr(pub u64);

impl fmt::Display for Esr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let esr = self.0;
        let abort = |f: &mut fmt::Formatter<'_>, what: &str, data: bool| {
            let (status, level) = fault_status(esr & 0x3f);
            write!(f, "{what}: {status}")?;
            if let Some(level) = level {
                write!(f, " level {level}")?;
            }
            if data {
                // WnR: 1 なら書き込みで起きた
                let dir = if esr & (1 << 6) != 0 { "write" } else { "read" };
                write!(f, ", {dir}")?;
            }
            Ok(())
        };
        match ExceptionClass::from_esr(esr) {
            ExceptionClass::Unknown => write!(f, "unknown reason (undefined instruction?)"),
            ExceptionClass::FpAccess => write!(f, "FP/SIMD access trapped"),
            ExceptionClass::Svc64 => write!(f, "svc #{}", imm16(esr)),
            ExceptionClass::SysReg => write!(f, "system register access trapped"),
            ExceptionClass::InstructionAbortLowerEl => {
                abort(f, "instruction abort (lower EL)", false)
            }
            ExceptionClass::InstructionAbortSameEl => {
                abort(f, "instruction abort (same EL)", false)
            }
            ExceptionClass::PcAlignment => write!(f, "PC alignment fault"),
            ExceptionClass::DataAbortLowerEl => abort(f, "data abort (lower EL)", true),
            ExceptionClass::DataAbortSameEl => abort(f, "data abort (same EL)", true),
            ExceptionClass::SpAlignment => write!(f, "SP alignment fault"),
            ExceptionClass::Brk => write!(f, "brk #{}", imm16(esr)),
            ExceptionClass::Other(ec) => write!(f, "exception class {ec:#04x}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::string::ToString;

    fn esr(ec: u32, iss: u32) -> u64 {
        ((ec as u64) << 26) | (1 << 25) | iss as u64
    }

    #[test]
    fn decodes_exception_class() {
        assert_eq!(
            ExceptionClass::from_esr(esr(0x15, 0)),
            ExceptionClass::Svc64
        );
        assert_eq!(ExceptionClass::from_esr(esr(0x3c, 0)), ExceptionClass::Brk);
        assert_eq!(
            ExceptionClass::from_esr(esr(0x25, 0)),
            ExceptionClass::DataAbortSameEl
        );
        assert_eq!(
            ExceptionClass::from_esr(esr(0x24, 0)),
            ExceptionClass::DataAbortLowerEl
        );
        assert_eq!(
            ExceptionClass::from_esr(esr(0x21, 0)),
            ExceptionClass::InstructionAbortSameEl
        );
        assert_eq!(
            ExceptionClass::from_esr(esr(0x00, 0)),
            ExceptionClass::Unknown
        );
        assert_eq!(
            ExceptionClass::from_esr(esr(0x2f, 0)),
            ExceptionClass::Other(0x2f)
        );
    }

    #[test]
    fn svc_and_brk_immediate_comes_from_iss() {
        assert_eq!(imm16(esr(0x15, 0x1234)), 0x1234);
    }

    #[test]
    fn describes_data_abort_with_fault_status_and_direction() {
        // DFSC 0b000101 = レベル1の変換フォールト、WnR(bit 6)= 書き込み
        let text = Esr(esr(0x25, (1 << 6) | 0b000101)).to_string();
        assert_eq!(
            text,
            "data abort (same EL): translation fault level 1, write"
        );
    }

    #[test]
    fn describes_unknown_class_with_raw_value() {
        assert_eq!(Esr(esr(0x2f, 0)).to_string(), "exception class 0x2f");
    }

    #[test]
    fn describes_simple_classes() {
        assert_eq!(
            Esr(esr(0x00, 0)).to_string(),
            "unknown reason (undefined instruction?)"
        );
        assert_eq!(Esr(esr(0x3c, 7)).to_string(), "brk #7");
        assert_eq!(Esr(esr(0x15, 0)).to_string(), "svc #0");
    }
}
