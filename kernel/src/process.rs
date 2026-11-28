//! プロセス(ユーザープログラムの実行単位)。
//!
//! フェーズ1では同時に動くプロセスは1つだけ(qjs は1プロセスで動く)。
//! マルチタスクはフェーズ2で、ここにスケジューラを足す。

use crate::cred::Credentials;
use crate::exec::Loaded;
use crate::fs::vfs::{FdTable, File, O_RDWR, OpenFile, Vfs};
use crate::mm::user::UserMemory;
use crate::sync::SpinLock;
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::sync::Arc;

/// mmap で場所を選ぶときに使い始めるアドレス
pub const MMAP_BASE: u64 = 0x0000_4000_0000_0000;

/// プロセスの終わり方。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitStatus {
    /// exit(n) で終わった
    Code(i32),
    /// シグナルで終わった(フォールト、強制終了)
    Signal(i32),
}

impl ExitStatus {
    /// wait4 が返す値(Linux と同じ形:終了コードは 8 ビット左、シグナルは下位 7 ビット)
    pub fn wait_status(self) -> u32 {
        match self {
            Self::Code(n) => ((n as u32) & 0xff) << 8,
            Self::Signal(s) => (s as u32) & 0x7f,
        }
    }

    /// シェルが `$?` に入れる値(シグナルなら 128 + 番号)
    pub fn shell_code(self) -> i32 {
        match self {
            Self::Code(n) => n,
            Self::Signal(s) => 128 + s,
        }
    }
}

/// 新しいユーザー空間を作る関数(BSP が用意する。fork と execve で使う)
pub type SpaceFactory = fn() -> Result<alloc::boxed::Box<dyn hal::mmu::AddressSpace>, crate::Errno>;

fn no_space() -> Result<alloc::boxed::Box<dyn hal::mmu::AddressSpace>, crate::Errno> {
    Err(crate::Errno::ENOMEM)
}

pub struct Process {
    /// スレッドの番号(gettid)。タスクの番号と同じ
    pub pid: Pid,
    /// スレッドのグループ(= プロセス)の番号(getpid)。最初のスレッドの番号
    pub tgid: Pid,
    /// スレッドが終わるときに 0 を書いて futex で起こす場所(set_tid_address、CLONE_CHILD_CLEARTID)
    pub clear_child_tid: u64,
    /// プログラムの名前(/proc/<pid>/status の Name)
    pub name: String,
    /// 実行しているプログラムのファイル(リンクをたどり切った絶対パス。/proc/<pid>/exe)。
    /// カーネルが直接作り、まだ execve していないものは空
    pub exe: String,
    /// 親のプロセス(カーネルが直接動かしたものは 0)
    pub ppid: Pid,
    pub new_space: SpaceFactory,
    pub mem: UserMemory,
    /// プログラムの入口と初期スタック
    pub entry: Loaded,
    pub fds: FdTable,
    pub cwd: String,
    pub vfs: Arc<Vfs>,
    /// 誰として動いているか(ADR-0005)
    pub cred: Credentials,
    /// シグナルのハンドラの設定(スレッドで共有する。CLONE_SIGHAND)
    pub sig_actions: Arc<SpinLock<crate::signal::Actions>>,
    /// マスクしているシグナル(スレッドごと)
    pub sig_blocked: u64,
    /// シグナル用の別のスタック(sigaltstack):(場所, フラグ, 大きさ)。大きさ 0 はなし
    pub sig_altstack: (u64, i32, u64),
    /// rt_sigsuspend で差し替える前のマスク(ハンドラから戻るときに、これに戻す)
    pub saved_mask: Option<u64>,
    /// シグナルで EINTR になったシステムコール:(番号, 元の x0)。SA_RESTART ならやり直す
    pub restart: Option<(u64, u64)>,
    /// リアルタイム優先度(0 = 通常 SCHED_OTHER、1〜99 = SCHED_FIFO)
    pub rt_priority: u8,
    /// 作るときに消す権限のビット(umask)
    pub umask: u32,
}

impl Process {
    /// `stdio` を fd 0, 1, 2(標準入力・出力・エラー出力)にしたプロセスを作る。
    pub fn new(mut mem: UserMemory, entry: Loaded, vfs: Arc<Vfs>, stdio: Arc<dyn File>) -> Self {
        mem.set_brk(entry.brk);
        mem.set_mmap_next(MMAP_BASE);
        let mut fds = FdTable::new();
        for _ in 0..3 {
            fds.insert(OpenFile::new(stdio.clone(), O_RDWR))
                .expect("empty fd table");
        }
        Self {
            pid: 0,
            tgid: 0,
            clear_child_tid: 0,
            name: String::new(),
            exe: String::new(),
            ppid: 0,
            new_space: no_space,
            mem,
            entry,
            fds,
            cwd: String::from("/"),
            vfs,
            cred: Credentials::root(),
            sig_actions: Arc::new(SpinLock::new(crate::signal::Actions::default())),
            sig_blocked: 0,
            sig_altstack: (0, 0, 0),
            saved_mask: None,
            restart: None,
            rt_priority: 0,
            umask: 0o022,
        }
    }

    /// マスクを変える(SIGKILL と SIGSTOP は外す)。待ちを中断するかを判断するため、プロセスの
    /// 表にも写しを置く(システムコールの途中は、`Process` が表から取り出されているので)
    pub fn set_sig_blocked(&mut self, mask: u64) {
        self.sig_blocked = mask & !crate::signal::UNBLOCKABLE;
        PROCESSES.set_blocked(self.pid, self.sig_blocked);
    }

    /// 同じグループのスレッドを作る(clone の CLONE_VM | CLONE_FILES | CLONE_THREAD)。
    /// メモリと fd の表は共有し、親(wait4 で待つのは親のプロセスの親)も同じ
    pub fn thread(&self) -> Process {
        Process {
            pid: 0,
            tgid: self.tgid,
            clear_child_tid: 0,
            name: self.name.clone(),
            exe: self.exe.clone(),
            ppid: self.ppid,
            new_space: self.new_space,
            mem: self.mem.share(),
            entry: self.entry,
            fds: self.fds.share(),
            cwd: self.cwd.clone(),
            vfs: self.vfs.clone(),
            sig_actions: self.sig_actions.clone(),
            sig_blocked: self.sig_blocked,
            sig_altstack: (0, 0, 0), // 別のスタックはスレッドごと(新しいスレッドは持たない)
            saved_mask: None,
            restart: None,
            cred: self.cred.clone(),
            rt_priority: self.rt_priority,
            umask: self.umask,
        }
    }

    /// 自分の複製を作る(fork)。メモリは写し、開いたファイルは共有する。
    pub fn fork(&mut self) -> Result<Process, crate::Errno> {
        let space = (self.new_space)()?;
        let mem = self.mem.fork(space)?;
        Ok(Process {
            pid: 0,
            tgid: 0, // 新しいグループ(番号が決まったら、それと同じにする)
            clear_child_tid: 0,
            name: self.name.clone(),
            exe: self.exe.clone(),
            ppid: self.pid,
            new_space: self.new_space,
            mem,
            entry: self.entry,
            fds: self.fds.clone(),
            cwd: self.cwd.clone(),
            vfs: self.vfs.clone(),
            sig_actions: Arc::new(SpinLock::new(self.sig_actions.lock().clone())),
            sig_blocked: self.sig_blocked,
            sig_altstack: self.sig_altstack, // fork は受け継ぐ
            saved_mask: None,
            restart: None,
            cred: self.cred.clone(),
            rt_priority: self.rt_priority,
            umask: self.umask,
        })
    }
}

/// プロセスの番号(タスクの番号と同じ)
pub type Pid = crate::sched::Tid;

/// プロセスの表。動いているプロセスと、終わって終了ステータスだけが残ったもの(ゾンビ)を持つ。
/// 子が終わるのを待つ(wait4)タスクの待ち行列
pub static CHILD_WQ: crate::wait::WaitQueue = crate::wait::WaitQueue::new();

/// 表から取り出されている間も引ける、スレッドの情報
struct ThreadInfo {
    ppid: Pid,
    name: String,
    /// スレッドのグループ(= プロセス)
    tgid: Pid,
    /// 実行しているプログラムのファイル(空ならなし)
    exe: String,
}

pub struct ProcessTable {
    live: SpinLock<BTreeMap<Pid, Process>>,
    /// 動いているスレッドの情報。システムコールで取り出されている間も
    /// 引ける(/proc 用、グループのスレッドを探すため)
    info: SpinLock<BTreeMap<Pid, ThreadInfo>>,
    /// 保留中のシグナル(スレッドごとのビットの集まり。ユーザーに戻る直前に配る)。
    /// システムコールの途中で表から取り出されているスレッドにも送れるように、表とは別に持つ
    signals: SpinLock<BTreeMap<Pid, u64>>,
    /// スレッドのマスクの写し(`Process::set_sig_blocked` が書く)
    masks: SpinLock<BTreeMap<Pid, u64>>,
    /// 終わったプロセス:番号 → (親の番号, 終わり方)
    zombies: SpinLock<BTreeMap<Pid, (Pid, ExitStatus)>>,
}

impl ProcessTable {
    pub const fn new() -> Self {
        Self {
            live: SpinLock::new(BTreeMap::new()),
            info: SpinLock::new(BTreeMap::new()),
            signals: SpinLock::new(BTreeMap::new()),
            masks: SpinLock::new(BTreeMap::new()),
            zombies: SpinLock::new(BTreeMap::new()),
        }
    }

    pub fn insert(&self, pid: Pid, mut p: Process) {
        if p.tgid == 0 {
            p.tgid = pid; // グループがまだない:スレッドが 1 つのプロセス
        }
        self.info.lock().insert(
            pid,
            ThreadInfo {
                ppid: p.ppid,
                name: p.name.clone(),
                tgid: p.tgid,
                exe: p.exe.clone(),
            },
        );
        self.masks.lock().insert(pid, p.sig_blocked);
        self.live.lock().insert(pid, p);
    }

    pub fn add_info(&self, pid: Pid, ppid: Pid, name: &str) {
        self.info.lock().insert(
            pid,
            ThreadInfo {
                ppid,
                name: String::from(name),
                tgid: pid,
                exe: String::new(),
            },
        );
    }

    /// 実行しているプログラムのファイルを変える(execve のとき)
    pub fn set_exe(&self, pid: Pid, exe: &str) {
        if let Some(info) = self.info.lock().get_mut(&pid) {
            info.exe = String::from(exe);
        }
    }

    /// 実行しているプログラムのファイル(/proc/<pid>/exe)。なければ None
    pub fn exe_of(&self, pid: Pid) -> Option<String> {
        self.info
            .lock()
            .get(&pid)
            .map(|i| i.exe.clone())
            .filter(|e| !e.is_empty())
    }

    /// グループ `tgid` のスレッドの番号
    pub fn threads_of(&self, tgid: Pid) -> alloc::vec::Vec<Pid> {
        self.info
            .lock()
            .iter()
            .filter(|(_, i)| i.tgid == tgid)
            .map(|(pid, _)| *pid)
            .collect()
    }

    /// スレッド `tid` にシグナルを送る(保留にして、眠っていれば起こす)
    pub fn send(&self, tid: Pid, sig: u32) {
        *self.signals.lock().entry(tid).or_default() |= crate::signal::bit(sig);
        crate::sched::wake(tid);
        crate::wait::POLL_WQ.wake_all();
    }

    /// プロセス(スレッドのグループ `tgid`)に送る。受け取るのは最初のスレッド(いなければほかの
    /// スレッド)。`sig` が 0 なら、いるかを確かめるだけ。いなければ ESRCH
    pub fn send_group(&self, tgid: Pid, sig: u32) -> Result<(), crate::Errno> {
        let threads = self.threads_of(tgid);
        let target = if threads.contains(&tgid) {
            tgid
        } else {
            *threads.first().ok_or(crate::Errno::ESRCH)?
        };
        if sig != 0 {
            self.send(target, sig);
        }
        Ok(())
    }

    /// スレッド `tid` のグループ
    pub fn group_of(&self, tid: Pid) -> Option<Pid> {
        self.info.lock().get(&tid).map(|i| i.tgid)
    }

    /// 保留中のシグナル(マスクされたものも)
    pub fn pending(&self, tid: Pid) -> u64 {
        self.signals.lock().get(&tid).copied().unwrap_or(0)
    }

    /// マスクされていない保留中のシグナルを 1 つ取り出す(いちばん小さい番号)
    pub fn take_signal(&self, tid: Pid, blocked: u64) -> Option<u32> {
        let mut signals = self.signals.lock();
        let pending = signals.get_mut(&tid)?;
        let sig = crate::signal::next(*pending, blocked)?;
        *pending &= !crate::signal::bit(sig);
        Some(sig)
    }

    /// マスクの写しを書く
    pub fn set_blocked(&self, tid: Pid, mask: u64) {
        self.masks.lock().insert(tid, mask);
    }

    /// 待っているスレッド `tid` を中断するか(マスクされていない保留中のシグナルがある。マスクは写しで見る)
    pub fn interrupted(&self, tid: Pid) -> bool {
        let blocked = self.masks.lock().get(&tid).copied().unwrap_or(0);
        self.has_signal(tid, blocked)
    }

    /// マスクされていない保留中のシグナルがあるか(待っているシステムコールを中断するか)
    pub fn has_signal(&self, tid: Pid, blocked: u64) -> bool {
        crate::signal::next(self.pending(tid), blocked).is_some()
    }

    /// 名前を変える(execve のとき)
    pub fn rename(&self, pid: Pid, name: &str) {
        if let Some(info) = self.info.lock().get_mut(&pid) {
            info.name = String::from(name);
        }
    }

    /// 動いているプロセスの一覧:(番号, 親, 名前)
    pub fn infos(&self) -> alloc::vec::Vec<(Pid, Pid, String)> {
        self.info
            .lock()
            .iter()
            .map(|(pid, i)| (*pid, i.ppid, i.name.clone()))
            .collect()
    }

    pub fn contains(&self, pid: Pid) -> bool {
        self.live.lock().contains_key(&pid)
    }

    /// プロセスを表から取り出す。システムコールの間は表のロックを持たずに済む
    /// (待つシステムコールの間に、ほかのタスクが表を使えるように)。
    pub fn take(&self, pid: Pid) -> Option<Process> {
        self.live.lock().remove(&pid)
    }

    pub fn put_back(&self, pid: Pid, p: Process) {
        self.live.lock().insert(pid, p);
    }

    /// 表に入ったままのプロセスに `f` を使う。
    pub fn with<R>(&self, pid: Pid, f: impl FnOnce(&mut Process) -> R) -> Option<R> {
        self.live.lock().get_mut(&pid).map(f)
    }

    /// スレッドを終える。メモリはここで返し(ほかのスレッドが使っていなければ)、グループの
    /// 最初のスレッドなら終わり方を残す(親が wait4 で受け取る)。ほかのスレッドは何も残さない
    pub fn exit(&self, pid: Pid, status: ExitStatus) {
        let info = self.info.lock().remove(&pid);
        self.signals.lock().remove(&pid);
        self.masks.lock().remove(&pid);
        let process = self.live.lock().remove(&pid);
        // 表から取り出されたまま終わることもあるので、親は info からも引く
        let ppid = process
            .as_ref()
            .map(|p| p.ppid)
            .or(info.as_ref().map(|i| i.ppid))
            .unwrap_or(0);
        let tgid = process
            .as_ref()
            .map(|p| p.tgid)
            .or(info.as_ref().map(|i| i.tgid))
            .unwrap_or(pid);
        drop(process);
        // exit_group で、終わり方はもう残してある
        if tgid == pid && !self.zombies.lock().contains_key(&pid) {
            self.add_zombie(pid, ppid, status);
        }
    }

    /// スレッドのグループ全体を終える(exit_group)。終わり方をすぐ残し、ほかのスレッドには
    /// 強制終了の印を付けて起こす(眠っていれば、目を覚まして印に気づく)。呼んだスレッドは
    /// このあと自分で `exit` する
    pub fn exit_group(&self, me: Pid, tgid: Pid, status: ExitStatus) {
        let ppid = self.info.lock().get(&tgid).map_or(0, |i| i.ppid);
        if !self.zombies.lock().contains_key(&tgid) {
            self.add_zombie(tgid, ppid, status);
        }
        for t in self.threads_of(tgid) {
            if t != me {
                self.send(t, crate::signal::SIGKILL);
            }
        }
    }

    pub fn add_zombie(&self, pid: Pid, ppid: Pid, status: ExitStatus) {
        self.zombies.lock().insert(pid, (ppid, status));
        CHILD_WQ.wake_all(); // wait4 で待っている親
        // 親に SIGCHLD(既定は無視。シェルはハンドラで受ける)
        if ppid != 0 {
            let _ = self.send_group(ppid, crate::signal::SIGCHLD);
        }
    }

    /// 終わっていれば終わり方を返し、ゾンビを片付ける。
    pub fn try_wait(&self, pid: Pid) -> Option<ExitStatus> {
        self.zombies.lock().remove(&pid).map(|(_, s)| s)
    }

    /// `ppid` の子(`pid` が Some ならその子だけ)のうち、終わったものを1つ片付けて返す。
    /// まだ終わっていなければ `Ok(None)`、当てはまる子がいなければ `ECHILD`。
    pub fn wait_child(
        &self,
        ppid: Pid,
        pid: Option<Pid>,
    ) -> Result<Option<(Pid, ExitStatus)>, crate::Errno> {
        let matches = |child: Pid| pid.is_none_or(|p| p == child);
        {
            let mut zombies = self.zombies.lock();
            let found = zombies
                .iter()
                .find(|(child, (parent, _))| *parent == ppid && matches(**child))
                .map(|(child, _)| *child);
            if let Some(child) = found {
                let (_, status) = zombies.remove(&child).expect("found above");
                return Ok(Some((child, status)));
            }
        }
        // 子はシステムコールの途中で表(live)の外にいることがあるので、取り出している間も
        // 残る info(番号 → 親)で調べる
        let alive = self
            .info
            .lock()
            .iter()
            // 子のプロセスのスレッドも親は同じなので、グループの最初のスレッドだけを数える
            .any(|(child, i)| i.ppid == ppid && i.tgid == *child && matches(*child));
        // 番号を指定されたときは、スケジューラのタスクとして生きているかでも確かめる
        let alive = alive || pid.is_some_and(crate::sched::is_alive);
        if alive {
            Ok(None)
        } else {
            Err(crate::Errno::ECHILD)
        }
    }
}

impl Default for ProcessTable {
    fn default() -> Self {
        Self::new()
    }
}

/// カーネル全体のプロセスの表。
pub static PROCESSES: ProcessTable = ProcessTable::new();

/// ユーザー空間に戻る直前に必ず呼ばれる(CLAUDE.md:戻る直前の処理は1か所にまとめる)。
///
/// プリエンプション(タイマで印が付いていれば、ほかのタスクに譲る)をここで行う。
/// シグナルの配送とトレースも、後でここに足す。
pub fn before_return_to_user() {
    if crate::sched::take_resched() {
        crate::sched::yield_now();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::syscall::tests::process;

    #[test]
    fn a_registered_process_is_used_by_with_process() {
        let table = ProcessTable::new();
        let (p, _) = process();
        table.insert(5, p);
        assert_eq!(
            table.with(5, |p| p.cwd.clone()),
            Some(alloc::string::String::from("/"))
        );
        assert_eq!(table.with(6, |_| ()), None);
    }

    #[test]
    fn exit_turns_a_process_into_a_zombie_that_wait_collects_once() {
        let table = ProcessTable::new();
        let (p, _) = process();
        table.insert(5, p);
        assert_eq!(table.try_wait(5), None, "still running");
        table.exit(5, ExitStatus::Code(42));
        assert!(!table.contains(5), "memory is released at exit");
        assert_eq!(table.try_wait(5), Some(ExitStatus::Code(42)));
        assert_eq!(table.try_wait(5), None, "collected only once");
    }

    #[test]
    fn take_and_put_back_lets_a_syscall_run_without_holding_the_table() {
        let table = ProcessTable::new();
        let (p, _) = process();
        table.insert(1, p);
        let mut p = table.take(1).unwrap();
        assert!(!table.contains(1));
        p.cwd = alloc::string::String::from("/etc");
        table.put_back(1, p);
        assert_eq!(table.with(1, |p| p.cwd.clone()).unwrap(), "/etc");
    }

    #[test]
    fn a_child_in_the_middle_of_a_syscall_still_counts_as_a_child() {
        // 子が自分の子を待っている間(表から取り出されている間)も、親の wait4(-1) は
        // 「子がいない(ECHILD)」ではなく「まだ終わっていない」にならないといけない
        let table = ProcessTable::new();
        let (mut child, _) = process();
        child.ppid = 1;
        table.insert(2, child);
        let taken = table.take(2).unwrap();
        assert_eq!(table.wait_child(1, None), Ok(None));
        // 取り出されたまま終わっても、親が分かる
        drop(taken);
        table.exit(2, ExitStatus::Code(3));
        assert_eq!(
            table.wait_child(1, None),
            Ok(Some((2, ExitStatus::Code(3))))
        );
        assert_eq!(table.wait_child(1, None), Err(crate::Errno::ECHILD));
    }
}
