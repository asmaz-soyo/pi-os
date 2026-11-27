//! シグナルのシステムコール:rt_sigaction、rt_sigprocmask、rt_sigpending、kill、tkill、tgkill。
//! 配るのは、ユーザーに戻る直前(boot の before_return_to_user)。

use crate::Errno;
use crate::process::{PROCESSES, Pid, Process};
use crate::signal::{SigAction, UNBLOCKABLE, valid};
use hal::addr::VirtAddr;

/// sigset_t の大きさ(カーネルの見方は 64 ビット)
const SIGSET_SIZE: u64 = 8;

/// rt_sigaction(sig, act, oldact, sigsetsize)
pub fn rt_sigaction(
    p: &mut Process,
    sig: u64,
    act: u64,
    old: u64,
    size: u64,
) -> Result<u64, Errno> {
    if !valid(sig) || size != SIGSET_SIZE {
        return Err(Errno::EINVAL);
    }
    let sig = sig as u32;
    let new = if act != 0 {
        let mut b = [0u8; 32];
        p.mem.copy_from_user(VirtAddr(act), &mut b)?;
        Some(SigAction::from_bytes(&b))
    } else {
        None
    };
    let prev = p.sig_actions.lock().get(sig);
    if let Some(a) = new {
        p.sig_actions.lock().set(sig, a)?;
    }
    if old != 0 {
        p.mem.copy_to_user(VirtAddr(old), &prev.to_bytes())?;
    }
    Ok(0)
}

/// rt_sigprocmask(how, set, oldset, sigsetsize)
pub fn rt_sigprocmask(
    p: &mut Process,
    how: u64,
    set: u64,
    old: u64,
    size: u64,
) -> Result<u64, Errno> {
    const SIG_BLOCK: u64 = 0;
    const SIG_UNBLOCK: u64 = 1;
    const SIG_SETMASK: u64 = 2;
    if size != SIGSET_SIZE {
        return Err(Errno::EINVAL);
    }
    let prev = p.sig_blocked;
    if set != 0 {
        let mut b = [0u8; 8];
        p.mem.copy_from_user(VirtAddr(set), &mut b)?;
        let m = u64::from_le_bytes(b);
        let blocked = match how {
            SIG_BLOCK => prev | m,
            SIG_UNBLOCK => prev & !m,
            SIG_SETMASK => m,
            _ => return Err(Errno::EINVAL),
        };
        p.sig_blocked = blocked & !UNBLOCKABLE;
    }
    if old != 0 {
        p.mem.copy_to_user(VirtAddr(old), &prev.to_le_bytes())?;
    }
    Ok(0)
}

/// rt_sigpending(set, sigsetsize):マスクされて保留になっているもの
pub fn rt_sigpending(p: &mut Process, set: u64, size: u64) -> Result<u64, Errno> {
    if size != SIGSET_SIZE {
        return Err(Errno::EINVAL);
    }
    let pending = PROCESSES.pending(p.pid) & p.sig_blocked;
    p.mem.copy_to_user(VirtAddr(set), &pending.to_le_bytes())?;
    Ok(0)
}

/// kill(pid, sig)。pid が 0 か負(プロセスのグループ)は、まだグループがないので自分のプロセス
pub fn kill(p: &mut Process, pid: i64, sig: u64) -> Result<u64, Errno> {
    if sig != 0 && !valid(sig) {
        return Err(Errno::EINVAL);
    }
    let target: Pid = if pid > 0 { pid as Pid } else { p.tgid };
    PROCESSES.send_group(target, sig as u32)?;
    Ok(0)
}

/// tgkill(tgid, tid, sig) / tkill(tid, sig):スレッドを指して送る
pub fn tgkill(_p: &mut Process, tgid: Option<u64>, tid: u64, sig: u64) -> Result<u64, Errno> {
    if sig != 0 && !valid(sig) {
        return Err(Errno::EINVAL);
    }
    let tid = tid as Pid;
    match PROCESSES.group_of(tid) {
        Some(g) if tgid.is_none_or(|t| t as Pid == g) => {
            if sig != 0 {
                PROCESSES.send(tid, sig as u32);
            }
            Ok(0)
        }
        _ => Err(Errno::ESRCH),
    }
}
