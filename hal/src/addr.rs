//! 物理アドレスと仮想アドレス。取り違えないよう別の型にする。

use core::ops::{Add, Sub};

/// ページの大きさ(4KiB)。
pub const PAGE_SIZE: u64 = 4096;

macro_rules! address {
    ($name:ident, $doc:literal) => {
        #[doc = $doc]
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
        pub struct $name(pub u64);

        impl $name {
            pub const fn as_u64(self) -> u64 {
                self.0
            }
            pub const fn align_down(self) -> Self {
                Self(self.0 & !(PAGE_SIZE - 1))
            }
            pub const fn align_up(self) -> Self {
                Self((self.0 + PAGE_SIZE - 1) & !(PAGE_SIZE - 1))
            }
            pub const fn is_aligned(self) -> bool {
                self.0 % PAGE_SIZE == 0
            }
        }

        impl Add<u64> for $name {
            type Output = Self;
            fn add(self, rhs: u64) -> Self {
                Self(self.0 + rhs)
            }
        }

        impl Sub for $name {
            type Output = u64;
            fn sub(self, rhs: Self) -> u64 {
                self.0 - rhs.0
            }
        }

        impl core::fmt::LowerHex for $name {
            fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
                core::fmt::LowerHex::fmt(&self.0, f)
            }
        }
    };
}

address!(PhysAddr, "物理アドレス(RAM やデバイスの本当の番地)。");
address!(VirtAddr, "仮想アドレス(MMU を通して CPU が使う番地)。");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_alignment_helpers() {
        assert_eq!(PhysAddr(0x1234).align_down(), PhysAddr(0x1000));
        assert_eq!(PhysAddr(0x1234).align_up(), PhysAddr(0x2000));
        assert_eq!(PhysAddr(0x2000).align_up(), PhysAddr(0x2000));
        assert!(PhysAddr(0x3000).is_aligned());
        assert!(!VirtAddr(0x3001).is_aligned());
    }

    #[test]
    fn addresses_support_offset_arithmetic() {
        assert_eq!(PhysAddr(0x1000) + 0x20, PhysAddr(0x1020));
        assert_eq!(PhysAddr(0x3000) - PhysAddr(0x1000), 0x2000);
        assert_eq!(VirtAddr(0x10) + 0x10, VirtAddr(0x20));
    }
}
