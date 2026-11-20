//! 起動時に実行するコマンドの一覧(`/etc/init.rc`)。
//!
//! 1 行に 1 つのコマンド(プログラムのパスと引数を空白で区切る)。`#` から行末まではコメント。
//! フェーズ1には fork / exec がないので、カーネルが上から順に1つずつ実行する。

use alloc::vec::Vec;

/// `init.rc` の中身をコマンド(引数の並び)の一覧にする。
pub fn parse(script: &str) -> Vec<Vec<&str>> {
    script
        .lines()
        .map(|line| line.split('#').next().unwrap_or(""))
        .map(|line| line.split_whitespace().collect::<Vec<_>>())
        .filter(|args| !args.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_line_is_a_command_with_arguments() {
        let script = "# 起動時に実行する\n/bin/hello\n\n/bin/qjs  /hello.js # JS\n   \n";
        assert_eq!(
            parse(script),
            [&["/bin/hello"][..], &["/bin/qjs", "/hello.js"][..]]
        );
    }

    #[test]
    fn empty_script_has_no_commands() {
        assert!(parse("").is_empty());
        assert!(parse("# only a comment\n").is_empty());
    }
}
