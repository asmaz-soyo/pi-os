//! シグナルフレーム(Linux の arm64 の `struct rt_sigframe` と同じ並び)。
//!
//! ハンドラを呼ぶとき、カーネルは割り込まれたときのレジスタとマスクを、ユーザーのスタックに積む。
//! ハンドラが戻ると、restorer(musl が用意する)が rt_sigreturn を呼び、カーネルはここから戻す。
//!
//! ```text
//! sp →  0     siginfo(128 バイト:signo、errno、code、pid、uid)
//!       128   ucontext:uc_flags、uc_link、uc_stack(24)、uc_sigmask(+40)、詰め物
//!       304     sigcontext:fault_address、x0〜x30、sp、pc、pstate
//!       592       __reserved:fpsimd_context(magic、size、fpsr、fpcr、v0〜v31)、終わりの印
//!       4688  フレームの記録(fp、lr):デバッガがハンドラから呼び出し元をたどれるように
//! ```

use crate::trap::TrapFrame;

pub const FRAME_SIZE: usize = 4704;
pub const UC: usize = 128;
const UC_STACK: usize = UC + 16;
pub const UC_SIGMASK: usize = UC + 40;
pub const MCONTEXT: usize = UC + 176;
const REGS: usize = MCONTEXT + 8;
const SP: usize = MCONTEXT + 256;
pub const PC: usize = MCONTEXT + 264;
const PSTATE: usize = MCONTEXT + 272;
pub const FPSIMD: usize = MCONTEXT + 288;
const FRAME_RECORD: usize = 4688;
pub const FPSIMD_MAGIC: u32 = 0x4650_8001;
const FPSIMD_SIZE: u32 = 528;
/// ユーザーが戻してよい PSTATE のビット(条件フラグ NZCV だけ。例外レベルや割り込みのマスクは戻させない)
const PSTATE_USER: u64 = 0xf000_0000;

/// ハンドラに渡すシグナルの情報(siginfo_t の、使う部分)
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SigInfo {
    pub signo: i32,
    pub code: i32,
    pub pid: u32,
    pub uid: u32,
}

/// シグナル用のスタック(stack_t)
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Stack {
    pub sp: u64,
    pub flags: i32,
    pub size: u64,
}

/// スタックの今の位置 `sp` の下に、フレームを置く場所(16 バイト境界)
pub fn frame_address(sp: u64) -> u64 {
    (sp - FRAME_SIZE as u64) & !15
}

/// 割り込まれたときのレジスタ `frame`、元のマスク `mask` などを `buf` に書く
pub fn write_frame(
    buf: &mut [u8; FRAME_SIZE],
    frame: &TrapFrame,
    info: &SigInfo,
    mask: u64,
    stack: &Stack,
) {
    buf.fill(0);
    let mut put = |off: usize, bytes: &[u8]| buf[off..off + bytes.len()].copy_from_slice(bytes);
    // siginfo
    put(0, &info.signo.to_le_bytes());
    put(8, &info.code.to_le_bytes());
    put(16, &info.pid.to_le_bytes());
    put(20, &info.uid.to_le_bytes());
    // ucontext
    put(UC_STACK, &stack.sp.to_le_bytes());
    put(UC_STACK + 8, &stack.flags.to_le_bytes());
    put(UC_STACK + 16, &stack.size.to_le_bytes());
    put(UC_SIGMASK, &mask.to_le_bytes());
    // sigcontext
    put(MCONTEXT, &frame.far.to_le_bytes());
    for (i, x) in frame.x.iter().enumerate() {
        put(REGS + i * 8, &x.to_le_bytes());
    }
    put(SP, &frame.sp_el0.to_le_bytes());
    put(PC, &frame.elr.to_le_bytes());
    put(PSTATE, &frame.spsr.to_le_bytes());
    // FP/SIMD(__reserved の最初の「部品」。そのあとの 0 の 8 バイトが終わりの印)
    put(FPSIMD, &FPSIMD_MAGIC.to_le_bytes());
    put(FPSIMD + 4, &FPSIMD_SIZE.to_le_bytes());
    put(FPSIMD + 8, &(frame.fpsr as u32).to_le_bytes());
    put(FPSIMD + 12, &(frame.fpcr as u32).to_le_bytes());
    for (i, q) in frame.q.iter().enumerate() {
        put(FPSIMD + 16 + i * 16, &q.to_le_bytes());
    }
    // フレームの記録:(割り込まれたときの fp, 割り込まれた場所)
    put(FRAME_RECORD, &frame.x[29].to_le_bytes());
    put(FRAME_RECORD + 8, &frame.elr.to_le_bytes());
}

/// ハンドラから始めるようにレジスタを変える:x0 = 番号、x1 = &siginfo、x2 = &ucontext、
/// x29 = フレームの記録、x30(戻り先)= restorer、sp = フレーム、pc = ハンドラ
pub fn enter_handler(frame: &mut TrapFrame, addr: u64, sig: u32, handler: u64, restorer: u64) {
    frame.x[0] = sig as u64;
    frame.x[1] = addr;
    frame.x[2] = addr + UC as u64;
    frame.x[29] = addr + FRAME_RECORD as u64;
    frame.x[30] = restorer;
    frame.sp_el0 = addr;
    frame.elr = handler;
}

/// rt_sigreturn:`buf`(sp から読んだフレーム)からレジスタを戻し、元のマスクを返す
pub fn read_frame(buf: &[u8; FRAME_SIZE], frame: &mut TrapFrame) -> u64 {
    let u64_at = |off: usize| u64::from_le_bytes(buf[off..off + 8].try_into().unwrap());
    let u32_at = |off: usize| u32::from_le_bytes(buf[off..off + 4].try_into().unwrap());
    for (i, x) in frame.x.iter_mut().enumerate() {
        *x = u64_at(REGS + i * 8);
    }
    frame.sp_el0 = u64_at(SP);
    frame.elr = u64_at(PC);
    frame.spsr = u64_at(PSTATE) & PSTATE_USER;
    if u32_at(FPSIMD) == FPSIMD_MAGIC {
        frame.fpsr = u32_at(FPSIMD + 8) as u64;
        frame.fpcr = u32_at(FPSIMD + 12) as u64;
        for (i, q) in frame.q.iter_mut().enumerate() {
            let off = FPSIMD + 16 + i * 16;
            *q = u128::from_le_bytes(buf[off..off + 16].try_into().unwrap());
        }
    }
    u64_at(UC_SIGMASK)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::boxed::Box;

    fn sample() -> TrapFrame {
        let mut f = TrapFrame::default();
        for (i, x) in f.x.iter_mut().enumerate() {
            *x = 0x1000 + i as u64;
        }
        f.sp_el0 = 0x7fff_f000;
        f.elr = 0x40_1234;
        f.spsr = 0x6000_0000; // Z と C
        f.tpidr_el0 = 0x7777;
        for (i, q) in f.q.iter_mut().enumerate() {
            *q = (i as u128) << 64 | 0xabc;
        }
        f.fpcr = 0x0300_0000;
        f.fpsr = 0x10;
        f
    }

    fn u64_at(b: &[u8], off: usize) -> u64 {
        u64::from_le_bytes(b[off..off + 8].try_into().unwrap())
    }

    #[test]
    fn the_frame_follows_the_linux_layout() {
        let mut buf = Box::new([0u8; FRAME_SIZE]);
        let info = SigInfo {
            signo: 10,
            code: 0,
            pid: 42,
            uid: 0,
        };
        let stack = Stack {
            sp: 0x5000_0000,
            flags: 2,
            size: 0x4000,
        };
        write_frame(&mut buf, &sample(), &info, 0b1010, &stack);
        assert_eq!(
            i32::from_le_bytes(buf[0..4].try_into().unwrap()),
            10,
            "si_signo"
        );
        assert_eq!(
            u32::from_le_bytes(buf[16..20].try_into().unwrap()),
            42,
            "si_pid"
        );
        assert_eq!(u64_at(&buf[..], UC_STACK), 0x5000_0000, "uc_stack.ss_sp");
        assert_eq!(u64_at(&buf[..], UC_STACK + 16), 0x4000, "uc_stack.ss_size");
        assert_eq!(u64_at(&buf[..], UC_SIGMASK), 0b1010, "uc_sigmask");
        assert_eq!(u64_at(&buf[..], REGS + 8 * 5), 0x1005, "x5");
        assert_eq!(u64_at(&buf[..], SP), 0x7fff_f000);
        assert_eq!(u64_at(&buf[..], PC), 0x40_1234);
        assert_eq!(u64_at(&buf[..], PSTATE), 0x6000_0000);
        assert_eq!(
            u32::from_le_bytes(buf[FPSIMD..FPSIMD + 4].try_into().unwrap()),
            FPSIMD_MAGIC
        );
        assert_eq!(
            u32::from_le_bytes(buf[FPSIMD + 4..FPSIMD + 8].try_into().unwrap()),
            528
        );
        assert_eq!(
            u64_at(&buf[..], FPSIMD + 16 + 16 * 3 + 8),
            3,
            "v3 の上位 64 ビット"
        );
        assert_eq!(
            u64_at(&buf[..], FRAME_RECORD + 8),
            0x40_1234,
            "lr = 割り込まれた場所"
        );
    }

    #[test]
    fn entering_a_handler_sets_the_arguments_and_the_return_address() {
        let mut f = sample();
        let addr = frame_address(f.sp_el0);
        assert_eq!(addr % 16, 0);
        assert!(addr + FRAME_SIZE as u64 <= f.sp_el0);
        enter_handler(&mut f, addr, 10, 0x40_8000, 0x40_9000);
        assert_eq!((f.x[0], f.x[1], f.x[2]), (10, addr, addr + UC as u64));
        assert_eq!((f.x[29], f.x[30]), (addr + FRAME_RECORD as u64, 0x40_9000));
        assert_eq!((f.sp_el0, f.elr), (addr, 0x40_8000));
    }

    #[test]
    fn sigreturn_restores_everything_but_privileged_pstate_bits() {
        let before = sample();
        let mut buf = Box::new([0u8; FRAME_SIZE]);
        write_frame(
            &mut buf,
            &before,
            &SigInfo::default(),
            0x55,
            &Stack::default(),
        );
        // ハンドラの中でレジスタが変わる。ハンドラが uc の pstate を書き換えて EL1 に入ろうとしても
        let mut f = before.clone();
        enter_handler(&mut f, 0x1000, 10, 0x2000, 0x3000);
        f.q[7] = 0;
        buf[PSTATE..PSTATE + 8].copy_from_slice(&(0x6000_0000u64 | 0x3c5).to_le_bytes());
        let mask = read_frame(&buf, &mut f);
        assert_eq!(mask, 0x55);
        assert_eq!(f.x, before.x);
        assert_eq!((f.sp_el0, f.elr), (before.sp_el0, before.elr));
        assert_eq!(f.spsr, 0x6000_0000, "条件フラグだけ。EL0 のまま");
        assert_eq!(f.q, before.q);
        assert_eq!((f.fpcr, f.fpsr), (before.fpcr, before.fpsr));
        assert_eq!(
            f.tpidr_el0, before.tpidr_el0,
            "TLS はフレームに入らず、そのまま"
        );
    }
}
