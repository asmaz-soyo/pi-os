//! プロセス(ユーザープログラムの実行単位)。
//!
//! フェーズ1では同時に動くプロセスは1つだけ(qjs は1プロセスで動く)。
//! マルチタスクはフェーズ2で、ここにスケジューラを足す。

use crate::cred::Credentials;
use crate::exec::Loaded;
use crate::fs::vfs::{FdTable, File, O_RDWR, OpenFile, Vfs};
use crate::mm::user::UserMemory;
use crate::sync::SpinLock;
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
        }
    }
}

/// 今動いているプロセス。
pub static CURRENT: SpinLock<Option<Process>> = SpinLock::new(None);

/// ユーザー空間に戻る直前に必ず呼ばれる(CLAUDE.md:戻る直前の処理は1か所にまとめる)。
///
/// シグナルの配送、プリエンプション(タスクの切り替え)、トレースは、後でここに足す。
pub fn before_return_to_user() {}
