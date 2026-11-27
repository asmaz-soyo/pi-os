//! シグナルのシステムコールのテスト(配送とハンドラは QEMU で確かめる)。

use super::tests::{SCRATCH, call, err, get, process, put};
use super::*;
use crate::Errno;
use crate::process::PROCESSES;
use crate::signal::*;

fn sigaction(p: &mut crate::process::Process, sig: u32, act: Option<SigAction>) -> u64 {
    let act_ptr = match act {
        Some(a) => {
            put(p, SCRATCH, &a.to_bytes());
            SCRATCH
        }
        None => 0,
    };
    call(
        p,
        nr::RT_SIGACTION,
        [sig as u64, act_ptr, SCRATCH + 0x40, 8, 0, 0],
    )
}

fn old_action(p: &mut crate::process::Process) -> SigAction {
    SigAction::from_bytes(&get(p, SCRATCH + 0x40, 32).try_into().unwrap())
}

#[test]
fn sigaction_stores_and_returns_the_previous_setting() {
    let (mut p, _) = process();
    let h = SigAction {
        handler: 0x40_1000,
        flags: SA_SIGINFO | SA_RESTORER,
        restorer: 0x40_2000,
        mask: bit(SIGUSR2),
    };
    assert_eq!(sigaction(&mut p, SIGUSR1, Some(h)), 0);
    assert_eq!(old_action(&mut p), SigAction::default(), "前は既定");
    assert_eq!(sigaction(&mut p, SIGUSR1, None), 0);
    assert_eq!(old_action(&mut p), h);
    assert_eq!(sigaction(&mut p, SIGKILL, Some(h)), err(Errno::EINVAL));
    assert_eq!(sigaction(&mut p, 65, None), err(Errno::EINVAL));
    assert_eq!(
        call(&mut p, nr::RT_SIGACTION, [SIGUSR1 as u64, 0, 0, 4, 0, 0]),
        err(Errno::EINVAL)
    );
}

#[test]
fn sigprocmask_blocks_unblocks_and_never_blocks_sigkill() {
    const SIG_BLOCK: u64 = 0;
    const SIG_UNBLOCK: u64 = 1;
    const SIG_SETMASK: u64 = 2;
    let (mut p, _) = process();
    let set = |p: &mut crate::process::Process, m: u64| put(p, SCRATCH, &m.to_le_bytes());
    set(&mut p, bit(SIGINT) | bit(SIGKILL));
    assert_eq!(
        call(
            &mut p,
            nr::RT_SIGPROCMASK,
            [SIG_BLOCK, SCRATCH, SCRATCH + 8, 8, 0, 0]
        ),
        0
    );
    assert_eq!(get(&mut p, SCRATCH + 8, 8), 0u64.to_le_bytes());
    assert_eq!(p.sig_blocked, bit(SIGINT), "SIGKILL はマスクできない");
    set(&mut p, bit(SIGTERM));
    call(&mut p, nr::RT_SIGPROCMASK, [SIG_BLOCK, SCRATCH, 0, 8, 0, 0]);
    set(&mut p, bit(SIGINT));
    call(
        &mut p,
        nr::RT_SIGPROCMASK,
        [SIG_UNBLOCK, SCRATCH, 0, 8, 0, 0],
    );
    assert_eq!(p.sig_blocked, bit(SIGTERM));
    set(&mut p, bit(SIGUSR1));
    call(
        &mut p,
        nr::RT_SIGPROCMASK,
        [SIG_SETMASK, SCRATCH, SCRATCH + 8, 8, 0, 0],
    );
    assert_eq!(get(&mut p, SCRATCH + 8, 8), bit(SIGTERM).to_le_bytes());
    assert_eq!(p.sig_blocked, bit(SIGUSR1));
    assert_eq!(
        call(&mut p, nr::RT_SIGPROCMASK, [7, SCRATCH, 0, 8, 0, 0]),
        err(Errno::EINVAL)
    );
}

#[test]
fn kill_and_tgkill_make_a_signal_pending_for_the_target() {
    let (mut p, _) = process();
    PROCESSES.add_info(9100, 1, "target");
    assert_eq!(
        call(&mut p, nr::KILL, [9100, SIGTERM as u64, 0, 0, 0, 0]),
        0
    );
    assert_eq!(PROCESSES.pending(9100), bit(SIGTERM));
    // シグナル 0:送らずに、いるかどうかだけを確かめる
    assert_eq!(call(&mut p, nr::KILL, [9100, 0, 0, 0, 0, 0]), 0);
    assert_eq!(
        call(&mut p, nr::KILL, [9199, SIGTERM as u64, 0, 0, 0, 0]),
        err(Errno::ESRCH)
    );
    assert_eq!(
        call(&mut p, nr::KILL, [9100, 99, 0, 0, 0, 0]),
        err(Errno::EINVAL)
    );
    assert_eq!(
        call(&mut p, nr::TGKILL, [9100, 9100, SIGUSR1 as u64, 0, 0, 0]),
        0
    );
    assert_eq!(PROCESSES.pending(9100), bit(SIGTERM) | bit(SIGUSR1));
    assert_eq!(
        call(&mut p, nr::TGKILL, [1234, 9100, SIGUSR1 as u64, 0, 0, 0]),
        err(Errno::ESRCH)
    );
    assert_eq!(PROCESSES.take_signal(9100, 0), Some(SIGUSR1));
    assert_eq!(
        PROCESSES.take_signal(9100, bit(SIGTERM)),
        None,
        "マスクされたものは残る"
    );
    assert_eq!(call(&mut p, nr::RT_SIGPENDING, [SCRATCH, 8, 0, 0, 0, 0]), 0);
    PROCESSES.exit(9100, crate::process::ExitStatus::Code(0));
}

#[test]
fn fork_copies_the_handlers_and_a_thread_shares_them() {
    let (mut p, _) = process();
    let h = SigAction {
        handler: 0x40_1000,
        ..Default::default()
    };
    let mut child = p.fork().unwrap();
    let thread = p.thread();
    p.sig_actions.lock().set(SIGUSR1, h).unwrap();
    assert_eq!(
        thread.sig_actions.lock().get(SIGUSR1),
        h,
        "スレッドは同じ設定"
    );
    assert_eq!(
        child.sig_actions.lock().get(SIGUSR1),
        SigAction::default(),
        "fork は写し"
    );
    child.sig_blocked = 5;
    assert_eq!(p.sig_blocked, 0);
}
