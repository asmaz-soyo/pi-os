//! MBR(マスターブートレコード)のパーティション表。
//!
//! ディスクの先頭 512 バイトの、446 バイト目から 16 バイト × 4 個がパーティション表。
//! 最後の 2 バイトは印 `0x55 0xAA`。Raspberry Pi の SD カードは「p1:FAT32(`/boot`)、
//! p2:Linux のファイルシステム」の形が多い。

use alloc::vec::Vec;

/// パーティションの種類:FAT32(LBA)
pub const KIND_FAT32_LBA: u8 = 0x0c;
/// パーティションの種類:Linux
pub const KIND_LINUX: u8 = 0x83;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Partition {
    /// 種類(0x0c = FAT32、0x83 = Linux など)
    pub kind: u8,
    /// 最初のブロック(LBA)
    pub start: u32,
    /// ブロックの数
    pub count: u32,
}

/// 先頭のセクタからパーティションの一覧を読む。MBR でなければ `None`。
pub fn parse(sector: &[u8; 512]) -> Option<Vec<Partition>> {
    if sector[510] != 0x55 || sector[511] != 0xaa {
        return None;
    }
    Some(
        (0..4)
            .map(|i| &sector[446 + i * 16..446 + i * 16 + 16])
            .map(|e| Partition {
                kind: e[4],
                start: u32::from_le_bytes(e[8..12].try_into().unwrap()),
                count: u32::from_le_bytes(e[12..16].try_into().unwrap()),
            })
            .filter(|p| p.kind != 0 && p.count != 0)
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// パーティション表の 1 項目(16 バイト)を作る
    fn entry(kind: u8, start: u32, count: u32) -> [u8; 16] {
        let mut e = [0u8; 16];
        e[4] = kind;
        e[8..12].copy_from_slice(&start.to_le_bytes());
        e[12..16].copy_from_slice(&count.to_le_bytes());
        e
    }

    fn mbr(entries: &[[u8; 16]]) -> [u8; 512] {
        let mut s = [0u8; 512];
        for (i, e) in entries.iter().enumerate() {
            s[446 + i * 16..446 + i * 16 + 16].copy_from_slice(e);
        }
        s[510] = 0x55;
        s[511] = 0xaa;
        s
    }

    #[test]
    fn parses_the_partitions_of_a_raspberry_pi_card() {
        // p1:FAT32(LBA、0x0c)、p2:Linux(0x83)
        let sector = mbr(&[entry(0x0c, 8192, 524288), entry(0x83, 532480, 2_000_000)]);
        let parts = parse(&sector).unwrap();
        assert_eq!(
            parts,
            [
                Partition {
                    kind: 0x0c,
                    start: 8192,
                    count: 524288
                },
                Partition {
                    kind: 0x83,
                    start: 532480,
                    count: 2_000_000
                },
            ]
        );
    }

    #[test]
    fn empty_entries_are_skipped() {
        let sector = mbr(&[entry(0, 0, 0), entry(0x83, 2048, 100)]);
        assert_eq!(parse(&sector).unwrap().len(), 1);
    }

    #[test]
    fn a_sector_without_the_signature_is_not_an_mbr() {
        let mut sector = mbr(&[entry(0x83, 2048, 100)]);
        sector[511] = 0;
        assert_eq!(parse(&sector), None);
    }
}
