//! 複数の fd の準備ができるのを待つシステムコール(pselect6、ppoll)。
//!
//! 「準備ができたか」を繰り返し確かめ、その合間にほかのタスクへ譲る。

use crate::Errno;
use crate::process::Process;
use crate::wait::{earliest, register_poll};
use alloc::vec;
use alloc::vec::Vec;
use hal::addr::VirtAddr;

const POLLIN: u16 = 0x1;
const POLLOUT: u16 = 0x4;
const POLLNVAL: u16 = 0x20;

/// timespec のポインタから「待ち終わる時刻」(ナノ秒)。NULL なら無期限。
fn deadline(p: &mut Process, ts: u64) -> Result<Option<u64>, Errno> {
    if ts == 0 {
        return Ok(None);
    }
    let mut b = [0u8; 16];
    p.mem.copy_from_user(VirtAddr(ts), &mut b)?;
    let sec = u64::from_le_bytes(b[..8].try_into().unwrap());
    let nsec = u64::from_le_bytes(b[8..].try_into().unwrap());
    let wait = sec.saturating_mul(1_000_000_000).saturating_add(nsec);
    Ok(Some(crate::time::now_ns().saturating_add(wait)))
}

fn expired(deadline: Option<u64>) -> bool {
    deadline.is_some_and(|d| crate::time::now_ns() >= d)
}

fn readable(p: &Process, fd: u64) -> Option<bool> {
    p.fds.get(fd).ok().map(|f| f.file.poll_readable())
}

fn read_set(p: &mut Process, va: u64, words: usize) -> Result<Vec<u64>, Errno> {
    if va == 0 {
        return Ok(vec![0; words]);
    }
    let mut b = vec![0u8; words * 8];
    p.mem.copy_from_user(VirtAddr(va), &mut b)?;
    Ok(b.chunks(8)
        .map(|c| u64::from_le_bytes(c.try_into().unwrap()))
        .collect())
}

fn write_set(p: &mut Process, va: u64, set: &[u64]) -> Result<(), Errno> {
    if va == 0 {
        return Ok(());
    }
    let b: Vec<u8> = set.iter().flat_map(|w| w.to_le_bytes()).collect();
    p.mem.copy_to_user(VirtAddr(va), &b)
}

/// pselect6(nfds, readfds, writefds, exceptfds, timeout, sigmask)
pub fn pselect6(
    p: &mut Process,
    nfds: u64,
    rd: u64,
    wr: u64,
    ex: u64,
    ts: u64,
) -> Result<u64, Errno> {
    if nfds > 1024 {
        return Err(Errno::EINVAL);
    }
    let words = (nfds as usize).div_ceil(64);
    let want_r = read_set(p, rd, words)?;
    let want_w = read_set(p, wr, words)?;
    let deadline = deadline(p, ts)?;
    loop {
        // 登録してから見る(見たあと、眠る前に読めるようになっても、起こしてもらえる)
        let reg = register_poll();
        let mut wake_at = deadline;
        let mut got_r = vec![0u64; words];
        let mut got_w = vec![0u64; words];
        let mut count = 0;
        for fd in 0..nfds {
            let (w, bit) = ((fd / 64) as usize, 1u64 << (fd % 64));
            if want_r[w] & bit != 0 {
                wake_at = earliest(wake_at, event_at(p, fd));
                match readable(p, fd) {
                    Some(true) => {
                        got_r[w] |= bit;
                        count += 1;
                    }
                    Some(false) => {}
                    None => return Err(Errno::EBADF),
                }
            }
            if want_w[w] & bit != 0 {
                // 書き込みは今のところ待たずにできる
                p.fds.get(fd)?;
                got_w[w] |= bit;
                count += 1;
            }
        }
        if count > 0 || expired(deadline) {
            write_set(p, rd, &got_r)?;
            write_set(p, wr, &got_w)?;
            write_set(p, ex, &vec![0; words])?;
            return Ok(count);
        }
        reg.sleep(wake_at);
    }
}

/// ppoll(fds, nfds, timeout, sigmask, sigsetsize)
pub fn ppoll(p: &mut Process, fds: u64, nfds: u64, ts: u64) -> Result<u64, Errno> {
    if nfds > 1024 {
        return Err(Errno::EINVAL);
    }
    let mut raw = vec![0u8; nfds as usize * 8];
    p.mem.copy_from_user(VirtAddr(fds), &mut raw)?;
    let deadline = deadline(p, ts)?;
    loop {
        let reg = register_poll();
        let mut wake_at = deadline;
        let mut count = 0;
        for entry in raw.chunks_mut(8) {
            let fd = i32::from_le_bytes(entry[..4].try_into().unwrap());
            let events = u16::from_le_bytes(entry[4..6].try_into().unwrap());
            if fd >= 0 {
                wake_at = earliest(wake_at, event_at(p, fd as u64));
            }
            let revents = if fd < 0 {
                0
            } else {
                match readable(p, fd as u64) {
                    None => POLLNVAL,
                    Some(r) => {
                        let mut rev = events & POLLOUT;
                        if r && events & POLLIN != 0 {
                            rev |= POLLIN;
                        }
                        rev
                    }
                }
            };
            entry[6..8].copy_from_slice(&revents.to_le_bytes());
            if revents != 0 {
                count += 1;
            }
        }
        if count > 0 || expired(deadline) {
            p.mem.copy_to_user(VirtAddr(fds), &raw)?;
            return Ok(count);
        }
        reg.sleep(wake_at);
    }
}

/// fd のファイルが、時間で読めるようになる時刻(timerfd など)
pub(super) fn event_at(p: &Process, fd: u64) -> Option<u64> {
    p.fds.get(fd).ok().and_then(|f| f.file.next_event_at())
}
