//! シーケンス番号(32 ビットで一周する)。
//!
//! 番号は 2^32 で 0 に戻るので、普通の `<` では比べられない(0xffff_fff0 の 32 先は 0x10)。
//! RFC 9293 と同じく「差を符号付きで見る」:`a < b` は `(a - b) as i32 < 0`。
//! 2 つの番号の差が 2^31 より小さいあいだは正しく比べられる(ウィンドウはそれよりずっと小さい)。

use core::cmp::Ordering;
use core::ops::{Add, Sub};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Seq(pub u32);

impl Add<u32> for Seq {
    type Output = Seq;
    fn add(self, n: u32) -> Seq {
        Seq(self.0.wrapping_add(n))
    }
}

/// 2 つの番号の距離(`a - b`。a が b より先にあるときに使う)
impl Sub for Seq {
    type Output = u32;
    fn sub(self, other: Seq) -> u32 {
        self.0.wrapping_sub(other.0)
    }
}

impl PartialOrd for Seq {
    fn partial_cmp(&self, other: &Seq) -> Option<Ordering> {
        // 差を符号付きにすると、半周より手前は負、先は正になる
        Some((self.0.wrapping_sub(other.0) as i32).cmp(&0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_compare_across_the_wrap() {
        let near_end = Seq(0xffff_fff0);
        let after = near_end + 0x20;
        assert_eq!(after, Seq(0x10));
        assert!(near_end < after);
        assert!(after > near_end);
        assert!(near_end <= after && after >= near_end);
        assert_eq!(after - near_end, 0x20);
        // 半周(2^31)より近ければ、順番どおり
        assert!(Seq(5) < Seq(5) + 0x7fff_ffff);
    }
}
