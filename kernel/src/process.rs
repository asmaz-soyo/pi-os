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

pub struct Process {
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
            mem,
            entry,
            brk: entry.brk,
            mmap_next: MMAP_BASE,
            fds,
            cwd: String::from("/"),
            vfs,
            cred: Credentials::root(),
            pending_kill: None,
        }
    }
}

/// プロセスの番号(タスクの番号と同じ)
pub type Pid = crate::sched::Tid;

/// プロセスの表。動いているプロセスと、終わって終了ステータスだけが残ったもの(ゾンビ)を持つ。
pub struct ProcessTable {
    live: SpinLock<BTreeMap<Pid, Process>>,
    zombies: SpinLock<BTreeMap<Pid, i32>>,
}

impl ProcessTable {
    pub const fn new() -> Self {
        Self {
            live: SpinLock::new(BTreeMap::new()),
            zombies: SpinLock::new(BTreeMap::new()),
        }
    }

    pub fn insert(&self, pid: Pid, p: Process) {
        self.live.lock().insert(pid, p);
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

    /// プロセスを終える。メモリはここで返し、終了ステータスだけを残す。
    pub fn exit(&self, pid: Pid, status: i32) {
        let process = self.live.lock().remove(&pid);
        drop(process);
        self.zombies.lock().insert(pid, status);
    }

    /// 終わっていれば終了ステータスを返し、ゾンビを片付ける。
    pub fn try_wait(&self, pid: Pid) -> Option<i32> {
        self.zombies.lock().remove(&pid)
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
        table.exit(5, 42);
        assert!(!table.contains(5), "memory is released at exit");
        assert_eq!(table.try_wait(5), Some(42));
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
}
