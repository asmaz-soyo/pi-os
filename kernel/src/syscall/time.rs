//! 待つ・時間に関するシステムコール(nanosleep、timerfd、epoll)。

use crate::Errno;
use crate::fs::epoll::{EPOLLIN, EPOLLOUT, Epoll};
use crate::fs::timerfd::TimerFd;
use crate::fs::vfs::{O_RDWR, OpenFile};
use crate::process::Process;
use alloc::sync::Arc;
use alloc::vec::Vec;
use hal::addr::VirtAddr;

fn read_timespec(p: &mut Process, va: u64) -> Result<u64, Errno> {
    let mut b = [0u8; 16];
    p.mem.copy_from_user(VirtAddr(va), &mut b)?;
    let sec = u64::from_le_bytes(b[..8].try_into().unwrap());
    let nsec = u64::from_le_bytes(b[8..].try_into().unwrap());
    if nsec >= 1_000_000_000 {
        return Err(Errno::EINVAL);
    }
    Ok(sec.saturating_mul(1_000_000_000).saturating_add(nsec))
}

fn timespec_bytes(ns: u64) -> [u8; 16] {
    let mut b = [0u8; 16];
    b[..8].copy_from_slice(&(ns / 1_000_000_000).to_le_bytes());
    b[8..].copy_from_slice(&(ns % 1_000_000_000).to_le_bytes());
    b
}

fn sleep_until(deadline: u64) {
    // スケジューラに眠らせてもらう(待つ間、ほかのタスクが CPU を使える)
    crate::sched::sleep_until(deadline);
}

/// nanosleep(req, rem)
pub fn nanosleep(p: &mut Process, req: u64) -> Result<u64, Errno> {
    let wait = read_timespec(p, req)?;
    sleep_until(crate::time::now_ns().saturating_add(wait));
    Ok(0)
}

const TIMER_ABSTIME: u64 = 1;

/// clock_nanosleep(clock, flags, req, rem)
pub fn clock_nanosleep(p: &mut Process, clock: u64, flags: u64, req: u64) -> Result<u64, Errno> {
    let t = read_timespec(p, req)?;
    let deadline = if flags & TIMER_ABSTIME != 0 {
        // 実時間の時計なら、起動からの時間に直す
        if clock == 0 {
            t.saturating_sub(crate::time::BOOT_EPOCH * 1_000_000_000)
        } else {
            t
        }
    } else {
        crate::time::now_ns().saturating_add(t)
    };
    sleep_until(deadline);
    Ok(0)
}

fn timerfd(p: &Process, fd: u64) -> Result<Arc<OpenFile>, Errno> {
    let f = p.fds.get(fd)?;
    if f.file.as_any().is_some_and(|a| a.is::<TimerFd>()) {
        Ok(f)
    } else {
        Err(Errno::EINVAL)
    }
}

/// timerfd_create(clockid, flags)
pub fn timerfd_create(p: &mut Process) -> Result<u64, Errno> {
    let fd = p
        .fds
        .insert(OpenFile::new(Arc::new(TimerFd::new()), O_RDWR))?;
    Ok(fd as u64)
}

/// timerfd_settime(fd, flags, new_value, old_value)
pub fn timerfd_settime(
    p: &mut Process,
    fd: u64,
    flags: u64,
    new: u64,
    old: u64,
) -> Result<u64, Errno> {
    let f = timerfd(p, fd)?;
    let t = f
        .file
        .as_any()
        .and_then(|a| a.downcast_ref::<TimerFd>())
        .expect("checked above");
    if old != 0 {
        let (interval, value) = t.get();
        let mut b = timespec_bytes(interval).to_vec();
        b.extend(timespec_bytes(value));
        p.mem.copy_to_user(VirtAddr(old), &b)?;
    }
    let interval = read_timespec(p, new)?;
    let value = read_timespec(p, new + 16)?;
    let first = match (value, flags & TIMER_ABSTIME != 0) {
        (0, _) => None, // 0 なら止める
        (v, true) => Some(v),
        (v, false) => Some(crate::time::now_ns().saturating_add(v)),
    };
    t.set(first, interval);
    Ok(0)
}

/// timerfd_gettime(fd, curr_value)
pub fn timerfd_gettime(p: &mut Process, fd: u64, curr: u64) -> Result<u64, Errno> {
    let f = timerfd(p, fd)?;
    let t = f
        .file
        .as_any()
        .and_then(|a| a.downcast_ref::<TimerFd>())
        .expect("checked above");
    let (interval, value) = t.get();
    let mut b = timespec_bytes(interval).to_vec();
    b.extend(timespec_bytes(value));
    p.mem.copy_to_user(VirtAddr(curr), &b)?;
    Ok(0)
}

/// epoll_create1(flags)
pub fn epoll_create1(p: &mut Process) -> Result<u64, Errno> {
    let fd = p
        .fds
        .insert(OpenFile::new(Arc::new(Epoll::new()), O_RDWR))?;
    Ok(fd as u64)
}

fn epoll(p: &Process, fd: u64) -> Result<Arc<OpenFile>, Errno> {
    let f = p.fds.get(fd)?;
    if f.file.as_any().is_some_and(|a| a.is::<Epoll>()) {
        Ok(f)
    } else {
        Err(Errno::EINVAL)
    }
}

const EPOLL_CTL_ADD: u64 = 1;
const EPOLL_CTL_DEL: u64 = 2;
const EPOLL_CTL_MOD: u64 = 3;

/// epoll_ctl(epfd, op, fd, event)
pub fn epoll_ctl(p: &mut Process, epfd: u64, op: u64, fd: u64, event: u64) -> Result<u64, Errno> {
    let ep = epoll(p, epfd)?;
    p.fds.get(fd)?;
    let e = ep
        .file
        .as_any()
        .and_then(|a| a.downcast_ref::<Epoll>())
        .expect("checked above");
    let read_event = |p: &mut Process| -> Result<(u32, u64), Errno> {
        let mut b = [0u8; 16];
        p.mem.copy_from_user(VirtAddr(event), &mut b)?;
        Ok((
            u32::from_le_bytes(b[..4].try_into().unwrap()),
            u64::from_le_bytes(b[8..].try_into().unwrap()),
        ))
    };
    match op {
        EPOLL_CTL_ADD => {
            let ev = read_event(p)?;
            let mut interest = e.interest.lock();
            if interest.contains_key(&fd) {
                return Err(Errno::EEXIST);
            }
            interest.insert(fd, ev);
        }
        EPOLL_CTL_MOD => {
            let ev = read_event(p)?;
            *e.interest.lock().get_mut(&fd).ok_or(Errno::ENOENT)? = ev;
        }
        EPOLL_CTL_DEL => {
            e.interest.lock().remove(&fd).ok_or(Errno::ENOENT)?;
        }
        _ => return Err(Errno::EINVAL),
    }
    Ok(0)
}

/// epoll_pwait(epfd, events, maxevents, timeout(ミリ秒、-1 なら無期限), sigmask)
pub fn epoll_pwait(
    p: &mut Process,
    epfd: u64,
    events: u64,
    max: u64,
    timeout: i32,
) -> Result<u64, Errno> {
    if max == 0 {
        return Err(Errno::EINVAL);
    }
    let ep = epoll(p, epfd)?;
    let e = ep
        .file
        .as_any()
        .and_then(|a| a.downcast_ref::<Epoll>())
        .expect("checked above");
    let deadline = (timeout >= 0).then(|| crate::time::now_ns() + timeout as u64 * 1_000_000);
    loop {
        let interest: Vec<(u64, (u32, u64))> =
            e.interest.lock().iter().map(|(k, v)| (*k, *v)).collect();
        let mut out = Vec::new();
        for (fd, (want, data)) in interest {
            let Ok(f) = p.fds.get(fd) else { continue };
            let mut got = 0;
            if want & EPOLLIN != 0 && f.file.poll_readable() {
                got |= EPOLLIN;
            }
            if want & EPOLLOUT != 0 {
                got |= EPOLLOUT;
            }
            if got != 0 {
                out.extend_from_slice(&got.to_le_bytes());
                out.extend_from_slice(&[0u8; 4]);
                out.extend_from_slice(&data.to_le_bytes());
                if out.len() / 16 == max as usize {
                    break;
                }
            }
        }
        if !out.is_empty() || deadline.is_some_and(|d| crate::time::now_ns() >= d) {
            p.mem.copy_to_user(VirtAddr(events), &out)?;
            return Ok(out.len() as u64 / 16);
        }
        crate::sched::relax();
    }
}
