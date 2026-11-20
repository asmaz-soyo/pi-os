//! プロセス(ユーザープログラムの実行単位)。
//!
//! フェーズ1では同時に動くプロセスは1つだけ(qjs は1プロセスで動く)。
//! マルチタスクはフェーズ2で、ここにスケジューラを足す。

use crate::exec::Loaded;
use crate::mm::user::UserMemory;
use crate::sync::SpinLock;
use alloc::boxed::Box;
use hal::console::Console;

pub struct Process {
    pub mem: UserMemory,
    /// プログラムの入口と初期スタック
    pub entry: Loaded,
    /// ヒープ(brk)の今の終わり
    pub brk: u64,
    /// 標準出力・標準エラー出力の行き先(VFS ができたら fd の表に置き換える)
    pub stdout: Box<dyn Console + Send>,
}

impl Process {
    pub fn new(mem: UserMemory, entry: Loaded, stdout: Box<dyn Console + Send>) -> Self {
        Self {
            mem,
            entry,
            brk: entry.brk,
            stdout,
        }
    }
}

/// 今動いているプロセス。
pub static CURRENT: SpinLock<Option<Process>> = SpinLock::new(None);

/// ユーザー空間に戻る直前に必ず呼ばれる(CLAUDE.md:戻る直前の処理は1か所にまとめる)。
///
/// シグナルの配送、プリエンプション(タスクの切り替え)、トレースは、後でここに足す。
pub fn before_return_to_user() {}
