//! futex(uaddr, op, val, timeout / val2, uaddr2, val3)。待ちの表は `crate::futex`。
//!
//! - FUTEX_WAIT:`*uaddr == val` なら、起こされるか期限(相対時間)まで眠る。違えば EAGAIN
//! - FUTEX_WAIT_BITSET:期限が絶対時刻(bitset は使わず、全部が当たるものとして扱う)
//! - FUTEX_WAKE(_BITSET):最大 `val` 人を起こし、起こした数を返す
//! - FUTEX_REQUEUE / FUTEX_CMP_REQUEUE:`val` 人を起こし、続く最大 `val2` 人を `uaddr2` の列へ移す
//!   (条件変数の broadcast。全員を一斉に起こしてミューテックスを奪い合わせない)
//! - FUTEX_PRIVATE_FLAG(同じプロセスの中だけ):鍵がもともとアドレス空間ごとなので、同じに扱う

use super::time::read_timespec;
use crate::Errno;
use crate::futex::{FUTEXES, Key};
use crate::process::{PROCESSES, Process};
use hal::addr::VirtAddr;

const FUTEX_WAIT: u64 = 0;
const FUTEX_WAKE: u64 = 1;
const FUTEX_REQUEUE: u64 = 3;
const FUTEX_CMP_REQUEUE: u64 = 4;
const FUTEX_WAIT_BITSET: u64 = 9;
const FUTEX_WAKE_BITSET: u64 = 10;
const FUTEX_PRIVATE_FLAG: u64 = 128;
const FUTEX_CLOCK_REALTIME: u64 = 256;

pub fn futex(
    p: &mut Process,
    uaddr: u64,
    op: u64,
    val: u64,
    timeout: u64,
    uaddr2: u64,
    val3: u64,
) -> Result<u64, Errno> {
    if !uaddr.is_multiple_of(4) {
        return Err(Errno::EINVAL);
    }
    let key = (p.mem.id(), uaddr);
    let val = val as u32;
    match op & !(FUTEX_PRIVATE_FLAG | FUTEX_CLOCK_REALTIME) {
        FUTEX_WAIT => {
            let deadline = if timeout == 0 {
                None
            } else {
                Some(crate::time::now_ns().saturating_add(read_timespec(p, timeout)?))
            };
            wait(p, key, val, deadline)
        }
        FUTEX_WAIT_BITSET => {
            let deadline = if timeout == 0 {
                None
            } else {
                let at = read_timespec(p, timeout)?;
                // 実時間の時計なら、起動からの時間に直す
                Some(if op & FUTEX_CLOCK_REALTIME != 0 {
                    at.saturating_sub(crate::time::BOOT_EPOCH * 1_000_000_000)
                } else {
                    at
                })
            };
            wait(p, key, val, deadline)
        }
        FUTEX_WAKE | FUTEX_WAKE_BITSET => Ok(crate::futex::wake(key, val as usize) as u64),
        FUTEX_REQUEUE | FUTEX_CMP_REQUEUE => {
            if op & !FUTEX_PRIVATE_FLAG == FUTEX_CMP_REQUEUE && load(p, uaddr)? != val3 as u32 {
                return Err(Errno::EAGAIN);
            }
            // val2 は timeout の場所に入って来る
            let to = (p.mem.id(), uaddr2);
            let woken = FUTEXES
                .lock()
                .requeue(key, val as usize, to, timeout as usize);
            for &t in &woken {
                crate::sched::wake(t);
            }
            Ok(woken.len() as u64)
        }
        _ => Err(Errno::ENOSYS),
    }
}

fn load(p: &mut Process, uaddr: u64) -> Result<u32, Errno> {
    let mut b = [0u8; 4];
    p.mem.copy_from_user(VirtAddr(uaddr), &mut b)?;
    Ok(u32::from_le_bytes(b))
}

/// `*uaddr == val` なら列に積んで眠る。起こされたら(列からいなくなっていたら)0
fn wait(p: &mut Process, key: Key, val: u32, deadline: Option<u64>) -> Result<u64, Errno> {
    let me = p.pid;
    {
        // 値を確かめてから列に積むまで、表のロックを持ったまま(そのあいだに WAKE が来て、
        // 起こしてもらえないまま眠り込むことがないように)
        let mut f = FUTEXES.lock();
        if load(p, key.1)? != val {
            return Err(Errno::EAGAIN);
        }
        f.enqueue(key, me);
    }
    loop {
        if !FUTEXES.lock().is_queued(key, me) {
            return Ok(0); // 起こされた
        }
        // exit_group でほかのスレッドが終わらせようとしている
        if PROCESSES.is_killed(me) {
            FUTEXES.lock().remove(key, me);
            return Err(Errno::EINTR);
        }
        if deadline.is_some_and(|d| crate::time::now_ns() >= d) {
            FUTEXES.lock().remove(key, me);
            return Err(Errno::ETIMEDOUT);
        }
        if crate::sched::running() {
            crate::sched::sleep_until(deadline.unwrap_or(u64::MAX));
        } else {
            core::hint::spin_loop();
        }
    }
}
