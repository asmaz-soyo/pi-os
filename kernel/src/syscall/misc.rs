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

const SCHED_OTHER: u64 = 0;
const SCHED_FIFO: u64 = 1;

/// 対象のプロセス:0 か自分の番号だけ(他のプロセスはまだ扱わない)
fn sched_target(p: &Process, pid: u64) -> Result<(), Errno> {
    if pid == 0 || pid == p.pid as u64 {
        Ok(())
    } else {
        Err(Errno::ESRCH)
    }
}

/// sched_setscheduler(pid, policy, param):SCHED_OTHER(優先度 0)と SCHED_FIFO(1〜99)。
/// リアルタイムにできるのは root だけ。
pub fn sched_setscheduler(
    p: &mut Process,
    pid: u64,
    policy: u64,
    param: u64,
) -> Result<u64, Errno> {
    sched_target(p, pid)?;
    let mut b = [0u8; 4];
    p.mem.copy_from_user(VirtAddr(param), &mut b)?;
    let prio = i32::from_le_bytes(b);
    let prio = match policy {
        SCHED_OTHER if prio == 0 => 0,
        SCHED_FIFO if (1..=99).contains(&prio) => prio as u8,
        _ => return Err(Errno::EINVAL),
    };
    if prio > 0 && p.cred.euid != 0 {
        return Err(Errno::EPERM);
    }
    p.rt_priority = prio;
    crate::sched::set_priority(p.pid, prio);
    Ok(0)
}

/// sched_getscheduler(pid)
pub fn sched_getscheduler(p: &mut Process, pid: u64) -> Result<u64, Errno> {
    sched_target(p, pid)?;
    Ok(if p.rt_priority > 0 {
        SCHED_FIFO
    } else {
        SCHED_OTHER
    })
}

/// prctl(option, arg2, ...):今は PR_SET_NAME(プロセスの名前を変える。/proc の Name に出る)だけ。
pub fn prctl(p: &mut Process, option: u64, arg: u64) -> Result<u64, Errno> {
    const PR_SET_NAME: u64 = 15;
    match option {
        PR_SET_NAME => {
            // 名前は最大 16 バイト(終わりの 0 を含む)
            let raw = p.mem.read_cstr(VirtAddr(arg), 64)?;
            p.name = alloc::string::String::from_utf8_lossy(&raw[..raw.len().min(15)]).into_owned();
            crate::process::PROCESSES.rename(p.pid, &p.name);
            Ok(0)
        }
        _ => Err(Errno::EINVAL),
    }
}

/// sigaltstack(new, old):シグナルのハンドラを動かす別のスタック(SA_ONSTACK のとき使う)。
/// `stack_t { void *ss_sp; int ss_flags; size_t ss_size; }`(24 バイト)
pub fn sigaltstack(p: &mut Process, new: u64, old: u64) -> Result<u64, Errno> {
    const SS_DISABLE: i32 = 2;
    const MINSIGSTKSZ: u64 = 5120; // arm64
    let prev = p.sig_altstack;
    if new != 0 {
        let mut b = [0u8; 24];
        p.mem.copy_from_user(VirtAddr(new), &mut b)?;
        let sp = u64::from_le_bytes(b[0..8].try_into().unwrap());
        let flags = i32::from_le_bytes(b[8..12].try_into().unwrap());
        let size = u64::from_le_bytes(b[16..24].try_into().unwrap());
        p.sig_altstack = if flags & SS_DISABLE != 0 {
            (0, SS_DISABLE, 0)
        } else if size < MINSIGSTKSZ {
            return Err(Errno::ENOMEM);
        } else {
            (sp, 0, size)
        };
    }
    if old != 0 {
        let (sp, flags, size) = prev;
        let flags = if size == 0 { SS_DISABLE } else { flags };
        let mut b = [0u8; 24];
        b[0..8].copy_from_slice(&sp.to_le_bytes());
        b[8..12].copy_from_slice(&flags.to_le_bytes());
        b[16..24].copy_from_slice(&size.to_le_bytes());
        p.mem.copy_to_user(VirtAddr(old), &b)?;
    }
    Ok(0)
}

/// sysinfo(info):起動からの秒数と、メモリの量(`struct sysinfo`、112 バイト)
pub fn sysinfo(p: &mut Process, info: u64) -> Result<u64, Errno> {
    let (free, total) = crate::mm::frame_stats();
    let page = hal::addr::PAGE_SIZE;
    let mut b = [0u8; 112];
    b[0..8].copy_from_slice(&(crate::time::now_ns() / 1_000_000_000).to_le_bytes());
    b[32..40].copy_from_slice(&(total as u64 * page).to_le_bytes()); // totalram
    b[40..48].copy_from_slice(&(free as u64 * page).to_le_bytes()); // freeram
    let procs = crate::process::PROCESSES.infos().len() as u16;
    b[80..82].copy_from_slice(&procs.to_le_bytes());
    b[104..108].copy_from_slice(&1u32.to_le_bytes()); // mem_unit:上の値はバイト単位
    p.mem.copy_to_user(VirtAddr(info), &b)?;
    Ok(0)
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

/// カーネルの中で使う乱数(execve の AT_RANDOM など)。
pub fn fill_random(buf: &mut [u8]) {
    for chunk in buf.chunks_mut(8) {
        let r = next_random().to_le_bytes();
        chunk.copy_from_slice(&r[..chunk.len()]);
    }
}
