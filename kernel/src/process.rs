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
    pub pid: Pid,
    /// プログラムの名前(/proc/<pid>/status の Name)
    pub name: String,
    /// 親のプロセス(カーネルが直接動かしたものは 0)
    pub ppid: Pid,
    pub new_space: SpaceFactory,
    pub mem: UserMemory,
    /// プログラムの入口と初期スタック
    pub entry: Loaded,
    /// ヒープ(brk)の今の終わり
    pub brk: u64,
    /// 次に mmap で使うアドレス
    pub mmap_next: u64,
    pub fds: FdTable,
    pub cwd: String,
    pub vfs: Arc<Vfs>,
    /// 誰として動いているか(ADR-0005)
    pub cred: Credentials,
    /// 保留中のシグナル(今は強制終了だけ。ユーザーに戻る直前に処理する)
    pub pending_kill: Option<i32>,
    /// リアルタイム優先度(0 = 通常 SCHED_OTHER、1〜99 = SCHED_FIFO)
    pub rt_priority: u8,
    /// 作るときに消す権限のビット(umask)
    pub umask: u32,
}

impl Process {
    /// `stdio` を fd 0, 1, 2(標準入力・出力・エラー出力)にしたプロセスを作る。
    pub fn new(mem: UserMemory, entry: Loaded, vfs: Arc<Vfs>, stdio: Arc<dyn File>) -> Self {
        let mut fds = FdTable::new();
        for _ in 0..3 {
            fds.insert(OpenFile::new(stdio.clone(), O_RDWR))
                .expect("empty fd table");
        }
        Self {
            pid: 0,
            name: String::new(),
            ppid: 0,
            new_space: no_space,
            mem,
            entry,
            brk: entry.brk,
            mmap_next: MMAP_BASE,
            fds,
            cwd: String::from("/"),
            vfs,
            cred: Credentials::root(),
            pending_kill: None,
            rt_priority: 0,
            umask: 0o022,
        }
    }

    /// 自分の複製を作る(fork)。メモリは写し、開いたファイルは共有する。
    pub fn fork(&mut self) -> Result<Process, crate::Errno> {
        let space = (self.new_space)()?;
        let mem = self.mem.fork(space)?;
        Ok(Process {
            pid: 0,
            name: self.name.clone(),
            ppid: self.pid,
            new_space: self.new_space,
            mem,
            entry: self.entry,
            brk: self.brk,
            mmap_next: self.mmap_next,
            fds: self.fds.clone(),
            cwd: self.cwd.clone(),
            vfs: self.vfs.clone(),
            cred: self.cred.clone(),
            pending_kill: None,
            rt_priority: self.rt_priority,
            umask: self.umask,
        })
    }
}

/// プロセスの番号(タスクの番号と同じ)
pub type Pid = crate::sched::Tid;

/// プロセスの表。動いているプロセスと、終わって終了ステータスだけが残ったもの(ゾンビ)を持つ。
pub struct ProcessTable {
    live: SpinLock<BTreeMap<Pid, Process>>,
    /// 動いているプロセスの (親, 名前)。システムコールで取り出されている間も引ける(/proc 用)
    info: SpinLock<BTreeMap<Pid, (Pid, String)>>,
    /// 終わったプロセス:番号 → (親の番号, 終わり方)
    zombies: SpinLock<BTreeMap<Pid, (Pid, ExitStatus)>>,
}

impl ProcessTable {
    pub const fn new() -> Self {
        Self {
            live: SpinLock::new(BTreeMap::new()),
            info: SpinLock::new(BTreeMap::new()),
            zombies: SpinLock::new(BTreeMap::new()),
        }
    }

    pub fn insert(&self, pid: Pid, p: Process) {
        self.add_info(pid, p.ppid, &p.name);
        self.live.lock().insert(pid, p);
    }

    pub fn add_info(&self, pid: Pid, ppid: Pid, name: &str) {
        self.info.lock().insert(pid, (ppid, String::from(name)));
    }

    /// 名前を変える(execve のとき)
    pub fn rename(&self, pid: Pid, name: &str) {
        if let Some(entry) = self.info.lock().get_mut(&pid) {
            entry.1 = String::from(name);
        }
    }

    /// 動いているプロセスの一覧:(番号, 親, 名前)
    pub fn infos(&self) -> alloc::vec::Vec<(Pid, Pid, String)> {
        self.info
            .lock()
            .iter()
            .map(|(pid, (ppid, name))| (*pid, *ppid, name.clone()))
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

    /// プロセスを終える。メモリはここで返し、終わり方だけを残す。
    pub fn exit(&self, pid: Pid, status: ExitStatus) {
        let info = self.info.lock().remove(&pid);
        let process = self.live.lock().remove(&pid);
        // 表から取り出されたまま終わることもあるので、親は info からも引く
        let ppid = process
            .as_ref()
            .map(|p| p.ppid)
            .or(info.map(|(ppid, _)| ppid))
            .unwrap_or(0);
        drop(process);
        self.add_zombie(pid, ppid, status);
    }

    pub fn add_zombie(&self, pid: Pid, ppid: Pid, status: ExitStatus) {
        self.zombies.lock().insert(pid, (ppid, status));
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
            .any(|(child, (parent, _))| *parent == ppid && matches(*child));
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
