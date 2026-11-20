//! 時刻、乱数、システムの情報などのシステムコール。

use crate::Errno;
use crate::process::Process;
use crate::sync::SpinLock;
use alloc::vec;
use hal::addr::VirtAddr;

const CLOCK_REALTIME: u64 = 0;
const CLOCK_MONOTONIC: u64 = 1;
const CLOCK_PROCESS_CPUTIME_ID: u64 = 2;
const CLOCK_MONOTONIC_RAW: u64 = 4;
const CLOCK_REALTIME_COARSE: u64 = 5;
const CLOCK_MONOTONIC_COARSE: u64 = 6;
const CLOCK_BOOTTIME: u64 = 7;

/// clock_gettime(clock, timespec)
pub fn clock_gettime(p: &mut Process, clock: u64, ts: u64) -> Result<u64, Errno> {
    let now = crate::time::now_ns();
    let ns = match clock {
        CLOCK_REALTIME | CLOCK_REALTIME_COARSE => crate::time::BOOT_EPOCH * 1_000_000_000 + now,
        CLOCK_MONOTONIC
        | CLOCK_MONOTONIC_RAW
        | CLOCK_MONOTONIC_COARSE
        | CLOCK_BOOTTIME
        | CLOCK_PROCESS_CPUTIME_ID => now,
        _ => return Err(Errno::EINVAL),
    };
    let mut b = [0u8; 16];
    b[..8].copy_from_slice(&(ns / 1_000_000_000).to_le_bytes());
    b[8..].copy_from_slice(&(ns % 1_000_000_000).to_le_bytes());
    p.mem.copy_to_user(VirtAddr(ts), &b)?;
    Ok(0)
}

/// sched_getaffinity(pid, size, mask):CPU 0 だけ。
pub fn sched_getaffinity(p: &mut Process, size: u64, mask: u64) -> Result<u64, Errno> {
    if size < 8 {
        return Err(Errno::EINVAL);
    }
    p.mem.copy_to_user(VirtAddr(mask), &1u64.to_le_bytes())?;
    Ok(8)
}

/// uname(buf):`struct utsname`(65 バイトの文字列 × 6)
pub fn uname(p: &mut Process, buf: u64) -> Result<u64, Errno> {
    let fields: [&[u8]; 6] = [b"Pi-OS", b"pi", b"0.1.0", b"#1 Pi-OS", b"aarch64", b""];
    let mut b = [0u8; 65 * 6];
    for (i, f) in fields.iter().enumerate() {
        b[i * 65..i * 65 + f.len()].copy_from_slice(f);
    }
    p.mem.copy_to_user(VirtAddr(buf), &b)?;
    Ok(0)
}

/// 乱数の状態(xorshift64*)。暗号には使えない。ハードウェアの乱数源はフェーズ2以降
static RNG: SpinLock<u64> = SpinLock::new(0);

fn next_random() -> u64 {
    let mut s = RNG.lock();
    if *s == 0 {
        *s = crate::time::now_ns() ^ 0x9e37_79b9_7f4a_7c15;
    }
    *s ^= *s >> 12;
    *s ^= *s << 25;
    *s ^= *s >> 27;
    s.wrapping_mul(0x2545_f491_4f6c_dd1d)
}

/// getrandom(buf, len, flags)
pub fn getrandom(p: &mut Process, buf: u64, len: u64) -> Result<u64, Errno> {
    let len = len.min(256) as usize;
    let mut b = vec![0u8; len];
    for chunk in b.chunks_mut(8) {
        let r = next_random().to_le_bytes();
        chunk.copy_from_slice(&r[..chunk.len()]);
    }
    p.mem.copy_to_user(VirtAddr(buf), &b)?;
    Ok(len as u64)
}
