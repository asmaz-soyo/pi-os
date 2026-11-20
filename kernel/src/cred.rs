//! 資格情報(誰として動いているか)と、ファイルの権限の確認(ADR-0005)。
//!
//! Linux と同じ形:プロセスは実 / 実効のユーザー ID とグループ ID、補助グループを持つ。
//! 権限の確認には実効 ID を使う。uid 0(root)は読み書きの確認を通り抜ける。

use crate::Errno;
use crate::fs::vfs::Stat;
use alloc::vec::Vec;
use core::ops::BitOr;

/// 求める操作(読み・書き・実行の組み合わせ)。値はパーミッションのビットと同じ。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Access(u32);

impl Access {
    pub const READ: Self = Self(0o4);
    pub const WRITE: Self = Self(0o2);
    pub const EXEC: Self = Self(0o1);
    pub const NONE: Self = Self(0);

    pub fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }
}

impl BitOr for Access {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Credentials {
    pub uid: u32,
    pub euid: u32,
    pub gid: u32,
    pub egid: u32,
    /// 補助グループ
    pub groups: Vec<u32>,
}

impl Credentials {
    pub fn root() -> Self {
        Self::user(0, 0)
    }

    pub fn user(uid: u32, gid: u32) -> Self {
        Self {
            uid,
            euid: uid,
            gid,
            egid: gid,
            groups: Vec::new(),
        }
    }

    fn in_group(&self, gid: u32) -> bool {
        self.egid == gid || self.groups.contains(&gid)
    }

    /// `stat` のファイルに `want` の操作をしてよいか。
    pub fn check(&self, stat: &Stat, want: Access) -> Result<(), Errno> {
        if self.euid == 0 {
            // root でも、実行はどれかの実行ビットが立っているときだけ
            let any_exec = stat.mode & 0o111 != 0;
            return if !want.contains(Access::EXEC) || any_exec {
                Ok(())
            } else {
                Err(Errno::EACCES)
            };
        }
        // 所有者 → グループ → その他の順に、最初に当てはまった3ビットだけを見る
        let bits = if self.euid == stat.uid {
            stat.mode >> 6
        } else if self.in_group(stat.gid) {
            stat.mode >> 3
        } else {
            stat.mode
        } & 0o7;
        if Access(bits).contains(want) {
            Ok(())
        } else {
            Err(Errno::EACCES)
        }
    }
}

impl Default for Credentials {
    fn default() -> Self {
        Self::root()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Errno;
    use crate::fs::vfs::{FileKind, Stat};

    fn file(mode: u32, uid: u32, gid: u32) -> Stat {
        Stat {
            kind: FileKind::Regular,
            mode,
            size: 0,
            ino: 1,
            uid,
            gid,
        }
    }

    fn user(uid: u32, gid: u32) -> Credentials {
        Credentials::user(uid, gid)
    }

    const R: Access = Access::READ;
    const W: Access = Access::WRITE;
    const X: Access = Access::EXEC;

    #[test]
    fn owner_group_and_other_bits_are_used_in_that_order() {
        let f = file(0o640, 1000, 100);
        assert_eq!(user(1000, 100).check(&f, R | W), Ok(()), "owner: rw-");
        assert_eq!(user(1000, 100).check(&f, X), Err(Errno::EACCES));
        assert_eq!(user(2000, 100).check(&f, R), Ok(()), "group: r--");
        assert_eq!(user(2000, 100).check(&f, W), Err(Errno::EACCES));
        assert_eq!(
            user(3000, 300).check(&f, R),
            Err(Errno::EACCES),
            "other: ---"
        );
    }

    #[test]
    fn owner_bits_win_even_when_group_bits_would_allow() {
        // 所有者には読みを許さず、グループには許す。所有者本人は読めない(Linux と同じ)
        let f = file(0o040, 1000, 100);
        assert_eq!(user(1000, 100).check(&f, R), Err(Errno::EACCES));
        assert_eq!(user(2000, 100).check(&f, R), Ok(()));
    }

    #[test]
    fn supplementary_groups_count_as_group_membership() {
        let f = file(0o040, 0, 50);
        let mut c = user(1000, 100);
        assert_eq!(c.check(&f, R), Err(Errno::EACCES));
        c.groups.push(50);
        assert_eq!(c.check(&f, R), Ok(()));
    }

    #[test]
    fn root_bypasses_read_and_write_but_needs_some_exec_bit() {
        let root = Credentials::root();
        assert_eq!(root.check(&file(0o000, 1000, 100), R | W), Ok(()));
        assert_eq!(root.check(&file(0o644, 1000, 100), X), Err(Errno::EACCES));
        assert_eq!(root.check(&file(0o010, 1000, 100), X), Ok(()));
    }

    #[test]
    fn effective_ids_decide_access() {
        let mut c = user(1000, 100);
        c.euid = 0;
        assert_eq!(
            c.check(&file(0o000, 5, 5), R),
            Ok(()),
            "euid 0 acts as root"
        );
    }
}
