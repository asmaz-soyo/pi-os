//! プロセス関係のシステムコール。

/// exit / exit_group の引数を終了ステータスにする(下位 8 ビットだけが親に見える)。
pub fn exit_status(arg: u64) -> i32 {
    (arg & 0xff) as i32
}
