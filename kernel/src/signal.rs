//! シグナル:番号、既定の動作、ハンドラの設定(sigaction)、保留中のシグナルから次に配るものを選ぶ。
//!
//! ```text
//! kill / tgkill / 子の終わり(SIGCHLD)── 保留中のビットを立てる(スレッドごと)── 眠っていれば起こす
//!                                                  ↓
//!            ユーザーに戻る直前:マスクされていない、いちばん小さい番号を取り出す
//!                     SIG_IGN・既定が無視 → 捨てる / 既定が終了 → グループごと終わる / ハンドラ → 呼ぶ
//! ```

/// シグナルの番号(Linux の aarch64 と同じ)
pub const SIGHUP: u32 = 1;
pub const SIGINT: u32 = 2;
pub const SIGQUIT: u32 = 3;
pub const SIGILL: u32 = 4;
pub const SIGABRT: u32 = 6;
pub const SIGKILL: u32 = 9;
pub const SIGUSR1: u32 = 10;
pub const SIGSEGV: u32 = 11;
pub const SIGUSR2: u32 = 12;
pub const SIGPIPE: u32 = 13;
pub const SIGALRM: u32 = 14;
pub const SIGTERM: u32 = 15;
pub const SIGCHLD: u32 = 17;
pub const SIGCONT: u32 = 18;
pub const SIGSTOP: u32 = 19;
pub const SIGTSTP: u32 = 20;
pub const SIGTTIN: u32 = 21;
pub const SIGTTOU: u32 = 22;
pub const SIGURG: u32 = 23;
pub const SIGWINCH: u32 = 28;
/// いちばん大きい番号(リアルタイムシグナルを含む)
pub const NSIG: u32 = 64;

pub const SIG_DFL: u64 = 0;
pub const SIG_IGN: u64 = 1;

/// sigaction のフラグ
pub const SA_SIGINFO: u64 = 4;
pub const SA_ONSTACK: u64 = 0x0800_0000;
pub const SA_RESTART: u64 = 0x1000_0000;
pub const SA_NODEFER: u64 = 0x4000_0000;
pub const SA_RESETHAND: u64 = 0x8000_0000;
pub const SA_RESTORER: u64 = 0x0400_0000;

/// シグナル `sig` のビット
pub const fn bit(sig: u32) -> u64 {
    1 << (sig - 1)
}

/// マスクできないシグナル
pub const UNBLOCKABLE: u64 = bit(SIGKILL) | bit(SIGSTOP);

/// 1 つのシグナルの扱い(`struct k_sigaction`:handler, flags, restorer, mask の 32 バイト)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SigAction {
    pub handler: u64,
    pub flags: u64,
    pub restorer: u64,
    pub mask: u64,
}

impl SigAction {
    pub fn from_bytes(b: &[u8; 32]) -> Self {
        let w = |i: usize| u64::from_le_bytes(b[i * 8..i * 8 + 8].try_into().unwrap());
        Self {
            handler: w(0),
            flags: w(1),
            restorer: w(2),
            mask: w(3),
        }
    }

    pub fn to_bytes(self) -> [u8; 32] {
        let mut b = [0u8; 32];
        for (i, v) in [self.handler, self.flags, self.restorer, self.mask]
            .into_iter()
            .enumerate()
        {
            b[i * 8..i * 8 + 8].copy_from_slice(&v.to_le_bytes());
        }
        b
    }
}

/// 既定の動作
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DefaultAction {
    /// 終わる
    Terminate,
    /// 何もしない
    Ignore,
}

/// 既定の動作(止める・続けるはジョブ制御がまだないので、無視として扱う)
pub fn default_action(sig: u32) -> DefaultAction {
    match sig {
        SIGCHLD | SIGURG | SIGWINCH | SIGCONT | SIGSTOP | SIGTSTP | SIGTTIN | SIGTTOU => {
            DefaultAction::Ignore
        }
        _ => DefaultAction::Terminate,
    }
}

/// シグナルをどう処理するか
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Disposition {
    Ignore,
    Terminate,
    Handle(SigAction),
}

/// プロセスのハンドラの設定(スレッドで共有する)
#[derive(Clone)]
pub struct Actions {
    table: [SigAction; NSIG as usize],
}

impl Default for Actions {
    fn default() -> Self {
        Self {
            table: [SigAction::default(); NSIG as usize],
        }
    }
}

impl Actions {
    pub fn get(&self, sig: u32) -> SigAction {
        self.table[(sig - 1) as usize]
    }

    /// 設定を変える。SIGKILL と SIGSTOP は変えられない(EINVAL)
    pub fn set(&mut self, sig: u32, act: SigAction) -> Result<(), crate::Errno> {
        if sig == SIGKILL || sig == SIGSTOP {
            return Err(crate::Errno::EINVAL);
        }
        self.table[(sig - 1) as usize] = act;
        Ok(())
    }

    /// シグナル `sig` をどう処理するか
    pub fn disposition(&self, sig: u32) -> Disposition {
        let act = self.get(sig);
        match act.handler {
            SIG_IGN => Disposition::Ignore,
            SIG_DFL => match default_action(sig) {
                DefaultAction::Terminate => Disposition::Terminate,
                DefaultAction::Ignore => Disposition::Ignore,
            },
            _ => Disposition::Handle(act),
        }
    }

    /// execve:ハンドラは新しいプログラムにないので既定に戻す(無視の設定はそのまま。Linux と同じ)
    pub fn reset_for_exec(&mut self) {
        for act in &mut self.table {
            if act.handler != SIG_IGN {
                *act = SigAction::default();
            }
        }
    }
}

/// 保留中(`pending`)のうち、マスク(`blocked`)されていない、いちばん小さい番号
pub fn next(pending: u64, blocked: u64) -> Option<u32> {
    let ready = pending & !(blocked & !UNBLOCKABLE);
    (ready != 0).then(|| ready.trailing_zeros() + 1)
}

/// シグナルで EINTR になったとき、SA_RESTART のハンドラのあとでやり直すシステムコール
/// (Linux で ERESTARTSYS を返すもの。nanosleep・poll・sigsuspend などは、やり直さずに EINTR)
pub fn restartable(nr: u64) -> bool {
    use crate::syscall::nr::*;
    matches!(
        nr,
        READ | WRITE
            | READV
            | WRITEV
            | PREAD64
            | PWRITE64
            | OPENAT
            | IOCTL
            | WAIT4
            | ACCEPT
            | ACCEPT4
            | CONNECT
            | RECVFROM
            | SENDTO
            | RECVMSG
            | SENDMSG
            | FUTEX
    )
}

/// 番号が正しいか(1〜64)
pub fn valid(sig: u64) -> bool {
    (1..=NSIG as u64).contains(&sig)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Errno;

    #[test]
    fn defaults_terminate_except_the_ones_that_are_only_news() {
        for sig in [
            SIGHUP, SIGINT, SIGKILL, SIGTERM, SIGUSR1, SIGPIPE, SIGALRM, 40,
        ] {
            assert_eq!(default_action(sig), DefaultAction::Terminate, "{sig}");
        }
        // 子が終わった、窓の大きさが変わった、などは知らせるだけ
        for sig in [SIGCHLD, SIGURG, SIGWINCH, SIGCONT, SIGSTOP, SIGTSTP] {
            assert_eq!(default_action(sig), DefaultAction::Ignore, "{sig}");
        }
    }

    #[test]
    fn the_lowest_unblocked_pending_signal_comes_first() {
        let pending = bit(SIGUSR1) | bit(SIGINT) | bit(SIGTERM);
        assert_eq!(next(pending, 0), Some(SIGINT));
        assert_eq!(next(pending, bit(SIGINT)), Some(SIGUSR1));
        assert_eq!(next(pending, !0), None, "全部マスクされている");
        // SIGKILL はマスクできない
        assert_eq!(next(bit(SIGKILL), !0), Some(SIGKILL));
        assert_eq!(next(0, 0), None);
        assert_eq!(next(bit(64), 0), Some(64));
    }

    #[test]
    fn handlers_ignores_and_defaults() {
        let mut a = Actions::default();
        assert_eq!(a.disposition(SIGTERM), Disposition::Terminate);
        assert_eq!(a.disposition(SIGCHLD), Disposition::Ignore);
        a.set(
            SIGTERM,
            SigAction {
                handler: SIG_IGN,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(a.disposition(SIGTERM), Disposition::Ignore);
        let h = SigAction {
            handler: 0x40_1000,
            flags: SA_SIGINFO,
            restorer: 0x40_2000,
            mask: 0,
        };
        a.set(SIGUSR1, h).unwrap();
        assert_eq!(a.disposition(SIGUSR1), Disposition::Handle(h));
        assert_eq!(a.get(SIGUSR1), h);
        // SIGKILL と SIGSTOP は変えられない
        assert_eq!(a.set(SIGKILL, h), Err(Errno::EINVAL));
        assert_eq!(a.set(SIGSTOP, h), Err(Errno::EINVAL));
        // exec:ハンドラは既定に、無視はそのまま
        a.reset_for_exec();
        assert_eq!(a.disposition(SIGUSR1), Disposition::Terminate);
        assert_eq!(a.disposition(SIGTERM), Disposition::Ignore);
    }

    #[test]
    fn sigaction_bytes_round_trip() {
        let h = SigAction {
            handler: 1,
            flags: 2,
            restorer: 3,
            mask: 4,
        };
        assert_eq!(SigAction::from_bytes(&h.to_bytes()), h);
    }
}
